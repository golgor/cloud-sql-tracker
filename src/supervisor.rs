//! The systemd `--user` adapter for one Connection's managed Proxy process.
//!
//! See `docs/modules.v1.md` ("supervisor — systemd adapter") for the frozen
//! seam this module implements, and `docs/research/supervisor-io.md` for the
//! normative D-Bus calls. One concrete adapter: **zbus** on the user/session
//! bus. No `systemd-run` / `systemctl` shell-out, no `trait Supervisor`.
//!
//! This module does not build Reconcile's `Observation` — it returns
//! [`UnitSnapshot`], a supervisor-local shape. `commands::status` (#42) maps
//! a `show` snapshot into `reconcile::UnitObservation`.
//! `commands::mutate` (#43) calls `start_transient` and `stop`.
//! `commands::doctor` (#44) calls `systemd_user_check` directly.

use std::collections::HashMap;
use std::path::Path;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::model::{self, CheckRow, CheckStatus, Connection, UnitName};

const SYSTEMD_DESTINATION: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER_INTERFACE: &str = "org.freedesktop.systemd1.Manager";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const UNIT_INTERFACE: &str = "org.freedesktop.systemd1.Unit";
const SERVICE_INTERFACE: &str = "org.freedesktop.systemd1.Service";

/// The D-Bus error systemd returns from `GetUnit`/`StopUnit` for a Unit name
/// it has never loaded — a successful "absent" outcome, not an I/O error
/// (`docs/research/supervisor-io.md`, "Failure classification").
const NO_SUCH_UNIT: &str = "org.freedesktop.systemd1.NoSuchUnit";

/// The generic D-Bus error a session bus returns for a well-known name with
/// no owner and no activatable service — the shape a call to
/// `org.freedesktop.systemd1` takes when the session bus itself is reachable
/// but no systemd user manager is registered on it. Distinct from
/// [`NO_SUCH_UNIT`], which is systemd's own error for a Unit **name** it has
/// never loaded.
const SERVICE_UNKNOWN: &str = "org.freedesktop.DBus.Error.ServiceUnknown";

/// `TimeoutStopUSec` for a started proxy Unit: 30s, matching
/// `docs/research/systemd-user-units.md` (`TimeoutStopSec=30`).
const TIMEOUT_STOP_USEC: u64 = 30_000_000;

const SYSTEMD_USER_HINT: &str = "Needs a systemd --user session: log in graphically, or run \
     `loginctl enable-linger $USER` for a headless/SSH session.";

// ---------------------------------------------------------------------------
// UnitSnapshot: what one `show` call learns about a Unit.
// ---------------------------------------------------------------------------

/// Everything Reconcile's clean-stop-vs-crash table needs about one Unit,
/// from a single `show` call (`docs/modules.v1.md`, "supervisor").
///
/// This module does not classify clean stop vs crash itself — that
/// judgement belongs to `reconcile` (pure), which this module does not
/// import (`docs/modules.v1.md`, "Dependency direction"). `commands` maps
/// a `Loaded` snapshot into `reconcile::UnitObservation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnitSnapshot {
    /// `LoadState=not-found`: systemd has never loaded this Unit.
    Absent,
    Loaded {
        active_state: UnitActiveState,
        /// Raw `SubState` (e.g. `running`, `dead`, `start-pre`). Diagnostic
        /// detail only — Reconcile's truth table does not branch on it.
        sub_state: String,
        /// `MainPID`; `None` when systemd reports `0` (no live process).
        main_pid: Option<u32>,
        result: UnitResult,
        exec_outcome: ExecOutcome,
        /// `ExecMainStartTimestampMonotonic`; `None` when systemd reports
        /// `0` (unknown / no start attempt yet).
        started_at_monotonic_usec: Option<u64>,
    },
}

/// A Unit's `ActiveState`, as reported by systemd today or in the future.
///
/// `Unknown` keeps a future systemd value visible instead of silently
/// mapping it onto a known one (`docs/research/supervisor-io.md`, "unknown
/// future state/result string").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnitActiveState {
    Inactive,
    Activating,
    Active,
    Deactivating,
    Failed,
    Unknown(String),
}

fn map_active_state(raw: String) -> UnitActiveState {
    match raw.as_str() {
        "inactive" => UnitActiveState::Inactive,
        "activating" => UnitActiveState::Activating,
        "active" => UnitActiveState::Active,
        "deactivating" => UnitActiveState::Deactivating,
        "failed" => UnitActiveState::Failed,
        _ => UnitActiveState::Unknown(raw),
    }
}

/// systemd's `Result=` value for a Unit. Only meaningful once
/// `active_state` is [`UnitActiveState::Failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnitResult {
    Success,
    ExitCode,
    Signal,
    Timeout,
    CoreDump,
    ExecCondition,
    Unknown(String),
}

fn map_unit_result(raw: String) -> UnitResult {
    match raw.as_str() {
        "success" => UnitResult::Success,
        "exit-code" => UnitResult::ExitCode,
        "signal" => UnitResult::Signal,
        "timeout" => UnitResult::Timeout,
        "core-dump" => UnitResult::CoreDump,
        "exec-condition" => UnitResult::ExecCondition,
        _ => UnitResult::Unknown(raw),
    }
}

/// SIGCHLD `si_code` values systemd copies into `ExecMainCode` (`wait(2)`),
/// named so a bare `1` never has to be re-decoded by every caller.
mod sigchld {
    pub(super) const CLD_EXITED: i32 = 1;
    pub(super) const CLD_KILLED: i32 = 2;
    pub(super) const CLD_DUMPED: i32 = 3;
}

/// What `ExecMainStatus` means, decided by `ExecMainCode`
/// (`docs/research/supervisor-io.md`, "exit-vs-signal discriminator").
/// Reconcile — not this adapter — decides clean stop vs crash from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecOutcome {
    /// `ExecMainCode == 0`: the Unit has no exec result yet.
    NotExited,
    ExitCode(i32),
    Signal(i32),
    /// An `ExecMainCode` this adapter does not know how to read.
    Unknown {
        code: i32,
        status: i32,
    },
}

fn exec_outcome(exec_main_code: i32, exec_main_status: i32) -> ExecOutcome {
    match exec_main_code {
        0 => ExecOutcome::NotExited,
        sigchld::CLD_EXITED => ExecOutcome::ExitCode(exec_main_status),
        sigchld::CLD_KILLED | sigchld::CLD_DUMPED => ExecOutcome::Signal(exec_main_status),
        code => ExecOutcome::Unknown {
            code,
            status: exec_main_status,
        },
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a supervisor D-Bus operation failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SupervisorError {
    #[error("could not reach the systemd user bus: {0}")]
    Bus(#[source] zbus::Error),
    #[error("systemd D-Bus call failed: {0}")]
    Call(#[source] zbus::Error),
    #[error("systemd did not report the `{property}` property for a Unit")]
    MissingProperty { property: &'static str },
    #[error("systemd reported an unexpected type for the `{property}` property")]
    MalformedProperty { property: &'static str },
    #[error(transparent)]
    UnitName(#[from] model::UnitNameError),
}

impl SupervisorError {
    /// Whether this failure means "cannot operate at all" rather than
    /// "this one Connection's operation failed"
    /// (`docs/cli-contract.v1.md`, "Exit code table": exit `3`,
    /// "Dependency": "no systemd user bus"). `commands::mutate` (#43) uses
    /// this to keep a whole-command environmental failure distinct from a
    /// per-Connection one, instead of every non-`Bus` variant collapsing
    /// into the same generic per-id failure.
    pub(crate) fn is_dependency(&self) -> bool {
        match self {
            SupervisorError::Bus(_) => true,
            SupervisorError::Call(err) => is_service_unknown(err),
            SupervisorError::MissingProperty { .. }
            | SupervisorError::MalformedProperty { .. }
            | SupervisorError::UnitName(_) => false,
        }
    }
}

fn is_no_such_unit(err: &zbus::Error) -> bool {
    matches!(err, zbus::Error::MethodError(name, _, _) if name.as_str() == NO_SUCH_UNIT)
}

fn is_service_unknown(err: &zbus::Error) -> bool {
    matches!(err, zbus::Error::MethodError(name, _, _) if name.as_str() == SERVICE_UNKNOWN)
}

// ---------------------------------------------------------------------------
// Connection helpers
// ---------------------------------------------------------------------------

fn connect() -> Result<zbus::blocking::Connection, SupervisorError> {
    zbus::blocking::Connection::session().map_err(SupervisorError::Bus)
}

fn manager_proxy(
    conn: &zbus::blocking::Connection,
) -> Result<zbus::blocking::Proxy<'_>, SupervisorError> {
    zbus::blocking::Proxy::new(conn, SYSTEMD_DESTINATION, MANAGER_PATH, MANAGER_INTERFACE)
        .map_err(SupervisorError::Bus)
}

fn properties_proxy<'a>(
    conn: &'a zbus::blocking::Connection,
    object_path: &'a OwnedObjectPath,
) -> Result<zbus::blocking::Proxy<'a>, SupervisorError> {
    zbus::blocking::Proxy::new(
        conn,
        SYSTEMD_DESTINATION,
        object_path.as_str(),
        PROPERTIES_INTERFACE,
    )
    .map_err(SupervisorError::Bus)
}

fn get_all_properties(
    properties: &zbus::blocking::Proxy<'_>,
    interface: &str,
) -> Result<HashMap<String, OwnedValue>, SupervisorError> {
    properties
        .call("GetAll", &(interface,))
        .map_err(SupervisorError::Call)
}

fn required<T>(
    props: &HashMap<String, OwnedValue>,
    property: &'static str,
) -> Result<T, SupervisorError>
where
    T: TryFrom<OwnedValue>,
{
    let value = props
        .get(property)
        .cloned()
        .ok_or(SupervisorError::MissingProperty { property })?;
    T::try_from(value).map_err(|_| SupervisorError::MalformedProperty { property })
}

// ---------------------------------------------------------------------------
// The frozen interface
// ---------------------------------------------------------------------------

/// Load/missing plus everything Reconcile needs about one Unit
/// (`docs/modules.v1.md`, "supervisor").
pub(crate) fn show(unit: &UnitName) -> Result<UnitSnapshot, SupervisorError> {
    let conn = connect()?;
    let manager = manager_proxy(&conn)?;

    let object_path: OwnedObjectPath = match manager.call("GetUnit", &(unit.as_str(),)) {
        Ok(path) => path,
        Err(err) if is_no_such_unit(&err) => return Ok(UnitSnapshot::Absent),
        Err(err) => return Err(SupervisorError::Call(err)),
    };

    read_snapshot(&conn, &object_path)
}

fn read_snapshot(
    conn: &zbus::blocking::Connection,
    object_path: &OwnedObjectPath,
) -> Result<UnitSnapshot, SupervisorError> {
    let properties = properties_proxy(conn, object_path)?;
    let unit_props = get_all_properties(&properties, UNIT_INTERFACE)?;
    let service_props = get_all_properties(&properties, SERVICE_INTERFACE)?;

    let active_state = map_active_state(required(&unit_props, "ActiveState")?);
    let sub_state = required(&unit_props, "SubState")?;

    let main_pid = match required::<u32>(&service_props, "MainPID")? {
        0 => None,
        pid => Some(pid),
    };
    let result = map_unit_result(required(&service_props, "Result")?);
    let exec_main_code = required(&service_props, "ExecMainCode")?;
    let exec_main_status = required(&service_props, "ExecMainStatus")?;
    let started_at_monotonic_usec =
        match required::<u64>(&service_props, "ExecMainStartTimestampMonotonic")? {
            0 => None,
            usec => Some(usec),
        };

    Ok(UnitSnapshot::Loaded {
        active_state,
        sub_state,
        main_pid,
        result,
        exec_outcome: exec_outcome(exec_main_code, exec_main_status),
        started_at_monotonic_usec,
    })
}

/// Start `connection`'s Proxy as a transient `Type=exec` Unit
/// (`docs/research/supervisor-io.md`, "v1 D-Bus calls", `start_transient`).
///
/// `env` is the already-resolved environment to forward (e.g. `HOME`,
/// `GOOGLE_APPLICATION_CREDENTIALS`) — this module does not resolve ADC or
/// `PATH` itself; that is `env`'s job (`docs/modules.v1.md`, "env").
///
pub(crate) fn start_transient(
    connection: &Connection,
    proxy_bin: &Path,
    env: &[(String, String)],
) -> Result<(), SupervisorError> {
    let unit = model::unit_name(&connection.id)?;
    let conn = connect()?;
    let manager = manager_proxy(&conn)?;

    // A Unit that ended up `ActiveState=failed` (crash, exec failure, ...)
    // stays loaded until something clears it — `StartTransientUnit` then
    // rejects the same unit name with systemd's `UnitExists`. `stop`
    // already clears this after a successful `StopUnit`
    // (`docs/modules.v1.md`, "supervisor": "reset-failed after stop (and
    // on restart)"), but a plain `start` on a Connection nobody stopped
    // first (e.g. right after the proxy crashed on its own) never goes
    // through `stop`. Best-effort here too: a failed reset must not abort
    // the start attempt — `StartTransientUnit` below is the call that
    // actually needs to succeed.
    let _: Result<(), zbus::Error> = manager.call("ResetFailedUnit", &(unit.as_str(),));

    let exec_start: Vec<(String, Vec<String>, bool)> = vec![(
        proxy_bin.display().to_string(),
        proxy_argv(connection, proxy_bin),
        false,
    )];
    let environment: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();

    let properties: Vec<(&str, Value<'_>)> = vec![
        (
            "Description",
            Value::new(format!("Cloud SQL Auth Proxy ({})", connection.id)),
        ),
        ("Type", Value::new("exec")),
        ("KillMode", Value::new("control-group")),
        ("TimeoutStopUSec", Value::new(TIMEOUT_STOP_USEC)),
        ("Restart", Value::new("no")),
        ("Environment", Value::new(environment)),
        ("ExecStart", Value::new(exec_start)),
    ];
    let aux: Vec<(&str, Vec<(&str, Value<'_>)>)> = Vec::new();

    manager
        .call::<_, _, OwnedObjectPath>(
            "StartTransientUnit",
            &(unit.as_str(), "fail", properties, aux),
        )
        .map_err(SupervisorError::Call)?;
    Ok(())
}

/// Always appended exactly once by [`proxy_argv`], even if `extra_args`
/// already repeats it.
const EXIT_ZERO_ON_SIGTERM: &str = "--exit-zero-on-sigterm";

/// The proxy argv for `ExecStart`, in the order
/// `docs/research/supervisor-io.md` ("v1 D-Bus calls", `ExecStart`)
/// specifies. `argv[0]` is always the absolute binary path (`execve`
/// convention). `--auto-iam-authn` is not in that doc's template but is a
/// frozen config flag (`docs/config.v1.md`, "Connection fields",
/// `auto_iam_authn`) — grouped with the other boolean flags.
fn proxy_argv(connection: &Connection, proxy_bin: &Path) -> Vec<String> {
    let mut argv = vec![
        proxy_bin.display().to_string(),
        format!("--address={}", connection.address),
        format!("--port={}", connection.port),
    ];
    if connection.private_ip {
        argv.push("--private-ip".to_string());
    }
    if connection.auto_iam_authn {
        argv.push("--auto-iam-authn".to_string());
    }
    // A configured `extra_args` may already repeat this flag; keep exactly
    // one copy, immediately before the final INSTANCE argument
    // (`docs/research/supervisor-io.md`).
    argv.extend(
        connection
            .extra_args
            .iter()
            .filter(|arg| arg.as_str() != EXIT_ZERO_ON_SIGTERM)
            .cloned(),
    );
    argv.push(EXIT_ZERO_ON_SIGTERM.to_string());
    argv.push(connection.instance.clone());
    argv
}

/// Stop `unit` (our Unit only — never kill-by-PID) and best-effort clear its
/// failed state (`docs/modules.v1.md`, "supervisor"). A Unit systemd has
/// never loaded is already stopped: idempotent success, not an error
/// (`docs/research/supervisor-io.md`, "stop").
///
pub(crate) fn stop(unit: &UnitName) -> Result<(), SupervisorError> {
    let conn = connect()?;
    let manager = manager_proxy(&conn)?;

    match manager.call::<_, _, OwnedObjectPath>("StopUnit", &(unit.as_str(), "replace")) {
        Ok(_) => {
            // A failed reset-failed must not undo a successful stop.
            let _: Result<(), zbus::Error> = manager.call("ResetFailedUnit", &(unit.as_str(),));
            Ok(())
        }
        Err(err) if is_no_such_unit(&err) => Ok(()),
        Err(err) => Err(SupervisorError::Call(err)),
    }
}

/// Doctor's `systemd_user` row (`docs/doctor.v1.md`, "`systemd_user` —
/// hard"): can this environment reach the systemd user manager at all.
pub(crate) fn systemd_user_check() -> CheckRow {
    match manager_version() {
        Ok(version) => CheckRow {
            id: "systemd_user".to_string(),
            status: CheckStatus::Pass,
            detail: format!("user bus ok (systemd {version})"),
            hint: None,
        },
        Err(err) => CheckRow {
            id: "systemd_user".to_string(),
            status: CheckStatus::Fail,
            detail: err.to_string(),
            hint: Some(SYSTEMD_USER_HINT.to_string()),
        },
    }
}

fn manager_version() -> Result<String, SupervisorError> {
    let conn = connect()?;
    let manager = manager_proxy(&conn)?;
    manager
        .get_property("Version")
        .map_err(SupervisorError::Call)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- SupervisorError::is_dependency --------------------------------------
    //
    // `is_service_unknown`/`is_no_such_unit` themselves need a real
    // `zbus::Message` inside `zbus::Error::MethodError`, which requires a
    // live bus connection to construct — the same reason `is_no_such_unit`
    // has no direct unit test. These exercise `is_dependency` through the
    // variants that do not need one.

    #[test]
    fn is_dependency_is_true_for_a_bus_failure() {
        let err = SupervisorError::Bus(zbus::Error::Address("no bus".to_string()));
        assert!(err.is_dependency());
    }

    #[test]
    fn is_dependency_is_false_for_a_missing_property() {
        let err = SupervisorError::MissingProperty {
            property: "MainPID",
        };
        assert!(!err.is_dependency());
    }

    #[test]
    fn is_dependency_is_false_for_a_malformed_property() {
        let err = SupervisorError::MalformedProperty {
            property: "MainPID",
        };
        assert!(!err.is_dependency());
    }

    #[test]
    fn is_dependency_is_false_for_a_unit_name_error() {
        let err = SupervisorError::UnitName(model::UnitNameError::Empty);
        assert!(!err.is_dependency());
    }

    fn connection(private_ip: bool, auto_iam_authn: bool, extra_args: Vec<&str>) -> Connection {
        Connection {
            id: "fe-dev".to_string(),
            name: "Frontend dev".to_string(),
            group: "fe".to_string(),
            instance: "proj:region:inst".to_string(),
            address: "127.0.0.1".to_string(),
            port: 15432,
            private_ip,
            auto_iam_authn,
            extra_args: extra_args.into_iter().map(str::to_string).collect(),
            enabled: true,
        }
    }

    #[test]
    fn proxy_argv_has_the_minimal_shape_in_order() {
        let bin = Path::new("/usr/bin/cloud-sql-proxy");
        let argv = proxy_argv(&connection(false, false, vec![]), bin);

        assert_eq!(
            argv,
            vec![
                "/usr/bin/cloud-sql-proxy",
                "--address=127.0.0.1",
                "--port=15432",
                "--exit-zero-on-sigterm",
                "proj:region:inst",
            ]
        );
    }

    #[test]
    fn proxy_argv_adds_private_ip_and_auto_iam_authn_before_extra_args() {
        let bin = Path::new("/usr/bin/cloud-sql-proxy");
        let argv = proxy_argv(&connection(true, true, vec!["--debug"]), bin);

        assert_eq!(
            argv,
            vec![
                "/usr/bin/cloud-sql-proxy",
                "--address=127.0.0.1",
                "--port=15432",
                "--private-ip",
                "--auto-iam-authn",
                "--debug",
                "--exit-zero-on-sigterm",
                "proj:region:inst",
            ]
        );
    }

    #[test]
    fn proxy_argv_always_has_exactly_one_exit_zero_on_sigterm() {
        let bin = Path::new("/usr/bin/cloud-sql-proxy");
        for argv in [
            proxy_argv(&connection(false, false, vec![]), bin),
            proxy_argv(&connection(true, true, vec!["--a", "--b"]), bin),
            // A configured `extra_args` that already repeats the flag (once
            // or twice) must not produce a second or third copy.
            proxy_argv(
                &connection(false, false, vec!["--exit-zero-on-sigterm"]),
                bin,
            ),
            proxy_argv(
                &connection(
                    false,
                    false,
                    vec!["--exit-zero-on-sigterm", "--a", "--exit-zero-on-sigterm"],
                ),
                bin,
            ),
        ] {
            let count = argv
                .iter()
                .filter(|arg| arg.as_str() == "--exit-zero-on-sigterm")
                .count();
            assert_eq!(count, 1);
            // ...and it always comes right before the final positional
            // INSTANCE argument (`docs/research/supervisor-io.md`).
            assert_eq!(argv[argv.len() - 2], "--exit-zero-on-sigterm");
        }
    }

    #[test]
    fn map_active_state_recognizes_every_frozen_value() {
        assert_eq!(
            map_active_state("inactive".into()),
            UnitActiveState::Inactive
        );
        assert_eq!(
            map_active_state("activating".into()),
            UnitActiveState::Activating
        );
        assert_eq!(map_active_state("active".into()), UnitActiveState::Active);
        assert_eq!(
            map_active_state("deactivating".into()),
            UnitActiveState::Deactivating
        );
        assert_eq!(map_active_state("failed".into()), UnitActiveState::Failed);
    }

    #[test]
    fn map_active_state_keeps_an_unknown_value_visible() {
        assert_eq!(
            map_active_state("reloading".into()),
            UnitActiveState::Unknown("reloading".to_string())
        );
    }

    #[test]
    fn map_unit_result_recognizes_every_frozen_value() {
        assert_eq!(map_unit_result("success".into()), UnitResult::Success);
        assert_eq!(map_unit_result("exit-code".into()), UnitResult::ExitCode);
        assert_eq!(map_unit_result("signal".into()), UnitResult::Signal);
        assert_eq!(map_unit_result("timeout".into()), UnitResult::Timeout);
        assert_eq!(map_unit_result("core-dump".into()), UnitResult::CoreDump);
        assert_eq!(
            map_unit_result("exec-condition".into()),
            UnitResult::ExecCondition
        );
    }

    #[test]
    fn map_unit_result_keeps_an_unknown_value_visible() {
        // e.g. newer systemd's `oom-kill` (`docs/research/supervisor-io.md`,
        // discarded argv table).
        assert_eq!(
            map_unit_result("oom-kill".into()),
            UnitResult::Unknown("oom-kill".to_string())
        );
    }

    #[test]
    fn exec_outcome_reads_zero_code_as_not_exited() {
        assert_eq!(exec_outcome(0, 0), ExecOutcome::NotExited);
    }

    #[test]
    fn exec_outcome_reads_cld_exited_as_an_exit_code() {
        assert_eq!(
            exec_outcome(sigchld::CLD_EXITED, 0),
            ExecOutcome::ExitCode(0)
        );
        assert_eq!(
            exec_outcome(sigchld::CLD_EXITED, 143),
            ExecOutcome::ExitCode(143)
        );
    }

    #[test]
    fn exec_outcome_reads_cld_killed_and_cld_dumped_as_a_signal() {
        assert_eq!(exec_outcome(sigchld::CLD_KILLED, 9), ExecOutcome::Signal(9));
        assert_eq!(
            exec_outcome(sigchld::CLD_DUMPED, 11),
            ExecOutcome::Signal(11)
        );
    }

    #[test]
    fn exec_outcome_keeps_an_unknown_code_visible() {
        assert_eq!(
            exec_outcome(4, 7),
            ExecOutcome::Unknown { code: 4, status: 7 }
        );
    }
}
