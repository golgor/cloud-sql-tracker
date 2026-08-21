//! Layer 2 proof for issue #23 (`docs/verification.v1.md`, "CI layers"):
//! the **built binary's** `status --json` / `doctor --json` **stdout**
//! must validate against `schemas/status.v1.json` /
//! `schemas/doctor.v1.json`, and `config::parse` must agree with
//! `schemas/config.v1.json` on the same bytes. In-process serde alone
//! (already covered in `src/commands/status.rs` / `src/commands/
//! doctor.rs`) is **not** sufficient for this — see issue #47.
//!
//! Same spawn discipline as `tests/cli_smoke.rs`: real
//! `CARGO_BIN_EXE_*` process, never `cli::run()` in-process.
//!
//! **Pick:** spawn the binary and read real stdout for `status`/`doctor`;
//! validate `examples/connections.json` against the config schema
//! directly with `jsonschema`, and prove the binary's own parser agrees
//! by reading `doctor --json`'s `config` check row (never fails, needs no
//! systemd) instead of calling `config::parse` (`pub(crate)`, not visible
//! from an integration test). **Why:** proves the real process's stdout
//! and the real parser without exposing new public API just for tests.
//! **Discarded:** adding a public `config::parse` wrapper, or a second
//! `--json`-agnostic test binary. **Unchanged:** `cli::run()` takes no
//! argv; `commands`/`config` visibility stays `pub(crate)`.
//!
//! **Residual risk (documented, not fixed here):** `status --json` talks
//! to the real systemd user bus (`supervisor::show`). Some CI runners
//! have no session bus (`docs/research/systemd-user-units.md`: "SSH-only
//! ... contexts without a user manager"). The `status` test below still
//! performs the full schema assertion whenever the bus is reachable
//! (exit `0`), and degrades to a visible skip note instead of a hard
//! failure only when the environment itself lacks a user bus (exit `3`,
//! `doctor`'s own `systemd_user` row already reports `fail` in the same
//! run) — it does not silently pass by ignoring a real regression.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static UNIQUE: AtomicU64 = AtomicU64::new(0);

/// A `connections.json` fixture under the system temp dir, removed on
/// drop — same disposable-fixture pattern as `tests/cli_smoke.rs`.
struct ConfigFixture(PathBuf);

impl ConfigFixture {
    fn write(json: &str) -> Self {
        let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "cloud-sql-tracker-cli-json-contracts-{}-{unique}.json",
            std::process::id()
        ));
        let mut file = std::fs::File::create(&path).expect("create fixture config file");
        file.write_all(json.as_bytes())
            .expect("write fixture config file");
        ConfigFixture(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ConfigFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One enabled Connection — enough to exercise `status --json` without
/// depending on a real `cloud-sql-proxy` binary (a Unit systemd has never
/// loaded reconciles to `stopped`, which is still a fully valid Status
/// row).
const MINIMAL_CONFIG: &str = r#"{
    "version": 1,
    "connections": [
        {"id": "fe-dev", "name": "FE Dev", "group": "fe", "instance": "proj:region:inst", "port": 15432}
    ]
}"#;

/// `docs/config.v1.md` is closed to unknown keys; `schemas/config.v1.json`
/// sets `additionalProperties: false` at the top level. Matches
/// `src/config.rs::parse_rejects_an_unknown_top_level_key`'s fixture so
/// both proofs exercise the identical shape.
const UNKNOWN_TOP_LEVEL_KEY_CONFIG: &str = r#"{"version": 1, "connections": [], "bogus": true}"#;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cloud-sql-tracker"))
}

fn manifest_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_schema(relative: &str) -> serde_json::Value {
    let path = manifest_path(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

/// Validates `instance` against the schema at `schema_relative_path`
/// (relative to the crate root), collecting every violation into the
/// panic message instead of only the first.
fn assert_validates(schema_relative_path: &str, instance: &serde_json::Value) {
    let schema = read_schema(schema_relative_path);
    let validator =
        jsonschema::validator_for(&schema).expect("compile schema (checked in and Layer-1-clean)");
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{schema_relative_path} rejected instance:\n{}\n\ninstance:\n{}",
        errors.join("\n"),
        serde_json::to_string_pretty(instance).unwrap_or_default(),
    );
}

fn parse_stdout_json(stdout: &[u8]) -> serde_json::Value {
    let text = std::str::from_utf8(stdout).expect("stdout is utf8");
    serde_json::from_str(text)
        .unwrap_or_else(|err| panic!("stdout is not valid JSON ({err}):\n{text}"))
}

// ---------------------------------------------------------------------------
// status --json
// ---------------------------------------------------------------------------

/// `status --json` stdout must validate against `schemas/status.v1.json`
/// (`docs/verification.v1.md`, "Status / Doctor JSON": "does not replace
/// #23 Layer 2"). A never-started Connection still reconciles to a fully
/// valid `stopped` row — no real `cloud-sql-proxy` process is needed.
///
/// See this file's module doc comment for the documented systemd-user-bus
/// residual risk this test degrades gracefully on, without silently
/// skipping a real regression when the bus **is** reachable.
#[test]
fn status_json_stdout_validates_against_the_status_schema() {
    let config = ConfigFixture::write(MINIMAL_CONFIG);
    let output = bin()
        .args([
            "--config",
            config.path().to_str().unwrap(),
            "status",
            "--json",
        ])
        .output()
        .expect("spawn the binary");

    match output.status.code() {
        Some(0) => {
            let instance = parse_stdout_json(&output.stdout);
            assert_validates("schemas/status.v1.json", &instance);
            assert_eq!(instance["version"], 1);
            assert_eq!(instance["connections"].as_array().unwrap().len(), 1);
            assert_eq!(instance["connections"][0]["id"], "fe-dev");
        }
        Some(3) => {
            // `docs/cli-contract.v1.md`, "`status`": exit `3` is "a hard
            // dependency failure that prevents producing status" — the
            // systemd user bus is unreachable in this environment
            // (`docs/research/systemd-user-units.md`: "SSH-only ...
            // contexts without a user manager"), not a code regression.
            eprintln!(
                "SKIP (environment): status --json exited 3 (no systemd user \
                 bus in this environment) instead of validating stdout. \
                 stderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        other => panic!(
            "unexpected exit {other:?} for status --json; stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

// ---------------------------------------------------------------------------
// doctor --json
// ---------------------------------------------------------------------------

/// `doctor --json` stdout must validate against `schemas/doctor.v1.json`
/// with a valid config. Unlike `status`, this needs no systemd user bus
/// to produce schema-valid JSON: `commands::doctor` builds one `CheckRow`
/// per check regardless of pass/warn/fail, so the document shape does not
/// depend on any check's outcome, only on the checklist running at all
/// (`docs/doctor.v1.md`: `doctor` never fail-fasts).
#[test]
fn doctor_json_stdout_validates_against_the_doctor_schema_with_a_valid_config() {
    let config = ConfigFixture::write(MINIMAL_CONFIG);
    let output = bin()
        .args([
            "--config",
            config.path().to_str().unwrap(),
            "doctor",
            "--json",
        ])
        .output()
        .expect("spawn the binary");

    // `docs/cli-contract.v1.md`, "`doctor`": exit `0` (ok) or `3` (a check
    // failed) are both legitimate outcomes here — CI's environment may
    // legitimately fail `systemd_user` / `adc` / `journal_user`; the
    // document must still be schema-valid either way.
    assert!(
        matches!(output.status.code(), Some(0) | Some(3)),
        "unexpected exit {:?} for doctor --json; stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let instance = parse_stdout_json(&output.stdout);
    assert_validates("schemas/doctor.v1.json", &instance);
    assert_eq!(instance["version"], 1);

    let config_check = instance["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "config")
        .expect("doctor always reports a config check row");
    assert_eq!(config_check["status"], "pass");
}

/// A config file the binary cannot even read must still produce a
/// schema-valid Doctor report — `commands::doctor` takes the **path**,
/// not an already-loaded `Config`, precisely so a bad config degrades to
/// one failed check row instead of no document at all
/// (`docs/doctor.v1.md`, "Config load path").
#[test]
fn doctor_json_stdout_validates_against_the_doctor_schema_with_a_missing_config() {
    let mut missing = std::env::temp_dir();
    missing.push(format!(
        "cloud-sql-tracker-cli-json-contracts-missing-{}.json",
        std::process::id()
    ));

    let output = bin()
        .args(["--config", missing.to_str().unwrap(), "doctor", "--json"])
        .output()
        .expect("spawn the binary");

    // A failed `config` check makes `ok: false` -> exit `3`
    // (`docs/cli-contract.v1.md`, "`doctor`": "`3` if any check fails").
    assert_eq!(output.status.code(), Some(3));

    let instance = parse_stdout_json(&output.stdout);
    assert_validates("schemas/doctor.v1.json", &instance);
    assert_eq!(instance["ok"], false);

    let config_check = instance["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "config")
        .expect("doctor always reports a config check row");
    assert_eq!(config_check["status"], "fail");
}

// ---------------------------------------------------------------------------
// config parse <-> config schema agreement
// ---------------------------------------------------------------------------

/// The golden config both **validates** against `schemas/config.v1.json`
/// and **parses** through the real binary — the two proofs Layer 1
/// (`scripts/validate-contracts.sh`) and in-process `config::parse` unit
/// tests each only give half of on their own (`docs/verification.v1.md`,
/// "CI layers"). Read through `doctor --json`'s own `config` check row
/// instead of calling `config::parse` directly: it is `pub(crate)`, not
/// visible from an integration test crate, and the doctor row needs no
/// systemd user bus either way.
#[test]
fn golden_config_matches_the_schema_and_the_binary_accepts_it() {
    let golden_path = manifest_path("examples/connections.json");
    let golden_text =
        std::fs::read_to_string(&golden_path).expect("read examples/connections.json");
    let golden: serde_json::Value =
        serde_json::from_str(&golden_text).expect("parse examples/connections.json");
    assert_validates("schemas/config.v1.json", &golden);

    let output = bin()
        .args([
            "--config",
            golden_path.to_str().unwrap(),
            "doctor",
            "--json",
        ])
        .output()
        .expect("spawn the binary");
    let instance = parse_stdout_json(&output.stdout);
    let config_check = instance["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "config")
        .expect("doctor always reports a config check row");
    assert_eq!(config_check["status"], "pass");
    assert!(
        config_check["detail"]
            .as_str()
            .unwrap()
            .contains("7 connections"),
        "expected the golden's 7 connections in the config check detail, got: {}",
        config_check["detail"]
    );
}

/// A top-level unknown key is rejected **both** by the schema
/// (`additionalProperties: false`) **and** by the real binary's parser —
/// exit `2`, the usage/config class (`docs/config.v1.md`, "Load failure ->
/// exit 2"). Uses `status` (not `--json`): a rejected config never
/// reaches `commands::status`, so this needs no systemd user bus.
#[test]
fn unknown_top_level_key_is_rejected_by_the_schema_and_the_binary() {
    let schema = read_schema("schemas/config.v1.json");
    let validator = jsonschema::validator_for(&schema).expect("compile the config schema");
    let instance: serde_json::Value =
        serde_json::from_str(UNKNOWN_TOP_LEVEL_KEY_CONFIG).expect("fixture is valid JSON");
    assert!(
        validator.iter_errors(&instance).next().is_some(),
        "schemas/config.v1.json must reject an unknown top-level key"
    );

    let config = ConfigFixture::write(UNKNOWN_TOP_LEVEL_KEY_CONFIG);
    let status = bin()
        .args(["--config", config.path().to_str().unwrap(), "status"])
        .status()
        .expect("spawn the binary");
    assert_eq!(status.code(), Some(2));
}
