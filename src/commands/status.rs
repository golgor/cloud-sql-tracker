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
use crate::model::{
    self, Connection, ErrorCode, GroupCounts, HealthState, Source, StatusDocument, StatusRow,
    UnitName,
};
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
///
/// Named `StatusCommandError` (not `StatusError`) to stay distinct from
/// [`model::StatusError`], the Status row `error` object
/// (`AGENTS.md`, "Writing": same word for the same thing — these are two
/// different things that happen to share a plain-English name).
#[derive(Debug, thiserror::Error)]
pub(crate) enum StatusCommandError {
    #[error(transparent)]
    Select(#[from] SelectError),
    #[error("connection `{id}`: {source}")]
    UnitName {
        id: String,
        #[source]
        source: model::UnitNameError,
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
pub(crate) fn status(
    config: &Config,
    selector: &Selector,
) -> Result<StatusDocument, StatusCommandError> {
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
///
/// A Connection whose `address` does not parse as an IP degrades to its own
/// error row instead of failing the whole document
/// (`docs/cli-contract.v1.md`, "status": "Exit `0` even when some
/// connections have `state: error` (errors are data)") — `docs/config.v1.md`
/// only requires `address` to be a non-empty string, so this is a runtime
/// possibility, not just a hypothetical.
fn observe_and_reconcile(
    connection: &Connection,
    now: SystemTime,
    mono_now_usec: Option<u64>,
) -> Result<StatusRow, StatusCommandError> {
    let unit = model::unit_name(&connection.id).map_err(|source| StatusCommandError::UnitName {
        id: connection.id.clone(),
        source,
    })?;

    let address: IpAddr = match connection.address.parse() {
        Ok(address) => address,
        Err(_) => return Ok(invalid_address_row(connection, unit)),
    };

    let snapshot = supervisor::show(&unit).map_err(|source| StatusCommandError::Supervisor {
        id: connection.id.clone(),
        source: Box::new(source),
    })?;
    let port_observation = port::observe(address, connection.port);

    let observation = Observation {
        unit: map_unit_snapshot(snapshot, now, mono_now_usec),
        port: port_observation,
    };
    Ok(reconcile::reconcile(connection, &observation, now))
}

/// A Status row for a Connection whose `address` cannot be parsed as an IP.
/// Pure: no I/O, so `status`'s per-row degrade path is directly testable.
fn invalid_address_row(connection: &Connection, unit: UnitName) -> StatusRow {
    StatusRow {
        id: connection.id.clone(),
        name: connection.name.clone(),
        group: connection.group.clone(),
        instance: connection.instance.clone(),
        address: connection.address.clone(),
        port: connection.port,
        private_ip: connection.private_ip,
        state: HealthState::Error,
        source: Source::None,
        pid: None,
        unit: Some(unit),
        port_open: false,
        uptime_sec: None,
        error: Some(model::StatusError {
            code: ErrorCode::Config,
            detail: format!(
                "connection `{}` has an invalid address `{}`",
                connection.id, connection.address
            ),
        }),
    }
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
        UnitActiveState::Unknown(_) => UnitState::Failed(conservative_crash_signal()),
    }
}

/// A signal `reconcile::classify_failure_signal` always reads as
/// [`FailureKind::Crashed`] (`error.code: unit_failed`), never a clean stop
/// or `exec_failed` — `exec_main_status: -1` is not a real signal number,
/// only a sentinel that fails every pattern `classify_failure_signal`
/// checks (`docs/reconcile.v1.md`, "Clean stop vs failed unit"). Shared by
/// every "this adapter does not trust what systemd just reported" path: an
/// unrecognized `ActiveState`, and a `Result=`/`ExecMainCode` pair that
/// disagrees with itself.
fn conservative_crash_signal() -> FailureSignal {
    FailureSignal {
        result: ReconcileUnitResult::Signal,
        exec_main_status: -1,
    }
}

/// Maps `supervisor`'s already-decoded `Result=`/`ExecMainCode`/
/// `ExecMainStatus` reading onto `reconcile::FailureSignal`'s narrower
/// clean-stop-vs-crash vocabulary (`docs/reconcile.v1.md`, "Clean stop vs
/// failed unit").
///
/// `Result=` and `ExecMainCode` normally agree
/// (`docs/research/supervisor-io.md`, "exit-vs-signal discriminator"): a
/// `Result=signal`/`core-dump` Unit reports its `ExecMainStatus` as a
/// signal number (`ExecOutcome::Signal`), and `Result=success`/`exit-code`
/// reports it as an exit code (`ExecOutcome::ExitCode`). This function only
/// trusts `exec_main_status`'s value when the two agree — a
/// `Result=signal` paired with `ExecOutcome::ExitCode(15)`, for example,
/// must never read as the clean-stop pattern `(Signal, 15)` just because
/// `15` happened to be the number `supervisor` reported for something
/// else entirely.
fn failure_signal(result: SupervisorUnitResult, exec_outcome: ExecOutcome) -> FailureSignal {
    match result {
        SupervisorUnitResult::Success => match exec_outcome {
            ExecOutcome::ExitCode(0) => FailureSignal {
                result: ReconcileUnitResult::Success,
                exec_main_status: 0,
            },
            _ => conservative_crash_signal(),
        },
        SupervisorUnitResult::ExitCode => match exec_outcome {
            ExecOutcome::ExitCode(status) => FailureSignal {
                result: ReconcileUnitResult::ExitCode,
                exec_main_status: status,
            },
            _ => conservative_crash_signal(),
        },
        SupervisorUnitResult::Signal => match exec_outcome {
            ExecOutcome::Signal(status) => FailureSignal {
                result: ReconcileUnitResult::Signal,
                exec_main_status: status,
            },
            _ => conservative_crash_signal(),
        },
        // `classify_failure_signal` never treats `CoreDump`/`Timeout`/
        // `ExecCondition` as a clean stop regardless of `exec_main_status`'s
        // value, so there is no clean-stop pattern for a mismatched pair to
        // accidentally satisfy here — the value is carried through only for
        // `error.detail`/diagnostics.
        SupervisorUnitResult::CoreDump => FailureSignal {
            result: ReconcileUnitResult::CoreDump,
            exec_main_status: exec_status_value(exec_outcome),
        },
        SupervisorUnitResult::Timeout => FailureSignal {
            result: ReconcileUnitResult::Timeout,
            exec_main_status: exec_status_value(exec_outcome),
        },
        SupervisorUnitResult::ExecCondition => FailureSignal {
            result: ReconcileUnitResult::ExecCondition,
            exec_main_status: exec_status_value(exec_outcome),
        },
        // An unrecognized future `Result=` value: conservative crash, the
        // same spirit as an unrecognized `ActiveState`
        // (`docs/research/supervisor-io.md`, "unknown future state/result
        // string").
        SupervisorUnitResult::Unknown(_) => conservative_crash_signal(),
    }
}

fn exec_status_value(exec_outcome: ExecOutcome) -> i32 {
    match exec_outcome {
        ExecOutcome::NotExited => 0,
        ExecOutcome::ExitCode(status) => status,
        ExecOutcome::Signal(status) => status,
        ExecOutcome::Unknown { status, .. } => status,
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

/// A `CLOCK_MONOTONIC` reading, the same clock domain systemd samples
/// `ExecMainStartTimestampMonotonic` from
/// (`docs/research/supervisor-io.md`, "Use monotonic start time for
/// Reconcile"). **Not** `/proc/uptime`: that file is `CLOCK_BOOTTIME`,
/// which keeps advancing while the machine is suspended, so on a laptop
/// that suspends it would drift from systemd's timestamp by the total
/// suspend time since boot and silently corrupt `uptime_sec` / the start
/// window (`clock_gettime(2)`:
/// <https://man7.org/linux/man-pages/man2/clock_gettime.2.html>).
/// `clock_gettime` is the Linux syscall that reads a kernel clock; `rustix`
/// wraps it as a safe, allocation-free function.
///
/// `None` degrades `started_at` to unknown — `reconcile` already treats
/// that as outside the start window — rather than failing `status`
/// entirely, the same best-effort spirit as `port`'s listener attribution.
/// In practice this is always `Some` on Linux: `rustix::time::clock_gettime`
/// is infallible for `ClockId::Monotonic`, and the `Option` here only
/// guards the `i64` -> `u64` conversion.
fn monotonic_now_usec() -> Option<u64> {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let sec_usec = u64::try_from(now.tv_sec).ok()?.checked_mul(1_000_000)?;
    let nsec_usec = u64::try_from(now.tv_nsec).ok()? / 1_000;
    sec_usec.checked_add(nsec_usec)
}

// ---------------------------------------------------------------------------
// StatusRow[] -> StatusDocument.
// ---------------------------------------------------------------------------

/// Assembles the Status document's aggregates and `connections[]` from
/// already-reconciled rows (`docs/status-document.v1.md`, "Top-level
/// object"). Pure: `now` and `cli_version` are explicit inputs so this is
/// testable without a real clock or `CARGO_PKG_VERSION`.
fn assemble_document(rows: Vec<StatusRow>, now: SystemTime, cli_version: String) -> StatusDocument {
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
        // `monotonic_now_usec` can fail to read `CLOCK_MONOTONIC`; a Unit
        // that does report a start time must still degrade to `None`, not
        // panic or silently look "just started".
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
        assert_eq!(signal, conservative_crash_signal());
    }

    // -- map_active_state: every branch, not just Failed/Unknown -----------

    #[test]
    fn map_active_state_inactive_is_idle() {
        let state = map_active_state(
            UnitActiveState::Inactive,
            SupervisorUnitResult::Success,
            ExecOutcome::NotExited,
        );
        assert_eq!(state, UnitState::Idle);
    }

    #[test]
    fn map_active_state_activating_is_activating() {
        let state = map_active_state(
            UnitActiveState::Activating,
            SupervisorUnitResult::Success,
            ExecOutcome::NotExited,
        );
        assert_eq!(state, UnitState::Activating);
    }

    #[test]
    fn map_active_state_deactivating_is_deactivating() {
        let state = map_active_state(
            UnitActiveState::Deactivating,
            SupervisorUnitResult::Success,
            ExecOutcome::NotExited,
        );
        assert_eq!(state, UnitState::Deactivating);
    }

    // -- failure_signal: Result=/ExecMainCode agreement ---------------------

    #[test]
    fn failure_signal_signal_result_with_matching_signal_outcome_is_trusted() {
        let signal = failure_signal(SupervisorUnitResult::Signal, ExecOutcome::Signal(15));
        assert_eq!(
            signal,
            FailureSignal {
                result: ReconcileUnitResult::Signal,
                exec_main_status: 15,
            }
        );
    }

    #[test]
    fn failure_signal_exit_code_result_with_matching_exit_code_outcome_is_trusted() {
        let signal = failure_signal(SupervisorUnitResult::ExitCode, ExecOutcome::ExitCode(0));
        assert_eq!(
            signal,
            FailureSignal {
                result: ReconcileUnitResult::ExitCode,
                exec_main_status: 0,
            }
        );
    }

    #[test]
    fn failure_signal_exit_code_result_with_a_clean_stop_status_143_is_trusted() {
        let signal = failure_signal(SupervisorUnitResult::ExitCode, ExecOutcome::ExitCode(143));
        assert_eq!(
            signal,
            FailureSignal {
                result: ReconcileUnitResult::ExitCode,
                exec_main_status: 143,
            }
        );
    }

    #[test]
    fn failure_signal_success_result_with_exit_zero_is_trusted() {
        let signal = failure_signal(SupervisorUnitResult::Success, ExecOutcome::ExitCode(0));
        assert_eq!(
            signal,
            FailureSignal {
                result: ReconcileUnitResult::Success,
                exec_main_status: 0,
            }
        );
    }

    #[test]
    fn failure_signal_signal_result_with_a_mismatched_exit_code_outcome_never_reads_as_clean() {
        // `Result=signal` paired with `ExecOutcome::ExitCode(15)` (not
        // `ExecOutcome::Signal(15)`) is the exact malformed pair a reviewer
        // flagged: naively reusing `15` here would satisfy
        // `classify_failure_signal`'s `(Signal, 15)` clean-stop pattern for
        // the wrong reason.
        let signal = failure_signal(SupervisorUnitResult::Signal, ExecOutcome::ExitCode(15));
        assert_eq!(signal, conservative_crash_signal());
    }

    #[test]
    fn failure_signal_exit_code_result_with_not_exited_never_reads_as_clean() {
        // `Result=exit-code` paired with `ExecOutcome::NotExited` (status
        // defaults to `0`) must not satisfy the `(ExitCode, 0)` clean-stop
        // pattern — there was no real exit code to read.
        let signal = failure_signal(SupervisorUnitResult::ExitCode, ExecOutcome::NotExited);
        assert_eq!(signal, conservative_crash_signal());
    }

    #[test]
    fn failure_signal_success_result_with_a_mismatched_signal_outcome_never_reads_as_clean() {
        let signal = failure_signal(SupervisorUnitResult::Success, ExecOutcome::Signal(9));
        assert_eq!(signal, conservative_crash_signal());
    }

    #[test]
    fn failure_signal_unknown_exec_outcome_is_conservative_for_every_result() {
        let signal = failure_signal(
            SupervisorUnitResult::ExitCode,
            ExecOutcome::Unknown { code: 4, status: 0 },
        );
        assert_eq!(signal, conservative_crash_signal());
    }

    #[test]
    fn failure_signal_core_dump_result_is_always_crash_regardless_of_status() {
        // `classify_failure_signal` has no clean-stop pattern for
        // `CoreDump`, so the exact status value carried through does not
        // change the outcome — covered here for completeness of the
        // `Result=` match.
        let signal = failure_signal(SupervisorUnitResult::CoreDump, ExecOutcome::Signal(6));
        assert_eq!(
            signal,
            FailureSignal {
                result: ReconcileUnitResult::CoreDump,
                exec_main_status: 6,
            }
        );
    }

    #[test]
    fn failure_signal_exec_condition_result_carries_its_status_through() {
        let signal = failure_signal(
            SupervisorUnitResult::ExecCondition,
            ExecOutcome::ExitCode(1),
        );
        assert_eq!(
            signal,
            FailureSignal {
                result: ReconcileUnitResult::ExecCondition,
                exec_main_status: 1,
            }
        );
    }

    // -- monotonic_now_usec ---------------------------------------------------

    #[test]
    fn monotonic_now_usec_reads_clock_monotonic_successfully() {
        // Infallible on Linux; this is a sanity check that the syscall
        // wiring and unit conversion do not panic or return `None` on the
        // platform this crate targets.
        let usec = monotonic_now_usec().expect("CLOCK_MONOTONIC should be available on Linux");
        assert!(
            usec > 0,
            "a booted machine's CLOCK_MONOTONIC reading is never zero"
        );
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

    fn connection(id: &str, group: &str, port: u16) -> Connection {
        Connection {
            id: id.to_string(),
            name: id.to_string(),
            group: group.to_string(),
            instance: "proj:region:inst".to_string(),
            address: "127.0.0.1".to_string(),
            port,
            private_ip: false,
            auto_iam_authn: false,
            extra_args: Vec::new(),
            enabled: true,
        }
    }

    fn validate_against_status_schema(doc: &StatusDocument) {
        let instance = serde_json::to_value(doc).expect("StatusDocument serializes");

        let schema_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/status.v1.json");
        let schema_text = std::fs::read_to_string(schema_path).expect("read the status schema");
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

    /// Proves the whole pure pipeline this ticket (#42) adds —
    /// `Observation` -> `reconcile::reconcile` -> `assemble_document` ->
    /// serde -- not just `StatusDocument`'s own serde attributes
    /// (`docs/verification.v1.md`, "Status / Doctor JSON": "Construct
    /// **Observation** in-process"). Still in-process: this does not close
    /// #23 (real `status --json` stdout).
    #[test]
    fn status_document_built_through_reconcile_serializes_to_the_status_v1_schema_shape() {
        use crate::model::{PortObservation, PortProbe};

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);

        let running = connection("backend-dev", "backend", 15432);
        let running_row = reconcile::reconcile(
            &running,
            &Observation {
                unit: UnitObservation {
                    state: UnitState::Active,
                    main_pid: Some(4242),
                    started_at: Some(now - Duration::from_secs(3_600)),
                },
                port: PortObservation {
                    probe: PortProbe::Open,
                    listener_pid: Some(4242),
                    listener_name: Some("cloud-sql-proxy".to_string()),
                },
            },
            now,
        );
        assert_eq!(running_row.state, HealthState::Running);

        let failed = connection("fe-dev", "fe", 15434);
        let failed_row = reconcile::reconcile(
            &failed,
            &Observation {
                unit: UnitObservation {
                    state: UnitState::Failed(FailureSignal {
                        result: ReconcileUnitResult::Signal,
                        exec_main_status: 9,
                    }),
                    main_pid: Some(4243),
                    started_at: None,
                },
                port: PortObservation {
                    probe: PortProbe::Closed,
                    listener_pid: None,
                    listener_name: None,
                },
            },
            now,
        );
        assert_eq!(failed_row.state, HealthState::Error);

        let doc = assemble_document(vec![running_row, failed_row], now, "0.1.0".to_string());
        validate_against_status_schema(&doc);
    }

    // -- invalid_address_row (must produce a document, never fail it) ------

    #[test]
    fn invalid_address_row_is_a_config_error_row() {
        let target = connection("bad-address", "fe", 15432);
        let unit = model::unit_name(&target.id).expect("a valid test id");
        let row = invalid_address_row(&target, unit.clone());

        assert_eq!(row.id, "bad-address");
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::None);
        assert_eq!(row.pid, None);
        assert_eq!(row.unit, Some(unit));
        assert!(!row.port_open);
        assert_eq!(row.uptime_sec, None);
        let error = row.error.expect("an invalid address is a Config error");
        assert_eq!(error.code, ErrorCode::Config);
    }

    #[test]
    fn invalid_address_row_still_validates_against_the_status_v1_schema() {
        let target = connection("bad-address", "fe", 15432);
        let unit = model::unit_name(&target.id).expect("a valid test id");
        let row = invalid_address_row(&target, unit);
        let doc = assemble_document(vec![row], now(), "0.1.0".to_string());
        validate_against_status_schema(&doc);
    }
}
