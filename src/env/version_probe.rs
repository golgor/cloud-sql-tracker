//! `cloud-sql-proxy -v` spawn, timeout, and identity parsing for doctor's
//! `proxy_bin` row (`docs/doctor.v1.md`, "`proxy_bin` — hard").
//!
//! Split out of `env` (`docs/modules.v1.md`, "`env` — proxy binary + ADC")
//! because this file mixes process spawn, timeouts, and thread joins with
//! pure parsing; `env`'s public seam (`proxy_bin_check`) is unchanged by
//! this split.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use rustix::process::{kill_process_group, Pid, Signal};

/// How long doctor waits for `cloud-sql-proxy -v` before failing the check
/// (`docs/doctor.v1.md`, "`proxy_bin` — hard").
///
/// Why this exists at all: plain `Command::output()` blocks until the
/// child exits, with no way to give up. `proxy_bin` is an
/// operator-configured path, and the Omarchy plugin runs doctor on every
/// panel open (plugin issue #31). A misconfigured binary that waits on
/// stdin, or never exits, would hang the panel forever. The probe needs a
/// hard ceiling so a bad `proxy_bin` fails doctor instead of freezing it.
const PROXY_VERSION_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// `Child::wait_with_output` returned an OS error, or the waiter
    /// thread ended without sending a result.
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
    let child = spawn_version_probe(path)?;

    // Save the pid before the child moves into the waiter thread below.
    // The timeout path needs it to kill the process group; by then the
    // `Child` value itself belongs to that thread, not this one.
    let pid = child.id();

    // Rust's std has no "wait for a child, but give up after N seconds".
    // `Child::wait` blocks forever, and `Child::try_wait` never blocks at
    // all. So we block in a side thread and bound the *channel receive*
    // on this thread instead of bounding the wait itself.
    //
    // That thread calls `wait_with_output`, not a bare `wait`, so it also
    // drains stdout and stderr while it waits. Draining matters: a pipe
    // buffer is about 64 KB, so a binary that writes more than that
    // blocks on its own `write()` call and never exits. Without draining
    // concurrently with the wait, a merely chatty binary would look
    // exactly like a hang. This is also why one thread is enough here,
    // where the previous version needed two hand-rolled reader threads:
    // `wait_with_output` already does that draining for us.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(PROXY_VERSION_TIMEOUT) {
        Ok(Ok(output)) => Ok(ProxyVersionOutput {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        }),
        Ok(Err(err)) => Err(ProxyVersionError::WaitFailed {
            detail: err.to_string(),
        }),
        Err(RecvTimeoutError::Timeout) => {
            terminate_version_probe(pid);
            Err(ProxyVersionError::TimedOut)
        }
        // The waiter thread panicked or dropped its sender without
        // sending — not expected, but do not panic the caller over it.
        Err(RecvTimeoutError::Disconnected) => Err(ProxyVersionError::WaitFailed {
            detail: "version probe wait thread ended without a result".to_string(),
        }),
    }
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

/// Kill the version-probe process group on a timeout.
///
/// The child was started with [`CommandExt::process_group`]`(0)`, so its
/// PID is also the process-group id. Killing the group, not just the
/// direct child, matters because a wrapper script can start helpers (for
/// example `sleep`); killing only the direct child would leave those
/// helpers running and still holding the pipes open.
///
/// This only sends the signal. The waiter thread still owns the `Child`
/// value and reaps it via its own `wait_with_output` call once the kill
/// lands, so no separate `wait()` is needed here.
fn terminate_version_probe(pid: u32) {
    if let Some(pid) = Pid::from_raw(pid as i32) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
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

const MAX_PROXY_VERSION_TOKEN_LEN: usize = 128;

/// A version-ish token has at least one ASCII digit, printable ASCII, and is <= 128 bytes
/// (covers `2.25.2+linux.amd64` and plain `2.25.2`).
fn token_looks_like_version(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_PROXY_VERSION_TOKEN_LEN
        && token.bytes().all(|b| (0x20..=0x7E).contains(&b))
        && token.chars().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

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

    #[test]
    fn parse_proxy_version_output_accepts_128_byte_token() {
        // "v1" (2) + "0".repeat(126) = 128 bytes token
        let token = format!("v1{}", "0".repeat(126));
        assert_eq!(token.len(), 128);
        let line = format!("cloud-sql-proxy version {token}\n");
        let version = parse_proxy_version_output(line.as_bytes(), b"")
            .expect("128-byte version token must parse");
        assert_eq!(version, token);
    }

    #[test]
    fn parse_proxy_version_output_rejects_129_byte_token() {
        let token = format!("v1{}", "0".repeat(127));
        assert_eq!(token.len(), 129);
        let line = format!("cloud-sql-proxy version {token}\n");
        let err = parse_proxy_version_output(line.as_bytes(), b"")
            .expect_err("129-byte version token must reject");
        assert!(matches!(err, ProxyVersionError::IdentityMismatch { .. }));
    }
}
