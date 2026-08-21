//! Shared vocabulary types for the control plane. Types, not behavior.
//!
//! See `docs/modules.v1.md` ("model — types, not behavior") for the freeze
//! this module implements. Nothing here does I/O or holds a `clap` type.
//!
//! This ticket ([#35](https://github.com/golgor/cloud-sql-tracker/issues/35))
//! lands the frozen shapes ahead of their consumers: `config` (#36),
//! `reconcile` (#37), the adapters (#39–#41), and `commands` (#42+). Until
//! those land, nothing outside this module's own tests references most of
//! these items, so `rustc`/clippy see them as dead code under `-D warnings`.
//! `unit_name` is the exception — it is this ticket's tested proof.
#![allow(dead_code)]

/// The systemd `--user` unit name for one Connection's managed Proxy process.
///
/// Always `cloud-sql-proxy-<sanitized-id>.service`. `supervisor`, `journal`,
/// and the Status `unit` field all go through [`unit_name`] so the string is
/// assembled in exactly one place.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UnitName(String);

impl UnitName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UnitName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

const MAX_UNIT_NAME_LEN: usize = 255;

/// The systemd `.service` unit name for a Connection id.
///
/// `cloud-sql-proxy-<id>.service` after id sanitizing per
/// `docs/research/systemd-user-units.md` ("Unit name pattern and `<id>`
/// restrictions"). A config-valid id (`^[a-zA-Z0-9][a-zA-Z0-9_-]*$`) already
/// satisfies the unit-name charset, so it passes through unchanged.
pub(crate) fn unit_name(id: &str) -> Result<UnitName, UnitNameError> {
    let sanitized = sanitize_unit_id(id)?;
    let name = format!("cloud-sql-proxy-{sanitized}.service");
    if name.len() > MAX_UNIT_NAME_LEN {
        return Err(UnitNameError::TooLong {
            len: name.len(),
            max: MAX_UNIT_NAME_LEN,
        });
    }
    Ok(UnitName(name))
}

/// Steps 1–4 of the control-plane `<id>` rules in
/// `docs/research/systemd-user-units.md`.
fn sanitize_unit_id(id: &str) -> Result<String, UnitNameError> {
    let mapped: String = id
        .chars()
        .map(|c| if is_unit_safe_char(c) { c } else { '-' })
        .collect();
    let collapsed = collapse_repeated_dashes(&mapped);
    let trimmed = collapsed.trim_matches(|c: char| c == '.' || c == '-');

    if trimmed.is_empty() {
        return Err(UnitNameError::Empty);
    }

    Ok(trimmed.to_string())
}

fn is_unit_safe_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | ',' | '-')
}

fn collapse_repeated_dashes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_was_dash = false;
    for c in s.chars() {
        if c == '-' {
            if prev_was_dash {
                continue;
            }
            prev_was_dash = true;
        } else {
            prev_was_dash = false;
        }
        out.push(c);
    }
    out
}

/// Why a Connection id could not become a unit name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnitNameError {
    /// The id sanitizes to nothing (e.g. `"---"`).
    Empty,
    /// `cloud-sql-proxy-<id>.service` would exceed the systemd unit-name
    /// length limit (`len` bytes, `max` allowed).
    TooLong { len: usize, max: usize },
}

impl std::fmt::Display for UnitNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnitNameError::Empty => {
                write!(f, "connection id sanitizes to an empty unit name")
            }
            UnitNameError::TooLong { len, max } => write!(
                f,
                "unit name is {len} bytes, exceeds the systemd limit of {max}"
            ),
        }
    }
}

impl std::error::Error for UnitNameError {}

/// One configured Cloud SQL instance plus its fixed local listen endpoint.
///
/// Fields after config merge (`docs/config.v1.md`, "Connection fields").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Connection {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) instance: String,
    pub(crate) address: String,
    pub(crate) port: u16,
    pub(crate) private_ip: bool,
    pub(crate) auto_iam_authn: bool,
    pub(crate) extra_args: Vec<String>,
    pub(crate) enabled: bool,
}

/// A Connection's health, produced by Reconcile (`docs/reconcile.v1.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HealthState {
    Stopped,
    Starting,
    Running,
    Error,
}

/// Process ownership signal, orthogonal to [`HealthState`].
///
/// Only `unit` | `none` in v1 — there is no `orphan` Source
/// (`CONTEXT.md`, "Source"; `docs/status-document.v1.md`, "source").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// Process is the MainPID of our expected user Unit.
    Unit,
    /// No managed Unit process attributed.
    None,
}

/// Stable machine token for a Status row `error.code`
/// (`docs/status-document.v1.md`, "error object"). New codes are additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorCode {
    BinMissing,
    PortInUse,
    ExecFailed,
    UnitFailed,
    StartTimeout,
    Auth,
    Config,
    Unknown,
}

/// Present on a Status row only when `state == Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusError {
    pub(crate) code: ErrorCode,
    pub(crate) detail: String,
}

/// One `connections[]` element of the Status document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) instance: String,
    pub(crate) address: String,
    pub(crate) port: u16,
    pub(crate) private_ip: bool,
    pub(crate) state: HealthState,
    pub(crate) source: Source,
    pub(crate) pid: Option<u32>,
    pub(crate) unit: Option<UnitName>,
    pub(crate) port_open: bool,
    pub(crate) uptime_sec: Option<u64>,
    pub(crate) error: Option<StatusError>,
}

/// Per-group counters inside the Status document's `groups` map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GroupCounts {
    pub(crate) running: u32,
    pub(crate) starting: u32,
    pub(crate) error: u32,
    pub(crate) stopped: u32,
    pub(crate) total: u32,
}

/// The `status --json` document (`docs/status-document.v1.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusDocument {
    pub(crate) version: u32,
    pub(crate) ts: String,
    pub(crate) cli_version: String,
    pub(crate) running: u32,
    pub(crate) starting: u32,
    pub(crate) error: u32,
    pub(crate) stopped: u32,
    pub(crate) total: u32,
    pub(crate) groups: std::collections::BTreeMap<String, GroupCounts>,
    pub(crate) connections: Vec<StatusRow>,
}

/// One `doctor` check's severity (`docs/doctor.v1.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// One `doctor --json` `checks[]` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckRow {
    pub(crate) id: String,
    pub(crate) status: CheckStatus,
    pub(crate) detail: String,
    pub(crate) hint: Option<String>,
}

/// The `doctor --json` document (`docs/doctor.v1.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DoctorReport {
    pub(crate) version: u32,
    pub(crate) cli_version: String,
    pub(crate) ok: bool,
    pub(crate) checks: Vec<CheckRow>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_name_formats_a_plain_id() {
        let name = unit_name("fe-dev").expect("fe-dev is a valid id");
        assert_eq!(name.as_str(), "cloud-sql-proxy-fe-dev.service");
    }

    #[test]
    fn unit_name_maps_disallowed_characters_to_a_dash() {
        let name = unit_name("fe dev!").expect("sanitizing should not fail");
        assert_eq!(name.as_str(), "cloud-sql-proxy-fe-dev.service");
    }

    #[test]
    fn unit_name_collapses_repeated_dashes_from_sanitizing() {
        let name = unit_name("a!!!b").expect("sanitizing should not fail");
        assert_eq!(name.as_str(), "cloud-sql-proxy-a-b.service");
    }

    #[test]
    fn unit_name_trims_leading_and_trailing_dot_or_dash() {
        let name = unit_name(".-a.b-.").expect("sanitizing should not fail");
        assert_eq!(name.as_str(), "cloud-sql-proxy-a.b.service");
    }

    #[test]
    fn unit_name_rejects_an_id_that_sanitizes_to_nothing() {
        let err = unit_name("---").expect_err("an all-dash id sanitizes to nothing");
        assert_eq!(err, UnitNameError::Empty);
    }

    #[test]
    fn unit_name_rejects_an_id_that_makes_the_unit_name_too_long() {
        // "cloud-sql-proxy-" (16) + id (232) + ".service" (8) = 256 > 255.
        let long_id = "a".repeat(232);
        let err = unit_name(&long_id).expect_err("256 bytes exceeds the systemd limit");
        assert_eq!(err, UnitNameError::TooLong { len: 256, max: 255 });
    }

    #[test]
    fn unit_name_accepts_every_golden_config_id_unchanged() {
        for id in [
            "backend-dev",
            "backend-prod",
            "fe-dev",
            "fe-prod",
            "fe-rw-prod",
            "iot-dev",
            "iot-prod",
        ] {
            let name = unit_name(id).expect("golden ids are already unit-safe");
            assert_eq!(name.as_str(), format!("cloud-sql-proxy-{id}.service"));
        }
    }
}
