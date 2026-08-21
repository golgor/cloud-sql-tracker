//! Pure Health classification for one Connection at one instant.
//!
//! See `docs/reconcile.v1.md` for the normative truth table this module
//! implements, and `docs/modules.v1.md` ("reconcile — deepest pure module")
//! for the frozen seam: `reconcile(identity, observation, now) ->
//! StatusRowFields`.
//!
//! No I/O, no `clap`, no `trait Supervisor`. `commands` (#42+) will gather
//! [`Observation`] from `supervisor::show` (#40) and `port::observe` (#39)
//! and call [`reconcile`]; until then nothing outside this module's own
//! tests uses these items, so the plain (non-test) library build sees them
//! as dead code under `-D warnings`.
//! Remove this `allow` once `commands` (#42) starts calling [`reconcile`].
#![allow(dead_code)]

use std::time::{Duration, SystemTime};

use crate::model::{self, Connection, ErrorCode, HealthState, Source, StatusError, StatusRow};

/// The start window (`docs/reconcile.v1.md`, "Start window"): a Connection
/// stays `starting` for at most this long after a start attempt before
/// Reconcile calls it `start_timeout` / `unit_failed` instead. Same numeric
/// default as the CLI's `--wait-ms` (`docs/cli-contract.v1.md`), but that
/// flag only bounds how long a command *blocks*; this constant is what
/// Reconcile itself uses on every call, including plain `status`.
pub(crate) const START_WINDOW: Duration = Duration::from_millis(10_000);

// ---------------------------------------------------------------------------
// Observation: everything Reconcile needs about one Connection right now.
// ---------------------------------------------------------------------------

/// One Connection's observed signals at one instant
/// (`docs/reconcile.v1.md`, "Observation inputs"). `commands` builds this
/// from `supervisor::show` + `port::observe`; Reconcile only classifies it.
#[derive(Debug, Clone)]
pub(crate) struct Observation {
    pub(crate) unit: UnitObservation,
    pub(crate) port: PortObservation,
}

/// What we know about a Connection's systemd `--user` Unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UnitObservation {
    pub(crate) state: UnitState,
    /// Main proxy PID if the Unit currently has a live process.
    pub(crate) main_pid: Option<u32>,
    /// When the current start attempt began (systemd's
    /// `ExecMainStartTimestamp` or equivalent). `None` when the Unit is
    /// [`UnitState::Idle`] or has no known start attempt yet.
    pub(crate) started_at: Option<SystemTime>,
}

/// A Unit's `ActiveState`, simplified to what Reconcile's truth table
/// distinguishes (`docs/reconcile.v1.md`, "Truth table").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitState {
    /// Not loaded (including "unit not found"), or loaded and
    /// `inactive`/`dead`. Not failed.
    Idle,
    Activating,
    Active,
    Deactivating,
    /// `ActiveState=failed`. See `docs/reconcile.v1.md`,
    /// "Clean stop vs failed unit".
    Failed(FailureSignal),
}

/// `systemd`'s `Result=` value for a failed Unit — only the values
/// `docs/reconcile.v1.md` ("Clean stop vs failed unit") discriminates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitResult {
    Success,
    ExitCode,
    Signal,
    Timeout,
    CoreDump,
    /// Failed before exec succeeded (systemd's `exec-condition` or similar).
    ExecCondition,
}

/// The raw fields Reconcile needs to tell a clean stop from a real failure,
/// for a Unit whose `ActiveState=failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FailureSignal {
    pub(crate) result: UnitResult,
    /// `ExecMainStatus`: the exit code, or (by systemd convention) 128 +
    /// signal number when the process was killed.
    pub(crate) exec_main_status: i32,
}

/// What we know about a Connection's local port right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortObservation {
    pub(crate) probe: PortProbe,
    /// Best-effort PID of whatever currently holds the listen socket.
    pub(crate) listener_pid: Option<u32>,
    /// Best-effort process name for that PID (e.g. `/proc/<pid>/comm`).
    /// `error.detail` text only — never part of the Status schema.
    pub(crate) listener_name: Option<String>,
}

/// A TCP probe result for a Connection's configured `address:port`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortProbe {
    Open,
    Closed,
    Unreachable,
}

// ---------------------------------------------------------------------------
// The pure entry point.
// ---------------------------------------------------------------------------

/// Classify one Connection's Health state from a single observation.
///
/// Pure: the same `identity`, `observation`, and `now` always produce the
/// same [`StatusRow`] fields (`docs/reconcile.v1.md`, "Mental model").
///
/// `identity.enabled` is intentionally never read here — Reconcile reports
/// the real Health of the machine; `enabled` only gates start policy
/// (`docs/reconcile.v1.md`, "`enabled: false`").
pub(crate) fn reconcile(
    identity: &Connection,
    observation: &Observation,
    now: SystemTime,
) -> StatusRow {
    let port_open = port_is_open(observation.port.probe);
    let classification = classify(
        &observation.unit,
        port_open,
        &observation.port,
        now,
        identity.port,
    );

    StatusRow {
        id: identity.id.clone(),
        name: identity.name.clone(),
        group: identity.group.clone(),
        instance: identity.instance.clone(),
        address: identity.address.clone(),
        port: identity.port,
        private_ip: identity.private_ip,
        state: classification.state,
        source: classification.source,
        pid: classification.pid,
        unit: model::unit_name(&identity.id).ok(),
        port_open,
        uptime_sec: classification.uptime_sec,
        error: classification.error,
    }
}

// ---------------------------------------------------------------------------
// Classification.
// ---------------------------------------------------------------------------

/// Health-related [`StatusRow`] fields, before identity fields are added.
struct Classification {
    state: HealthState,
    source: Source,
    pid: Option<u32>,
    uptime_sec: Option<u64>,
    error: Option<StatusError>,
}

/// What one Unit-state branch decides. `pid`/`uptime_sec` are derived
/// uniformly afterwards from `source`, so branches do not repeat that logic.
struct Outcome {
    state: HealthState,
    source: Source,
    error: Option<StatusError>,
}

fn classify(
    unit: &UnitObservation,
    port_open: bool,
    port: &PortObservation,
    now: SystemTime,
    port_number: u16,
) -> Classification {
    let outcome = match unit.state {
        UnitState::Idle => classify_inactive(port_open, port, port_number),
        UnitState::Activating => classify_activating(unit, port_open, port, now, port_number),
        UnitState::Active => classify_active(unit, port_open, port, now, port_number),
        UnitState::Deactivating => classify_deactivating(unit, port_open),
        UnitState::Failed(signal) => classify_failed(signal, unit, port_open),
    };

    // `pid`/`uptime_sec` are only reported for a process we attribute to our
    // Unit (`docs/reconcile.v1.md`, "Outputs (Status row)": "when known and
    // relevant").
    let pid = if outcome.source == Source::Unit {
        unit.main_pid
    } else {
        None
    };
    let uptime_sec = if outcome.source == Source::Unit {
        uptime_since(unit.started_at, now)
    } else {
        None
    };

    Classification {
        state: outcome.state,
        source: outcome.source,
        pid,
        uptime_sec,
        error: outcome.error,
    }
}

/// Truth-table rows for `Unit = none / inactive / dead`
/// ([`UnitState::Idle`]).
fn classify_inactive(port_open: bool, port: &PortObservation, port_number: u16) -> Outcome {
    if port_open {
        // No unit at all owns this port; any holder is a conflict.
        return Outcome {
            state: HealthState::Error,
            source: Source::None,
            error: Some(port_in_use_error(port_number, port)),
        };
    }
    Outcome {
        state: HealthState::Stopped,
        source: Source::None,
        error: None,
    }
}

/// Truth-table rows for `Unit = activating`.
fn classify_activating(
    unit: &UnitObservation,
    port_open: bool,
    port: &PortObservation,
    now: SystemTime,
    port_number: u16,
) -> Outcome {
    if port_open {
        if listener_matches_unit(unit.main_pid, port.listener_pid) {
            return Outcome {
                state: HealthState::Running,
                source: Source::Unit,
                error: None,
            };
        }
        // A known holder that is not our MainPID conflicts even mid-start.
        return Outcome {
            state: HealthState::Error,
            source: Source::Unit,
            error: Some(port_in_use_error(port_number, port)),
        };
    }

    if within_start_window(unit.started_at, now) {
        // Still starting; the process may not be attributed yet.
        let source = if unit.main_pid.is_some() {
            Source::Unit
        } else {
            Source::None
        };
        return Outcome {
            state: HealthState::Starting,
            source,
            error: None,
        };
    }

    Outcome {
        state: HealthState::Error,
        source: Source::Unit,
        error: Some(StatusError {
            code: ErrorCode::StartTimeout,
            detail: "unit is still activating after the start window".to_string(),
        }),
    }
}

/// Truth-table rows for `Unit = active`.
fn classify_active(
    unit: &UnitObservation,
    port_open: bool,
    port: &PortObservation,
    now: SystemTime,
    port_number: u16,
) -> Outcome {
    if port_open {
        if listener_matches_unit(unit.main_pid, port.listener_pid) {
            return Outcome {
                state: HealthState::Running,
                source: Source::Unit,
                error: None,
            };
        }
        return Outcome {
            state: HealthState::Error,
            source: Source::Unit,
            error: Some(port_in_use_error(port_number, port)),
        };
    }

    if within_start_window(unit.started_at, now) {
        return Outcome {
            state: HealthState::Starting,
            source: Source::Unit,
            error: None,
        };
    }

    Outcome {
        state: HealthState::Error,
        source: Source::Unit,
        error: Some(StatusError {
            code: ErrorCode::UnitFailed,
            detail: "unit is active but the port is still closed past the start window".to_string(),
        }),
    }
}

/// Truth-table rows for `Unit = deactivating`.
fn classify_deactivating(unit: &UnitObservation, port_open: bool) -> Outcome {
    if port_open {
        // Still accepting clients while stopping.
        return Outcome {
            state: HealthState::Running,
            source: Source::Unit,
            error: None,
        };
    }
    let source = if unit.main_pid.is_some() {
        Source::Unit
    } else {
        Source::None
    };
    Outcome {
        state: HealthState::Stopped,
        source,
        error: None,
    }
}

/// Truth-table rows for `Unit = failed`. `port_open` does not change the
/// outcome here: an explicit failed-unit row overrides the general "port
/// open without healthy ownership" rule (`docs/reconcile.v1.md`, "Authority:
/// truth table vs priority summary" — the table wins).
fn classify_failed(signal: FailureSignal, unit: &UnitObservation, port_open: bool) -> Outcome {
    let kind = classify_failure_signal(signal);

    if kind == FailureKind::CleanStop && !port_open && unit.main_pid.is_none() {
        // Our own `--exit-zero-on-sigterm` stop pattern, port closed, no
        // live process left: a clean stop, not a failure.
        return Outcome {
            state: HealthState::Stopped,
            source: Source::None,
            error: None,
        };
    }

    let source = if unit.main_pid.is_some() {
        Source::Unit
    } else {
        Source::None
    };
    let code = match kind {
        FailureKind::ExecFailed => ErrorCode::ExecFailed,
        // A "clean stop" signal that still has a live process attributed is
        // a contradiction we have not seen in practice; treat it as a crash
        // rather than silently reporting success.
        FailureKind::Crashed | FailureKind::CleanStop => ErrorCode::UnitFailed,
    };
    Outcome {
        state: HealthState::Error,
        source,
        error: Some(StatusError {
            code,
            detail: failure_detail(kind).to_string(),
        }),
    }
}

/// Whether a failed Unit's raw signal matches a clean stop, an exec
/// failure, or a crash (`docs/reconcile.v1.md`, "Clean stop vs failed
/// unit"). `ExecMainCode` (exited vs killed) is not modeled: the doc calls
/// the `Result`/`ExecMainStatus` pairs below "sufficient when in doubt".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    /// Never reached a steady running process.
    ExecFailed,
    /// Crash, unexpected nonzero exit, SIGKILL, stop-timeout, or OOM.
    Crashed,
    /// Branch A, B, or C of "Clean stop vs failed unit".
    CleanStop,
}

fn classify_failure_signal(signal: FailureSignal) -> FailureKind {
    use UnitResult::*;

    match (signal.result, signal.exec_main_status) {
        // Branch A: proxy honored --exit-zero-on-sigterm.
        (Success, _) => FailureKind::CleanStop,
        (ExitCode, 0) => FailureKind::CleanStop,
        // Branch B: killed by the default KillSignal (SIGTERM = 15).
        (Signal, 15) => FailureKind::CleanStop,
        // Branch C: exited 143 (128 + SIGTERM) after SIGTERM.
        (ExitCode, 143) => FailureKind::CleanStop,
        (ExecCondition, _) => FailureKind::ExecFailed,
        // SIGKILL (9), stop timeout, OOM/core-dump, or any other nonzero
        // exit / signal: a real crash, not a clean stop.
        (Signal, _) | (Timeout, _) | (CoreDump, _) | (ExitCode, _) => FailureKind::Crashed,
    }
}

fn failure_detail(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::ExecFailed => "unit failed to exec the proxy binary",
        FailureKind::Crashed => "unit crashed or was killed unexpectedly",
        FailureKind::CleanStop => "unit reported a clean stop but a process is still attributed",
    }
}

// ---------------------------------------------------------------------------
// Small named helpers (docs/reconcile.v1.md subsections).
// ---------------------------------------------------------------------------

/// `docs/reconcile.v1.md`, "PID attribution": an observed port holder is
/// consistent with our Unit owning the port when no other holder is known,
/// or the known holder is our Unit's `MainPID`.
fn listener_matches_unit(main_pid: Option<u32>, listener_pid: Option<u32>) -> bool {
    match listener_pid {
        None => true,
        Some(pid) => Some(pid) == main_pid,
    }
}

/// `docs/reconcile.v1.md`, "`port_open` mapping": only an `Open` probe
/// counts. `Unreachable` (timeout / filtered) is treated the same as
/// `Closed` — rare on loopback, and v1 does not add a third Health path
/// for it.
fn port_is_open(probe: PortProbe) -> bool {
    matches!(probe, PortProbe::Open)
}

/// `docs/reconcile.v1.md`, "Start window": true while the Unit's current
/// start attempt is at most [`START_WINDOW`] old. A **missing** `started_at`
/// gives Reconcile no timestamp to judge freshness from. Treat it as
/// **outside** the window, never "just started", so an undated Connection
/// does not stay `starting` forever (`docs/reconcile.v1.md`, "Start
/// window": "after the window, do not leave the row in eternal
/// starting").
fn within_start_window(started_at: Option<SystemTime>, now: SystemTime) -> bool {
    match started_at {
        None => false,
        Some(t) => age_since(t, now) <= START_WINDOW,
    }
}

fn age_since(started_at: SystemTime, now: SystemTime) -> Duration {
    // A `started_at` after `now` (clock skew) is treated as just started
    // rather than an error.
    now.duration_since(started_at).unwrap_or(Duration::ZERO)
}

fn uptime_since(started_at: Option<SystemTime>, now: SystemTime) -> Option<u64> {
    started_at.map(|t| now.duration_since(t).unwrap_or(Duration::ZERO).as_secs())
}

/// `docs/reconcile.v1.md`, "Holder identity in errors".
fn port_in_use_error(port_number: u16, port: &PortObservation) -> StatusError {
    StatusError {
        code: ErrorCode::PortInUse,
        detail: port_in_use_detail(
            port_number,
            port.listener_pid,
            port.listener_name.as_deref(),
        ),
    }
}

fn port_in_use_detail(
    port_number: u16,
    listener_pid: Option<u32>,
    listener_name: Option<&str>,
) -> String {
    match (listener_pid, listener_name) {
        (Some(pid), Some(name)) => format!("port {port_number} held by {name} (pid {pid})"),
        (Some(pid), None) => format!("port {port_number} held by pid {pid}"),
        (None, _) => format!("port {port_number} held by unknown process"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- fixtures -----------------------------------------------------------

    fn connection(id: &str, port: u16) -> Connection {
        Connection {
            id: id.to_string(),
            name: "Test Connection".to_string(),
            group: "test".to_string(),
            instance: "proj:region:inst".to_string(),
            address: "127.0.0.1".to_string(),
            port,
            private_ip: false,
            auto_iam_authn: false,
            extra_args: Vec::new(),
            enabled: true,
        }
    }

    fn unit_with(
        state: UnitState,
        main_pid: Option<u32>,
        started_at: Option<SystemTime>,
    ) -> UnitObservation {
        UnitObservation {
            state,
            main_pid,
            started_at,
        }
    }

    fn idle_unit() -> UnitObservation {
        unit_with(UnitState::Idle, None, None)
    }

    fn port_with(
        probe: PortProbe,
        listener_pid: Option<u32>,
        listener_name: Option<&str>,
    ) -> PortObservation {
        PortObservation {
            probe,
            listener_pid,
            listener_name: listener_name.map(str::to_string),
        }
    }

    fn closed_port() -> PortObservation {
        port_with(PortProbe::Closed, None, None)
    }

    fn open_port() -> PortObservation {
        port_with(PortProbe::Open, None, None)
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    fn observe(unit: UnitObservation, port: PortObservation) -> Observation {
        Observation { unit, port }
    }

    // -- Row 1: none / inactive / dead, port closed -> stopped / none ------

    #[test]
    fn idle_unit_with_closed_port_is_stopped() {
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(idle_unit(), closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Stopped);
        assert_eq!(row.source, Source::None);
        assert_eq!(row.pid, None);
        assert!(!row.port_open);
        assert_eq!(row.error, None);
    }

    // -- Row 2: none / inactive, port open -> error / none / port_in_use ---

    #[test]
    fn idle_unit_with_open_port_is_a_port_conflict() {
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(idle_unit(), open_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::None);
        assert_eq!(row.error.unwrap().code, ErrorCode::PortInUse);
    }

    #[test]
    fn port_conflict_detail_prefers_holder_name_and_pid() {
        let port = port_with(PortProbe::Open, Some(4321), Some("docker-proxy"));
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(idle_unit(), port),
            now(),
        );
        let detail = row.error.unwrap().detail;
        assert_eq!(detail, "port 15432 held by docker-proxy (pid 4321)");
    }

    #[test]
    fn port_conflict_detail_falls_back_to_pid_only() {
        let port = port_with(PortProbe::Open, Some(4321), None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(idle_unit(), port),
            now(),
        );
        assert_eq!(row.error.unwrap().detail, "port 15432 held by pid 4321");
    }

    #[test]
    fn port_conflict_detail_falls_back_to_unknown_process() {
        let port = port_with(PortProbe::Open, None, None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(idle_unit(), port),
            now(),
        );
        assert_eq!(
            row.error.unwrap().detail,
            "port 15432 held by unknown process"
        );
    }

    // -- port_open mapping: Unreachable behaves like Closed -----------------

    #[test]
    fn unreachable_port_probe_is_treated_as_closed() {
        let port = port_with(PortProbe::Unreachable, None, None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(idle_unit(), port),
            now(),
        );
        assert!(!row.port_open);
        assert_eq!(row.state, HealthState::Stopped);
    }

    // -- Row 3: activating, port closed, within window -> starting ---------

    #[test]
    fn activating_within_start_window_is_starting_without_attributed_process() {
        let unit = unit_with(
            UnitState::Activating,
            None,
            Some(now() - Duration::from_secs(5)),
        );
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Starting);
        assert_eq!(row.source, Source::None);
        assert_eq!(row.pid, None);
    }

    #[test]
    fn activating_within_start_window_with_known_pid_is_starting_and_unit_owned() {
        let unit = unit_with(
            UnitState::Activating,
            Some(111),
            Some(now() - Duration::from_secs(5)),
        );
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Starting);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.pid, Some(111));
    }

    #[test]
    fn activating_exactly_at_the_start_window_boundary_is_still_starting() {
        let unit = unit_with(UnitState::Activating, None, Some(now() - START_WINDOW));
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Starting);
    }

    // -- Row 4: activating, port closed, past window -> start_timeout ------

    #[test]
    fn activating_with_unknown_start_time_and_closed_port_is_a_start_timeout() {
        // A missing `started_at` is not "just started" — Reconcile has no
        // timestamp to judge freshness from, so it must not stay `starting`
        // forever (`docs/reconcile.v1.md`, "Start window": "after the
        // window, do not leave the row in eternal starting").
        let unit = unit_with(UnitState::Activating, None, None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.error.unwrap().code, ErrorCode::StartTimeout);
    }

    #[test]
    fn activating_past_the_start_window_is_a_start_timeout() {
        let unit = unit_with(
            UnitState::Activating,
            None,
            Some(now() - START_WINDOW - Duration::from_millis(1)),
        );
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.error.unwrap().code, ErrorCode::StartTimeout);
    }

    // -- Row 5: activating, port open, MainPID/unknown ok -> running -------

    #[test]
    fn activating_with_open_port_and_unknown_listener_is_running() {
        let unit = unit_with(UnitState::Activating, Some(111), Some(now()));
        let port = port_with(PortProbe::Open, None, None);
        let row = reconcile(&connection("fe-dev", 15432), &observe(unit, port), now());
        assert_eq!(row.state, HealthState::Running);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.pid, Some(111));
    }

    #[test]
    fn activating_with_open_port_and_matching_listener_is_running() {
        let unit = unit_with(UnitState::Activating, Some(111), Some(now()));
        let port = port_with(PortProbe::Open, Some(111), None);
        let row = reconcile(&connection("fe-dev", 15432), &observe(unit, port), now());
        assert_eq!(row.state, HealthState::Running);
    }

    #[test]
    fn activating_with_open_port_and_mismatched_listener_is_a_port_conflict() {
        let unit = unit_with(UnitState::Activating, Some(111), Some(now()));
        let port = port_with(PortProbe::Open, Some(222), None);
        let row = reconcile(&connection("fe-dev", 15432), &observe(unit, port), now());
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.error.unwrap().code, ErrorCode::PortInUse);
    }

    // -- Row 6/7: active, port open ------------------------------------------

    #[test]
    fn active_with_open_port_and_matching_pid_is_running() {
        let unit = unit_with(
            UnitState::Active,
            Some(111),
            Some(now() - Duration::from_secs(42)),
        );
        let port = port_with(PortProbe::Open, Some(111), None);
        let row = reconcile(&connection("fe-dev", 15432), &observe(unit, port), now());
        assert_eq!(row.state, HealthState::Running);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.uptime_sec, Some(42));
    }

    #[test]
    fn active_with_open_port_and_unknown_listener_is_running() {
        let unit = unit_with(UnitState::Active, Some(111), Some(now()));
        let port = port_with(PortProbe::Open, None, None);
        let row = reconcile(&connection("fe-dev", 15432), &observe(unit, port), now());
        assert_eq!(row.state, HealthState::Running);
    }

    #[test]
    fn active_with_open_port_and_mismatched_listener_is_a_port_conflict() {
        let unit = unit_with(UnitState::Active, Some(111), Some(now()));
        let port = port_with(PortProbe::Open, Some(222), Some("docker-proxy"));
        let row = reconcile(&connection("fe-dev", 15432), &observe(unit, port), now());
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::Unit);
        // Source stays Unit even though the port belongs to someone else —
        // unlike the idle-unit case, our Unit is genuinely active.
        assert_eq!(row.pid, Some(111));
        assert_eq!(row.error.unwrap().code, ErrorCode::PortInUse);
    }

    // -- Row 8: active, port closed, within window -> starting -------------

    #[test]
    fn active_within_start_window_and_closed_port_is_starting_and_unit_owned() {
        // Source is fixed `unit` here even without a known pid — unlike the
        // analogous activating row, which is ambiguous.
        let unit = unit_with(
            UnitState::Active,
            None,
            Some(now() - Duration::from_secs(5)),
        );
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Starting);
        assert_eq!(row.source, Source::Unit);
    }

    // -- Row 9: active, port closed, past window -> unit_failed -------------

    #[test]
    fn active_past_start_window_and_closed_port_is_unit_failed() {
        let unit = unit_with(
            UnitState::Active,
            Some(111),
            Some(now() - START_WINDOW - Duration::from_secs(1)),
        );
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.error.unwrap().code, ErrorCode::UnitFailed);
    }

    #[test]
    fn active_with_unknown_start_time_and_closed_port_is_unit_failed() {
        // Same reasoning as the `activating` row above: an unknown start
        // time must not read as "inside the start window".
        let unit = unit_with(UnitState::Active, Some(111), None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.error.unwrap().code, ErrorCode::UnitFailed);
    }

    // -- Row 10/11: deactivating --------------------------------------------

    #[test]
    fn deactivating_with_open_port_is_still_running() {
        let unit = unit_with(UnitState::Deactivating, Some(111), None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, open_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Running);
        assert_eq!(row.source, Source::Unit);
    }

    #[test]
    fn deactivating_with_closed_port_and_known_pid_is_stopped_and_unit_owned() {
        let unit = unit_with(UnitState::Deactivating, Some(111), None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Stopped);
        assert_eq!(row.source, Source::Unit);
    }

    #[test]
    fn deactivating_with_closed_port_and_no_pid_is_stopped_and_unmanaged() {
        let unit = unit_with(UnitState::Deactivating, None, None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Stopped);
        assert_eq!(row.source, Source::None);
    }

    // -- Row 12: failed (crash / exec) --------------------------------------

    #[test]
    fn failed_unit_with_a_crash_signal_is_unit_failed() {
        let signal = FailureSignal {
            result: UnitResult::ExitCode,
            exec_main_status: 1,
        };
        let unit = unit_with(UnitState::Failed(signal), Some(111), None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.error.unwrap().code, ErrorCode::UnitFailed);
    }

    #[test]
    fn failed_unit_with_a_core_dump_signal_is_unit_failed() {
        // OOM kill and other core-dumps surface through systemd as
        // `Result=core-dump` (`docs/reconcile.v1.md`, "failed (crash / exec
        // / OOM)"). End-to-end through `reconcile`, not just the internal
        // classifier, this must still land on `unit_failed`.
        let signal = FailureSignal {
            result: UnitResult::CoreDump,
            exec_main_status: 6,
        };
        let unit = unit_with(UnitState::Failed(signal), Some(111), None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Error);
        assert_eq!(row.source, Source::Unit);
        assert_eq!(row.error.unwrap().code, ErrorCode::UnitFailed);
    }

    #[test]
    fn failed_unit_with_an_exec_condition_signal_is_exec_failed() {
        let signal = FailureSignal {
            result: UnitResult::ExecCondition,
            exec_main_status: 1,
        };
        let unit = unit_with(UnitState::Failed(signal), None, None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.error.unwrap().code, ErrorCode::ExecFailed);
    }

    #[test]
    fn failed_unit_reports_its_own_code_even_when_the_port_is_open() {
        // The table's failed row overrides the general port-conflict rule.
        let signal = FailureSignal {
            result: UnitResult::ExitCode,
            exec_main_status: 1,
        };
        let unit = unit_with(UnitState::Failed(signal), Some(111), None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, open_port()),
            now(),
        );
        assert_eq!(row.error.unwrap().code, ErrorCode::UnitFailed);
    }

    // -- Row 13: failed, clean stop, port closed, no process -> stopped ----

    #[test]
    fn failed_unit_with_a_clean_stop_signal_and_no_process_is_stopped() {
        let signal = FailureSignal {
            result: UnitResult::Success,
            exec_main_status: 0,
        };
        let unit = unit_with(UnitState::Failed(signal), None, None);
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(unit, closed_port()),
            now(),
        );
        assert_eq!(row.state, HealthState::Stopped);
        assert_eq!(row.source, Source::None);
        assert_eq!(row.error, None);
    }

    // -- Clean-stop vs crash signal classification (pure fn) ---------------

    #[test]
    fn clean_stop_branch_a_success_result() {
        let signal = FailureSignal {
            result: UnitResult::Success,
            exec_main_status: 0,
        };
        assert_eq!(classify_failure_signal(signal), FailureKind::CleanStop);
    }

    #[test]
    fn clean_stop_branch_a_exit_code_zero() {
        let signal = FailureSignal {
            result: UnitResult::ExitCode,
            exec_main_status: 0,
        };
        assert_eq!(classify_failure_signal(signal), FailureKind::CleanStop);
    }

    #[test]
    fn clean_stop_branch_b_sigterm_signal() {
        let signal = FailureSignal {
            result: UnitResult::Signal,
            exec_main_status: 15,
        };
        assert_eq!(classify_failure_signal(signal), FailureKind::CleanStop);
    }

    #[test]
    fn clean_stop_branch_c_exit_code_143() {
        let signal = FailureSignal {
            result: UnitResult::ExitCode,
            exec_main_status: 143,
        };
        assert_eq!(classify_failure_signal(signal), FailureKind::CleanStop);
    }

    #[test]
    fn sigkill_signal_is_a_crash_not_a_clean_stop() {
        let signal = FailureSignal {
            result: UnitResult::Signal,
            exec_main_status: 9,
        };
        assert_eq!(classify_failure_signal(signal), FailureKind::Crashed);
    }

    #[test]
    fn stop_timeout_result_is_a_crash() {
        let signal = FailureSignal {
            result: UnitResult::Timeout,
            exec_main_status: 0,
        };
        assert_eq!(classify_failure_signal(signal), FailureKind::Crashed);
    }

    #[test]
    fn core_dump_result_is_a_crash() {
        let signal = FailureSignal {
            result: UnitResult::CoreDump,
            exec_main_status: 6,
        };
        assert_eq!(classify_failure_signal(signal), FailureKind::Crashed);
    }

    #[test]
    fn nonzero_exit_code_other_than_143_is_a_crash() {
        let signal = FailureSignal {
            result: UnitResult::ExitCode,
            exec_main_status: 2,
        };
        assert_eq!(classify_failure_signal(signal), FailureKind::Crashed);
    }

    #[test]
    fn exec_condition_result_is_exec_failed() {
        let signal = FailureSignal {
            result: UnitResult::ExecCondition,
            exec_main_status: 1,
        };
        assert_eq!(classify_failure_signal(signal), FailureKind::ExecFailed);
    }

    // -- unit_name pass-through ----------------------------------------------

    #[test]
    fn status_row_carries_the_expected_unit_name() {
        let row = reconcile(
            &connection("fe-dev", 15432),
            &observe(idle_unit(), closed_port()),
            now(),
        );
        assert_eq!(row.unit.unwrap().as_str(), "cloud-sql-proxy-fe-dev.service");
    }

    // -- enabled is ignored ---------------------------------------------------

    #[test]
    fn disabled_connection_still_reports_its_real_health() {
        let mut connection = connection("fe-dev", 15432);
        connection.enabled = false;
        let unit = unit_with(UnitState::Active, Some(111), Some(now()));
        let port = port_with(PortProbe::Open, Some(111), None);
        let row = reconcile(&connection, &observe(unit, port), now());
        assert_eq!(row.state, HealthState::Running);
    }
}
