//! `logs(cfg, id, lines) -> Result<Dump, …>` (`docs/modules.v1.md`,
//! "commands": "logs" row) — a facade over [`journal::dump`].
//!
//! `docs/logs.v1.md`: this module only resolves `id` to a [`UnitName`] and
//! returns whatever [`journal::dump`] reports. Hint wording and which
//! stream (stdout vs stderr) it goes on is `cli`'s (#45) job, not this
//! module's — `logs` never prints.

use crate::config::{self, Config};
use crate::journal::{self, Dump, JournalError};
use crate::model::{self, UnitName, UnitNameError};

/// Why `logs` could not produce a [`Dump`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum LogsCommandError {
    /// `docs/logs.v1.md`, "CLI": "Unknown `ID` → exit `2`".
    #[error("unknown connection id `{0}`")]
    UnknownId(String),
    #[error("connection `{id}`: {source}")]
    UnitName {
        id: String,
        #[source]
        source: UnitNameError,
    },
    #[error(transparent)]
    Journal(#[from] JournalError),
}

/// Dump `id`'s Unit's user-journal lines, most recent `lines` at most
/// (`docs/logs.v1.md`). `config` is already loaded — `logs`'s fail-fast
/// config load (unlike `doctor`) is `cli`'s (#45) job.
///
/// Only reachable from `cli` (#45) so far — this ticket
/// ([#44](https://github.com/golgor/cloud-sql-tracker/issues/44)) proves
/// [`resolve_unit`]'s id-to-unit resolution through this module's own unit
/// tests; a successful [`journal::dump`] call needs a real `journalctl`
/// binary, which `docs/verification.v1.md` does not require as a unit test
/// (the same allowance already used for `journal`'s own adapter tests).
#[allow(dead_code)]
pub(crate) fn logs(config: &Config, id: &str, lines: u32) -> Result<Dump, LogsCommandError> {
    let unit = resolve_unit(config, id)?;
    Ok(journal::dump(&unit, lines)?)
}

/// Pure: resolves a Connection id to its Unit name, or the one usage error
/// `docs/logs.v1.md` defines for `logs` (`UnknownId`) plus the unit-name
/// error every id-taking command shares.
fn resolve_unit(config: &Config, id: &str) -> Result<UnitName, LogsCommandError> {
    let connection =
        config::by_id(config, id).ok_or_else(|| LogsCommandError::UnknownId(id.to_string()))?;
    model::unit_name(&connection.id).map_err(|source| LogsCommandError::UnitName {
        id: connection.id.clone(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Connection;

    fn connection(id: &str) -> Connection {
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
            enabled: true,
        }
    }

    fn config(connections: Vec<Connection>) -> Config {
        Config {
            proxy_bin: "cloud-sql-proxy".to_string(),
            connections,
        }
    }

    #[test]
    fn resolve_unit_returns_the_matching_connections_unit_name() {
        let config = config(vec![connection("fe-dev")]);

        let unit = resolve_unit(&config, "fe-dev").expect("fe-dev is in the config");

        assert_eq!(unit.as_str(), "cloud-sql-proxy-fe-dev.service");
    }

    #[test]
    fn resolve_unit_errors_for_an_unknown_id() {
        let config = config(vec![connection("fe-dev")]);

        let err = resolve_unit(&config, "nope").expect_err("nope is not in the config");

        assert_eq!(err.to_string(), "unknown connection id `nope`");
        assert!(matches!(err, LogsCommandError::UnknownId(id) if id == "nope"));
    }

    #[test]
    fn resolve_unit_finds_a_connection_even_when_others_do_not_match() {
        let config = config(vec![connection("backend-dev"), connection("fe-dev")]);

        let unit = resolve_unit(&config, "fe-dev").expect("fe-dev is in the config");

        assert_eq!(unit.as_str(), "cloud-sql-proxy-fe-dev.service");
    }
}
