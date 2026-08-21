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
    #[error("connection `{id}`: {reason}")]
    InvalidConnection { id: String, reason: String },
    #[error("duplicate connection id `{0}`")]
    DuplicateId(String),
    #[error("duplicate port {0}")]
    DuplicatePort(u16),
    #[error("duplicate instance `{0}`")]
    DuplicateInstance(String),
}

/// Read `path` then [`parse`] its bytes.
///
/// Used by `commands`/`cli` (#42+); exercised directly by this module's
/// tests until then.
#[allow(dead_code)]
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
    check_uniqueness(&connections)?;

    Ok(Config {
        proxy_bin: raw
            .proxy_bin
            .unwrap_or_else(|| DEFAULT_PROXY_BIN.to_string()),
        connections,
    })
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
///
/// Used by `commands`/`cli` (#42+); exercised directly by this module's
/// tests until then.
#[allow(dead_code)]
pub(crate) fn by_id<'a>(config: &'a Config, id: &str) -> Option<&'a Connection> {
    config
        .connections
        .iter()
        .find(|connection| connection.id == id)
}

/// Raw shapes mirroring `schemas/config.v1.json`. `Option` fields may be
/// filled by [`RawDefaults`] during merge; unknown keys are rejected at
/// every level (`docs/config.v1.md`, "Strict keys").
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: i64,
    proxy_bin: Option<String>,
    defaults: Option<RawDefaults>,
    connections: Vec<RawConnection>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    name: Option<String>,
    group: Option<String>,
    instance: Option<String>,
    port: Option<u16>,
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
    name: Option<String>,
    group: Option<String>,
    instance: Option<String>,
    port: Option<u16>,
    address: Option<String>,
    private_ip: Option<bool>,
    auto_iam_authn: Option<bool>,
    extra_args: Option<Vec<String>>,
    enabled: Option<bool>,
}

/// Built-in defaults (`docs/config.v1.md`, "Built-in defaults") layered
/// under file `defaults`, then the connection object (`docs/config.v1.md`,
/// "Merge order").
fn merge_connection(defaults: &RawDefaults, raw: RawConnection) -> Result<Connection, ConfigError> {
    let id = raw.id;
    validate_id(&id).map_err(|reason| invalid(&id, reason))?;

    let name = raw
        .name
        .or_else(|| defaults.name.clone())
        .ok_or_else(|| missing(&id, "name"))?;
    let group = raw
        .group
        .or_else(|| defaults.group.clone())
        .ok_or_else(|| missing(&id, "group"))?;
    let instance = raw
        .instance
        .or_else(|| defaults.instance.clone())
        .ok_or_else(|| missing(&id, "instance"))?;
    validate_instance(&instance).map_err(|reason| invalid(&id, reason))?;
    let port = raw
        .port
        .or(defaults.port)
        .ok_or_else(|| missing(&id, "port"))?;
    validate_port(port).map_err(|reason| invalid(&id, reason))?;

    Ok(Connection {
        id,
        name,
        group,
        instance,
        address: raw
            .address
            .or_else(|| defaults.address.clone())
            .unwrap_or_else(|| DEFAULT_ADDRESS.to_string()),
        port,
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

fn missing(id: &str, field: &str) -> ConfigError {
    invalid(id, format!("missing required field `{field}`"))
}

fn invalid(id: &str, reason: impl Into<String>) -> ConfigError {
    ConfigError::InvalidConnection {
        id: id.to_string(),
        reason: reason.into(),
    }
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
        assert_eq!(
            backend_dev.instance,
            "toolsense-dev:europe-west1:backend-postgres-1"
        );
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
    fn parse_rejects_a_missing_required_field_with_no_default() {
        let json = r#"{
            "version": 1,
            "connections": [
                {"id": "a", "name": "A", "group": "g", "port": 15432}
            ]
        }"#;
        let err = parse(json.as_bytes())
            .expect_err("a connection with no instance and no default must reject");
        assert!(matches!(err, ConfigError::InvalidConnection { .. }));
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
    fn parse_lets_file_defaults_supply_a_shared_group() {
        let json = r#"{
            "version": 1,
            "defaults": {"group": "shared"},
            "connections": [
                {"id": "a", "name": "A", "instance": "p:r:i1", "port": 15432},
                {"id": "b", "name": "B", "instance": "p:r:i2", "port": 15433}
            ]
        }"#;
        let config = parse(json.as_bytes()).expect("group can come from defaults");
        assert_eq!(config.connections[0].group, "shared");
        assert_eq!(config.connections[1].group, "shared");
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
