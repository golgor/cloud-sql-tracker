//! `cloud-sql-proxy -v` spawn, timeout, and identity parsing for doctor's
//! `proxy_bin` row (`docs/doctor.v1.md`, "`proxy_bin` — hard").
//!
//! Split out of `env` (`docs/modules.v1.md`, "`env` — proxy binary + ADC")
//! because this file mixes process spawn, timeouts, and thread joins with
//! pure parsing; `env`'s public seam (`proxy_bin_check`) is unchanged by
//! this split.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{kill_process_group, Pid, Signal};

/// How long doctor waits for `cloud-sql-proxy -v` before failing the check
/// (`docs/doctor.v1.md`, "`proxy_bin` — hard").
const PROXY_VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// Poll interval while waiting for the version child to exit.
const PROXY_VERSION_POLL: Duration = Duration::from_millis(20);

const PROXY_BIN_IDENTITY_HINT: &str = "The resolved binary did not identify as cloud-sql-proxy. \
                                        Install cloud-sql-proxy, or set \"proxy_bin\" in \
                                        connections.json to the real proxy path.";

const PROXY_BIN_PROBE_HINT: &str = "Could not read a version from the resolved binary. \
                                     Install cloud-sql-proxy, or set \"proxy_bin\" in \
                                     connections.json to a working proxy path.";

/// Why the post-resolve version probe failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProxyVersionError {
    /// `Command::spawn` failed (permissions, I/O, ...).
    SpawnFailed { detail: String },
    /// `Child::try_wait` returned an OS error while polling for exit.
    WaitFailed { detail: String },
    /// Child did not exit within [`PROXY_VERSION_TIMEOUT`].
    TimedOut,
    /// Child exited non-zero.
    NonZeroExit { status: String },
    /// Output was empty, wrong product, or missing a version token.
    IdentityMismatch { detail: String },
}

impl ProxyVersionError {
    pub(super) fn hint(&self) -> &'static str {
        match self {
            ProxyVersionError::IdentityMismatch { .. } => PROXY_BIN_IDENTITY_HINT,
            ProxyVersionError::SpawnFailed { .. }
            | ProxyVersionError::WaitFailed { .. }
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
            ProxyVersionError::WaitFailed { detail } => {
                write!(f, "could not wait for version probe: {detail}")
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
pub(super) fn probe_proxy_version(path: &Path) -> Result<String, ProxyVersionError> {
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
    // wait loop small. A read error (pipe closed early, etc.) is dropped on
    // purpose: the reader still returns whatever bytes it collected, and a
    // truncated or empty read fails identity parsing on its own.
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
                return Err(ProxyVersionError::WaitFailed {
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
fn spawn_version_probe(path: &Path) -> Result<Child, ProxyVersionError> {
    Command::new(path)
        .arg("-v")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|err| ProxyVersionError::SpawnFailed {
            detail: err.to_string(),
        })
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
/// The marker may appear anywhere in the line (a log-prefixed line still
/// matches), not only at its start. Looks at stdout first, then stderr, so
/// a binary that writes the line to either stream still passes. Pure: no
/// process spawn.
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
/// `cloud-sql-proxy version <token>` anywhere on one of its lines.
fn version_token_from_bytes(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    text.lines().find_map(version_token_from_line)
}

fn version_token_from_line(line: &str) -> Option<String> {
    const MARKER: &str = "cloud-sql-proxy version ";
    let (_, rest) = line.split_once(MARKER)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
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

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        {
            use std::io::Write;
            let mut file = fs::File::create(&path).expect("create test fixture file");
            file.write_all(body.as_bytes())
                .expect("write test fixture file");
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("set test fixture file permissions");
        path
    }

    /// Run [`probe_proxy_version`] for a **test** fixture, retrying a
    /// couple of times only on `ETXTBSY` ("Text file busy").
    ///
    /// Test-only, and deliberately not in production: `resolve_proxy_bin`
    /// only ever resolves an operator's already-installed
    /// `cloud-sql-proxy`; it never writes the file it is about to exec, so
    /// production has nothing to race. This suite's own tests measurably
    /// do — several `#[test]` fns across this crate (`journal`'s fake
    /// `journalctl` fixtures included) write a fresh script and exec it,
    /// all as threads in one `cargo test` binary. Measured on this repo:
    /// `main`'s existing test suite (no version-probe tests) is 10/10
    /// clean at default parallelism; adding these spawn-heavy tests to the
    /// binary made `ETXTBSY` appear intermittently even when a fixture is
    /// written once and never rewritten, and even though scanning every
    /// process's open file descriptors at the moment of failure found no
    /// lingering writer — i.e. this is transient fork/exec contention
    /// across threads under a heavily multi-threaded test binary, not a
    /// leftover write handle on any one fixture. Full measurements:
    /// [issue #82](https://github.com/golgor/cloud-sql-tracker/issues/82).
    /// A short, bounded, error-specific retry here is the smallest fix
    /// that matches the actual, transient cause.
    fn probe_proxy_version_for_test(path: &Path) -> Result<String, ProxyVersionError> {
        const ATTEMPTS: usize = 3;
        const BACKOFF: Duration = Duration::from_millis(5);
        let mut last = None;
        for _ in 0..ATTEMPTS {
            match probe_proxy_version(path) {
                Err(ProxyVersionError::SpawnFailed { detail })
                    if detail.contains("Text file busy") =>
                {
                    last = Some(ProxyVersionError::SpawnFailed { detail });
                    std::thread::sleep(BACKOFF);
                }
                other => return other,
            }
        }
        Err(last.expect("loop runs at least once"))
    }

    #[test]
    fn parse_proxy_version_output_reads_the_real_version_line() {
        let version =
            parse_proxy_version_output(b"cloud-sql-proxy version 2.25.2+linux.amd64\n", b"")
                .expect("real cloud-sql-proxy -v line must parse");

        assert_eq!(version, "2.25.2+linux.amd64");
    }

    #[test]
    fn parse_proxy_version_output_reads_a_line_with_a_log_prefix() {
        let version = parse_proxy_version_output(
            b"2026/08/23 15:59:27 cloud-sql-proxy version 2.25.2+linux.amd64\n",
            b"",
        )
        .expect("a decorated line still contains the version marker");

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
    fn probe_proxy_version_passes_for_a_script_that_prints_the_real_line() {
        let dir = TempDir::new("probe-ok");
        let bin = write_script(
            dir.path(),
            "cloud-sql-proxy",
            "#!/bin/sh\necho 'cloud-sql-proxy version 2.25.2+linux.amd64'\n",
        );

        let version = probe_proxy_version_for_test(&bin)
            .expect("script prints a real cloud-sql-proxy -v line");

        assert_eq!(version, "2.25.2+linux.amd64");
    }

    #[test]
    fn probe_proxy_version_fails_for_a_script_with_wrong_identity() {
        let dir = TempDir::new("probe-wrong");
        let bin = write_script(
            dir.path(),
            "cloud-sql-proxy",
            "#!/bin/sh\necho 'hello from some other tool'\n",
        );

        let err = probe_proxy_version_for_test(&bin).expect_err("wrong identity must fail");

        assert!(matches!(err, ProxyVersionError::IdentityMismatch { .. }));
    }

    #[test]
    fn probe_proxy_version_fails_for_a_script_that_exits_non_zero() {
        let dir = TempDir::new("probe-exit");
        let bin = write_script(
            dir.path(),
            "cloud-sql-proxy",
            "#!/bin/sh\necho 'cloud-sql-proxy version 2.25.2+linux.amd64'\nexit 1\n",
        );

        let err = probe_proxy_version_for_test(&bin).expect_err("non-zero exit must fail");

        assert!(matches!(err, ProxyVersionError::NonZeroExit { .. }));
    }

    #[test]
    fn probe_proxy_version_fails_when_the_script_hangs_past_the_timeout() {
        let dir = TempDir::new("probe-hang");
        // Sleep longer than PROXY_VERSION_TIMEOUT so the probe must kill it.
        let bin = write_script(dir.path(), "cloud-sql-proxy", "#!/bin/sh\nsleep 30\n");

        let started = Instant::now();
        let err = probe_proxy_version_for_test(&bin).expect_err("a hanging script must time out");
        let elapsed = started.elapsed();

        assert!(matches!(err, ProxyVersionError::TimedOut));
        // Bound the wait so a hang in the probe itself fails the test suite.
        assert!(
            elapsed < Duration::from_secs(5),
            "version probe took too long: {elapsed:?}"
        );
    }
}
