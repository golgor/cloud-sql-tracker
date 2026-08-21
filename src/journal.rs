//! Journal dump for `logs`, and the doctor `journal_user` smoke check.
//!
//! See `docs/modules.v1.md` ("journal — logs + doctor smoke") and
//! `docs/logs.v1.md` for the frozen shapes this module implements.
//! `journalctl` is shelled out to — the interface we want, per
//! `docs/research/journalctl-logs.md` — never a Rust journal-reading crate.
//!
//! This ticket ([#41](https://github.com/golgor/cloud-sql-tracker/issues/41))
//! lands the adapter ahead of its caller, `commands::logs` / `commands::doctor`
//! (#44). Until that lands, nothing outside this module's own tests calls
//! these `pub(crate)` functions, so `rustc`/clippy see them as dead code
//! under `-D warnings`. Remove this `allow` once a caller lands.
//!
//! Hint wording and which stream (stdout vs stderr) a hint goes on are
//! `cli`'s job, not this module's (`docs/modules.v1.md`): `dump` only
//! reports whether journalctl produced any lines, never a message meant
//! for a human. Likewise this module never chooses a process exit code.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::process::{Command, Output};

use crate::model::{CheckRow, CheckStatus, UnitName};

const JOURNALCTL_BIN: &str = "journalctl";

/// A journal read, per `docs/logs.v1.md` ("Empty journal / never started").
///
/// `Bytes` carries `journalctl`'s stdout **unchanged** — `cli` writes it to
/// stdout as-is. `Empty` means journalctl succeeded but matched no lines
/// (never started, vacuumed, or a unit that never existed); `cli` decides
/// the hint text and prints it on stderr, not this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dump {
    Empty,
    Bytes(Vec<u8>),
}

/// Why a journal read or smoke check could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JournalError {
    /// `journalctl` is not on `PATH`.
    NotFound,
    /// `journalctl` ran but exited non-zero (unusable user journal, bad
    /// flags, permission issue, ...). `detail` is its stderr, or a
    /// fallback describing the exit status when stderr was empty.
    Failed { detail: String },
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalError::NotFound => write!(f, "journalctl not found on PATH"),
            JournalError::Failed { detail } => write!(f, "journalctl failed: {detail}"),
        }
    }
}

impl std::error::Error for JournalError {}

/// Read `unit`'s user-journal lines, most recent `lines` at most
/// (`docs/logs.v1.md`, normative argv template).
///
/// `journalctl --user --unit=<unit> --no-pager --quiet -n <lines>
/// -o short-iso`, captured rather than inherited, so an empty match can be
/// told apart from real lines.
pub(crate) fn dump(unit: &UnitName, lines: u32) -> Result<Dump, JournalError> {
    let unit_arg = format!("--unit={}", unit.as_str());
    let lines_arg = lines.to_string();
    let output = run_journalctl(&[
        "--user",
        &unit_arg,
        "--no-pager",
        "--quiet",
        "-n",
        &lines_arg,
        "-o",
        "short-iso",
    ])?;
    Ok(dump_from_output(output))
}

/// `docs/logs.v1.md`: empty means journalctl exited 0 **and** captured
/// stdout has no non-whitespace bytes. Anything else is real lines, kept
/// byte-for-byte.
fn dump_from_output(output: Output) -> Dump {
    if output.stdout.iter().all(u8::is_ascii_whitespace) {
        Dump::Empty
    } else {
        Dump::Bytes(output.stdout)
    }
}

/// Doctor's `journal_user` row (`docs/doctor.v1.md`, "`journal_user` —
/// hard"): a smoke that the user journal is usable for `logs`.
pub(crate) fn journal_user_check() -> CheckRow {
    check_row_for_journal_user(run_journalctl(&["--user", "-n", "0", "--no-pager"]))
}

fn check_row_for_journal_user(result: Result<Output, JournalError>) -> CheckRow {
    match result {
        Ok(_) => CheckRow {
            id: "journal_user".to_string(),
            status: CheckStatus::Pass,
            detail: "user journal is accessible".to_string(),
            hint: None,
        },
        Err(err) => CheckRow {
            id: "journal_user".to_string(),
            status: CheckStatus::Fail,
            detail: err.to_string(),
            hint: Some(
                "Check that a systemd --user session is active (e.g. \
                 `loginctl enable-linger $USER`) and that journald is running."
                    .to_string(),
            ),
        },
    }
}

/// Run `journalctl` with the process's real `PATH`. Delegates to
/// [`run_journalctl_with_path_env`] so tests can point `PATH` at a fake
/// `journalctl` instead.
fn run_journalctl(args: &[&str]) -> Result<Output, JournalError> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    run_journalctl_with_path_env(args, &path_env)
}

fn run_journalctl_with_path_env(args: &[&str], path_env: &OsStr) -> Result<Output, JournalError> {
    let output = Command::new(JOURNALCTL_BIN)
        .args(args)
        .env("PATH", path_env)
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => JournalError::NotFound,
            _ => JournalError::Failed {
                detail: err.to_string(),
            },
        })?;

    if output.status.success() {
        Ok(output)
    } else {
        Err(JournalError::Failed {
            detail: journalctl_failure_detail(&output),
        })
    }
}

fn journalctl_failure_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!("journalctl exited with status {}", output.status)
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    /// A directory under the system temp dir, removed on drop. Every test
    /// gets its own directory so parallel tests never share fixture files
    /// (same pattern as `env`'s test module).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "cloud-sql-tracker-journal-test-{label}-{}-{unique}",
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

    /// Write an executable shell script named `journalctl` into `dir`, so
    /// it is the only thing `run_journalctl_with_path_env` can find when
    /// `dir` is the whole `PATH`.
    fn write_fake_journalctl(dir: &Path, script: &str) {
        let path = dir.join("journalctl");
        fs::write(&path, script).expect("write fake journalctl script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("make fake journalctl script executable");
    }

    fn output_with(status_code: i32, stdout: &[u8]) -> Output {
        Output {
            status: ExitStatus::from_raw(status_code),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn dump_from_output_is_empty_when_stdout_has_no_bytes() {
        assert_eq!(dump_from_output(output_with(0, b"")), Dump::Empty);
    }

    #[test]
    fn dump_from_output_is_empty_when_stdout_is_only_whitespace() {
        assert_eq!(dump_from_output(output_with(0, b" \n\t\n")), Dump::Empty);
    }

    #[test]
    fn dump_from_output_keeps_real_lines_unchanged() {
        let lines = b"2024-01-01T00:00:00+00:00 fe-dev[123]: listening\n".to_vec();

        let result = dump_from_output(output_with(0, &lines));

        assert_eq!(result, Dump::Bytes(lines));
    }

    #[test]
    fn run_journalctl_returns_stdout_bytes_when_the_fake_binary_succeeds() {
        let dir = TempDir::new("succeeds");
        write_fake_journalctl(
            dir.path(),
            "#!/bin/sh\nprintf '%s\\n' 'line one' 'line two'\n",
        );

        let output = run_journalctl_with_path_env(&["--user"], dir.path().as_os_str())
            .expect("fake journalctl exits 0");

        assert_eq!(output.stdout, b"line one\nline two\n");
    }

    #[test]
    fn run_journalctl_reports_the_failure_detail_from_stderr() {
        let dir = TempDir::new("fails");
        write_fake_journalctl(
            dir.path(),
            "#!/bin/sh\necho 'no journal files were found' >&2\nexit 1\n",
        );

        let err = run_journalctl_with_path_env(&["--user"], dir.path().as_os_str())
            .expect_err("fake journalctl exits 1");

        assert_eq!(
            err,
            JournalError::Failed {
                detail: "no journal files were found".to_string(),
            }
        );
    }

    #[test]
    fn run_journalctl_falls_back_to_the_exit_status_when_stderr_is_empty() {
        let dir = TempDir::new("fails-silently");
        write_fake_journalctl(dir.path(), "#!/bin/sh\nexit 1\n");

        let err = run_journalctl_with_path_env(&["--user"], dir.path().as_os_str())
            .expect_err("fake journalctl exits 1 with no stderr");

        assert!(matches!(err, JournalError::Failed { detail } if detail.contains('1')));
    }

    #[test]
    fn run_journalctl_reports_not_found_when_missing_from_path() {
        let dir = TempDir::new("missing");

        let err = run_journalctl_with_path_env(&["--user"], dir.path().as_os_str())
            .expect_err("empty directory has no journalctl binary");

        assert_eq!(err, JournalError::NotFound);
    }

    #[test]
    fn check_row_for_journal_user_passes_when_journalctl_succeeds() {
        let row = check_row_for_journal_user(Ok(output_with(0, b"")));

        assert_eq!(row.id, "journal_user");
        assert_eq!(row.status, CheckStatus::Pass);
        assert_eq!(row.hint, None);
    }

    #[test]
    fn check_row_for_journal_user_fails_with_a_hint_when_journalctl_is_missing() {
        let row = check_row_for_journal_user(Err(JournalError::NotFound));

        assert_eq!(row.id, "journal_user");
        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains("journalctl"));
        assert!(row.hint.is_some());
    }
}
