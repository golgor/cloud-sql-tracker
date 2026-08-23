//! `cloud-sql-proxy` binary discovery and Application Default Credentials
//! (ADC) presence. Shared owner for PATH / ADC file discovery: `doctor` is a
//! **caller** of the `*_check` functions here, not a second implementation
//! (`docs/modules.v1.md`, "env — proxy binary + ADC"; ADC is a hard
//! requirement, [ADR 0002](../docs/adr/0002-adc-only-auth.md)).
//!
//! `commands::doctor` (#44) calls the `*_check` rows below.
//! `commands::start` (#43) calls `resolve_proxy_bin` / `adc_status` directly
//! and reuses `proxy_bin_check` / `adc_check` messages for bin_missing / auth.
//! Start only needs the resolve failure message from `proxy_bin_check`; the
//! version probe runs only on doctor's path after a successful resolve.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{kill_process_group, Pid, Signal};

use crate::model::{CheckRow, CheckStatus};

const DEFAULT_PROXY_BIN: &str = "cloud-sql-proxy";

/// How long doctor waits for `cloud-sql-proxy -v` before failing the check
/// (`docs/doctor.v1.md`, "`proxy_bin` — hard").
const PROXY_VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// Poll interval while waiting for the version child to exit.
const PROXY_VERSION_POLL: Duration = Duration::from_millis(20);

/// Resolve the `cloud-sql-proxy` binary to an absolute-or-`PATH`-checked
/// path, for **start** (`docs/config.v1.md`, "`proxy_bin` resolution").
///
/// `configured` is the connections file's top-level `proxy_bin` value, or
/// `None` to use the built-in default name. An absolute path must be an
/// executable file; a bare name is searched on `PATH` the same way a shell
/// would. Mutate (#43) will call this directly for **start**'s env
/// forwarding; [`proxy_bin_check`] below already makes it reachable.
///
/// This function only resolves a path. It does **not** spawn the binary or
/// check identity. Doctor's version probe lives in [`proxy_bin_check`].
pub(crate) fn resolve_proxy_bin(configured: Option<&str>) -> Result<PathBuf, ProxyBinError> {
    let name = configured.unwrap_or(DEFAULT_PROXY_BIN);
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    resolve_bin(name, &path_env)
}

/// Doctor's `proxy_bin` row (`docs/doctor.v1.md`, "`proxy_bin` — hard").
///
/// Resolve first. On resolve failure, return the existing fail row (start
/// reuses that message). On resolve success, spawn `resolved -v` with a
/// short timeout and require cloud-sql-proxy identity output.
pub(crate) fn proxy_bin_check(configured: Option<&str>) -> CheckRow {
    match resolve_proxy_bin(configured) {
        Err(err) => check_row_for_proxy_bin_resolve_error(err),
        Ok(path) => check_row_for_proxy_bin_probe(&path, probe_proxy_version(&path)),
    }
}

fn check_row_for_proxy_bin_resolve_error(err: ProxyBinError) -> CheckRow {
    CheckRow {
        id: "proxy_bin".to_string(),
        status: CheckStatus::Fail,
        detail: err.to_string(),
        hint: Some(PROXY_BIN_INSTALL_HINT.to_string()),
    }
}

/// Assemble the doctor row after a successful path resolve, from the
/// version-probe result. Pure so tests can feed scripted probe outcomes
/// without spawning (`AGENTS.md`: name the pure fn).
fn check_row_for_proxy_bin_probe(
    path: &Path,
    probe: Result<String, ProxyVersionError>,
) -> CheckRow {
    match probe {
        Ok(version) => CheckRow {
            id: "proxy_bin".to_string(),
            status: CheckStatus::Pass,
            detail: format_proxy_bin_pass_detail(path, &version),
            hint: None,
        },
        Err(err) => CheckRow {
            id: "proxy_bin".to_string(),
            status: CheckStatus::Fail,
            detail: format_proxy_version_fail_detail(path, &err),
            hint: Some(err.hint().to_string()),
        },
    }
}

/// Pass `detail` shape: `{path} ({version_token})`
/// (`docs/doctor.v1.md`, "`proxy_bin` — hard").
fn format_proxy_bin_pass_detail(path: &Path, version: &str) -> String {
    format!("{} ({version})", path.display())
}

fn format_proxy_version_fail_detail(path: &Path, err: &ProxyVersionError) -> String {
    format!("{}: {err}", path.display())
}

const PROXY_BIN_INSTALL_HINT: &str = "Install cloud-sql-proxy, or set \"proxy_bin\" in \
                                      connections.json to its absolute path.";

const PROXY_BIN_IDENTITY_HINT: &str = "The resolved binary did not identify as cloud-sql-proxy. \
                                      Install cloud-sql-proxy, or set \"proxy_bin\" in \
                                      connections.json to the real proxy path.";

const PROXY_BIN_PROBE_HINT: &str = "Could not read a version from the resolved binary. \
                                   Install cloud-sql-proxy, or set \"proxy_bin\" in \
                                   connections.json to a working proxy path.";

/// Why the post-resolve version probe failed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProxyVersionError {
    /// `Command::spawn` failed (permissions, I/O, ...).
    SpawnFailed { detail: String },
    /// Child did not exit within [`PROXY_VERSION_TIMEOUT`].
    TimedOut,
    /// Child exited non-zero.
    NonZeroExit { status: String },
    /// Output was empty, wrong product, or missing a version token.
    IdentityMismatch { detail: String },
}

impl ProxyVersionError {
    fn hint(&self) -> &'static str {
        match self {
            ProxyVersionError::IdentityMismatch { .. } => PROXY_BIN_IDENTITY_HINT,
            ProxyVersionError::SpawnFailed { .. }
            | ProxyVersionError::TimedOut
            | ProxyVersionError::NonZeroExit { .. } => PROXY_BIN_PROBE_HINT,
        }
    }
}

impl std::fmt::Display for ProxyVersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyVersionError::SpawnFailed { detail } => {
                write!(f, "could not run version probe: {detail}")
            }
            ProxyVersionError::TimedOut => write!(
                f,
                "version probe timed out after {}s",
                PROXY_VERSION_TIMEOUT.as_secs()
            ),
            ProxyVersionError::NonZeroExit { status } => {
                write!(f, "version probe exited {status}")
            }
            ProxyVersionError::IdentityMismatch { detail } => write!(f, "{detail}"),
        }
    }
}

/// Run `path -v` with a short timeout and parse the version token.
///
/// Spawn stays here (I/O). Identity rules live in
/// [`parse_proxy_version_output`].
fn probe_proxy_version(path: &Path) -> Result<String, ProxyVersionError> {
    let output = run_proxy_version_command(path)?;
    if !output.status.success() {
        return Err(ProxyVersionError::NonZeroExit {
            status: output.status.to_string(),
        });
    }
    parse_proxy_version_output(&output.stdout, &output.stderr)
}

struct ProxyVersionOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_proxy_version_command(path: &Path) -> Result<ProxyVersionOutput, ProxyVersionError> {
    // Own process group so a timeout can kill shell helpers (e.g. `sleep`)
    // started by a wrapper script, not only the direct child PID.
    let mut child = spawn_version_probe(path)?;

    // Read pipes on side threads while we wait. That avoids a pipe-buffer
    // deadlock if a bad binary writes a lot before exiting, and keeps the
    // wait loop small.
    let mut stdout_pipe = child
        .stdout
        .take()
        .expect("stdout was piped on the version probe child");
    let mut stderr_pipe = child
        .stderr
        .take()
        .expect("stderr was piped on the version probe child");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + PROXY_VERSION_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    terminate_version_probe(&mut child);
                    // Drop readers after kill so join cannot hang forever.
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(ProxyVersionError::TimedOut);
                }
                std::thread::sleep(PROXY_VERSION_POLL);
            }
            Err(err) => {
                terminate_version_probe(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(ProxyVersionError::SpawnFailed {
                    detail: err.to_string(),
                });
            }
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    Ok(ProxyVersionOutput {
        status,
        stdout,
        stderr,
    })
}

/// Spawn `path -v` in its own process group.
///
/// Retries a few times on `ETXTBSY` ("Text file busy"): Linux can return
/// that when a script was just written and is still open for write in the
/// same process (common in unit tests that create a temp executable).
fn spawn_version_probe(path: &Path) -> Result<Child, ProxyVersionError> {
    const SPAWN_ATTEMPTS: usize = 10;
    let mut last_err = None;
    for attempt in 0..SPAWN_ATTEMPTS {
        match Command::new(path)
            .arg("-v")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(err) if err.raw_os_error() == Some(libc_etxtbsy()) => {
                last_err = Some(err);
                // Brief backoff; the writer should have closed the file.
                std::thread::sleep(Duration::from_millis(5 + attempt as u64 * 5));
            }
            Err(err) => {
                return Err(ProxyVersionError::SpawnFailed {
                    detail: err.to_string(),
                });
            }
        }
    }
    Err(ProxyVersionError::SpawnFailed {
        detail: last_err
            .map(|err| err.to_string())
            .unwrap_or_else(|| "spawn failed after ETXTBSY retries".to_string()),
    })
}

/// `ETXTBSY` from the host libc. Kept as a tiny helper so the spawn loop
/// does not open-code the number.
fn libc_etxtbsy() -> i32 {
    // Linux ETXTBSY is 26. rustix/libc would also work; this avoids a new
    // direct libc dependency for one constant.
    26
}

/// Kill the version-probe process group, then reap the direct child.
///
/// The child was started with [`CommandExt::process_group`]`(0)`, so its
/// PID is also the process-group id. Group kill covers helper processes
/// a shell script may start (for example `sleep`).
fn terminate_version_probe(child: &mut Child) {
    if let Some(pid) = Pid::from_raw(child.id() as i32) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Parse `cloud-sql-proxy -v` output into the version token.
///
/// Accepts the real line shape:
/// `cloud-sql-proxy version 2.25.2+linux.amd64`
///
/// Looks at stdout first, then stderr, so a binary that writes the line to
/// either stream still passes. Pure: no process spawn.
fn parse_proxy_version_output(stdout: &[u8], stderr: &[u8]) -> Result<String, ProxyVersionError> {
    if let Some(version) = version_token_from_bytes(stdout) {
        return Ok(version);
    }
    if let Some(version) = version_token_from_bytes(stderr) {
        return Ok(version);
    }
    Err(ProxyVersionError::IdentityMismatch {
        detail: "output is not a cloud-sql-proxy version line".to_string(),
    })
}

/// Pull the version token from text that contains
/// `cloud-sql-proxy version <token>`.
fn version_token_from_bytes(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines() {
        if let Some(token) = version_token_from_line(line) {
            return Some(token);
        }
    }
    // Also accept a single-line blob without a trailing newline.
    version_token_from_line(text.trim())
}

fn version_token_from_line(line: &str) -> Option<String> {
    const MARKER: &str = "cloud-sql-proxy version ";
    let line = line.trim();
    let rest = line.strip_prefix(MARKER)?;
    let token = rest.split_whitespace().next().unwrap_or("");
    if token_looks_like_version(token) {
        Some(token.to_string())
    } else {
        None
    }
}

/// A version-ish token has at least one ASCII digit (covers
/// `2.25.2+linux.amd64` and plain `2.25.2`).
fn token_looks_like_version(token: &str) -> bool {
    !token.is_empty() && token.chars().any(|c| c.is_ascii_digit())
}

/// [`resolve_proxy_bin`]'s search, taking an explicit `PATH` value so tests
/// never touch the process environment.
///
/// An absolute `name` must itself be an executable file. A bare `name` is
/// searched on `PATH` like a shell would: the first executable match wins.
/// If a bare name matches a file on `PATH` that exists but is not
/// executable, that is reported as [`ProxyBinError::NotExecutable`] (the
/// same distinction the absolute-path branch already makes) rather than a
/// generic "not found" — a `+x`-only problem should not read like a
/// missing binary.
fn resolve_bin(name: &str, path_env: &OsStr) -> Result<PathBuf, ProxyBinError> {
    let candidate = Path::new(name);
    if candidate.is_absolute() {
        return require_executable_file(candidate).map(|()| candidate.to_path_buf());
    }

    let mut non_executable_match = None;
    for dir in std::env::split_paths(path_env) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Ok(candidate);
        }
        if non_executable_match.is_none() && candidate.is_file() {
            non_executable_match = Some(candidate);
        }
    }

    match non_executable_match {
        Some(path) => Err(ProxyBinError::NotExecutable { path }),
        None => Err(ProxyBinError::NotFound {
            name: name.to_string(),
        }),
    }
}

fn require_executable_file(path: &Path) -> Result<(), ProxyBinError> {
    if is_executable_file(path) {
        Ok(())
    } else {
        Err(ProxyBinError::NotExecutable {
            path: path.to_path_buf(),
        })
    }
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Why [`resolve_proxy_bin`] could not find a usable binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProxyBinError {
    /// A bare name was not found on any `PATH` entry.
    NotFound { name: String },
    /// An absolute path exists but is not an executable file.
    NotExecutable { path: PathBuf },
}

impl std::fmt::Display for ProxyBinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyBinError::NotFound { name } => write!(f, "{name} not found on PATH"),
            ProxyBinError::NotExecutable { path } => {
                write!(f, "{} is not an executable file", path.display())
            }
        }
    }
}

impl std::error::Error for ProxyBinError {}

const ADC_RELATIVE_PATH: &str = ".config/gcloud/application_default_credentials.json";

/// Application Default Credentials presence
/// (`docs/doctor.v1.md`, "`adc` — hard (local only)"): no network, no
/// `gcloud` invocation, no token fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdcStatus {
    /// Whether a readable credentials file was found.
    pub(crate) present: bool,
    /// The path checked, if one could be determined.
    pub(crate) path: Option<PathBuf>,
    /// Whether `GOOGLE_APPLICATION_CREDENTIALS` decided `path` (priority 1)
    /// rather than the default file under `HOME` (priority 2).
    pub(crate) gac_env_set: bool,
}

/// Read `GOOGLE_APPLICATION_CREDENTIALS` and `HOME` from the process
/// environment and resolve ADC presence, for **start**'s env forwarding.
/// Mutate (#43) will call this directly; [`adc_check`] below already
/// makes it reachable.
pub(crate) fn adc_status() -> AdcStatus {
    let gac_env = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_adc_status(home.as_deref(), gac_env.as_deref())
}

/// Doctor's `adc` row, built on [`adc_status`].
pub(crate) fn adc_check() -> CheckRow {
    check_row_for_adc(adc_status())
}

fn check_row_for_adc(status: AdcStatus) -> CheckRow {
    let id = "adc".to_string();
    if status.present {
        // `present` is only ever true alongside a path (see
        // `resolve_adc_status`); `display_path` still handles `None` so
        // this function cannot panic.
        return CheckRow {
            id,
            status: CheckStatus::Pass,
            detail: display_path(status.path.as_deref()),
            hint: None,
        };
    }

    CheckRow {
        id,
        status: CheckStatus::Fail,
        detail: missing_adc_detail(&status),
        hint: Some(ADC_HINT.to_string()),
    }
}

/// [`adc_status`]'s resolution, taking `HOME` and
/// `GOOGLE_APPLICATION_CREDENTIALS` explicitly so tests never touch the
/// process environment.
fn resolve_adc_status(home: Option<&Path>, gac_env: Option<&Path>) -> AdcStatus {
    if let Some(path) = gac_env {
        return AdcStatus {
            present: is_readable_file(path),
            path: Some(path.to_path_buf()),
            gac_env_set: true,
        };
    }

    let default_path = home.map(|home| home.join(ADC_RELATIVE_PATH));
    AdcStatus {
        present: default_path.as_deref().is_some_and(is_readable_file),
        path: default_path,
        gac_env_set: false,
    }
}

fn is_readable_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => std::fs::File::open(path).is_ok(),
        _ => false,
    }
}

const ADC_HINT: &str = "Run: gcloud auth application-default login \u{2014} see \
                         https://cloud.google.com/docs/authentication/provide-credentials-adc";

fn display_path(path: Option<&Path>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn missing_adc_detail(status: &AdcStatus) -> String {
    match (&status.path, status.gac_env_set) {
        (Some(path), true) => format!(
            "GOOGLE_APPLICATION_CREDENTIALS is set to {} but that file is missing or unreadable",
            path.display()
        ),
        (Some(path), false) => format!(
            "no Application Default Credentials file at {} ({}) and \
             GOOGLE_APPLICATION_CREDENTIALS is unset",
            path.display(),
            existing_file_reason(path)
        ),
        (None, _) => {
            "could not determine a home directory to look for Application Default Credentials"
                .to_string()
        }
    }
}

/// Whether a not-present ADC `path` is missing outright or exists but
/// failed [`is_readable_file`] (`docs/doctor.v1.md`, "`adc` — hard": the
/// file must exist **and** be readable). Only called for a `path` that is
/// already known not to be present, so the wording is always a negative
/// one of these two.
fn existing_file_reason(path: &Path) -> &'static str {
    let exists = std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file());
    if exists {
        "exists but is unreadable"
    } else {
        "does not exist"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    /// A directory under the system temp dir, removed on drop. Every test
    /// gets its own directory so parallel tests never share fixture files.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "cloud-sql-tracker-env-test-{label}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir for test fixture");
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_file(dir: &Path, name: &str, contents: &[u8], mode: u32) -> PathBuf {
        // Write to a sibling temp name, fsync, then rename into place.
        // That avoids Linux ETXTBSY when a test spawns the file immediately
        // after create (write handle must be fully closed first).
        let path = dir.join(name);
        let tmp = dir.join(format!(".{name}.tmp"));
        {
            use std::io::Write;
            let mut file = fs::File::create(&tmp).expect("create test fixture temp file");
            file.write_all(contents).expect("write test fixture file");
            file.sync_all().expect("fsync test fixture file");
        }
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))
            .expect("set test fixture file permissions");
        fs::rename(&tmp, &path).expect("rename test fixture into place");
        path
    }

    fn write_executable(dir: &Path, name: &str) -> PathBuf {
        write_file(dir, name, b"#!/bin/sh\n", 0o755)
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        write_file(dir, name, body.as_bytes(), 0o755)
    }

    fn write_non_executable(dir: &Path, name: &str) -> PathBuf {
        write_file(dir, name, b"not a binary\n", 0o644)
    }

    fn write_default_adc_file(home: &Path) -> PathBuf {
        let path = home.join(ADC_RELATIVE_PATH);
        fs::create_dir_all(path.parent().unwrap()).expect("create fixture ADC parent dir");
        fs::write(&path, b"{}").expect("write fixture ADC file");
        path
    }

    fn path_env(dirs: &[&Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs).expect("join test PATH fixture")
    }

    #[test]
    fn resolve_bin_finds_an_executable_bare_name() {
        let dir = TempDir::new("found");
        let bin = write_executable(dir.path(), "cloud-sql-proxy");

        let resolved = resolve_bin("cloud-sql-proxy", &path_env(&[dir.path()]))
            .expect("cloud-sql-proxy is executable on PATH");

        assert_eq!(resolved, bin);
    }

    #[test]
    fn resolve_bin_skips_a_non_executable_match_before_the_real_one() {
        let wrong = TempDir::new("skip-wrong");
        let right = TempDir::new("skip-right");
        write_non_executable(wrong.path(), "cloud-sql-proxy");
        let bin = write_executable(right.path(), "cloud-sql-proxy");

        let resolved = resolve_bin("cloud-sql-proxy", &path_env(&[wrong.path(), right.path()]))
            .expect("the second PATH entry is executable");

        assert_eq!(resolved, bin);
    }

    #[test]
    fn resolve_bin_rejects_a_name_missing_from_every_entry() {
        let dir = TempDir::new("missing");

        let err = resolve_bin("cloud-sql-proxy", &path_env(&[dir.path()]))
            .expect_err("no PATH entry has this name");

        assert_eq!(
            err,
            ProxyBinError::NotFound {
                name: "cloud-sql-proxy".to_string()
            }
        );
    }

    #[test]
    fn resolve_bin_reports_not_executable_when_the_only_match_lacks_the_exec_bit() {
        let dir = TempDir::new("path-not-executable");
        let bin = write_non_executable(dir.path(), "cloud-sql-proxy");

        let err = resolve_bin("cloud-sql-proxy", &path_env(&[dir.path()]))
            .expect_err("the only PATH match is not executable");

        assert_eq!(err, ProxyBinError::NotExecutable { path: bin });
    }

    #[test]
    fn resolve_bin_accepts_an_absolute_executable_path() {
        let dir = TempDir::new("absolute-ok");
        let bin = write_executable(dir.path(), "cloud-sql-proxy");

        let resolved = resolve_bin(bin.to_str().unwrap(), &path_env(&[]))
            .expect("absolute path is executable");

        assert_eq!(resolved, bin);
    }

    #[test]
    fn resolve_bin_rejects_an_absolute_non_executable_path() {
        let dir = TempDir::new("absolute-bad");
        let bin = write_non_executable(dir.path(), "cloud-sql-proxy");

        let err = resolve_bin(bin.to_str().unwrap(), &path_env(&[]))
            .expect_err("absolute path is not executable");

        assert_eq!(err, ProxyBinError::NotExecutable { path: bin });
    }

    #[test]
    fn parse_proxy_version_output_reads_the_real_version_line() {
        let version =
            parse_proxy_version_output(b"cloud-sql-proxy version 2.25.2+linux.amd64\n", b"")
                .expect("real cloud-sql-proxy -v line must parse");

        assert_eq!(version, "2.25.2+linux.amd64");
    }

    #[test]
    fn parse_proxy_version_output_falls_back_to_stderr() {
        let version = parse_proxy_version_output(b"", b"cloud-sql-proxy version 2.0.0\n")
            .expect("version line on stderr must parse");

        assert_eq!(version, "2.0.0");
    }

    #[test]
    fn parse_proxy_version_output_rejects_empty_output() {
        let err =
            parse_proxy_version_output(b"", b"").expect_err("empty output is not a version line");

        assert!(matches!(err, ProxyVersionError::IdentityMismatch { .. }));
    }

    #[test]
    fn parse_proxy_version_output_rejects_unrelated_help_text() {
        let err = parse_proxy_version_output(b"Usage: some-other-tool [flags]\n", b"")
            .expect_err("unrelated help text is not cloud-sql-proxy");

        assert!(matches!(err, ProxyVersionError::IdentityMismatch { .. }));
    }

    #[test]
    fn parse_proxy_version_output_rejects_a_line_without_a_version_token() {
        let err = parse_proxy_version_output(b"cloud-sql-proxy version \n", b"")
            .expect_err("missing version token must fail");

        assert!(matches!(err, ProxyVersionError::IdentityMismatch { .. }));
    }

    #[test]
    fn parse_proxy_version_output_rejects_a_wrong_product_name() {
        let err = parse_proxy_version_output(b"other-proxy version 1.2.3\n", b"")
            .expect_err("wrong product name must fail");

        assert!(matches!(err, ProxyVersionError::IdentityMismatch { .. }));
    }

    #[test]
    fn check_row_for_proxy_bin_probe_passes_with_path_and_version() {
        let path = PathBuf::from("/usr/bin/cloud-sql-proxy");

        let row = check_row_for_proxy_bin_probe(&path, Ok("2.25.2+linux.amd64".to_string()));

        assert_eq!(row.id, "proxy_bin");
        assert_eq!(row.status, CheckStatus::Pass);
        assert_eq!(row.detail, "/usr/bin/cloud-sql-proxy (2.25.2+linux.amd64)");
        assert_eq!(row.hint, None);
    }

    #[test]
    fn check_row_for_proxy_bin_probe_fails_on_identity_mismatch() {
        let path = PathBuf::from("/usr/bin/not-proxy");

        let row = check_row_for_proxy_bin_probe(
            &path,
            Err(ProxyVersionError::IdentityMismatch {
                detail: "output is not a cloud-sql-proxy version line".to_string(),
            }),
        );

        assert_eq!(row.id, "proxy_bin");
        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains("/usr/bin/not-proxy"));
        assert!(row.hint.is_some());
    }

    #[test]
    fn proxy_bin_check_fails_with_a_hint_when_not_found() {
        let row = check_row_for_proxy_bin_resolve_error(ProxyBinError::NotFound {
            name: "cloud-sql-proxy".to_string(),
        });

        assert_eq!(row.id, "proxy_bin");
        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains("cloud-sql-proxy"));
        assert!(row.hint.is_some());
    }

    #[test]
    fn probe_proxy_version_passes_for_a_script_that_prints_the_real_line() {
        let dir = TempDir::new("probe-ok");
        let bin = write_script(
            dir.path(),
            "cloud-sql-proxy",
            "#!/bin/sh\necho 'cloud-sql-proxy version 2.25.2+linux.amd64'\n",
        );

        let row = check_row_for_proxy_bin_probe(&bin, probe_proxy_version(&bin));

        assert_eq!(row.status, CheckStatus::Pass);
        assert_eq!(
            row.detail,
            format!("{} (2.25.2+linux.amd64)", bin.display())
        );
        assert_eq!(row.hint, None);
    }

    #[test]
    fn probe_proxy_version_fails_for_a_script_with_wrong_identity() {
        let dir = TempDir::new("probe-wrong");
        let bin = write_script(
            dir.path(),
            "cloud-sql-proxy",
            "#!/bin/sh\necho 'hello from some other tool'\n",
        );

        let row = check_row_for_proxy_bin_probe(&bin, probe_proxy_version(&bin));

        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains(&bin.display().to_string()));
        assert!(row.hint.is_some());
    }

    #[test]
    fn probe_proxy_version_fails_for_a_script_that_exits_non_zero() {
        let dir = TempDir::new("probe-exit");
        let bin = write_script(
            dir.path(),
            "cloud-sql-proxy",
            "#!/bin/sh\necho 'cloud-sql-proxy version 2.25.2+linux.amd64'\nexit 1\n",
        );

        let row = check_row_for_proxy_bin_probe(&bin, probe_proxy_version(&bin));

        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains("exited"));
        assert!(row.hint.is_some());
    }

    #[test]
    fn probe_proxy_version_fails_when_the_script_hangs_past_the_timeout() {
        let dir = TempDir::new("probe-hang");
        // Sleep longer than PROXY_VERSION_TIMEOUT so the probe must kill it.
        let bin = write_script(dir.path(), "cloud-sql-proxy", "#!/bin/sh\nsleep 30\n");

        let started = Instant::now();
        let row = check_row_for_proxy_bin_probe(&bin, probe_proxy_version(&bin));
        let elapsed = started.elapsed();

        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains("timed out"));
        assert!(row.hint.is_some());
        // Bound the wait so a hang in the probe itself fails the test suite.
        assert!(
            elapsed < Duration::from_secs(5),
            "version probe took too long: {elapsed:?}"
        );
    }

    #[test]
    fn resolve_adc_status_prefers_the_env_var_over_the_default_file() {
        let dir = TempDir::new("adc-env-priority");
        write_default_adc_file(dir.path());
        let gac_file = write_file(dir.path(), "service-account.json", b"{}", 0o600);

        let status = resolve_adc_status(Some(dir.path()), Some(&gac_file));

        assert!(status.present);
        assert_eq!(status.path, Some(gac_file));
        assert!(status.gac_env_set);
    }

    #[test]
    fn resolve_adc_status_fails_when_the_env_var_points_at_a_missing_file() {
        let dir = TempDir::new("adc-env-missing");
        let missing = dir.path().join("does-not-exist.json");

        let status = resolve_adc_status(Some(dir.path()), Some(&missing));

        assert!(!status.present);
        assert_eq!(status.path, Some(missing));
        assert!(status.gac_env_set);
    }

    #[test]
    fn resolve_adc_status_falls_back_to_the_default_file_under_home() {
        let dir = TempDir::new("adc-default-present");
        let default_file = write_default_adc_file(dir.path());

        let status = resolve_adc_status(Some(dir.path()), None);

        assert!(status.present);
        assert_eq!(status.path, Some(default_file));
        assert!(!status.gac_env_set);
    }

    #[test]
    fn resolve_adc_status_reports_missing_when_the_default_file_is_absent() {
        let dir = TempDir::new("adc-default-missing");

        let status = resolve_adc_status(Some(dir.path()), None);

        assert!(!status.present);
        assert_eq!(status.path, Some(dir.path().join(ADC_RELATIVE_PATH)));
        assert!(!status.gac_env_set);
    }

    #[test]
    fn resolve_adc_status_reports_absent_when_the_default_file_exists_but_is_unreadable() {
        let dir = TempDir::new("adc-default-restricted");
        let path = write_default_adc_file(dir.path());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000))
            .expect("remove read permission from fixture ADC file");

        let status = resolve_adc_status(Some(dir.path()), None);

        assert!(!status.present);
        assert_eq!(status.path, Some(path));
        assert!(!status.gac_env_set);
    }

    #[test]
    fn adc_check_fail_detail_says_unreadable_for_an_existing_but_unreadable_default_file() {
        let dir = TempDir::new("adc-check-restricted");
        let path = write_default_adc_file(dir.path());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000))
            .expect("remove read permission from fixture ADC file");

        let row = check_row_for_adc(resolve_adc_status(Some(dir.path()), None));

        assert_eq!(row.status, CheckStatus::Fail);
        assert!(
            row.detail.contains("unreadable"),
            "detail should say the file is unreadable, got: {}",
            row.detail
        );
    }

    #[test]
    fn resolve_adc_status_without_a_home_directory_has_no_path() {
        let status = resolve_adc_status(None, None);

        assert!(!status.present);
        assert_eq!(status.path, None);
        assert!(!status.gac_env_set);
    }

    #[test]
    fn adc_check_passes_with_the_credentials_path_as_detail() {
        let dir = TempDir::new("adc-check-pass");
        let default_file = write_default_adc_file(dir.path());

        let row = check_row_for_adc(resolve_adc_status(Some(dir.path()), None));

        assert_eq!(row.id, "adc");
        assert_eq!(row.status, CheckStatus::Pass);
        assert_eq!(row.detail, default_file.display().to_string());
        assert_eq!(row.hint, None);
    }

    #[test]
    fn adc_check_fails_with_a_google_adc_hint_when_missing() {
        let dir = TempDir::new("adc-check-fail");

        let row = check_row_for_adc(resolve_adc_status(Some(dir.path()), None));

        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains("Application Default Credentials"));
        let hint = row.hint.expect("fail must carry a hint");
        assert!(hint.contains("gcloud auth application-default login"));
        assert!(
            hint.contains("https://cloud.google.com/docs/authentication/provide-credentials-adc")
        );
    }
}
