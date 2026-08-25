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
//! **Residual risk, and how this file resolves it (must-fix from PR #68
//! review):** `status --json` on a **non-empty** config talks to the real
//! systemd user bus (`supervisor::show`). Some CI runners have no session
//! bus (`docs/research/systemd-user-units.md`: "SSH-only ... contexts
//! without a user manager"), which would otherwise make that one
//! assertion silently unexecuted in CI. Two separate tests split this
//! apart instead of one bus-dependent test carrying the whole proof:
//!
//! - [`status_json_stdout_with_zero_connections_validates_against_the_status_schema`]
//!   uses `connections: []`. `commands::status` never calls
//!   `supervisor::show` / `port::observe` for an empty selector expansion
//!   (`src/commands/select.rs::expand`), so this run needs **no** systemd
//!   user bus and always exits `0`. This is the **unconditional** Layer 2
//!   proof for the Status envelope shape issue #47 requires — it always
//!   runs for real in CI, never skips.
//! - [`status_json_stdout_validates_against_the_status_schema`] uses one
//!   Connection and additionally proves the populated `connections[]`
//!   row shape. It performs the full schema assertion whenever the bus is
//!   reachable (exit `0`), and degrades to a visible skip note **only**
//!   when stderr names the specific environmental causes
//!   `SupervisorError::is_dependency` already classifies as "cannot
//!   operate at all" (`src/supervisor.rs`): no session bus reachable, or
//!   the bus is up but no systemd user manager is registered on it
//!   (`ServiceUnknown`). Any other exit `3` (e.g. `MissingProperty` /
//!   `MalformedProperty` — a real parsing regression against a bus that
//!   **is** working) panics instead of skipping.

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
/// row). A synthetic id (`layer2-fixture`, not a real operator Connection
/// like `fe-dev`) keeps this test's assertions independent of whatever
/// Units happen to exist on the machine running it.
const MINIMAL_CONFIG: &str = r#"{
    "version": 1,
    "connections": [
        {"id": "layer2-fixture", "name": "Layer 2 Fixture", "group": "fe", "instance": "proj:region:inst", "port": 15432}
    ]
}"#;

/// Zero Connections — `status --json` still produces a fully valid
/// (empty) Status document from this (`docs/config.v1.md`: "Empty
/// `connections: []` is valid"). `select::expand` on `Selector::All`
/// with no Connections returns `Ok(vec![])`, so `commands::status` never
/// calls `supervisor::show` / `port::observe` at all — this fixture's
/// `status --json` run needs **no** systemd user bus and is therefore a
/// hard, unconditional CI proof of the real stdout shape (see this file's
/// module doc comment: the richer single-Connection test below still has
/// to tolerate "no user bus" as an environment gap).
const EMPTY_CONFIG: &str = r#"{"version": 1, "connections": []}"#;

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
    assert!(
        stdout.ends_with(b"\n"),
        "stdout must end with a final newline"
    );
    let text = std::str::from_utf8(stdout).expect("stdout is utf8");
    serde_json::from_str(text)
        .unwrap_or_else(|err| panic!("stdout is not valid JSON ({err}):\n{text}"))
}

// ---------------------------------------------------------------------------
// status --json
// ---------------------------------------------------------------------------

/// **Hard, unconditional Layer 2 proof** — issue #47's first must-prove
/// ("`status --json` stdout validates against `schemas/status.v1.json`").
/// Zero Connections means `commands::status` never reaches
/// `supervisor::show` / `port::observe` at all (`src/commands/select.rs`,
/// `Selector::All` on an empty config returns `Ok(vec![])`), so this test
/// needs no systemd user bus and must always exit `0`. This is the case
/// this file's module doc comment says CI can rely on for real, every
/// run — never a skip.
#[test]
fn status_json_stdout_with_zero_connections_validates_against_the_status_schema() {
    let config = ConfigFixture::write(EMPTY_CONFIG);
    let output = bin()
        .args([
            "--config",
            config.path().to_str().unwrap(),
            "status",
            "--json",
        ])
        .output()
        .expect("spawn the binary");

    assert_eq!(
        output.status.code(),
        Some(0),
        "status --json on an empty config must always succeed (no supervisor \
         I/O for zero Connections); stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let instance = parse_stdout_json(&output.stdout);
    assert_validates("schemas/status.v1.json", &instance);
    assert_eq!(instance["version"], 1);
    assert_eq!(instance["total"], 0);
    assert!(instance["connections"].as_array().unwrap().is_empty());
}

/// Richer proof over [`status_json_stdout_with_zero_connections_validates_against_the_status_schema`]:
/// with one Connection, `status --json` stdout must also validate the
/// populated `connections[]` row shape against `schemas/status.v1.json`.
/// A never-started Connection still reconciles to a fully valid `stopped`
/// row — no real `cloud-sql-proxy` process is needed.
///
/// This one **does** need the real systemd user bus
/// (`supervisor::show`). See this file's module doc comment: the skip
/// branch below only fires for the specific environmental causes
/// `SupervisorError::is_dependency` classifies as "cannot operate at
/// all" — never for an unrelated regression that also happens to exit
/// `3`.
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
            assert_eq!(instance["connections"][0]["id"], "layer2-fixture");
        }
        Some(3) if stderr_indicates_no_systemd_user_manager(&output.stderr) => {
            // `docs/cli-contract.v1.md`, "`status`": exit `3` is "a hard
            // dependency failure that prevents producing status" — the
            // systemd user bus is unreachable, or reachable but has no
            // registered user manager, in this environment
            // (`docs/research/systemd-user-units.md`: "SSH-only ...
            // contexts without a user manager"; `SupervisorError::
            // is_dependency`), not a code regression.
            eprintln!(
                "SKIP (environment): status --json exited 3 with no systemd \
                 user manager reachable in this environment, instead of \
                 validating stdout. stderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        other => panic!(
            "unexpected exit {other:?} for status --json (not a recognized \
             \"no user manager\" environment gap — see \
             stderr_indicates_no_systemd_user_manager); stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

/// Whether `stderr` names one of the two environmental “cannot operate at
/// all” causes `SupervisorError::is_dependency` classifies
/// (`src/supervisor.rs`): no session bus reachable at all
/// (`SupervisorError::Bus`'s message), or a bus that is reachable but has
/// no systemd user manager registered on it (`ServiceUnknown`, wrapped by
/// `SupervisorError::Call`). Anything else — in particular
/// `MissingProperty` / `MalformedProperty`, which mean systemd **did**
/// answer but this adapter misread the reply — is a real regression the
/// caller must not treat as an environment skip.
fn stderr_indicates_no_systemd_user_manager(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("could not reach the systemd user bus") || text.contains("ServiceUnknown")
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
    // Same disposable-fixture unique-path scheme as `ConfigFixture`
    // (PID + a monotonic counter) even though no file is ever created
    // here — PID alone can collide with a stale leftover from a prior,
    // possibly killed, test run using the same PID.
    let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
    let mut missing = std::env::temp_dir();
    missing.push(format!(
        "cloud-sql-tracker-cli-json-contracts-missing-{}-{unique}.json",
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

    // `docs/cli-contract.v1.md`, "`doctor`": `0` (ok) or `3` (a check
    // failed) are both legitimate outcomes — CI's environment may
    // legitimately fail `systemd_user` / `adc` / `journal_user` even
    // though `config` itself passes, matching the sibling doctor tests'
    // discipline above.
    assert!(
        matches!(output.status.code(), Some(0) | Some(3)),
        "unexpected exit {:?} for doctor --json on the golden config; stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let instance = parse_stdout_json(&output.stdout);
    let config_check = instance["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "config")
        .expect("doctor always reports a config check row");
    assert_eq!(config_check["status"], "pass");

    // Derived from the golden itself, not hard-coded — adding an 8th
    // golden Connection must not silently break this unrelated test.
    let golden_connection_count = golden["connections"].as_array().unwrap().len();
    let expected_detail_fragment = format!("({golden_connection_count} connections)");
    assert!(
        config_check["detail"]
            .as_str()
            .unwrap()
            .contains(&expected_detail_fragment),
        "expected the golden's {golden_connection_count} connections in the config check detail, got: {}",
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

/// Identity fields (`id`, `name`, `group`, `instance`, `port`) in `defaults`
/// are rejected **both** by the schema (`additionalProperties: false` on
/// `defaultsObject`) **and** by the binary's parser — exit `2`.
#[test]
fn identity_fields_in_defaults_are_rejected_by_the_schema_and_the_binary() {
    let schema = read_schema("schemas/config.v1.json");
    let validator = jsonschema::validator_for(&schema).expect("compile the config schema");
    let fields = [
        ("id", r#""a""#),
        ("name", r#""Shared Name""#),
        ("group", r#""shared""#),
        ("instance", r#""p:r:i""#),
        ("port", "15432"),
    ];

    for (field, val) in fields {
        let json =
            format!(r#"{{"version": 1, "defaults": {{"{field}": {val}}}, "connections": []}}"#);
        let instance: serde_json::Value =
            serde_json::from_str(&json).expect("fixture is valid JSON");
        assert!(
            validator.iter_errors(&instance).next().is_some(),
            "schemas/config.v1.json must reject defaults.{field}"
        );

        let config = ConfigFixture::write(&json);
        let status = bin()
            .args(["--config", config.path().to_str().unwrap(), "status"])
            .status()
            .expect("spawn the binary");
        assert_eq!(
            status.code(),
            Some(2),
            "binary must exit 2 when defaults.{field} is set"
        );
    }
}

// ---------------------------------------------------------------------------
// Layer 2 built-binary fixture proofs (maximum config input coverage)
// ---------------------------------------------------------------------------

#[test]
fn status_json_stdout_32_row_max_config_input_coverage_stays_under_cap() {
    let rows: Vec<String> = (0..32)
        .map(|i| {
            let id = format!("c{i:063}");
            assert_eq!(id.len(), 64);
            let name = format!("\"\\{}", "a".repeat(62));
            assert_eq!(name.len(), 64);
            let name_json = serde_json::to_string(&name).unwrap();
            let group = format!("g{i:031}");
            assert_eq!(group.len(), 32);
            let group_json = serde_json::to_string(&group).unwrap();
            let instance = format!("proj:reg{i}:{}", "i".repeat(256 - 9 - format!("{i}").len()));
            assert_eq!(instance.len(), 256);
            let extra_args: Vec<String> = (0..16).map(|_| "x".repeat(128)).collect();
            let json_args = serde_json::to_string(&extra_args).unwrap();
            // Non-IP address forces a per-connection config error row in status
            // without requiring a systemd user bus session in CI.
            let address = format!("invalid.local.host{}", "a".repeat(253 - 18));
            assert_eq!(address.len(), 253);

            format!(
                r#"{{"id": "{id}", "name": {name_json}, "group": {group_json}, "instance": "{instance}", "address": "{address}", "port": {port}, "extra_args": {json_args}}}"#,
                port = 20000 + i
            )
        })
        .collect();

    let json_content = format!(r#"{{"version": 1, "connections": [{}]}}"#, rows.join(","));
    let config = ConfigFixture::write(&json_content);

    let output = bin()
        .args([
            "--config",
            config.path().to_str().unwrap(),
            "status",
            "--json",
        ])
        .output()
        .expect("spawn binary");

    assert_eq!(
        output.status.code(),
        Some(0),
        "status --json 32-row config must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout_bytes = &output.stdout;
    assert!(
        stdout_bytes.len() <= 262_144,
        "stdout byte length {} exceeds 262144 bytes",
        stdout_bytes.len()
    );

    let instance = parse_stdout_json(stdout_bytes);
    assert_validates("schemas/status.v1.json", &instance);
    assert_eq!(instance["total"], 32);
    eprintln!(
        "MEASURED: 32-row observed binary status JSON stdout size: {} bytes (cap: 262144)",
        stdout_bytes.len()
    );
}

#[test]
fn doctor_json_stdout_max_config_input_coverage_stays_under_cap() {
    let long_path_dir = "d".repeat(200);
    let config = ConfigFixture::write(EMPTY_CONFIG);
    let path_with_long_dir = config
        .path()
        .parent()
        .unwrap()
        .join(long_path_dir)
        .join("conn.json");

    let output = bin()
        .args([
            "--config",
            path_with_long_dir.to_str().unwrap(),
            "doctor",
            "--json",
        ])
        .output()
        .expect("spawn binary");

    assert_eq!(output.status.code(), Some(3)); // config check fails for missing path -> exit 3

    let stdout_bytes = &output.stdout;
    assert!(
        stdout_bytes.len() <= 65_536,
        "stdout byte length {} exceeds 65536 bytes",
        stdout_bytes.len()
    );

    let instance = parse_stdout_json(stdout_bytes);
    assert_validates("schemas/doctor.v1.json", &instance);
    assert_eq!(instance["ok"], false);
    assert_eq!(instance["checks"].as_array().unwrap().len(), 6);
    eprintln!(
        "MEASURED: observed Doctor JSON stdout size: {} bytes (cap: 65536)",
        stdout_bytes.len()
    );
}

#[test]
fn over_limit_config_field_rejects_with_exit_code_2() {
    let over_limit_id = "a".repeat(65);
    let json_content = format!(
        r#"{{"version": 1, "connections": [{{"id": "{over_limit_id}", "name": "A", "group": "g", "instance": "p:r:i", "port": 15432}}]}}"#
    );
    let config = ConfigFixture::write(&json_content);

    let output = bin()
        .args(["--config", config.path().to_str().unwrap(), "status"])
        .output()
        .expect("spawn binary");

    assert_eq!(output.status.code(), Some(2));
}
