//! `doctor(cfg_path) -> DoctorReport` (`docs/modules.v1.md`, "commands":
//! "doctor" row) — orchestrates the six checks in `docs/doctor.v1.md`.
//!
//! Five checks are a single adapter's own `*_check` row
//! (`docs/modules.v1.md`, "Doctor check owners"): `config` (`config::load`),
//! `proxy_bin` / `adc` (`env`), `systemd_user` (`supervisor`), `journal_user`
//! (`journal`). `ports` is the one row this module stitches itself from two
//! adapters (`port::observe` + `supervisor::show`), because it is the only
//! check that needs both.
//!
//! Doctor never fails to produce a report: `docs/doctor.v1.md`, "Config
//! load path (important)" — "Doctor does not fail-fast before the
//! checklist." An unloadable config becomes the `config` check's own
//! `fail`; every other check still runs.

use std::net::IpAddr;
use std::path::Path;

use crate::config::{self, Config, ConfigError};
use crate::model::{
    self, CheckRow, CheckStatus, Connection, DoctorReport, PortObservation, PortProbe,
};
use crate::supervisor::{self, UnitSnapshot};
use crate::{env, journal, port};

/// Run every Doctor check for the config at `cfg_path`
/// (`docs/doctor.v1.md`, "Recommended check order"). `cli` (#45) resolves
/// `cfg_path` (`--config` or the default XDG path) before calling this.
///
/// Only reachable from `cli` (#45) so far — this ticket
/// ([#44](https://github.com/golgor/cloud-sql-tracker/issues/44)) proves
/// the per-check composition through this module's own unit tests instead,
/// the same spirit `commands::status` (#42) already established: `config`
/// and `ports` are exercised directly; `proxy_bin` / `systemd_user` / `adc`
/// / `journal_user` are each a single adapter's own `*_check`, already
/// tested in that adapter's module (`docs/verification.v1.md`: adapters
/// are not required as unit tests here too).
#[allow(dead_code)]
pub(crate) fn doctor(cfg_path: &Path) -> DoctorReport {
    let loaded = config::load(cfg_path);
    let config = loaded.as_ref().ok();

    let checks = vec![
        config_check(cfg_path, &loaded),
        env::proxy_bin_check(config.map(|config| config.proxy_bin.as_str())),
        supervisor::systemd_user_check(),
        env::adc_check(),
        journal::journal_user_check(),
        ports_check(config),
    ];
    let ok = !checks.iter().any(|check| check.status == CheckStatus::Fail);

    DoctorReport {
        version: 1,
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        ok,
        checks,
    }
}

/// The `config` check (`docs/doctor.v1.md`, "`config` — hard"): the same
/// [`config::load`] every other command fail-fasts on, but here a failure
/// is this one row's `fail`, never a process exit.
fn config_check(path: &Path, loaded: &Result<Config, ConfigError>) -> CheckRow {
    match loaded {
        Ok(config) => CheckRow {
            id: "config".to_string(),
            status: CheckStatus::Pass,
            detail: format!(
                "{} ({} connections)",
                path.display(),
                config.connections.len()
            ),
            hint: None,
        },
        Err(err) => CheckRow {
            id: "config".to_string(),
            status: CheckStatus::Fail,
            detail: format!("{}: {err}", path.display()),
            hint: Some(
                "See docs/config.v1.md and examples/connections.json for a valid config."
                    .to_string(),
            ),
        },
    }
}

/// The `ports` check (`docs/doctor.v1.md`, "`ports` — warn only"): the one
/// row `commands::doctor` stitches itself from two adapters
/// (`docs/modules.v1.md`, "Doctor check owners"). Never `fail` in v1 — only
/// `pass` or `warn`.
fn ports_check(config: Option<&Config>) -> CheckRow {
    let Some(config) = config else {
        // "Requires a successfully loaded config. Otherwise skipped ...
        // preferred: `ports` = `pass`, detail `skipped: config not
        // loaded`" (`docs/doctor.v1.md`, "`ports` — warn only").
        return CheckRow {
            id: "ports".to_string(),
            status: CheckStatus::Pass,
            detail: "skipped: config not loaded".to_string(),
            hint: None,
        };
    };

    let conflicts: Vec<String> = config
        .connections
        .iter()
        .filter_map(port_conflict)
        .collect();

    if conflicts.is_empty() {
        CheckRow {
            id: "ports".to_string(),
            status: CheckStatus::Pass,
            detail: "no port conflicts".to_string(),
            hint: None,
        }
    } else {
        CheckRow {
            id: "ports".to_string(),
            status: CheckStatus::Warn,
            detail: conflicts.join("; "),
            hint: Some(
                "Free the port (stop the leftover process) before start, or change the \
                 Connection port in config."
                    .to_string(),
            ),
        }
    }
}

/// One Connection's port scan (`docs/doctor.v1.md`, "`ports`" table).
/// `None` when nothing conflicts; `Some(detail)` when something else holds
/// the port.
///
/// A Connection whose `address` does not parse as an IP cannot be probed;
/// it is skipped here rather than invented as a conflict —
/// `commands::status` (#42) already reports that Connection's own row as
/// `error`/`config`.
fn port_conflict(connection: &Connection) -> Option<String> {
    let address: IpAddr = connection.address.parse().ok()?;
    let observation = port::observe(address, connection.port);
    if observation.probe != PortProbe::Open {
        // Skip the D-Bus round trip entirely when nothing is listening —
        // `classify_port` below would reach the same "OK" verdict anyway.
        return None;
    }
    classify_port(connection, &observation, our_unit_main_pid(connection))
}

/// Our Unit's `MainPID` for `connection`, or `None` when the Unit is not
/// loaded, has no live process, or could not be reached (bus down, D-Bus
/// error). Every one of those readings means "cannot confirm this is
/// ours" — the same conservative default [`classify_port`] needs
/// (`docs/modules.v1.md`, "ports": "never a `bool` \"in use?\" that
/// ignores the Unit").
fn our_unit_main_pid(connection: &Connection) -> Option<u32> {
    let unit = model::unit_name(&connection.id).ok()?;
    match supervisor::show(&unit).ok()? {
        UnitSnapshot::Loaded { main_pid, .. } => main_pid,
        UnitSnapshot::Absent => None,
    }
}

/// Pure port-conflict verdict for one Connection, given its port
/// observation and our own Unit's `MainPID` if known
/// (`docs/doctor.v1.md`, "`ports`" table: "Nothing listening" / "Listener
/// is our Unit MainPID" / "Anything else holds the port"). This ticket's
/// own tests exercise every row of that table directly; [`port_conflict`]
/// (impure) is the only caller.
fn classify_port(
    connection: &Connection,
    observation: &PortObservation,
    our_main_pid: Option<u32>,
) -> Option<String> {
    if observation.probe != PortProbe::Open {
        return None; // "Nothing listening" -> OK.
    }
    if observation.listener_pid.is_some() && observation.listener_pid == our_main_pid {
        return None; // "Listener is our Unit MainPID" -> OK.
    }
    Some(port_conflict_detail(connection, observation)) // "Anything else" -> Conflict.
}

fn port_conflict_detail(connection: &Connection, observation: &PortObservation) -> String {
    match (
        observation.listener_pid,
        observation.listener_name.as_deref(),
    ) {
        (Some(pid), Some(name)) => format!(
            "{} port {} held by {name} (pid {pid}); not our unit",
            connection.id, connection.port
        ),
        (Some(pid), None) => format!(
            "{} port {} held by pid {pid}; not our unit",
            connection.id, connection.port
        ),
        (None, _) => format!(
            "{} port {} held by an unrecognized process; not our unit",
            connection.id, connection.port
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};

    fn connection(id: &str, port: u16) -> Connection {
        Connection {
            id: id.to_string(),
            name: id.to_string(),
            group: "fe".to_string(),
            instance: "proj:region:inst".to_string(),
            address: "127.0.0.1".to_string(),
            port,
            private_ip: false,
            auto_iam_authn: false,
            extra_args: Vec::new(),
            enabled: true,
        }
    }

    fn config(connections: Vec<Connection>) -> Config {
        Config {
            proxy_bin: "cloud-sql-proxy".to_string(),
            connections,
        }
    }

    fn port_observation(probe: PortProbe, listener_pid: Option<u32>) -> PortObservation {
        PortObservation {
            probe,
            listener_pid,
            listener_name: None,
        }
    }

    // -- config_check ---------------------------------------------------------

    #[test]
    fn config_check_passes_with_the_path_and_connection_count() {
        let loaded = Ok(config(vec![connection("a", 15432), connection("b", 15433)]));
        let row = config_check(Path::new("/tmp/connections.json"), &loaded);

        assert_eq!(row.id, "config");
        assert_eq!(row.status, CheckStatus::Pass);
        assert!(row.detail.contains("/tmp/connections.json"));
        assert!(row.detail.contains('2'));
        assert_eq!(row.hint, None);
    }

    #[test]
    fn config_check_fails_with_a_hint_when_config_is_invalid() {
        let loaded: Result<Config, ConfigError> = Err(ConfigError::EmptyProxyBin);
        let row = config_check(Path::new("/tmp/connections.json"), &loaded);

        assert_eq!(row.id, "config");
        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains("proxy_bin"));
        assert!(row.hint.is_some());
    }

    // -- ports_check: config not loaded / no connections -----------------------
    // (real `port::observe`/`supervisor::show` calls stay out of these two
    // cases entirely, so they are safe without a live systemd session)

    #[test]
    fn ports_check_is_pass_with_a_skipped_detail_when_config_is_not_loaded() {
        let row = ports_check(None);

        assert_eq!(row.id, "ports");
        assert_eq!(row.status, CheckStatus::Pass);
        assert!(row.detail.contains("skipped"));
        assert_eq!(row.hint, None);
    }

    #[test]
    fn ports_check_is_pass_with_no_conflicts_for_an_empty_connection_list() {
        let row = ports_check(Some(&config(Vec::new())));

        assert_eq!(row.status, CheckStatus::Pass);
        assert_eq!(row.detail, "no port conflicts");
    }

    #[test]
    fn ports_check_is_pass_when_the_only_configured_port_is_closed() {
        // Bind to learn a free ephemeral port, then drop the listener
        // immediately so `port::observe` reports it Closed — this never
        // reaches `our_unit_main_pid`'s D-Bus call (`port_conflict` only
        // calls it once the probe is Open), so it stays safe without a
        // live systemd session (same pattern as `port.rs`'s own tests).
        let closed_port = {
            let listener =
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind an ephemeral port");
            listener.local_addr().unwrap().port()
        };

        let row = ports_check(Some(&config(vec![connection("fe-dev", closed_port)])));

        assert_eq!(row.status, CheckStatus::Pass);
        assert_eq!(row.detail, "no port conflicts");
    }

    // -- classify_port: pure, every row of the docs/doctor.v1.md table ---------

    #[test]
    fn classify_port_is_ok_when_probe_is_closed() {
        let observation = port_observation(PortProbe::Closed, Some(111));
        let verdict = classify_port(&connection("fe-dev", 15432), &observation, None);
        assert_eq!(verdict, None);
    }

    #[test]
    fn classify_port_is_ok_when_probe_is_unreachable() {
        let observation = port_observation(PortProbe::Unreachable, None);
        let verdict = classify_port(&connection("fe-dev", 15432), &observation, None);
        assert_eq!(verdict, None);
    }

    #[test]
    fn classify_port_is_ok_when_the_listener_is_our_units_main_pid() {
        let observation = port_observation(PortProbe::Open, Some(111));
        let verdict = classify_port(&connection("fe-dev", 15432), &observation, Some(111));
        assert_eq!(verdict, None);
    }

    #[test]
    fn classify_port_is_a_conflict_when_the_listener_is_a_different_pid() {
        let observation = port_observation(PortProbe::Open, Some(111));
        let verdict = classify_port(&connection("fe-dev", 15432), &observation, Some(222));
        assert!(verdict.unwrap().contains("111"));
    }

    #[test]
    fn classify_port_is_a_conflict_when_our_unit_has_no_live_process() {
        let observation = port_observation(PortProbe::Open, Some(111));
        let verdict = classify_port(&connection("fe-dev", 15432), &observation, None);
        assert!(verdict.is_some());
    }

    #[test]
    fn classify_port_is_a_conflict_when_the_listener_pid_is_unknown() {
        // An open port whose holder we cannot attribute is never assumed
        // to be ours (`docs/modules.v1.md`, "ports": "never a `bool`
        // \"in use?\" that ignores the Unit").
        let observation = port_observation(PortProbe::Open, None);
        let verdict = classify_port(&connection("fe-dev", 15432), &observation, Some(111));
        assert!(verdict.unwrap().contains("unrecognized"));
    }

    #[test]
    fn classify_port_detail_includes_the_holder_name_when_known() {
        let observation = PortObservation {
            probe: PortProbe::Open,
            listener_pid: Some(4321),
            listener_name: Some("docker-proxy".to_string()),
        };
        let verdict = classify_port(&connection("fe-dev", 15432), &observation, None).unwrap();

        assert!(verdict.contains("fe-dev"));
        assert!(verdict.contains("15432"));
        assert!(verdict.contains("docker-proxy"));
        assert!(verdict.contains("4321"));
    }

    // -- DoctorReport vs schemas/doctor.v1.json (shape proof only; does not
    // close #23 Layer 2 — `docs/verification.v1.md`, "Status / Doctor JSON") -

    #[test]
    fn doctor_report_serializes_to_the_doctor_v1_schema_shape() {
        let report = DoctorReport {
            version: 1,
            cli_version: "0.1.0".to_string(),
            ok: false,
            checks: vec![
                CheckRow {
                    id: "config".to_string(),
                    status: CheckStatus::Pass,
                    detail: "/home/you/connections.json (2 connections)".to_string(),
                    hint: None,
                },
                CheckRow {
                    id: "adc".to_string(),
                    status: CheckStatus::Fail,
                    detail: "no Application Default Credentials file".to_string(),
                    hint: Some("gcloud auth application-default login".to_string()),
                },
                CheckRow {
                    id: "ports".to_string(),
                    status: CheckStatus::Warn,
                    detail: "fe-dev port 15434 held by pid 12002; not our unit".to_string(),
                    hint: Some("Free the port before start.".to_string()),
                },
            ],
        };
        let instance = serde_json::to_value(&report).expect("DoctorReport serializes");

        let schema_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/doctor.v1.json");
        let schema_text = std::fs::read_to_string(schema_path).expect("read the doctor schema");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_text).expect("parse the doctor schema");
        let validator = jsonschema::validator_for(&schema).expect("compile the doctor schema");

        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|e| format!("{}: {e}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "doctor report rejected by schemas/doctor.v1.json:\n{}",
            errors.join("\n")
        );
    }
}
