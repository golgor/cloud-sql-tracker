//! `status(cfg, selector) -> StatusDocument` (`docs/modules.v1.md`,
//! "commands": "status" row) — the only I/O this ticket
//! ([#42](https://github.com/golgor/cloud-sql-tracker/issues/42)) wires up.
//!
//! Composition only: `supervisor::show` + `port::observe` become one
//! `reconcile::Observation`, `reconcile::reconcile` classifies it, and the
//! resulting rows are assembled into a [`model::StatusDocument`]. Reconcile
//! itself stays pure (`docs/modules.v1.md`, "Dependency direction");
//! everything below that composes its inputs from adapter-local shapes
//! lives here, not in `reconcile` or `supervisor`.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::model::{self, Connection, GroupCounts, HealthState, StatusDocument, StatusRow};
use crate::port;
use crate::reconcile::{
    self, FailureSignal, Observation, UnitObservation, UnitResult as ReconcileUnitResult, UnitState,
};
use crate::supervisor::{
    self, ExecOutcome, SupervisorError, UnitActiveState, UnitResult as SupervisorUnitResult,
    UnitSnapshot,
};

use super::select::{self, SelectError, Selector};

/// Why `status` could not produce a document.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StatusError {
    #[error(transparent)]
    Select(#[from] SelectError),
    #[error("connection `{id}`: {source}")]
    UnitName {
        id: String,
        #[source]
        source: model::UnitNameError,
    },
    #[error("connection `{id}` has an invalid address `{address}`: {source}")]
    InvalidAddress {
        id: String,
        address: String,
        #[source]
        source: std::net::AddrParseError,
    },
    #[error("connection `{id}`: {source}")]
    Supervisor {
        id: String,
        // Boxed: `SupervisorError` carries a `zbus::Error`, which makes it
        // much larger than this enum's other variants
        // (`clippy::result_large_err`).
        #[source]
        source: Box<SupervisorError>,
    },
}

/// Reconcile and report every Connection the selector names
/// (`docs/modules.v1.md`, "commands": "status"). `status` with no target is
/// `Selector::All` — that default lives in `cli` (#45), not here
/// (`docs/cli-contract.v1.md`, "Defaults").
///
/// Only reachable from `cli` (#45) so far — this ticket
/// ([#42](https://github.com/golgor/cloud-sql-tracker/issues/42)) proves
/// the composition through this module's own unit tests instead (real
/// `supervisor`/`port` I/O needs a live systemd user session, which
/// `docs/verification.v1.md` does not require as a unit test).
#[allow(dead_code)]
pub(crate) fn status(config: &Config, selector: &Selector) -> Result<StatusDocument, StatusError> {
    let targets = select::expand(config, selector)?;
    let now = SystemTime::now();
    let mono_now_usec = monotonic_now_usec();

    let mut rows = Vec::with_capacity(targets.len());
    for connection in targets {
        rows.push(observe_and_reconcile(connection, now, mono_now_usec)?);
    }

    Ok(assemble_document(
        rows,
        now,
        env!("CARGO_PKG_VERSION").to_string(),
    ))
}

/// One Connection's full round trip: gather Observation, then classify it.
fn observe_and_reconcile(
    connection: &Connection,
    now: SystemTime,
    mono_now_usec: Option<u64>,
) -> Result<StatusRow, StatusError> {
    let unit = model::unit_name(&connection.id).map_err(|source| StatusError::UnitName {
        id: connection.id.clone(),
        source,
    })?;
    let snapshot = supervisor::show(&unit).map_err(|source| StatusError::Supervisor {
        id: connection.id.clone(),
        source: Box::new(source),
    })?;
    let address: IpAddr =
        connection
            .address
            .parse()
            .map_err(|source| StatusError::InvalidAddress {
                id: connection.id.clone(),
                address: connection.address.clone(),
                source,
            })?;
    let port_observation = port::observe(address, connection.port);

    let observation = Observation {
        unit: map_unit_snapshot(snapshot, now, mono_now_usec),
        port: port_observation,
    };
    Ok(reconcile::reconcile(connection, &observation, now))
}

// ---------------------------------------------------------------------------
// UnitSnapshot -> UnitObservation (pure composition; `supervisor` and
// `reconcile` intentionally do not know about each other's types).
// ---------------------------------------------------------------------------

/// Maps a `supervisor`-local [`UnitSnapshot`] onto the narrower vocabulary
/// [`reconcile::UnitObservation`] needs. Pure given its inputs: `now` and
/// `mono_now_usec` are the only "current time" this function reads.
fn map_unit_snapshot(
    snapshot: UnitSnapshot,
    now: SystemTime,
    mono_now_usec: Option<u64>,
) -> UnitObservation {
    match snapshot {
        UnitSnapshot::Absent => UnitObservation {
            state: UnitState::Idle,
            main_pid: None,
            started_at: None,
        },
        UnitSnapshot::Loaded {
            active_state,
            main_pid,
            result,
            exec_outcome,
            started_at_monotonic_usec,
            ..
        } => {
            let started_at = match (started_at_monotonic_usec, mono_now_usec) {
                (Some(start_usec), Some(now_usec)) => {
                    started_at_from_monotonic(start_usec, now_usec, now)
                }
                _ => None,
            };
            UnitObservation {
                state: map_active_state(active_state, result, exec_outcome),
                main_pid,
                started_at,
            }
        }
    }
}

/// `docs/research/supervisor-io.md`, "unknown future state/result string":
/// an `ActiveState` this adapter does not recognize must never look
/// healthy. `reconcile::UnitState` has no `Unknown` variant, so this maps
/// to `Failed` with a signal `reconcile::classify_failure_signal` always
/// reads as a crash — the closest "surface as `error`, never silently
/// healthy" outcome the pure truth table already has a row for.
fn map_active_state(
    active_state: UnitActiveState,
    result: SupervisorUnitResult,
    exec_outcome: ExecOutcome,
) -> UnitState {
    match active_state {
        UnitActiveState::Inactive => UnitState::Idle,
        UnitActiveState::Activating => UnitState::Activating,
        UnitActiveState::Active => UnitState::Active,
        UnitActiveState::Deactivating => UnitState::Deactivating,
        UnitActiveState::Failed => UnitState::Failed(failure_signal(result, exec_outcome)),
        UnitActiveState::Unknown(_) => UnitState::Failed(unknown_active_state_signal()),
    }
}

/// A signal `reconcile::classify_failure_signal` always reads as
/// [`FailureKind::Crashed`], never a clean stop — `exec_main_status: -1`
/// is not a real signal number, only a sentinel that fails every clean-stop
/// pattern (`docs/reconcile.v1.md`, "Clean stop vs failed unit").
fn unknown_active_state_signal() -> FailureSignal {
    FailureSignal {
        result: ReconcileUnitResult::Signal,
        exec_main_status: -1,
    }
}

/// Maps `supervisor`'s already-decoded `Result=`/`ExecMainCode`/
/// `ExecMainStatus` reading onto `reconcile::FailureSignal`'s narrower
/// clean-stop-vs-crash vocabulary (`docs/reconcile.v1.md`, "Clean stop vs
/// failed unit"). `exec_main_status` is `ExecOutcome`'s inner status either
/// way — `reconcile` does not care whether it came from an exit code or a
/// signal number, only the `(result, exec_main_status)` pair.
fn failure_signal(result: SupervisorUnitResult, exec_outcome: ExecOutcome) -> FailureSignal {
    let exec_main_status = match exec_outcome {
        ExecOutcome::NotExited => 0,
        ExecOutcome::ExitCode(status) => status,
        ExecOutcome::Signal(status) => status,
        ExecOutcome::Unknown { status, .. } => status,
    };
    let result = match result {
        SupervisorUnitResult::Success => ReconcileUnitResult::Success,
        SupervisorUnitResult::ExitCode => ReconcileUnitResult::ExitCode,
        SupervisorUnitResult::Signal => ReconcileUnitResult::Signal,
        SupervisorUnitResult::Timeout => ReconcileUnitResult::Timeout,
        SupervisorUnitResult::CoreDump => ReconcileUnitResult::CoreDump,
        SupervisorUnitResult::ExecCondition => ReconcileUnitResult::ExecCondition,
        // An unrecognized future `Result=` value: `Timeout` is the closest
        // "never a clean stop" mapping in `classify_failure_signal`
        // (`docs/research/supervisor-io.md`, "unknown future state/result
        // string").
        SupervisorUnitResult::Unknown(_) => ReconcileUnitResult::Timeout,
    };
    FailureSignal {
        result,
        exec_main_status,
    }
}

/// `start_usec`/`now_usec` are both `CLOCK_MONOTONIC`-domain microseconds
/// since boot (`docs/research/supervisor-io.md`, "Use monotonic start time
/// for Reconcile"). `None` when the clock looks like it went backwards
/// (a race between the two reads) rather than reporting a future start.
fn started_at_from_monotonic(
    start_usec: u64,
    now_usec: u64,
    now: SystemTime,
) -> Option<SystemTime> {
    let age_usec = now_usec.checked_sub(start_usec)?;
    now.checked_sub(Duration::from_micros(age_usec))
}

/// A best-effort `CLOCK_MONOTONIC`-domain reading via `/proc/uptime`'s
/// first field, in the same clock domain systemd's
/// `ExecMainStartTimestampMonotonic` samples from. `None` degrades
/// `started_at` to unknown — `reconcile` already treats that as outside
/// the start window — rather than failing `status` entirely, the same
/// best-effort spirit as `port`'s listener attribution.
fn monotonic_now_usec() -> Option<u64> {
    let raw = std::fs::read_to_string("/proc/uptime").ok()?;
    let seconds: f64 = raw.split_whitespace().next()?.parse().ok()?;
    Some((seconds * 1_000_000.0) as u64)
}

// ---------------------------------------------------------------------------
// StatusRow[] -> StatusDocument.
// ---------------------------------------------------------------------------

/// Assembles the Status document's aggregates and `connections[]` from
/// already-reconciled rows (`docs/status-document.v1.md`, "Top-level
/// object"). Pure: `now` and `cli_version` are explicit inputs so this is
/// testable without a real clock or `CARGO_PKG_VERSION`.
pub(crate) fn assemble_document(
    rows: Vec<StatusRow>,
    now: SystemTime,
    cli_version: String,
) -> StatusDocument {
    let mut groups: BTreeMap<String, GroupCounts> = BTreeMap::new();
    let mut totals = GroupCounts::default();
    for row in &rows {
        tally(groups.entry(row.group.clone()).or_default(), row.state);
        tally(&mut totals, row.state);
    }

    StatusDocument {
        version: 1,
        ts: format_rfc3339(now),
        cli_version,
        running: totals.running,
        starting: totals.starting,
        error: totals.error,
        stopped: totals.stopped,
        total: totals.total,
        groups,
        connections: rows,
    }
}

fn tally(counts: &mut GroupCounts, state: HealthState) {
    match state {
        HealthState::Running => counts.running += 1,
        HealthState::Starting => counts.starting += 1,
        HealthState::Error => counts.error += 1,
        HealthState::Stopped => counts.stopped += 1,
    }
    counts.total += 1;
}

/// `ts` as a UTC RFC 3339 timestamp (`docs/status-document.v1.md`, "`ts`":
/// "local offset OK" — UTC's `Z` suffix is a valid RFC 3339 offset too).
///
/// Hand-rolled rather than a `time`/`chrono` dependency: no date/time crate
/// is already in this crate's dependency tree (`Cargo.toml`), and this is
/// the only field that needs one.
fn format_rfc3339(now: SystemTime) -> String {
    let total_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let days = (total_secs / 86_400) as i64;
    let secs_of_day = total_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: the proleptic Gregorian
/// year/month/day for `z` days since the Unix epoch (1970-01-01).
/// <http://howardhinnant.github.io/date_algorithms.html#civil_from_days>
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ErrorCode, Source};

    // -- map_unit_snapshot ---------------------------------------------------

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    #[test]
    fn map_unit_snapshot_absent_is_idle_with_no_pid_or_start_time() {
        let observation = map_unit_snapshot(UnitSnapshot::Absent, now(), Some(1_000_000_000));
        assert_eq!(observation.state, UnitState::Idle);
        assert_eq!(observation.main_pid, None);
        assert_eq!(observation.started_at, None);
    }

    #[test]
    fn map_unit_snapshot_active_with_pid_and_start_time_five_seconds_ago() {
        let snapshot = UnitSnapshot::Loaded {
            active_state: UnitActiveState::Active,
            sub_state: "running".to_string(),
            main_pid: Some(111),
            result: SupervisorUnitResult::Success,
            exec_outcome: ExecOutcome::NotExited,
            started_at_monotonic_usec: Some(5_000_000), // 5s since boot
        };
        // "Now" is 10s since boot -> the unit started 5s ago.
        let observation = map_unit_snapshot(snapshot, now(), Some(10_000_000));
        assert_eq!(observation.state, UnitState::Active);
        assert_eq!(observation.main_pid, Some(111));
        assert_eq!(observation.started_at, Some(now() - Duration::from_secs(5)));
    }

    #[test]
    fn map_unit_snapshot_failed_with_a_clean_stop_signal() {
        let snapshot = UnitSnapshot::Loaded {
            active_state: UnitActiveState::Failed,
            sub_state: "failed".to_string(),
            main_pid: None,
            result: SupervisorUnitResult::Signal,
            exec_outcome: ExecOutcome::Signal(15),
            started_at_monotonic_usec: None,
        };
        let observation = map_unit_snapshot(snapshot, now(), None);
        assert_eq!(
            observation.state,
            UnitState::Failed(FailureSignal {
                result: ReconcileUnitResult::Signal,
                exec_main_status: 15,
            })
        );
    }

    #[test]
    fn map_unit_snapshot_failed_with_exit_code_zero_is_a_clean_stop_signal() {
        let snapshot = UnitSnapshot::Loaded {
            active_state: UnitActiveState::Failed,
            sub_state: "failed".to_string(),
            main_pid: None,
            result: SupervisorUnitResult::ExitCode,
            exec_outcome: ExecOutcome::ExitCode(0),
            started_at_monotonic_usec: None,
        };
        let observation = map_unit_snapshot(snapshot, now(), None);
        assert_eq!(
            observation.state,
            UnitState::Failed(FailureSignal {
                result: ReconcileUnitResult::ExitCode,
                exec_main_status: 0,
            })
        );
    }

    #[test]
    fn map_unit_snapshot_missing_monotonic_now_leaves_started_at_unknown() {
        // `monotonic_now_usec` can fail to read `/proc/uptime`; a Unit that
        // does report a start time must still degrade to `None`, not panic
        // or silently look "just started".
        let snapshot = UnitSnapshot::Loaded {
            active_state: UnitActiveState::Active,
            sub_state: "running".to_string(),
            main_pid: Some(111),
            result: SupervisorUnitResult::Success,
            exec_outcome: ExecOutcome::NotExited,
            started_at_monotonic_usec: Some(5_000_000),
        };
        let observation = map_unit_snapshot(snapshot, now(), None);
        assert_eq!(observation.started_at, None);
    }

    #[test]
    fn map_unit_snapshot_unknown_active_state_is_never_healthy() {
        let snapshot = UnitSnapshot::Loaded {
            active_state: UnitActiveState::Unknown("reloading".to_string()),
            sub_state: "reloading".to_string(),
            main_pid: Some(111),
            result: SupervisorUnitResult::Success,
            exec_outcome: ExecOutcome::NotExited,
            started_at_monotonic_usec: None,
        };
        let observation = map_unit_snapshot(snapshot, now(), None);
        assert!(
            matches!(observation.state, UnitState::Failed(_)),
            "an unrecognized ActiveState must never map to Idle/Activating/Active/Deactivating"
        );
    }

    #[test]
    fn map_unit_snapshot_unknown_result_is_never_a_clean_stop() {
        let snapshot = UnitSnapshot::Loaded {
            active_state: UnitActiveState::Failed,
            sub_state: "failed".to_string(),
            main_pid: None,
            result: SupervisorUnitResult::Unknown("oom-kill".to_string()),
            exec_outcome: ExecOutcome::Signal(9),
            started_at_monotonic_usec: None,
        };
        let observation = map_unit_snapshot(snapshot, now(), None);
        let UnitState::Failed(signal) = observation.state else {
            panic!("expected a Failed state");
        };
        assert_eq!(signal.result, ReconcileUnitResult::Timeout);
    }

    // -- started_at_from_monotonic --------------------------------------------

    #[test]
    fn started_at_from_monotonic_subtracts_the_age() {
        let started_at = started_at_from_monotonic(5_000_000, 12_000_000, now());
        assert_eq!(started_at, Some(now() - Duration::from_secs(7)));
    }

    #[test]
    fn started_at_from_monotonic_is_none_when_the_clock_looks_like_it_went_backwards() {
        // `now_usec < start_usec` should not happen in practice, but two
        // separate reads (the Unit's start time, then our own clock) could
        // race; never report a start time in the future.
        let started_at = started_at_from_monotonic(12_000_000, 5_000_000, now());
        assert_eq!(started_at, None);
    }

    // -- assemble_document -----------------------------------------------------

    fn row(id: &str, group: &str, state: HealthState) -> StatusRow {
        StatusRow {
            id: id.to_string(),
            name: id.to_string(),
            group: group.to_string(),
            instance: "proj:region:inst".to_string(),
            address: "127.0.0.1".to_string(),
            port: 15432,
            private_ip: false,
            state,
            source: Source::None,
            pid: None,
            unit: None,
            port_open: state == HealthState::Running,
            uptime_sec: None,
            error: (state == HealthState::Error).then(|| crate::model::StatusError {
                code: ErrorCode::PortInUse,
                detail: "port held by someone else".to_string(),
            }),
        }
    }

    #[test]
    fn assemble_document_is_empty_for_zero_rows() {
        let doc = assemble_document(Vec::new(), now(), "0.1.0".to_string());
        assert_eq!(doc.version, 1);
        assert_eq!(doc.total, 0);
        assert_eq!(doc.running, 0);
        assert!(doc.groups.is_empty());
        assert!(doc.connections.is_empty());
    }

    #[test]
    fn assemble_document_counts_states_and_groups() {
        let rows = vec![
            row("a", "fe", HealthState::Running),
            row("b", "fe", HealthState::Error),
            row("c", "backend", HealthState::Stopped),
            row("d", "backend", HealthState::Starting),
        ];
        let doc = assemble_document(rows, now(), "0.1.0".to_string());

        assert_eq!(doc.running, 1);
        assert_eq!(doc.error, 1);
        assert_eq!(doc.stopped, 1);
        assert_eq!(doc.starting, 1);
        assert_eq!(doc.total, 4);

        let fe = doc.groups.get("fe").expect("fe group present");
        assert_eq!(fe.running, 1);
        assert_eq!(fe.error, 1);
        assert_eq!(fe.total, 2);

        let backend = doc.groups.get("backend").expect("backend group present");
        assert_eq!(backend.stopped, 1);
        assert_eq!(backend.starting, 1);
        assert_eq!(backend.total, 2);

        // Invariant (`docs/status-document.v1.md`): sum of group totals ==
        // total, and running + starting + error + stopped == total.
        let group_total_sum: u32 = doc.groups.values().map(|g| g.total).sum();
        assert_eq!(group_total_sum, doc.total);
        assert_eq!(
            doc.running + doc.starting + doc.error + doc.stopped,
            doc.total
        );
    }

    // -- format_rfc3339 --------------------------------------------------------

    #[test]
    fn format_rfc3339_formats_the_unix_epoch() {
        assert_eq!(
            format_rfc3339(SystemTime::UNIX_EPOCH),
            "1970-01-01T00:00:00Z"
        );
    }

    #[test]
    fn format_rfc3339_formats_a_known_recent_date() {
        // `date -u -d @1704067200 +%Y-%m-%dT%H:%M:%SZ` => 2024-01-01T00:00:00Z
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_067_200);
        assert_eq!(format_rfc3339(t), "2024-01-01T00:00:00Z");
    }

    #[test]
    fn format_rfc3339_formats_a_date_with_a_nonzero_time_of_day() {
        // `date -u -d @1613826296 +%Y-%m-%dT%H:%M:%SZ` => 2021-02-20T13:04:56Z
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_613_826_296);
        assert_eq!(format_rfc3339(t), "2021-02-20T13:04:56Z");
    }

    #[test]
    fn format_rfc3339_formats_the_last_second_of_a_leap_year() {
        // `date -u -d @946684799 +%Y-%m-%dT%H:%M:%SZ` => 1999-12-31T23:59:59Z
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(946_684_799);
        assert_eq!(format_rfc3339(t), "1999-12-31T23:59:59Z");
    }

    // -- StatusDocument vs schemas/status.v1.json (shape proof only; does
    // not close #23 — `docs/verification.v1.md`, "Status / Doctor JSON") ---

    #[test]
    fn status_document_serializes_to_the_status_v1_schema_shape() {
        let rows = vec![
            row("backend-dev", "backend", HealthState::Running),
            row("fe-dev", "fe", HealthState::Error),
        ];
        let doc = assemble_document(rows, now(), "0.1.0".to_string());
        let instance = serde_json::to_value(&doc).expect("StatusDocument serializes");

        let schema_text =
            std::fs::read_to_string("schemas/status.v1.json").expect("read the status schema");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_text).expect("parse the status schema");
        let validator = jsonschema::validator_for(&schema).expect("compile the status schema");

        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|e| format!("{}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "status document rejected by schemas/status.v1.json:\n{}",
            errors.join("\n")
        );
    }
}
