//! `cloud-sql-proxy` binary discovery and Application Default Credentials
//! (ADC) presence. Shared owner for PATH / ADC file discovery: `doctor` is a
//! **caller** of the `*_check` functions here, not a second implementation
//! (`docs/modules.v1.md`, "env — proxy binary + ADC"; ADC is a hard
//! requirement, [ADR 0002](../docs/adr/0002-adc-only-auth.md)).
//!
//! `commands::doctor` (#44) calls the `*_check` rows below, which already
//! make `resolve_proxy_bin` / `adc_status` reachable — mutate (#43) will
//! call them directly too, for **start**'s env forwarding, but neither
//! function needs an `#[allow(dead_code)]` today.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::model::{CheckRow, CheckStatus};

const DEFAULT_PROXY_BIN: &str = "cloud-sql-proxy";

/// Resolve the `cloud-sql-proxy` binary to an absolute-or-`PATH`-checked
/// path, for **start** (`docs/config.v1.md`, "`proxy_bin` resolution").
///
/// `configured` is the connections file's top-level `proxy_bin` value, or
/// `None` to use the built-in default name. An absolute path must be an
/// executable file; a bare name is searched on `PATH` the same way a shell
/// would. Mutate (#43) will call this directly for **start**'s env
/// forwarding; [`proxy_bin_check`] below already makes it reachable.
pub(crate) fn resolve_proxy_bin(configured: Option<&str>) -> Result<PathBuf, ProxyBinError> {
    let name = configured.unwrap_or(DEFAULT_PROXY_BIN);
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    resolve_bin(name, &path_env)
}

/// Doctor's `proxy_bin` row (`docs/doctor.v1.md`, "`proxy_bin` — hard"),
/// built on [`resolve_proxy_bin`].
pub(crate) fn proxy_bin_check(configured: Option<&str>) -> CheckRow {
    check_row_for_proxy_bin(resolve_proxy_bin(configured))
}

fn check_row_for_proxy_bin(resolved: Result<PathBuf, ProxyBinError>) -> CheckRow {
    match resolved {
        Ok(path) => CheckRow {
            id: "proxy_bin".to_string(),
            status: CheckStatus::Pass,
            detail: path.display().to_string(),
            hint: None,
        },
        Err(err) => CheckRow {
            id: "proxy_bin".to_string(),
            status: CheckStatus::Fail,
            detail: err.to_string(),
            hint: Some(
                "Install cloud-sql-proxy, or set \"proxy_bin\" in connections.json to its \
                 absolute path."
                    .to_string(),
            ),
        },
    }
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
        let path = dir.join(name);
        fs::write(&path, contents).expect("write test fixture file");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))
            .expect("set test fixture file permissions");
        path
    }

    fn write_executable(dir: &Path, name: &str) -> PathBuf {
        write_file(dir, name, b"#!/bin/sh\n", 0o755)
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
    fn proxy_bin_check_passes_with_the_resolved_path_as_detail() {
        let bin = PathBuf::from("/usr/bin/cloud-sql-proxy");

        let row = check_row_for_proxy_bin(Ok(bin.clone()));

        assert_eq!(row.id, "proxy_bin");
        assert_eq!(row.status, CheckStatus::Pass);
        assert_eq!(row.detail, bin.display().to_string());
        assert_eq!(row.hint, None);
    }

    #[test]
    fn proxy_bin_check_fails_with_a_hint_when_not_found() {
        let row = check_row_for_proxy_bin(Err(ProxyBinError::NotFound {
            name: "cloud-sql-proxy".to_string(),
        }));

        assert_eq!(row.id, "proxy_bin");
        assert_eq!(row.status, CheckStatus::Fail);
        assert!(row.detail.contains("cloud-sql-proxy"));
        assert!(row.hint.is_some());
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
