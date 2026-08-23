//! Pure target-selector expansion and the `--failed` error-state filter
//! (`docs/modules.v1.md`, "commands": "Selector: wholly in `commands`
//! (`select.rs`). `config` only `by_id`.").
//!
//! Both functions here are pure: no I/O, no clap types
//! (`docs/cli-contract.v1.md` owns the `ID | --group NAME | --all` argv
//! shape; `cli` (#45) maps parsed argv into [`Selector`]).

use crate::config::{self, Config};
use crate::model::{Connection, HealthState, StatusRow};

/// One resolved target set request, independent of how `cli` parsed it.
///
/// Exactly one of id / group / all (`docs/cli-contract.v1.md`, "Target
/// selectors": "Mutual exclusion"); enforcing that exclusivity is `cli`'s
/// job when it builds this value, not this module's.
///
/// `cli`'s `resolve_selector` (#45) constructs these from argv; this
/// ticket's own tests construct every variant directly against
/// [`expand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selector {
    /// Every connection in the config document, in config order.
    All,
    /// A single connection id.
    Id(String),
    /// Every connection in one group, in config order.
    Group(String),
}

/// Why a [`Selector`] could not be expanded against a [`Config`].
///
/// Both variants are the usage-error class (`docs/cli-contract.v1.md`,
/// exit `2`: "unknown id/group"); `cli` owns the actual exit code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SelectError {
    #[error("unknown connection id `{0}`")]
    UnknownId(String),
    #[error("unknown group `{0}`")]
    UnknownGroup(String),
}

/// Resolve a [`Selector`] to the Connections it names, in config order.
///
/// **Includes disabled connections** — `status` reports on every
/// configured connection regardless of `enabled` (`docs/config.v1.md`,
/// "disabled": "Included in `--group` / `--all` selectors: Yes for
/// status"). Mutating commands (#43) apply their own disabled-skip on top
/// of this same expansion; this function does not know about that policy.
pub(crate) fn expand<'a>(
    config: &'a Config,
    selector: &Selector,
) -> Result<Vec<&'a Connection>, SelectError> {
    match selector {
        Selector::All => Ok(config.connections.iter().collect()),
        Selector::Id(id) => config::by_id(config, id)
            .map(|connection| vec![connection])
            .ok_or_else(|| SelectError::UnknownId(id.clone())),
        Selector::Group(group) => {
            let matches: Vec<&Connection> = config
                .connections
                .iter()
                .filter(|connection| &connection.group == group)
                .collect();
            if matches.is_empty() {
                Err(SelectError::UnknownGroup(group.clone()))
            } else {
                Ok(matches)
            }
        }
    }
}

/// `restart --failed`'s error-state filter (`docs/cli-contract.v1.md`,
/// "`restart`": "Restrict the selected set to connections currently in
/// Health state `error` only"). This is a filter over **already-reconciled**
/// rows, not a fourth [`Selector`] variant — empty after filtering is
/// success, not an error (`docs/modules.v1.md`, "commands").
///
/// Only reachable from `restart --failed` (#43) so far.
#[allow(dead_code)]
pub(crate) fn filter_failed(rows: &[StatusRow]) -> Vec<&StatusRow> {
    rows.iter()
        .filter(|row| row.state == HealthState::Error)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(id: &str, group: &str, enabled: bool) -> Connection {
        Connection {
            id: id.to_string(),
            name: id.to_string(),
            group: group.to_string(),
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

    fn three_connection_config() -> Config {
        config(vec![
            connection("a", "fe", true),
            connection("b", "backend", true),
            connection("c", "fe", true),
        ])
    }

    // -- expand: All ---------------------------------------------------------

    #[test]
    fn expand_all_returns_every_connection_in_config_order() {
        let config = three_connection_config();
        let expanded = expand(&config, &Selector::All).expect("All always resolves");
        let ids: Vec<&str> = expanded.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn expand_all_includes_disabled_connections() {
        // `status` reports on every configured connection, not just the
        // enabled ones (`docs/config.v1.md`, "disabled": "Included in
        // `--group` / `--all` selectors: Yes for status").
        let config = config(vec![
            connection("a", "fe", true),
            connection("b", "fe", false),
        ]);
        let expanded = expand(&config, &Selector::All).expect("All always resolves");
        let ids: Vec<&str> = expanded.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    // -- expand: Id ------------------------------------------------------------

    #[test]
    fn expand_id_returns_the_matching_connection() {
        let config = three_connection_config();
        let expanded = expand(&config, &Selector::Id("b".to_string())).expect("b is in the config");
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].id, "b");
    }

    #[test]
    fn expand_id_errors_for_an_unknown_id() {
        let config = three_connection_config();
        let err = expand(&config, &Selector::Id("nope".to_string()))
            .expect_err("nope is not in the config");
        assert_eq!(err, SelectError::UnknownId("nope".to_string()));
    }

    #[test]
    fn expand_id_includes_a_disabled_connection() {
        // Selector expansion never applies the disabled policy; `cli` (#45)
        // decides that a single-id `start` on disabled exits 2
        // (`docs/config.v1.md`, "v1 rule (normative)").
        let config = config(vec![connection("a", "fe", false)]);
        let expanded = expand(&config, &Selector::Id("a".to_string())).expect("a is in the config");
        assert_eq!(expanded.len(), 1);
    }

    // -- expand: Group ---------------------------------------------------------

    #[test]
    fn expand_group_returns_every_connection_in_that_group_in_config_order() {
        let config = three_connection_config();
        let expanded =
            expand(&config, &Selector::Group("fe".to_string())).expect("fe has connections");
        let ids: Vec<&str> = expanded.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"]);
    }

    #[test]
    fn expand_group_errors_for_an_unknown_group() {
        let config = three_connection_config();
        let err = expand(&config, &Selector::Group("nope".to_string()))
            .expect_err("nope is not a configured group");
        assert_eq!(err, SelectError::UnknownGroup("nope".to_string()));
    }

    // -- filter_failed -----------------------------------------------------

    fn row(id: &str, state: HealthState) -> StatusRow {
        StatusRow {
            id: id.to_string(),
            name: id.to_string(),
            group: "fe".to_string(),
            instance: "proj:region:inst".to_string(),
            address: "127.0.0.1".to_string(),
            port: 15432,
            private_ip: false,
            enabled: true,
            state,
            source: crate::model::Source::None,
            pid: None,
            unit: None,
            port_open: false,
            uptime_sec: None,
            error: None,
        }
    }

    #[test]
    fn filter_failed_keeps_only_error_rows() {
        let rows = vec![
            row("a", HealthState::Running),
            row("b", HealthState::Error),
            row("c", HealthState::Stopped),
            row("d", HealthState::Error),
        ];
        let failed = filter_failed(&rows);
        let ids: Vec<&str> = failed.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "d"]);
    }

    #[test]
    fn filter_failed_is_empty_when_nothing_is_in_error() {
        let rows = vec![
            row("a", HealthState::Running),
            row("b", HealthState::Stopped),
        ];
        assert!(filter_failed(&rows).is_empty());
    }
}
