//! `start`/`stop`/`restart` (`docs/cli-contract.v1.md`, "`start`"/"`stop`"/
//! "`restart`") — this ticket ([#43](https://github.com/golgor/cloud-sql-tracker/issues/43))
//! on the `commands` seam (`docs/modules.v1.md`, "commands").
//!
//! Multi-target execution is **not transactional**
//! (`docs/cli-contract.v1.md`, "No transactional rollback"): every selected
//! Connection id is attempted independently, and one id's failure never
//! undoes another id's success. This module only classifies each id's
//! [`TargetResult`]; `cli` (#45) is the only place that turns a whole
//! [`BatchOutcome`] into an exit code.
//!
//! Reuses `status`'s Observation-gather + Reconcile round trip
//! (`super::status::observe_and_reconcile`) instead of a second
//! implementation — `supervisor`/`port` stay the only adapters either
//! module talks to.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::config::Config;
use crate::env;
use crate::model::{self, Connection, ErrorCode, HealthState, Source, StatusRow};
use crate::supervisor;

use super::select::{self, SelectError, Selector};
use super::status;

// ---------------------------------------------------------------------------
// Outcomes.
// ---------------------------------------------------------------------------

/// One selected Connection id's outcome from a mutating command
/// (`docs/cli-contract.v1.md`, "Multi-target execution and exit codes").
/// `cli` (#45) is the only place that turns a whole [`BatchOutcome`] into an
/// exit code — this module only classifies each id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetResult {
    /// The operation succeeded, including an idempotent no-op (already
    /// `running` via our Unit for `start`; already `stopped` for `stop`).
    Succeeded,
    /// Skipped during `--group`/`--all` expansion because the Connection is
    /// `enabled: false` (`docs/config.v1.md`, "`enabled: false`":
    /// "Multi-target: disabled connections are skipped ... they do not by
    /// themselves force exit 1/4"). Never produced by `stop` — a disabled
    /// Connection stops the same as any other id.
    SkippedDisabled,
    /// A single-id `start`/`restart` targeted a disabled Connection
    /// (`docs/config.v1.md`, "`enabled: false`": "Single-id start/restart
    /// on disabled -> exit 2"). `cli` maps this straight to the
    /// usage-error exit code, the same class as an unknown id — it is not
    /// a batch failure. Never produced by `stop`.
    RefusedDisabled,
    /// The operation was attempted and failed for this id. `code` follows
    /// the Status document's `error.code` catalog
    /// (`docs/status-document.v1.md`, "`error` object") so a mutating
    /// command's failures read the same vocabulary as `status`; `message`
    /// is a human-readable summary for stderr.
    Failed { code: ErrorCode, message: String },
}

/// One Connection id plus its [`TargetResult`], in selector order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetOutcome {
    pub(crate) id: String,
    pub(crate) result: TargetResult,
}

fn outcome_for(id: &str, result: TargetResult) -> TargetOutcome {
    TargetOutcome {
        id: id.to_string(),
        result,
    }
}

/// The result of a multi-target mutating command: one [`TargetOutcome`] per
/// selected Connection id (`docs/cli-contract.v1.md`, "No transactional
/// rollback").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct BatchOutcome {
    pub(crate) targets: Vec<TargetOutcome>,
}

impl BatchOutcome {
    /// [`TargetResult::Succeeded`] or [`TargetResult::SkippedDisabled`] —
    /// everything that is not a failure or a usage-class refusal.
    ///
    /// Only reachable from `cli` (#45) so far, which will use these two
    /// counts to choose between exit `0`/`1`/`4`
    /// (`docs/cli-contract.v1.md`, "Exit code table").
    #[allow(dead_code)]
    pub(crate) fn succeeded_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| {
                matches!(
                    target.result,
                    TargetResult::Succeeded | TargetResult::SkippedDisabled
                )
            })
            .count()
    }

    /// Only [`TargetResult::Failed`] — [`TargetResult::RefusedDisabled`] is
    /// a usage error (`docs/cli-contract.v1.md`, "Exit code table": exit
    /// `2`), not a batch failure contribution (exit `1`/`4`).
    #[allow(dead_code)]
    pub(crate) fn failed_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| matches!(target.result, TargetResult::Failed { .. }))
            .count()
    }
}

// ---------------------------------------------------------------------------
// Pure policy: disabled Connections, idempotency.
// ---------------------------------------------------------------------------

/// Whether a disabled Connection should be refused or skipped before any
/// I/O is attempted (`docs/config.v1.md`, "`enabled: false`"). `stop` never
/// calls this — a disabled Connection stops the same as any other id.
/// `None` means "this Connection is enabled; proceed".
fn disabled_policy(connection: &Connection, single_target: bool) -> Option<TargetResult> {
    if connection.enabled {
        return None;
    }
    Some(if single_target {
        TargetResult::RefusedDisabled
    } else {
        TargetResult::SkippedDisabled
    })
}

/// `start`'s idempotency rules, from an already-reconciled row
/// (`docs/cli-contract.v1.md`, "`start`": "Idempotency"). `None` means
/// "actually attempt to start this Connection".
fn start_idempotent_or_conflict(row: &StatusRow) -> Option<TargetResult> {
    if row.state == HealthState::Running && row.source == Source::Unit {
        return Some(TargetResult::Succeeded);
    }
    if let Some(error) = &row.error {
        if error.code == ErrorCode::PortInUse {
            // "Port held without our unit ... is not a successful no-op —
            // start fails for that id until the operator frees the port"
            // (`docs/cli-contract.v1.md`, "`start`": "Idempotency").
            return Some(TargetResult::Failed {
                code: ErrorCode::PortInUse,
                message: error.detail.clone(),
            });
        }
    }
    None
}

/// `stop`'s idempotency rule (`docs/cli-contract.v1.md`, "`stop`":
/// "Idempotency: already `stopped` -> success no-op"). `None` means
/// "actually attempt to stop this Connection".
fn stop_idempotent(row: &StatusRow) -> Option<TargetResult> {
    (row.state == HealthState::Stopped).then_some(TargetResult::Succeeded)
}

// ---------------------------------------------------------------------------
// start.
// ---------------------------------------------------------------------------

/// Start every Connection the selector names
/// (`docs/modules.v1.md`, "commands": "start" row). Only reachable from
/// `cli` (#45) so far, the same as `status` (#42) — this ticket
/// ([#43](https://github.com/golgor/cloud-sql-tracker/issues/43)) proves
/// the pure selection/idempotency/disabled policy through this module's own
/// unit tests; the real `supervisor`/`port` round trip needs a live
/// systemd user session, which `docs/verification.v1.md` does not require
/// as a unit test.
#[allow(dead_code)]
pub(crate) fn start(
    config: &Config,
    selector: &Selector,
    wait_ms: u64,
) -> Result<BatchOutcome, SelectError> {
    let connections = select::expand(config, selector)?;
    let single_target = matches!(selector, Selector::Id(_));
    let targets = connections
        .into_iter()
        .map(|connection| start_target(connection, config, wait_ms, single_target))
        .collect();
    Ok(BatchOutcome { targets })
}

fn start_target(
    connection: &Connection,
    config: &Config,
    wait_ms: u64,
    single_target: bool,
) -> TargetOutcome {
    if let Some(result) = disabled_policy(connection, single_target) {
        return outcome_for(&connection.id, result);
    }
    outcome_for(&connection.id, try_start(connection, config, wait_ms))
}

fn try_start(connection: &Connection, config: &Config, wait_ms: u64) -> TargetResult {
    let row = match reconcile_now(connection) {
        Ok(row) => row,
        Err(result) => return result,
    };
    if let Some(result) = start_idempotent_or_conflict(&row) {
        return result;
    }

    let proxy_bin = match resolve_proxy_bin_or_failure(config) {
        Ok(path) => path,
        Err(result) => return result,
    };
    let env_vars = match adc_env_or_failure() {
        Ok(vars) => vars,
        Err(result) => return result,
    };

    if let Err(err) = supervisor::start_transient(connection, &proxy_bin, &env_vars) {
        return TargetResult::Failed {
            code: ErrorCode::Unknown,
            message: err.to_string(),
        };
    }
    wait_for(connection, wait_ms, is_running, ErrorCode::StartTimeout)
}

/// `env::resolve_proxy_bin`, converted into a [`TargetResult::Failed`] with
/// `error.code: bin_missing` on failure (`docs/status-document.v1.md`,
/// "`error` object" catalog). Reuses `env::proxy_bin_check`'s message
/// instead of duplicating it.
fn resolve_proxy_bin_or_failure(config: &Config) -> Result<PathBuf, TargetResult> {
    env::resolve_proxy_bin(Some(&config.proxy_bin)).map_err(|_| {
        let check = env::proxy_bin_check(Some(&config.proxy_bin));
        TargetResult::Failed {
            code: ErrorCode::BinMissing,
            message: check_message(&check.detail, check.hint.as_deref()),
        }
    })
}

/// Application Default Credentials for the started proxy's environment
/// (`docs/adr/0002-adc-only-auth.md`), or a [`TargetResult::Failed`] with
/// `error.code: auth` when ADC is missing. Reuses `env::adc_check`'s
/// message instead of duplicating it.
fn adc_env_or_failure() -> Result<Vec<(String, String)>, TargetResult> {
    let status = env::adc_status();
    if !status.present {
        let check = env::adc_check();
        return Err(TargetResult::Failed {
            code: ErrorCode::Auth,
            message: check_message(&check.detail, check.hint.as_deref()),
        });
    }

    let mut env_vars = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        env_vars.push(("HOME".to_string(), home.to_string_lossy().into_owned()));
    }
    if let Some(path) = status.path {
        env_vars.push((
            "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
            path.display().to_string(),
        ));
    }
    Ok(env_vars)
}

fn check_message(detail: &str, hint: Option<&str>) -> String {
    match hint {
        Some(hint) => format!("{detail} ({hint})"),
        None => detail.to_string(),
    }
}

// ---------------------------------------------------------------------------
// stop.
// ---------------------------------------------------------------------------

/// Stop every Connection the selector names
/// (`docs/modules.v1.md`, "commands": "stop" row). See [`start`]'s doc
/// comment for why this is not unit-tested against a real systemd session.
#[allow(dead_code)]
pub(crate) fn stop(
    config: &Config,
    selector: &Selector,
    wait_ms: u64,
) -> Result<BatchOutcome, SelectError> {
    let connections = select::expand(config, selector)?;
    let targets = connections
        .into_iter()
        .map(|connection| stop_target(connection, wait_ms))
        .collect();
    Ok(BatchOutcome { targets })
}

fn stop_target(connection: &Connection, wait_ms: u64) -> TargetOutcome {
    outcome_for(&connection.id, try_stop(connection, wait_ms))
}

fn try_stop(connection: &Connection, wait_ms: u64) -> TargetResult {
    let row = match reconcile_now(connection) {
        Ok(row) => row,
        Err(result) => return result,
    };
    if let Some(result) = stop_idempotent(&row) {
        return result;
    }

    let unit = match model::unit_name(&connection.id) {
        Ok(unit) => unit,
        Err(err) => {
            return TargetResult::Failed {
                code: ErrorCode::Config,
                message: err.to_string(),
            }
        }
    };
    // Our Unit only — never kill-by-PID (`docs/modules.v1.md`,
    // "supervisor": "stop(unit) -> Result<()>": "Our Unit only"). Best-effort
    // `reset-failed` after stop is `supervisor::stop`'s own job.
    if let Err(err) = supervisor::stop(&unit) {
        return TargetResult::Failed {
            code: ErrorCode::Unknown,
            message: err.to_string(),
        };
    }
    wait_for(connection, wait_ms, is_stopped, ErrorCode::Unknown)
}

// ---------------------------------------------------------------------------
// restart.
// ---------------------------------------------------------------------------

/// Restart every Connection the selector names, or only those currently in
/// Health state `error` when `failed_only` (`docs/cli-contract.v1.md`,
/// "`restart`"). See [`start`]'s doc comment for why this is not
/// unit-tested against a real systemd session.
#[allow(dead_code)]
pub(crate) fn restart(
    config: &Config,
    selector: &Selector,
    wait_ms: u64,
    failed_only: bool,
) -> Result<BatchOutcome, SelectError> {
    let connections = select::expand(config, selector)?;
    let single_target = matches!(selector, Selector::Id(_));

    let targets = if failed_only {
        restart_failed_only(connections, config, wait_ms, single_target)
    } else {
        connections
            .into_iter()
            .map(|connection| restart_target(connection, config, wait_ms, single_target))
            .collect()
    };
    Ok(BatchOutcome { targets })
}

fn restart_target(
    connection: &Connection,
    config: &Config,
    wait_ms: u64,
    single_target: bool,
) -> TargetOutcome {
    if let Some(result) = disabled_policy(connection, single_target) {
        return outcome_for(&connection.id, result);
    }
    // "for each target: stop then start (no deeper magic)"
    // (`docs/cli-contract.v1.md`, "`restart`"). A stop failure skips the
    // start attempt entirely — a Connection that could not even be
    // stopped is not a candidate for a fresh start.
    let stop_result = try_stop(connection, wait_ms);
    if matches!(stop_result, TargetResult::Failed { .. }) {
        return outcome_for(&connection.id, stop_result);
    }
    outcome_for(&connection.id, try_start(connection, config, wait_ms))
}

/// `restart --failed`'s gather step (`docs/cli-contract.v1.md`,
/// "`restart`": "`--failed` is an error-state filter"). Reuses
/// `select::filter_failed` (#42) rather than a second Health-state check.
/// A Connection whose Health could not even be determined is reported as
/// its own failure instead of being silently dropped from, or silently
/// included in, the restart set.
fn restart_failed_only(
    connections: Vec<&Connection>,
    config: &Config,
    wait_ms: u64,
    single_target: bool,
) -> Vec<TargetOutcome> {
    let mut outcomes = Vec::new();
    let mut rows = Vec::with_capacity(connections.len());
    let mut observed = Vec::with_capacity(connections.len());
    for connection in connections {
        match reconcile_now(connection) {
            Ok(row) => {
                rows.push(row);
                observed.push(connection);
            }
            Err(result) => outcomes.push(outcome_for(&connection.id, result)),
        }
    }

    let failed_ids: HashSet<&str> = select::filter_failed(&rows)
        .into_iter()
        .map(|row| row.id.as_str())
        .collect();
    for connection in observed {
        if failed_ids.contains(connection.id.as_str()) {
            outcomes.push(restart_target(connection, config, wait_ms, single_target));
        }
    }
    outcomes
}

// ---------------------------------------------------------------------------
// Shared Observation gather + wait loop.
// ---------------------------------------------------------------------------

/// One reconcile round trip via `status::observe_and_reconcile`, with any
/// gather error already converted into a [`TargetResult::Failed`] so every
/// caller here handles one error shape.
fn reconcile_now(connection: &Connection) -> Result<StatusRow, TargetResult> {
    let now = SystemTime::now();
    let mono_now_usec = status::monotonic_now_usec();
    status::observe_and_reconcile(connection, now, mono_now_usec).map_err(|err| {
        TargetResult::Failed {
            code: ErrorCode::Unknown,
            message: err.to_string(),
        }
    })
}

fn is_running(row: &StatusRow) -> bool {
    row.state == HealthState::Running && row.source == Source::Unit
}

fn is_stopped(row: &StatusRow) -> bool {
    row.state == HealthState::Stopped
}

/// Poll `status::observe_and_reconcile` for up to `wait_ms` until
/// `is_done` is true (`docs/cli-contract.v1.md`, "`start`": "`--wait-ms N`
/// ... Default: 10000 (10s)"). `timeout_code` is the `error.code` a caller
/// reports when the deadline passes first — `start_timeout` for `start`,
/// `unknown` for `stop` (the Status document's catalog has no dedicated
/// "did not confirm stopped" code).
fn wait_for(
    connection: &Connection,
    wait_ms: u64,
    is_done: impl Fn(&StatusRow) -> bool,
    timeout_code: ErrorCode,
) -> TargetResult {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);
    let deadline = Instant::now() + Duration::from_millis(wait_ms);

    loop {
        match reconcile_now(connection) {
            Ok(row) if is_done(&row) => return TargetResult::Succeeded,
            Ok(_) => {}
            Err(result) => return result,
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return TargetResult::Failed {
                code: timeout_code,
                message: format!(
                    "connection `{}` did not confirm within {wait_ms}ms",
                    connection.id
                ),
            };
        }
        std::thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StatusError;

    fn connection(id: &str, enabled: bool) -> Connection {
        Connection {
            id: id.to_string(),
            name: id.to_string(),
            group: "fe".to_string(),
            instance: "proj:region:inst".to_string(),
            address: "127.0.0.1".to_string(),
            port: 15432,
            private_ip: false,
            auto_iam_authn: false,
            extra_args: Vec::new(),
            enabled,
        }
    }

    fn config(connections: Vec<Connection>) -> Config {
        Config {
            proxy_bin: "cloud-sql-proxy".to_string(),
            connections,
        }
    }

    fn row(state: HealthState, source: Source, error: Option<StatusError>) -> StatusRow {
        StatusRow {
            id: "a".to_string(),
            name: "a".to_string(),
            group: "fe".to_string(),
            instance: "proj:region:inst".to_string(),
            address: "127.0.0.1".to_string(),
            port: 15432,
            private_ip: false,
            state,
            source,
            pid: None,
            unit: None,
            port_open: state == HealthState::Running,
            uptime_sec: None,
            error,
        }
    }

    // -- disabled_policy -----------------------------------------------------

    #[test]
    fn disabled_policy_is_none_for_an_enabled_connection() {
        assert_eq!(disabled_policy(&connection("a", true), true), None);
        assert_eq!(disabled_policy(&connection("a", true), false), None);
    }

    #[test]
    fn disabled_policy_refuses_a_single_id_target() {
        assert_eq!(
            disabled_policy(&connection("a", false), true),
            Some(TargetResult::RefusedDisabled)
        );
    }

    #[test]
    fn disabled_policy_skips_a_multi_target_selector() {
        assert_eq!(
            disabled_policy(&connection("a", false), false),
            Some(TargetResult::SkippedDisabled)
        );
    }

    // -- start_idempotent_or_conflict -----------------------------------------

    #[test]
    fn start_idempotent_or_conflict_is_a_success_no_op_when_already_running_via_unit() {
        let row = row(HealthState::Running, Source::Unit, None);
        assert_eq!(
            start_idempotent_or_conflict(&row),
            Some(TargetResult::Succeeded)
        );
    }

    #[test]
    fn start_idempotent_or_conflict_fails_when_the_port_is_held_by_a_foreign_process() {
        let row = row(
            HealthState::Error,
            Source::None,
            Some(StatusError {
                code: ErrorCode::PortInUse,
                detail: "held by pid 999".to_string(),
            }),
        );
        let result =
            start_idempotent_or_conflict(&row).expect("port_in_use must not proceed to start");
        assert_eq!(
            result,
            TargetResult::Failed {
                code: ErrorCode::PortInUse,
                message: "held by pid 999".to_string(),
            }
        );
    }

    #[test]
    fn start_idempotent_or_conflict_proceeds_to_start_when_stopped() {
        let row = row(HealthState::Stopped, Source::None, None);
        assert_eq!(start_idempotent_or_conflict(&row), None);
    }

    #[test]
    fn start_idempotent_or_conflict_proceeds_to_start_when_running_without_our_unit() {
        // Running from an unexpected/foreign source is not our idempotent
        // no-op — only `source: unit` counts.
        let row = row(HealthState::Running, Source::None, None);
        assert_eq!(start_idempotent_or_conflict(&row), None);
    }

    // -- stop_idempotent -------------------------------------------------------

    #[test]
    fn stop_idempotent_is_a_success_no_op_when_already_stopped() {
        let row = row(HealthState::Stopped, Source::None, None);
        assert_eq!(stop_idempotent(&row), Some(TargetResult::Succeeded));
    }

    #[test]
    fn stop_idempotent_proceeds_to_stop_when_running() {
        let row = row(HealthState::Running, Source::Unit, None);
        assert_eq!(stop_idempotent(&row), None);
    }

    // -- BatchOutcome ------------------------------------------------------

    #[test]
    fn batch_outcome_counts_succeeded_and_skipped_together() {
        let batch = BatchOutcome {
            targets: vec![
                TargetOutcome {
                    id: "a".to_string(),
                    result: TargetResult::Succeeded,
                },
                TargetOutcome {
                    id: "b".to_string(),
                    result: TargetResult::SkippedDisabled,
                },
                TargetOutcome {
                    id: "c".to_string(),
                    result: TargetResult::Failed {
                        code: ErrorCode::Unknown,
                        message: "boom".to_string(),
                    },
                },
            ],
        };
        assert_eq!(batch.succeeded_count(), 2);
        assert_eq!(batch.failed_count(), 1);
    }

    #[test]
    fn batch_outcome_does_not_count_a_disabled_refusal_as_a_batch_failure() {
        // `RefusedDisabled` is a usage error (`cli` exit 2), not a batch
        // failure contribution.
        let batch = BatchOutcome {
            targets: vec![TargetOutcome {
                id: "a".to_string(),
                result: TargetResult::RefusedDisabled,
            }],
        };
        assert_eq!(batch.failed_count(), 0);
        assert_eq!(batch.succeeded_count(), 0);
    }

    // -- start/stop/restart wiring, with zero I/O ---------------------------
    //
    // A `--group`/`--all` selector that only expands to disabled
    // Connections proves the disabled-skip policy without ever calling
    // `supervisor`/`port` — this crate has no fake Supervisor to inject
    // (`docs/modules.v1.md`, "supervisor": "No `trait Supervisor`"), so
    // every test below either supplies zero connections or only disabled
    // ones, never reaching `try_start`/`try_stop`'s real I/O.

    #[test]
    fn start_on_all_with_zero_connections_is_an_empty_successful_batch() {
        let config = config(Vec::new());
        let batch = start(&config, &Selector::All, 10_000).expect("All always resolves");
        assert!(batch.targets.is_empty());
    }

    #[test]
    fn stop_on_all_with_zero_connections_is_an_empty_successful_batch() {
        let config = config(Vec::new());
        let batch = stop(&config, &Selector::All, 10_000).expect("All always resolves");
        assert!(batch.targets.is_empty());
    }

    #[test]
    fn restart_failed_on_all_with_zero_connections_is_an_empty_successful_batch() {
        let config = config(Vec::new());
        let batch = restart(&config, &Selector::All, 10_000, true).expect("All always resolves");
        assert!(batch.targets.is_empty());
    }

    #[test]
    fn start_with_an_unknown_id_propagates_the_selector_error() {
        let config = config(Vec::new());
        let err = start(&config, &Selector::Id("nope".to_string()), 10_000)
            .expect_err("nope is not in the config");
        assert_eq!(err, SelectError::UnknownId("nope".to_string()));
    }

    #[test]
    fn start_skips_a_disabled_connection_in_an_all_selector_without_any_io() {
        let config = config(vec![connection("a", false)]);
        let batch = start(&config, &Selector::All, 10_000).expect("All always resolves");
        assert_eq!(
            batch.targets,
            vec![TargetOutcome {
                id: "a".to_string(),
                result: TargetResult::SkippedDisabled,
            }]
        );
    }

    #[test]
    fn start_refuses_a_single_disabled_id_without_any_io() {
        let config = config(vec![connection("a", false)]);
        let batch =
            start(&config, &Selector::Id("a".to_string()), 10_000).expect("a is in the config");
        assert_eq!(
            batch.targets,
            vec![TargetOutcome {
                id: "a".to_string(),
                result: TargetResult::RefusedDisabled,
            }]
        );
    }

    #[test]
    fn restart_skips_a_disabled_connection_in_an_all_selector_without_any_io() {
        let config = config(vec![connection("a", false)]);
        let batch = restart(&config, &Selector::All, 10_000, false).expect("All always resolves");
        assert_eq!(
            batch.targets,
            vec![TargetOutcome {
                id: "a".to_string(),
                result: TargetResult::SkippedDisabled,
            }]
        );
    }

    #[test]
    fn restart_refuses_a_single_disabled_id_without_any_io() {
        let config = config(vec![connection("a", false)]);
        let batch = restart(&config, &Selector::Id("a".to_string()), 10_000, false)
            .expect("a is in the config");
        assert_eq!(
            batch.targets,
            vec![TargetOutcome {
                id: "a".to_string(),
                result: TargetResult::RefusedDisabled,
            }]
        );
    }
}
