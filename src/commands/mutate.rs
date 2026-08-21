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
use crate::model::{self, Connection, ErrorCode, HealthState, Source, StatusRow, UnitName};
use crate::supervisor::{self, SupervisorError, UnitSnapshot};

use super::select::{self, SelectError, Selector};
use super::status::{self, StatusCommandError};

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
    /// Could not even reach the systemd user bus for this id
    /// (`docs/cli-contract.v1.md`, "Exit code table": exit `3`,
    /// "Dependency": "no systemd user bus"). Kept separate from
    /// [`TargetResult::Failed`] instead of reusing `ErrorCode::Unknown`
    /// so `cli` (#45) can tell an environmental failure from a
    /// per-Connection one and "prefer `3` when failure is environmental
    /// rather than per-id" — this is a `commands`-internal signal, not a
    /// Status document `error.code`, so it needs no schema change.
    Dependency { message: String },
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
    /// Only [`TargetResult::Succeeded`] — a skip is neither a success nor
    /// a failure, so it is **not** folded in here. Counting
    /// [`TargetResult::SkippedDisabled`] as a success would make `cli`'s
    /// "every attempted target failed" exit `4`
    /// (`docs/cli-contract.v1.md`, "Exit code table") unreachable whenever
    /// a disabled id happened to be skipped alongside every enabled id
    /// failing.
    pub(crate) fn succeeded_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| target.result == TargetResult::Succeeded)
            .count()
    }

    /// [`TargetResult::SkippedDisabled`] only (`docs/config.v1.md`,
    /// "`enabled: false`": skipped ids "do not by themselves force exit
    /// `1`/`4`"). `cli` needs this separated from
    /// [`succeeded_count`](Self::succeeded_count) to apply that rule
    /// without a skip masquerading as a success.
    pub(crate) fn skipped_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| matches!(target.result, TargetResult::SkippedDisabled))
            .count()
    }

    /// Per-Connection operation failures only.
    /// [`TargetResult::RefusedDisabled`] is a usage error
    /// (`docs/cli-contract.v1.md`, "Exit code table": exit `2`), not a
    /// batch failure contribution (exit `1`/`4`), and
    /// [`TargetResult::Dependency`] is counted separately by
    /// [`dependency_count`](Self::dependency_count) so `cli` can prefer
    /// exit `3` when every failure is environmental.
    pub(crate) fn failed_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| matches!(target.result, TargetResult::Failed { .. }))
            .count()
    }

    /// [`TargetResult::Dependency`] only — an environmental failure such as
    /// an unreachable systemd user bus (`docs/cli-contract.v1.md`, "Exit
    /// code table": exit `3`, "Prefer `3` when failure is environmental
    /// rather than per-id").
    pub(crate) fn dependency_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|target| matches!(target.result, TargetResult::Dependency { .. }))
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
    if is_running(row) {
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
    is_stopped(row).then_some(TargetResult::Succeeded)
}

// ---------------------------------------------------------------------------
// start.
// ---------------------------------------------------------------------------

/// Start every Connection the selector names
/// (`docs/modules.v1.md`, "commands": "start" row). Called by `cli`'s
/// `start` subcommand (#45), the same as `status` (#42) — this ticket
/// ([#43](https://github.com/golgor/cloud-sql-tracker/issues/43)) proved
/// the pure selection/idempotency/disabled policy through this module's own
/// unit tests; the real `supervisor`/`port` round trip needs a live
/// systemd user session, which `docs/verification.v1.md` does not require
/// as a unit test.
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
    if row.state == HealthState::Starting {
        // The Unit is already `activating` (a retried `start`, or a
        // `restart` handoff that raced with systemd's own transition out
        // of `deactivating`) — a second `StartTransientUnit` for the same
        // unit name would fail with systemd's `UnitExists`
        // (`org.freedesktop.systemd1.Manager.StartTransientUnit`: a
        // still-loaded unit, including one mid-activation, is not a valid
        // target for a fresh transient start). Wait for it to finish
        // coming up instead of issuing a redundant start.
        return wait_for(connection, wait_ms, is_running, ErrorCode::StartTimeout);
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
        return supervisor_result(err);
    }
    wait_for(connection, wait_ms, is_running, ErrorCode::StartTimeout)
}

/// `env::resolve_proxy_bin`, converted into a [`TargetResult::Dependency`]
/// on failure. `config.proxy_bin` is one value for the whole config —
/// `Connection` has no per-id override — so an unresolved binary fails
/// identically for every target in this batch, matching
/// `docs/cli-contract.v1.md`'s exit `3`: "proxy binary unresolved when
/// required for the whole command". Reuses `env::proxy_bin_check`'s
/// message instead of duplicating it.
fn resolve_proxy_bin_or_failure(config: &Config) -> Result<PathBuf, TargetResult> {
    env::resolve_proxy_bin(Some(&config.proxy_bin)).map_err(|_| {
        let check = env::proxy_bin_check(Some(&config.proxy_bin));
        TargetResult::Dependency {
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
/// (`docs/modules.v1.md`, "commands": "stop" row). Called by `cli`'s
/// `stop` subcommand (#45). See [`start`]'s doc comment for why this is
/// not unit-tested against a real systemd session.
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

/// Plain `stop`'s entry point: Reconcile's Health `Stopped` is an
/// idempotent no-op here (`docs/cli-contract.v1.md`, "`stop`":
/// "Idempotency: already `stopped` -> success no-op") — a real
/// `supervisor::stop` call is skipped entirely. `restart` must **not**
/// share this shortcut; see [`try_stop_for_restart`].
fn try_stop(connection: &Connection, wait_ms: u64) -> TargetResult {
    let row = match reconcile_now(connection) {
        Ok(row) => row,
        Err(result) => return result,
    };
    if let Some(result) = stop_idempotent(&row) {
        return result;
    }
    stop_unit_and_wait(connection, wait_ms)
}

/// `restart`'s stop step. Deliberately **skips** [`stop_idempotent`]'s
/// Health-based shortcut: `docs/reconcile.v1.md`'s `deactivating` +
/// closed-port row already reports Health `Stopped` while the Unit is
/// still loaded (`docs/reconcile.v1.md:176`), and a fresh
/// `StartTransientUnit` for a still-loaded unit name — even `Inactive`,
/// even mid-`deactivating` — fails with systemd's `UnitExists`
/// (`docs/research/supervisor-io.md`). `restart` always issues the real
/// `supervisor::stop` call and waits for [`unit_safely_recreatable`]
/// (Absent only) before `try_start`, regardless of what Reconcile's
/// Health already says. `supervisor::stop` treats "Unit never loaded" as
/// an idempotent success, so a Connection that was already fully stopped
/// pays only one extra `GetUnit`/`StopUnit` round trip, not a real
/// failure.
fn try_stop_for_restart(connection: &Connection, wait_ms: u64) -> TargetResult {
    stop_unit_and_wait(connection, wait_ms)
}

/// The real stop + wait, shared by [`try_stop`] (once its Health
/// shortcut has ruled itself out) and [`try_stop_for_restart`] (always).
fn stop_unit_and_wait(connection: &Connection, wait_ms: u64) -> TargetResult {
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
        return supervisor_result(err);
    }
    wait_for_stop(&unit, wait_ms)
}

/// Whether `snapshot` means "safe to `StartTransientUnit` this unit name
/// again" — **Absent only**. `Manager.StartTransientUnit`'s `mode:
/// "fail"` treats any Unit systemd still has loaded as an existing
/// object — `Inactive`, `Failed`, mid-`deactivating`, all of them — so a
/// fresh transient start for that same name fails with `UnitExists`
/// regardless of `ActiveState` (`docs/research/supervisor-io.md`). Only a
/// Unit systemd has fully unloaded (garbage-collected) is a valid
/// `StartTransientUnit` target again.
///
/// This is deliberately **not** Reconcile's Health-based `stop`
/// idempotency: `docs/reconcile.v1.md`'s `deactivating` + closed-port row
/// already reports Health `Stopped` while the Unit itself is still
/// loaded (`docs/reconcile.v1.md:176`) — exactly the shape that used to
/// let `restart` race `StartTransientUnit` into `UnitExists`.
/// `start_transient`'s own best-effort `ResetFailedUnit` (`docs/modules.
/// v1.md`, "supervisor": "reset-failed after stop (and on restart)")
/// only clears the *failed flag* — it does not unload a still-loaded
/// Unit, so it cannot substitute for waiting here.
fn unit_safely_recreatable(snapshot: &UnitSnapshot) -> bool {
    matches!(snapshot, UnitSnapshot::Absent)
}

/// `stop`'s wait loop, once a real `supervisor::stop` call has been made.
/// Polls `supervisor::show` directly instead of `status::observe_and_
/// reconcile` — `docs/reconcile.v1.md`'s truth table lets an *active* Unit
/// report Health `error` too (e.g. our Unit is active but a mismatched
/// listener holds the port, `source: unit`), so a Reconcile-Health-based
/// wait can never safely short-circuit on a terminal `error` the way
/// `start`'s [`wait_for`] does: an in-flight stop's own Unit could produce
/// exactly that shape mid-poll. Completion here is decided solely by
/// [`unit_safely_recreatable`], which also happens to be the stronger
/// condition `restart`'s stop-then-start handoff actually needs.
fn wait_for_stop(unit: &UnitName, wait_ms: u64) -> TargetResult {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);
    let deadline = Instant::now() + Duration::from_millis(wait_ms);

    loop {
        let snapshot = match supervisor::show(unit) {
            Ok(snapshot) => snapshot,
            Err(err) => return supervisor_result(err),
        };
        if unit_safely_recreatable(&snapshot) {
            return TargetResult::Succeeded;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return TargetResult::Failed {
                code: ErrorCode::Unknown,
                message: format!("unit `{unit}` did not finish stopping within {wait_ms}ms"),
            };
        }
        std::thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

// ---------------------------------------------------------------------------
// restart.
// ---------------------------------------------------------------------------

/// Restart every Connection the selector names, or only those currently in
/// Health state `error` when `failed_only` (`docs/cli-contract.v1.md`,
/// "`restart`"). Called by `cli`'s `restart` subcommand (#45). See
/// [`start`]'s doc comment for why this is not unit-tested against a real
/// systemd session.
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

/// Whether `restart`'s stop step succeeded well enough to attempt
/// `try_start` next. A per-Connection [`TargetResult::Failed`] means this
/// Connection could not even be stopped; an environmental
/// [`TargetResult::Dependency`] (e.g. the systemd user bus is unreachable)
/// is a condition a second D-Bus call cannot route around either — both
/// must be returned as-is instead of falling through to a doomed
/// `try_start`. Pure and separated from [`restart_target`] itself so this
/// gate is unit-testable without a fake Supervisor
/// (`docs/modules.v1.md`, "supervisor": "No `trait Supervisor`").
fn stop_succeeded(result: &TargetResult) -> bool {
    *result == TargetResult::Succeeded
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
    // (`docs/cli-contract.v1.md`, "`restart`"). Uses [`try_stop_for_restart`],
    // not [`try_stop`] — see its doc comment for why a Health-based
    // idempotent stop is not safe before a fresh `StartTransientUnit`.
    // See [`stop_succeeded`] for why only a genuine success proceeds to
    // `try_start`.
    let stop_result = try_stop_for_restart(connection, wait_ms);
    if !stop_succeeded(&stop_result) {
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
///
/// The disabled policy is applied **before** the reconcile round trip, the
/// same as every other entry point here (`docs/config.v1.md`, "`enabled:
/// false`") — a disabled single id must be a usage refusal, never a
/// filtered-away empty-success batch, and a disabled id in `--group`/
/// `--all` is skipped without spending a D-Bus call + TCP probe on it.
/// Outcomes are collected into index-aligned slots so the returned order
/// always matches the selector's own order, even though disabled/gather-
/// failed ids and the eventually-restarted ids are decided in separate
/// passes.
fn restart_failed_only(
    connections: Vec<&Connection>,
    config: &Config,
    wait_ms: u64,
    single_target: bool,
) -> Vec<TargetOutcome> {
    let mut slots: Vec<Option<TargetOutcome>> = vec![None; connections.len()];
    let mut rows = Vec::with_capacity(connections.len());
    let mut observed = Vec::with_capacity(connections.len());

    for (index, connection) in connections.into_iter().enumerate() {
        if let Some(result) = disabled_policy(connection, single_target) {
            slots[index] = Some(outcome_for(&connection.id, result));
            continue;
        }
        match reconcile_now(connection) {
            Ok(row) => {
                rows.push(row);
                observed.push((index, connection));
            }
            Err(result) => slots[index] = Some(outcome_for(&connection.id, result)),
        }
    }

    let failed_ids: HashSet<&str> = select::filter_failed(&rows)
        .into_iter()
        .map(|row| row.id.as_str())
        .collect();
    for (index, connection) in observed {
        if failed_ids.contains(connection.id.as_str()) {
            slots[index] = Some(restart_target(connection, config, wait_ms, single_target));
        }
    }
    slots.into_iter().flatten().collect()
}

// ---------------------------------------------------------------------------
// Shared Observation gather + wait loop.
// ---------------------------------------------------------------------------

/// One reconcile round trip via `status::observe_and_reconcile`, with any
/// gather error already converted into a classified [`TargetResult`] so
/// every caller here handles one error shape.
fn reconcile_now(connection: &Connection) -> Result<StatusRow, TargetResult> {
    let now = SystemTime::now();
    let mono_now_usec = status::monotonic_now_usec();
    status::observe_and_reconcile(connection, now, mono_now_usec).map_err(status_command_result)
}

/// Classifies a [`StatusCommandError`] into the [`TargetResult`] a
/// mutating command reports. A [`SupervisorError::Bus`] failure inside it
/// is the same environmental "no user systemd" condition
/// `docs/cli-contract.v1.md`'s exit `3` describes
/// ([`supervisor_result`]); every other case is a per-Connection
/// operation failure.
fn status_command_result(err: StatusCommandError) -> TargetResult {
    match err {
        StatusCommandError::Supervisor { source, .. } => supervisor_result(*source),
        other => TargetResult::Failed {
            code: ErrorCode::Unknown,
            message: other.to_string(),
        },
    }
}

/// Classifies a [`SupervisorError`] returned directly from
/// `supervisor::start_transient`/`supervisor::stop` the same way
/// [`status_command_result`] classifies one wrapped inside a
/// [`StatusCommandError`] — one place decides "is this environmental".
/// Delegates to [`SupervisorError::is_dependency`] rather than repeating
/// the `Bus`-only check here, so both call sites and `supervisor` itself
/// agree on what counts as "cannot operate at all".
fn supervisor_result(err: SupervisorError) -> TargetResult {
    let message = err.to_string();
    if err.is_dependency() {
        TargetResult::Dependency { message }
    } else {
        TargetResult::Failed {
            code: ErrorCode::Unknown,
            message,
        }
    }
}

fn is_running(row: &StatusRow) -> bool {
    row.state == HealthState::Running && row.source == Source::Unit
}

/// `stop`'s **pre-check** for "nothing to do" (`docs/cli-contract.v1.md`,
/// "`stop`": "Idempotency: already `stopped` -> success no-op"). A
/// Connection whose Unit is still `active` (e.g. `error`/`port_in_use`
/// from a listener PID that does not match `MainPID`,
/// `docs/reconcile.v1.md`'s truth table) is **not** an idempotent no-op —
/// `stop` still needs to attempt the real `supervisor::stop` call on our
/// own Unit. Once that call is made, [`wait_for_stop`] decides completion
/// from the Unit's own `ActiveState`, not this Health check.
fn is_stopped(row: &StatusRow) -> bool {
    row.state == HealthState::Stopped
}

/// A reconciled row's own `error` is already stable — every `error.code`
/// `reconcile`/`status::observe_and_reconcile` can produce
/// (`port_in_use`, `start_timeout`, `unit_failed`, `exec_failed`,
/// `config`) is a terminal classification of the current observation, not
/// a transient one more polling will resolve (`docs/reconcile.v1.md`,
/// "Error codes produced by Reconcile"). Reporting it immediately keeps
/// the real cause visible instead of a generic "did not confirm" timeout
/// once `wait_ms` elapses.
fn terminal_error_result(row: &StatusRow) -> Option<TargetResult> {
    let error = row.error.as_ref()?;
    Some(TargetResult::Failed {
        code: error.code,
        message: error.detail.clone(),
    })
}

/// One poll's outcome inside [`wait_for`]'s loop: keep waiting, or stop
/// with a final [`TargetResult`]. Pure given its inputs — no I/O, no clock
/// read — so this is the seam unit tests exercise directly instead of the
/// real 200ms-interval sleep loop.
fn wait_step(
    row: &StatusRow,
    is_done: &impl Fn(&StatusRow) -> bool,
    remaining: Duration,
    wait_ms: u64,
    id: &str,
    timeout_code: ErrorCode,
) -> Option<TargetResult> {
    if is_done(row) {
        return Some(TargetResult::Succeeded);
    }
    if let Some(result) = terminal_error_result(row) {
        return Some(result);
    }
    if remaining.is_zero() {
        return Some(TargetResult::Failed {
            code: timeout_code,
            message: format!("connection `{id}` did not confirm within {wait_ms}ms"),
        });
    }
    None
}

/// Poll `status::observe_and_reconcile` for up to `wait_ms` until
/// `is_done` is true (`docs/cli-contract.v1.md`, "`start`": "`--wait-ms N`
/// ... Default: 10000 (10s)"), short-circuiting on a reconciled row that
/// already carries a terminal `error` ([`terminal_error_result`]).
/// `timeout_code` is the `error.code` a caller reports when the deadline
/// passes first with no terminal error.
///
/// `start` only — `stop` waits with [`wait_for_stop`] instead, because
/// short-circuiting on a reconciled row's `error` is only safe when that
/// `error` is already a terminal classification. `docs/reconcile.v1.md`'s
/// truth table lets an *active* Unit report Health `error` too, which a
/// stop-in-progress Unit can genuinely produce mid-poll.
fn wait_for(
    connection: &Connection,
    wait_ms: u64,
    is_done: impl Fn(&StatusRow) -> bool,
    timeout_code: ErrorCode,
) -> TargetResult {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);
    let deadline = Instant::now() + Duration::from_millis(wait_ms);

    loop {
        let row = match reconcile_now(connection) {
            Ok(row) => row,
            Err(result) => return result,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if let Some(result) = wait_step(
            &row,
            &is_done,
            remaining,
            wait_ms,
            &connection.id,
            timeout_code,
        ) {
            return result;
        }
        std::thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StatusError;
    use crate::supervisor::UnitActiveState;

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
    fn batch_outcome_counts_succeeded_skipped_and_failed_separately() {
        // A skip is neither a success nor a failure — folding it into
        // `succeeded_count` would make `cli`'s "every target failed" exit
        // `4` unreachable whenever a disabled id was skipped alongside
        // every enabled id failing.
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
                TargetOutcome {
                    id: "d".to_string(),
                    result: TargetResult::Dependency {
                        message: "no bus".to_string(),
                    },
                },
            ],
        };
        assert_eq!(batch.succeeded_count(), 1);
        assert_eq!(batch.skipped_count(), 1);
        assert_eq!(batch.failed_count(), 1);
        assert_eq!(batch.dependency_count(), 1);
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
        assert_eq!(batch.skipped_count(), 0);
        assert_eq!(batch.dependency_count(), 0);
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

    #[test]
    fn restart_failed_refuses_a_single_disabled_id_without_any_io() {
        // The disabled policy must run before the `--failed` gather step
        // — a disabled single-id target is a usage refusal, never a
        // filtered-away empty-success batch (`docs/config.v1.md`,
        // "`enabled: false`").
        let config = config(vec![connection("a", false)]);
        let batch = restart(&config, &Selector::Id("a".to_string()), 10_000, true)
            .expect("a is in the config");
        assert_eq!(
            batch.targets,
            vec![TargetOutcome {
                id: "a".to_string(),
                result: TargetResult::RefusedDisabled,
            }]
        );
    }

    #[test]
    fn restart_failed_skips_a_disabled_connection_in_an_all_selector_without_any_io() {
        let config = config(vec![connection("a", false)]);
        let batch = restart(&config, &Selector::All, 10_000, true).expect("All always resolves");
        assert_eq!(
            batch.targets,
            vec![TargetOutcome {
                id: "a".to_string(),
                result: TargetResult::SkippedDisabled,
            }]
        );
    }

    #[test]
    fn restart_failed_preserves_selector_order_across_multiple_disabled_ids() {
        // Outcomes are collected into index-aligned slots precisely so a
        // batch never reorders disabled/gather-failed ids relative to
        // restarted ones (`TargetOutcome`'s own doc comment: "in selector
        // order"). Zero I/O: every id here is disabled, so none reaches
        // the reconcile round trip.
        let config = config(vec![
            connection("a", false),
            connection("b", false),
            connection("c", false),
        ]);
        let batch = restart(&config, &Selector::All, 10_000, true).expect("All always resolves");
        let ids: Vec<&str> = batch.targets.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        assert!(batch
            .targets
            .iter()
            .all(|target| target.result == TargetResult::SkippedDisabled));
    }

    // -- stop_succeeded (restart's stop-then-start gate) --------------------

    #[test]
    fn stop_succeeded_is_true_only_for_a_genuine_success() {
        assert!(stop_succeeded(&TargetResult::Succeeded));
    }

    #[test]
    fn stop_succeeded_is_false_for_a_per_connection_failure() {
        // `restart` must not attempt `try_start` after a stop it could
        // not confirm — the failed stop's own result is the batch's
        // answer for this id.
        let result = TargetResult::Failed {
            code: ErrorCode::Unknown,
            message: "boom".to_string(),
        };
        assert!(!stop_succeeded(&result));
    }

    #[test]
    fn stop_succeeded_is_false_for_a_dependency_failure() {
        // An environmental stop failure (e.g. no systemd user bus) must
        // not be swallowed by falling through to `try_start` — a second
        // D-Bus call cannot succeed either.
        let result = TargetResult::Dependency {
            message: "no bus".to_string(),
        };
        assert!(!stop_succeeded(&result));
    }

    // -- error classification: SupervisorError / StatusCommandError ---------

    #[test]
    fn supervisor_result_classifies_a_bus_failure_as_dependency() {
        let err = SupervisorError::Bus(zbus::Error::Address("no bus".to_string()));
        let message = err.to_string();
        assert_eq!(supervisor_result(err), TargetResult::Dependency { message });
    }

    #[test]
    fn supervisor_result_classifies_every_other_variant_as_a_per_connection_failure() {
        let err = SupervisorError::MissingProperty {
            property: "MainPID",
        };
        let message = err.to_string();
        assert_eq!(
            supervisor_result(err),
            TargetResult::Failed {
                code: ErrorCode::Unknown,
                message,
            }
        );
    }

    // -- resolve_proxy_bin_or_failure ---------------------------------------

    #[test]
    fn resolve_proxy_bin_or_failure_is_a_dependency_not_a_per_connection_failure() {
        // `config.proxy_bin` is one value for the whole config — every
        // target in the batch fails identically when it cannot be
        // resolved, matching `docs/cli-contract.v1.md`'s exit `3`: "proxy
        // binary unresolved when required for the whole command".
        let config = config(vec![connection("a", true)]);
        let mut broken = config.clone();
        broken.proxy_bin = "this-binary-does-not-exist-anywhere".to_string();
        let result = resolve_proxy_bin_or_failure(&broken).expect_err("the binary cannot resolve");
        assert!(matches!(result, TargetResult::Dependency { .. }));
    }

    #[test]
    fn status_command_result_unwraps_a_bus_failure_from_inside_a_supervisor_error() {
        let source = SupervisorError::Bus(zbus::Error::Address("no bus".to_string()));
        let message = source.to_string();
        let err = StatusCommandError::Supervisor {
            id: "a".to_string(),
            source: Box::new(source),
        };
        assert_eq!(
            status_command_result(err),
            TargetResult::Dependency { message }
        );
    }

    #[test]
    fn status_command_result_treats_a_unit_name_error_as_a_per_connection_failure() {
        let err = StatusCommandError::UnitName {
            id: "a".to_string(),
            source: model::UnitNameError::Empty,
        };
        let message = err.to_string();
        assert_eq!(
            status_command_result(err),
            TargetResult::Failed {
                code: ErrorCode::Unknown,
                message,
            }
        );
    }

    // -- unit_safely_recreatable ---------------------------------------------

    fn loaded_snapshot(active_state: UnitActiveState, main_pid: Option<u32>) -> UnitSnapshot {
        UnitSnapshot::Loaded {
            active_state,
            sub_state: "n/a".to_string(),
            main_pid,
            result: crate::supervisor::UnitResult::Success,
            exec_outcome: crate::supervisor::ExecOutcome::NotExited,
            started_at_monotonic_usec: None,
        }
    }

    #[test]
    fn unit_safely_recreatable_is_true_only_when_absent() {
        assert!(unit_safely_recreatable(&UnitSnapshot::Absent));
    }

    #[test]
    fn unit_safely_recreatable_is_false_when_inactive_because_the_unit_is_still_loaded() {
        // A successful `GetUnit` — even for `Inactive` — means systemd has
        // not unloaded this unit name yet; `StartTransientUnit` for it
        // still fails with `UnitExists`.
        let snapshot = loaded_snapshot(UnitActiveState::Inactive, None);
        assert!(!unit_safely_recreatable(&snapshot));
    }

    #[test]
    fn unit_safely_recreatable_is_false_while_deactivating() {
        // The exact race this must-fix closes: Reconcile already reports
        // Health `Stopped` for `deactivating` + a closed port
        // (`docs/reconcile.v1.md`), but the Unit is still loaded and a
        // fresh `StartTransientUnit` for it would fail with `UnitExists`.
        let snapshot = loaded_snapshot(UnitActiveState::Deactivating, Some(123));
        assert!(!unit_safely_recreatable(&snapshot));
    }

    #[test]
    fn unit_safely_recreatable_is_false_while_active() {
        let snapshot = loaded_snapshot(UnitActiveState::Active, Some(123));
        assert!(!unit_safely_recreatable(&snapshot));
    }

    #[test]
    fn unit_safely_recreatable_is_false_while_activating() {
        let snapshot = loaded_snapshot(UnitActiveState::Activating, None);
        assert!(!unit_safely_recreatable(&snapshot));
    }

    #[test]
    fn unit_safely_recreatable_is_false_when_failed_with_no_live_process_because_the_unit_is_still_loaded(
    ) {
        // `ResetFailedUnit` (best-effort, inside `start_transient`) only
        // clears the failed flag — it does not unload the Unit, so a
        // `Failed` snapshot is still "loaded" and still not a valid
        // `StartTransientUnit` target.
        let snapshot = loaded_snapshot(UnitActiveState::Failed, None);
        assert!(!unit_safely_recreatable(&snapshot));
    }

    #[test]
    fn unit_safely_recreatable_is_false_when_failed_with_a_live_process() {
        let snapshot = loaded_snapshot(UnitActiveState::Failed, Some(123));
        assert!(!unit_safely_recreatable(&snapshot));
    }

    #[test]
    fn unit_safely_recreatable_is_false_for_an_unknown_active_state() {
        let snapshot = loaded_snapshot(UnitActiveState::Unknown("reloading".to_string()), None);
        assert!(!unit_safely_recreatable(&snapshot));
    }

    // -- terminal_error_result --------------------------------------------

    #[test]
    fn terminal_error_result_is_none_for_a_healthy_row() {
        let row = row(HealthState::Running, Source::Unit, None);
        assert_eq!(terminal_error_result(&row), None);
    }

    #[test]
    fn terminal_error_result_surfaces_the_rows_own_code_and_detail() {
        let row = row(
            HealthState::Error,
            Source::None,
            Some(StatusError {
                code: ErrorCode::PortInUse,
                detail: "held by pid 999".to_string(),
            }),
        );
        assert_eq!(
            terminal_error_result(&row),
            Some(TargetResult::Failed {
                code: ErrorCode::PortInUse,
                message: "held by pid 999".to_string(),
            })
        );
    }

    // -- wait_step (pure wait/timeout decision) -----------------------------

    #[test]
    fn wait_step_succeeds_immediately_once_is_done() {
        let row = row(HealthState::Running, Source::Unit, None);
        let result = wait_step(
            &row,
            &is_running,
            Duration::from_secs(5),
            10_000,
            "a",
            ErrorCode::StartTimeout,
        );
        assert_eq!(result, Some(TargetResult::Succeeded));
    }

    #[test]
    fn wait_step_short_circuits_on_a_terminal_error_even_with_time_remaining() {
        // A fresh `port_in_use` will not resolve itself by waiting longer
        // — report it now instead of polling until `wait_ms` elapses.
        let row = row(
            HealthState::Error,
            Source::None,
            Some(StatusError {
                code: ErrorCode::PortInUse,
                detail: "held by pid 999".to_string(),
            }),
        );
        let result = wait_step(
            &row,
            &is_running,
            Duration::from_secs(5),
            10_000,
            "a",
            ErrorCode::StartTimeout,
        );
        assert_eq!(
            result,
            Some(TargetResult::Failed {
                code: ErrorCode::PortInUse,
                message: "held by pid 999".to_string(),
            })
        );
    }

    #[test]
    fn wait_step_keeps_waiting_when_not_done_and_time_remains() {
        let row = row(HealthState::Starting, Source::Unit, None);
        let result = wait_step(
            &row,
            &is_running,
            Duration::from_secs(5),
            10_000,
            "a",
            ErrorCode::StartTimeout,
        );
        assert_eq!(result, None);
    }

    #[test]
    fn wait_step_times_out_with_the_given_code_when_remaining_is_zero() {
        let row = row(HealthState::Starting, Source::Unit, None);
        let result = wait_step(
            &row,
            &is_running,
            Duration::ZERO,
            10_000,
            "a",
            ErrorCode::StartTimeout,
        );
        assert_eq!(
            result,
            Some(TargetResult::Failed {
                code: ErrorCode::StartTimeout,
                message: "connection `a` did not confirm within 10000ms".to_string(),
            })
        );
    }
}
