//! Config file loading and validation (`docs/config.v1.md`).
//!
//! `parse` is the pure seam this module is tested at
//! (`docs/verification.v1.md`, "config::parse"). `load` adds the one bit of
//! I/O (`std::fs::read`); `default_path` and `by_id` are small lookups.
//! See `docs/modules.v1.md` ("config — deep validation") for the freeze.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use crate::model::Connection;

const DEFAULT_PROXY_BIN: &str = "cloud-sql-proxy";
const DEFAULT_ADDRESS: &str = "127.0.0.1";
const RESERVED_PORTS: [u16; 3] = [1433, 3306, 5432];
const MIN_UNPRIVILEGED_PORT: u16 = 1024;
const MAX_ID_LEN: usize = 64;
/// Hard cap on Connection count (`docs/config.v1.md`, "Top-level object",
/// the `connections` row). Counts every row, including `enabled: false`.
const MAX_CONNECTIONS: usize = 32;

/// Runtime Connection inventory after load, merge, and validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) proxy_bin: String,
    pub(crate) connections: Vec<Connection>,
}

/// Why a config file failed to load or validate.
#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("config version {found} is not supported (expected 1)")]
    UnsupportedVersion { found: i64 },
    #[error("proxy_bin must not be empty")]
    EmptyProxyBin,
    #[error("connection `{id}`: {reason}")]
    InvalidConnection { id: String, reason: String },
    #[error("duplicate connection id `{0}`")]
    DuplicateId(String),
    #[error("duplicate port {0}")]
    DuplicatePort(u16),
    #[error("duplicate instance `{0}`")]
    DuplicateInstance(String),
    #[error("config holds {count} connections, the maximum is {max}")]
    TooManyConnections { count: usize, max: usize },
}

/// Read `path` then [`parse`] its bytes.
///
/// `commands::doctor` (#44) calls this directly (its own `config` check
/// never fail-fasts on the result); other commands' fail-fast `load` call
/// arrives with `cli` (#45).
pub(crate) fn load(path: &Path) -> Result<Config, ConfigError> {
    let bytes = std::fs::read(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&bytes)
}

/// Parse, merge, and validate a `connections.json` document. Pure.
pub(crate) fn parse(bytes: &[u8]) -> Result<Config, ConfigError> {
    let raw: RawConfig = serde_json::from_slice(bytes)?;
    if raw.version != 1 {
        return Err(ConfigError::UnsupportedVersion { found: raw.version });
    }

    let defaults = raw.defaults.unwrap_or_default();
    let connections = raw
        .connections
        .into_iter()
        .map(|connection| merge_connection(&defaults, connection))
        .collect::<Result<Vec<_>, _>>()?;
    check_connection_count(&connections)?;
    check_uniqueness(&connections)?;

    Ok(Config {
        proxy_bin: validate_proxy_bin(raw.proxy_bin)?,
        connections,
    })
}

/// `schemas/config.v1.json` `proxy_bin`: `minLength: 1`. Absent falls back
/// to [`DEFAULT_PROXY_BIN`]; present-but-empty is a validation error, not a
/// silent default.
fn validate_proxy_bin(proxy_bin: Option<String>) -> Result<String, ConfigError> {
    match proxy_bin {
        None => Ok(DEFAULT_PROXY_BIN.to_string()),
        Some(bin) if bin.is_empty() => Err(ConfigError::EmptyProxyBin),
        Some(bin) => Ok(bin),
    }
}

/// `$XDG_CONFIG_HOME/cloud-sql-tracker/connections.json`, else
/// `~/.config/cloud-sql-tracker/connections.json`.
///
/// Used by `commands`/`cli` (#42+); exercised directly by this module's
/// tests until then.
#[allow(dead_code)]
pub(crate) fn default_path() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    config_home
        .join("cloud-sql-tracker")
        .join("connections.json")
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// Look up a Connection by id. No selector expansion — that stays in
/// `commands` (`docs/modules.v1.md`, "config — deep validation").
pub(crate) fn by_id<'a>(config: &'a Config, id: &str) -> Option<&'a Connection> {
    config
        .connections
        .iter()
        .find(|connection| connection.id == id)
}

/// Raw shapes mirroring `schemas/config.v1.json`. Unknown keys are
/// rejected at every level (`docs/config.v1.md`, "Strict keys").
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: i64,
    proxy_bin: Option<String>,
    defaults: Option<RawDefaults>,
    connections: Vec<RawConnection>,
}

/// Optional fields only. `schemas/config.v1.json` `connectionObject`
/// requires `id`/`name`/`group`/`instance`/`port` on every connection, so
/// those identity fields cannot come from here — only from [`RawConnection`]
/// (`docs/modules.v1.md`, "config — deep validation").
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    address: Option<String>,
    private_ip: Option<bool>,
    auto_iam_authn: Option<bool>,
    extra_args: Option<Vec<String>>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnection {
    id: String,
    name: String,
    group: String,
    instance: String,
    port: u16,
    address: Option<String>,
    private_ip: Option<bool>,
    auto_iam_authn: Option<bool>,
    extra_args: Option<Vec<String>>,
    enabled: Option<bool>,
}

/// Built-in defaults (`docs/config.v1.md`, "Built-in defaults") layered
/// under file `defaults`, then the connection object (`docs/config.v1.md`,
/// "Merge order"). Identity fields (`name`/`group`/`instance`/`port`) are
/// already required directly on `raw`; only the optional fields merge.
fn merge_connection(defaults: &RawDefaults, raw: RawConnection) -> Result<Connection, ConfigError> {
    let id = raw.id;
    validate_id(&id).map_err(|reason| invalid(&id, reason))?;
    validate_name(&raw.name).map_err(|reason| invalid(&id, reason))?;
    validate_group(&raw.group).map_err(|reason| invalid(&id, reason))?;
    validate_instance(&raw.instance).map_err(|reason| invalid(&id, reason))?;
    validate_port(raw.port).map_err(|reason| invalid(&id, reason))?;

    // address is merged from defaults, so it can only be validated once the
    // final value is known.
    let address = raw
        .address
        .or_else(|| defaults.address.clone())
        .unwrap_or_else(|| DEFAULT_ADDRESS.to_string());
    validate_address(&address).map_err(|reason| invalid(&id, reason))?;

    Ok(Connection {
        id,
        name: raw.name,
        group: raw.group,
        instance: raw.instance,
        address,
        port: raw.port,
        private_ip: raw.private_ip.or(defaults.private_ip).unwrap_or(false),
        auto_iam_authn: raw
            .auto_iam_authn
            .or(defaults.auto_iam_authn)
            .unwrap_or(false),
        extra_args: raw
            .extra_args
            .or_else(|| defaults.extra_args.clone())
            .unwrap_or_default(),
        enabled: raw.enabled.or(defaults.enabled).unwrap_or(true),
    })
}

fn invalid(id: &str, reason: impl Into<String>) -> ConfigError {
    ConfigError::InvalidConnection {
        id: id.to_string(),
        reason: reason.into(),
    }
}

/// Shared non-empty check for `name`, `group`, and `address`
/// (`docs/config.v1.md`, "Connection fields").
fn require_non_empty(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(())
    }
}

/// Non-empty (`docs/config.v1.md`, "Connection fields"). Free text
/// otherwise; no charset rule beyond that.
fn validate_name(name: &str) -> Result<(), String> {
    require_non_empty("name", name)
}

/// Non-empty and must not start with `-` (`docs/config.v1.md`,
/// "Connection fields"). Free text otherwise, e.g. `Prod EU (read only)`.
/// A leading `-` is rejected because clap reads it as an option, not a
/// value, so `--group -legacy` could never reach this Connection.
fn validate_group(group: &str) -> Result<(), String> {
    require_non_empty("group", group)?;
    if group.starts_with('-') {
        Err(format!("group `{group}` must not start with '-'"))
    } else {
        Ok(())
    }
}

/// Non-empty after the defaults merge (`docs/config.v1.md`, "Connection
/// fields"). Free text otherwise.
fn validate_address(address: &str) -> Result<(), String> {
    require_non_empty("address", address)
}

/// `^[a-zA-Z0-9][a-zA-Z0-9_-]*$`, length 1-64 (`docs/config.v1.md`,
/// "Connection fields").
fn validate_id(id: &str) -> Result<(), String> {
    let mut chars = id.chars();
    let starts_alphanumeric = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    let rest_is_id_safe = chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if starts_alphanumeric && rest_is_id_safe && id.len() <= MAX_ID_LEN {
        Ok(())
    } else {
        Err(format!(
            "id `{id}` must match ^[a-zA-Z0-9][a-zA-Z0-9_-]*$ and be at most {MAX_ID_LEN} bytes"
        ))
    }
}

/// `project:region:instance` — three non-empty, whitespace-free segments
/// (`docs/config.v1.md`, "Connection fields").
fn validate_instance(instance: &str) -> Result<(), String> {
    let segments: Vec<&str> = instance.split(':').collect();
    let shape_ok = segments.len() == 3
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && !segment.chars().any(char::is_whitespace));
    if shape_ok {
        Ok(())
    } else {
        Err(format!(
            "instance `{instance}` must be project:region:instance (three non-empty segments)"
        ))
    }
}

/// 1024-65535, excluding the reserved set (`docs/config.v1.md`,
/// "Reserved ports").
fn validate_port(port: u16) -> Result<(), String> {
    if port < MIN_UNPRIVILEGED_PORT {
        Err(format!(
            "port {port} is privileged (must be >= {MIN_UNPRIVILEGED_PORT})"
        ))
    } else if RESERVED_PORTS.contains(&port) {
        Err(format!("port {port} is reserved"))
    } else {
        Ok(())
    }
}

/// Refuse a config with more than [`MAX_CONNECTIONS`] rows. Counts every
/// Connection, including `enabled: false` (`docs/config.v1.md`,
/// "Top-level object", the `connections` row). All-or-nothing: one
/// Connection over the limit fails the whole document, same as any other
/// validation error.
fn check_connection_count(connections: &[Connection]) -> Result<(), ConfigError> {
    if connections.len() > MAX_CONNECTIONS {
        return Err(ConfigError::TooManyConnections {
            count: connections.len(),
            max: MAX_CONNECTIONS,
        });
    }
    Ok(())
}

/// Unique `id`, `port`, and `instance` across the whole file
/// (`docs/config.v1.md`, "Uniqueness").
fn check_uniqueness(connections: &[Connection]) -> Result<(), ConfigError> {
    let mut seen_ids = HashSet::new();
    let mut seen_ports = HashSet::new();
    let mut seen_instances = HashSet::new();

    for connection in connections {
        if !seen_ids.insert(connection.id.clone()) {
            return Err(ConfigError::DuplicateId(connection.id.clone()));
        }
        if !seen_ports.insert(connection.port) {
            return Err(ConfigError::DuplicatePort(connection.port));
        }
        if !seen_instances.insert(connection.instance.clone()) {
            return Err(ConfigError::DuplicateInstance(connection.instance.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../examples/connections.json");

    #[test]
    fn parse_loads_the_golden_config() {
        let config = parse(GOLDEN.as_bytes()).expect("golden config is valid");
        assert_eq!(config.proxy_bin, "cloud-sql-proxy");
        assert_eq!(config.connections.len(), 7);

        let backend_dev =
            by_id(&config, "backend-dev").expect("backend-dev is in the golden config");
        assert_eq!(backend_dev.name, "Backend Dev");
        assert_eq!(backend_dev.group, "backend");
        assert_eq!(backend_dev.instance, "acme-dev:europe-west1:backend-db-1");
        assert_eq!(backend_dev.port, 15432);
        assert_eq!(backend_dev.address, "127.0.0.1");
        assert!(!backend_dev.private_ip);
        assert!(!backend_dev.auto_iam_authn);
        assert!(backend_dev.extra_args.is_empty());
        assert!(backend_dev.enabled);
    }

    #[test]
    fn parse_rejects_an_unknown_top_level_key() {
        let json = r#"{"version": 1, "connections": [], "bogus": true}"#;
        let err = parse(json.as_bytes()).expect_err("unknown top-level key must reject");
        assert!(matches!(err, ConfigError::Json(_)));
    }

    #[test]
    fn parse_rejects_an_unknown_defaults_key() {
        let json = r#"{"version": 1, "defaults": {"prot": 1}, "connections": []}"#;
        let err = parse(json.as_bytes()).expect_err("unknown defaults key must reject");
        assert!(matches!(err, ConfigError::Json(_)));
    }

    #[test]
    fn parse_rejects_an_unknown_connection_key() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "p:r:i", "port": 15432, "prot": 1}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("unknown connection key must reject");
        assert!(matches!(err, ConfigError::Json(_)));
    }

    #[test]
    fn parse_rejects_a_duplicate_id() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "p:r:i1", "port": 15432},
                {"id": "a", "name": "A2", "group": "g", "instance": "p:r:i2", "port": 15433}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("duplicate id must reject");
        assert_eq!(err.to_string(), "duplicate connection id `a`");
    }

    #[test]
    fn parse_rejects_a_duplicate_port() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "p:r:i1", "port": 15432},
                {"id": "b", "name": "B", "group": "g", "instance": "p:r:i2", "port": 15432}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("duplicate port must reject");
        assert_eq!(err.to_string(), "duplicate port 15432");
    }

    #[test]
    fn parse_rejects_a_duplicate_instance() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "p:r:i", "port": 15432},
                {"id": "b", "name": "B", "group": "g", "instance": "p:r:i", "port": 15433}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("duplicate instance must reject");
        assert_eq!(err.to_string(), "duplicate instance `p:r:i`");
    }

    #[test]
    fn parse_rejects_reserved_ports() {
        for port in [1, 1023, 5432, 3306, 1433] {
            let json = format!(
                r#"{{"version": 1, "connections": [{{"id": "a", "name": "A", "group": "g", "instance": "p:r:i", "port": {port}}}]}}"#
            );
            let err = parse(json.as_bytes()).expect_err("reserved/privileged port must reject");
            assert!(
                matches!(err, ConfigError::InvalidConnection { .. }),
                "port {port}"
            );
        }
    }

    #[test]
    fn parse_accepts_the_lowest_and_highest_unprivileged_ports() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "p:r:i1", "port": 1024},
                {"id": "b", "name": "B", "group": "g", "instance": "p:r:i2", "port": 65535}
            ]
        }"#;
        let config = parse(json.as_bytes()).expect("1024 and 65535 are both allowed");
        assert_eq!(config.connections[0].port, 1024);
        assert_eq!(config.connections[1].port, 65535);
    }

    #[test]
    fn parse_rejects_an_invalid_id_charset() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "-a", "name": "A", "group": "g", "instance": "p:r:i", "port": 15432}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("id starting with a dash must reject");
        assert!(matches!(err, ConfigError::InvalidConnection { .. }));
    }

    #[test]
    fn parse_rejects_a_group_starting_with_a_dash() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "-legacy", "instance": "p:r:i", "port": 15432}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("a group starting with '-' must reject");
        assert_eq!(
            err.to_string(),
            "connection `a`: group `-legacy` must not start with '-'"
        );
    }

    #[test]
    fn parse_accepts_a_free_text_group_with_spaces_and_parens() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "Prod EU (read only)", "instance": "p:r:i", "port": 15432}
            ]
        }"#;
        let config = parse(json.as_bytes()).expect("free-text group must load");
        assert_eq!(config.connections[0].group, "Prod EU (read only)");
    }

    /// `starts_with('-')` checks the leading `char`, not the leading byte.
    /// A multi-byte first character (`ü`) must load, and a look-alike dash
    /// that is not ASCII `-` (U+2013 en dash) must also load. Locks the
    /// char-vs-byte behavior so a future `as_bytes()[0]` "optimization"
    /// fails loudly.
    #[test]
    fn parse_accepts_a_group_with_a_multi_byte_first_character() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "über", "instance": "p:r:i1", "port": 15432},
                {"id": "b", "name": "B", "group": "–legacy", "instance": "p:r:i2", "port": 15433}
            ]
        }"#;
        let config = parse(json.as_bytes()).expect("multi-byte first character must load");
        assert_eq!(config.connections[0].group, "über");
        assert_eq!(config.connections[1].group, "–legacy");
    }

    /// `schemas/config.v1.json` `group` pattern is `^[^-]` (anchored at the
    /// start, no `$`/`.*` reach across lines). `^[^-].*$` without the `m`
    /// flag rejected a value containing `\n` even though Rust's
    /// `validate_group` accepts any non-empty, non-dash-led string. Locks
    /// that a newline in `group` parses in Rust; see the acceptance report
    /// for the matching `check-jsonschema` run against the schema file.
    #[test]
    fn parse_accepts_a_group_containing_a_newline() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "line1\nline2", "instance": "p:r:i", "port": 15432}
            ]
        }"#;
        let config = parse(json.as_bytes()).expect("a group containing a newline must load");
        assert_eq!(config.connections[0].group, "line1\nline2");
    }

    #[test]
    fn parse_rejects_an_empty_group() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "", "instance": "p:r:i", "port": 15432}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("an empty group must reject");
        assert_eq!(err.to_string(), "connection `a`: group must not be empty");
    }

    #[test]
    fn parse_rejects_an_empty_name() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "", "group": "g", "instance": "p:r:i", "port": 15432}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("an empty name must reject");
        assert_eq!(err.to_string(), "connection `a`: name must not be empty");
    }

    #[test]
    fn parse_rejects_an_empty_address() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "p:r:i", "port": 15432, "address": ""}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("an empty address must reject");
        assert_eq!(err.to_string(), "connection `a`: address must not be empty");
    }

    #[test]
    fn parse_rejects_an_invalid_instance_shape() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "not-three-segments", "port": 15432}
            ]
        }"#;
        let err =
            parse(json.as_bytes()).expect_err("a non project:region:instance shape must reject");
        assert!(matches!(err, ConfigError::InvalidConnection { .. }));
    }

    #[test]
    fn parse_rejects_a_connection_missing_a_required_identity_field() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "port": 15432}
            ]
        }"#;
        let err = parse(json.as_bytes()).expect_err("a connection missing `instance` must reject");
        assert!(matches!(err, ConfigError::Json(_)));
    }

    #[test]
    fn parse_rejects_a_connection_missing_its_own_group() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "instance": "p:r:i1", "port": 15432}
            ]
        }"#;
        let err = parse(json.as_bytes())
            .expect_err("group is required on every connection, per schemas/config.v1.json");
        assert!(matches!(err, ConfigError::Json(_)));
    }

    #[test]
    fn parse_rejects_group_supplied_only_via_defaults() {
        let json = r#"{
            "version": 1,
            "defaults": {"group": "shared"},
            "connections": []
        }"#;
        let err =
            parse(json.as_bytes()).expect_err("group belongs on each connection, not defaults");
        assert!(matches!(err, ConfigError::Json(_)));
    }

    /// Builds a `connections.json` body with `count` valid, unique rows.
    /// Row 0 is `enabled: false`, so the 33-row test can prove a disabled
    /// row still counts toward the limit.
    fn connections_json(count: usize) -> String {
        let rows: Vec<String> = (0..count)
            .map(|i| {
                let enabled = if i == 0 { "false" } else { "true" };
                format!(
                    r#"{{"id": "c{i}", "name": "C{i}", "group": "g", "instance": "p:r:i{i}", "port": {port}, "enabled": {enabled}}}"#,
                    port = 20000 + i
                )
            })
            .collect();
        format!(r#"{{"version": 1, "connections": [{}]}}"#, rows.join(","))
    }

    #[test]
    fn parse_rejects_a_config_with_more_than_32_connections() {
        let json = connections_json(33);
        let err = parse(json.as_bytes()).expect_err("33 connections must reject");
        assert_eq!(
            err.to_string(),
            "config holds 33 connections, the maximum is 32"
        );
    }

    #[test]
    fn parse_accepts_a_config_with_exactly_32_connections() {
        let json = connections_json(32);
        let config = parse(json.as_bytes()).expect("32 connections is valid");
        assert_eq!(config.connections.len(), 32);
        assert!(!config.connections[0].enabled, "row 0 stays disabled");
    }

    #[test]
    fn parse_rejects_an_unsupported_version() {
        let json = r#"{"version": 2, "connections": []}"#;
        let err = parse(json.as_bytes()).expect_err("version 2 must reject");
        assert_eq!(
            err.to_string(),
            "config version 2 is not supported (expected 1)"
        );
    }

    #[test]
    fn parse_accepts_an_empty_connections_array() {
        let json = r#"{"version": 1, "connections": []}"#;
        let config = parse(json.as_bytes()).expect("an empty inventory is valid");
        assert!(config.connections.is_empty());
    }

    #[test]
    fn parse_applies_built_in_defaults_when_nothing_else_is_set() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "p:r:i", "port": 15432}
            ]
        }"#;
        let config = parse(json.as_bytes()).expect("built-in defaults fill the rest");
        let connection = &config.connections[0];
        assert_eq!(connection.address, "127.0.0.1");
        assert!(!connection.private_ip);
        assert!(!connection.auto_iam_authn);
        assert!(connection.extra_args.is_empty());
        assert!(connection.enabled);
    }

    #[test]
    fn parse_applies_file_defaults_over_built_ins() {
        let json = r#"{
            "version": 1,
            "defaults": {"address": "0.0.0.0", "enabled": false},
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "p:r:i", "port": 15432}
            ]
        }"#;
        let config = parse(json.as_bytes()).expect("file defaults apply");
        let connection = &config.connections[0];
        assert_eq!(connection.address, "0.0.0.0");
        assert!(!connection.enabled);
    }

    #[test]
    fn parse_lets_a_connection_override_file_defaults() {
        let json = r#"{
            "version": 1,
            "defaults": {"address": "0.0.0.0"},
            "connections": [
                {"id": "a", "name": "A", "group": "g", "instance": "p:r:i", "port": 15432, "address": "10.0.0.1"}
            ]
        }"#;
        let config = parse(json.as_bytes()).expect("a connection can override a default");
        assert_eq!(config.connections[0].address, "10.0.0.1");
    }

    #[test]
    fn parse_defaults_proxy_bin_when_absent() {
        let json = r#"{"version": 1, "connections": []}"#;
        let config = parse(json.as_bytes()).expect("empty inventory is valid");
        assert_eq!(config.proxy_bin, "cloud-sql-proxy");
    }

    #[test]
    fn parse_honors_an_explicit_proxy_bin() {
        let json = r#"{"version": 1, "proxy_bin": "/opt/bin/cloud-sql-proxy", "connections": []}"#;
        let config = parse(json.as_bytes()).expect("explicit proxy_bin is honored");
        assert_eq!(config.proxy_bin, "/opt/bin/cloud-sql-proxy");
    }

    #[test]
    fn parse_rejects_an_empty_proxy_bin() {
        let json = r#"{"version": 1, "proxy_bin": "", "connections": []}"#;
        let err = parse(json.as_bytes()).expect_err("an empty proxy_bin must reject");
        assert_eq!(err.to_string(), "proxy_bin must not be empty");
    }

    #[test]
    fn by_id_returns_none_for_an_unknown_id() {
        let config = parse(GOLDEN.as_bytes()).expect("golden config is valid");
        assert!(by_id(&config, "does-not-exist").is_none());
    }

    #[test]
    fn load_reads_and_parses_a_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("cst-config-test-{}.json", std::process::id()));
        std::fs::write(&path, GOLDEN).expect("can write a temp config file");

        let config = load(&path).expect("load reads and parses the file");
        assert_eq!(config.connections.len(), 7);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_reports_a_missing_file() {
        let path = std::env::temp_dir().join("cst-config-test-does-not-exist.json");
        let err = load(&path).expect_err("a missing file must error");
        assert!(matches!(err, ConfigError::Io { .. }));
    }

    #[test]
    fn default_path_prefers_xdg_config_home() {
        let previous = std::env::var("XDG_CONFIG_HOME").ok();
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/xdg-test-home");

        let path = default_path();
        assert_eq!(
            path,
            PathBuf::from("/tmp/xdg-test-home/cloud-sql-tracker/connections.json")
        );

        match previous {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
