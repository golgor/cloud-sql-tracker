//! Binary smokes for `cli` (#45) — spawn the **built binary**
//! (`CARGO_BIN_EXE_*`), never call `cli::run()` in-process
//! (`docs/verification.v1.md`, "`cli` smokes": "do **not** change
//! `cli::run()` to take argv"). Not a full argv matrix: `--version`/`-V`,
//! and the handful of usage failures the freeze calls out (unknown id,
//! missing start target, id+`--all`).
//!
//! **Pick:** plain `std::process::Command`, no `assert_cmd`/`predicates`.
//! **Why:** every assertion here is "exit code" and "one line of stdout",
//! which `std::process::Command::output()` already gives directly.
//! **Discarded:** `assert_cmd` (nicer builder/matcher API, but two new
//! dev-dependencies for output this simple). **Unchanged:** `cli::run()`
//! still takes no argv; these tests only ever go through the real binary.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static UNIQUE: AtomicU64 = AtomicU64::new(0);

/// A `connections.json` fixture under the system temp dir, removed on
/// drop — the same disposable-fixture pattern `src/env.rs`'s own tests
/// already use.
struct ConfigFixture(PathBuf);

impl ConfigFixture {
    fn write(json: &str) -> Self {
        let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "cloud-sql-tracker-cli-smoke-{}-{unique}.json",
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

/// One enabled Connection (`fe-dev`) — enough for the usage-error smokes
/// below, which never reach real supervisor/port I/O.
const MINIMAL_CONFIG: &str = r#"{
    "version": 1,
    "connections": [
        {"id": "fe-dev", "name": "FE Dev", "group": "fe", "instance": "proj:region:inst", "port": 15432}
    ]
}"#;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cloud-sql-tracker"))
}

#[test]
fn version_prints_the_bare_cargo_package_version() {
    // `docs/cli-contract.v1.md`, "Version": "Exactly one line on stdout,
    // no `v` prefix, no binary name."
    let output = bin().arg("--version").output().expect("spawn the binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf8");
    assert_eq!(stdout.trim(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn short_version_flag_prints_the_same_bare_version() {
    let output = bin().arg("-V").output().expect("spawn the binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf8");
    assert_eq!(stdout.trim(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn start_with_an_unknown_id_exits_usage() {
    // `docs/cli-contract.v1.md`, "Target selectors": "Unknown `ID` ...
    // exit `2`."
    let config = ConfigFixture::write(MINIMAL_CONFIG);
    let status = bin()
        .args(["--config", config.path().to_str().unwrap(), "start", "nope"])
        .status()
        .expect("spawn the binary");
    assert_eq!(status.code(), Some(2));
}

#[test]
fn start_without_a_target_exits_usage() {
    // `docs/cli-contract.v1.md`, "Target selectors": "Defaults":
    // "start/stop/restart: Error exit `2`."
    let config = ConfigFixture::write(MINIMAL_CONFIG);
    let status = bin()
        .args(["--config", config.path().to_str().unwrap(), "start"])
        .status()
        .expect("spawn the binary");
    assert_eq!(status.code(), Some(2));
}

#[test]
fn start_with_an_id_and_all_is_a_mutual_exclusion_usage_error() {
    // `docs/cli-contract.v1.md`, "Target selectors": "Mutual exclusion:
    // id, `--group`, and `--all` cannot combine. Violation -> exit `2`."
    let config = ConfigFixture::write(MINIMAL_CONFIG);
    let status = bin()
        .args([
            "--config",
            config.path().to_str().unwrap(),
            "start",
            "fe-dev",
            "--all",
        ])
        .status()
        .expect("spawn the binary");
    assert_eq!(status.code(), Some(2));
}
