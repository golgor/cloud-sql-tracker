//! The thin `clap` shell (`docs/modules.v1.md`, "`cli` — thin shell
//! (clap)"). This is the **only** place in the crate that:
//!
//! - holds a `clap` type,
//! - parses `std::env::args()`,
//! - chooses a process exit code (`0`/`1`/`2`/`3`/`4`,
//!   `docs/cli-contract.v1.md`, "Exit code table"),
//! - decides human vs `--json` printing.
//!
//! Everything else — selector expansion, Reconcile, mutating I/O, Doctor
//! checks, journal reads — is `commands::*` (#42–#44), already implemented
//! and already unit-tested at its own pure seams. This module only maps
//! parsed argv to those calls and their results to stdout/stderr/exit code.
//!
//! **`--version`/`-V`:** `clap`'s own built-in version flag always prints
//! `"{bin_name} {version}"`, but `docs/cli-contract.v1.md` requires a
//! **bare** semver line ("no `v` prefix, no binary name"). **Pick:** skip
//! `#[command(version)]` entirely and read a plain `-V`/`--version` boolean
//! flag ourselves, printing `env!("CARGO_PKG_VERSION")` directly. **Why:**
//! `clap_builder`'s `_render_version` has no template hook that drops the
//! binary name. **Discarded:** `Command::long_version`/`version_template`
//! (`clap_builder` 4.6 has neither). **Unchanged:** `-h`/`--help` still use
//! `clap`'s own default handling and its own exit codes (`0` on
//! `--help`/`-h`, `2` on a parse error) — that is `clap`'s well-known
//! convention, not a second exit-code authority this module reimplements.
//!
//! **Target selectors** (`ID | --group NAME | --all`) are **not** a `clap`
//! `ArgGroup`: mutual exclusion and the per-command default/required rule
//! (`docs/cli-contract.v1.md`, "Target selectors") are decided by
//! [`resolve_selector`], a plain function `clap` never sees, so that rule
//! has its own fast unit tests below instead of only living in
//! `ArgGroup` semantics.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::commands::{
    self, BatchOutcome, LogsCommandError, SelectError, StatusCommandError, TargetOutcome,
    TargetResult, DEFAULT_WAIT_MS,
};
use crate::config::{self, Config, ConfigError};
use crate::journal::Dump;
use crate::model::{
    self, CheckStatus, DoctorReport, ErrorCode, HealthState, Source, StatusDocument,
};

#[cfg(test)]
pub mod model_for_tests {
    pub use crate::model::*;
}

/// Output caps (`docs/cli-contract.v1.md`).
const STATUS_MAX_BYTES: usize = 262_144; // 256 KiB
const DOCTOR_MAX_BYTES: usize = 65_536; // 64 KiB

/// `--lines`'s default for `logs` (`docs/cli-contract.v1.md`, "`logs`").
const DEFAULT_LOG_LINES: u32 = 100;

// ---------------------------------------------------------------------------
// argv shape (`docs/cli-contract.v1.md`)
// ---------------------------------------------------------------------------

#[derive(Debug, Parser)]
#[command(name = "cloud-sql-tracker", disable_version_flag = true)]
struct Cli {
    /// Config file override. Default: `$XDG_CONFIG_HOME/cloud-sql-tracker/
    /// connections.json` if set, else `~/.config/cloud-sql-tracker/
    /// connections.json`.
    #[arg(long = "config", value_name = "PATH")]
    config: Option<PathBuf>,

    /// Print the bare version and exit (see this module's doc comment for
    /// why this is a plain flag, not `clap`'s own version handling).
    #[arg(short = 'V', long = "version")]
    version: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

/// One `ID | --group NAME | --all` selector request, shared by every
/// subcommand that takes a target (`docs/cli-contract.v1.md`, "Target
/// selectors"). Deliberately **not** a `clap` `ArgGroup` — see this
/// module's doc comment.
#[derive(Debug, Args)]
struct SelectorArgs {
    /// A single Connection id (positional).
    id: Option<String>,
    /// Every connection in this group.
    #[arg(long)]
    group: Option<String>,
    /// Every connection in the config document.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Reconcile and report (`docs/cli-contract.v1.md`, "`status`").
    Status {
        /// Stdout = one Status document (schema v1); no extra prose.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        selector: SelectorArgs,
    },
    /// Start target Connection(s) (`docs/cli-contract.v1.md`, "`start`").
    Start {
        #[arg(long = "wait-ms", default_value_t = DEFAULT_WAIT_MS)]
        wait_ms: u64,
        #[command(flatten)]
        selector: SelectorArgs,
    },
    /// Stop target Connection(s) (`docs/cli-contract.v1.md`, "`stop`").
    Stop {
        #[arg(long = "wait-ms", default_value_t = DEFAULT_WAIT_MS)]
        wait_ms: u64,
        #[command(flatten)]
        selector: SelectorArgs,
    },
    /// Stop then start targets (`docs/cli-contract.v1.md`, "`restart`").
    Restart {
        #[arg(long = "wait-ms", default_value_t = DEFAULT_WAIT_MS)]
        wait_ms: u64,
        /// Restrict the selected set to Health state `error` only.
        #[arg(long)]
        failed: bool,
        #[command(flatten)]
        selector: SelectorArgs,
    },
    /// Journal dump for one Connection (`docs/cli-contract.v1.md`,
    /// "`logs`").
    Logs {
        /// Single Connection id (no `--group`/`--all` in v1).
        id: String,
        #[arg(long, default_value_t = DEFAULT_LOG_LINES)]
        lines: u32,
    },
    /// Environment/config sanity checks (`docs/cli-contract.v1.md`,
    /// "`doctor`").
    Doctor {
        /// Stdout = one Doctor report (schema v1); no extra prose.
        #[arg(long)]
        json: bool,
    },
}

// ---------------------------------------------------------------------------
// Entry point.
// ---------------------------------------------------------------------------

/// Parse `std::env::args()`, run the requested command, and return the
/// process exit code (`docs/modules.v1.md`, "`cli`"). `main.rs` is only
/// `std::process::exit(cli::run())`.
pub fn run() -> i32 {
    let cli = Cli::parse();

    if cli.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return 0;
    }

    let Some(command) = cli.command else {
        eprintln!("error: a subcommand is required (see --help)");
        return 2;
    };

    let config_path = cli.config.unwrap_or_else(config::default_path);

    match command {
        Commands::Status { json, selector } => run_status(&config_path, selector, json),
        Commands::Start { wait_ms, selector } => {
            run_mutate(&config_path, selector, |config, selector| {
                commands::start(config, selector, wait_ms)
            })
        }
        Commands::Stop { wait_ms, selector } => {
            run_mutate(&config_path, selector, |config, selector| {
                commands::stop(config, selector, wait_ms)
            })
        }
        Commands::Restart {
            wait_ms,
            failed,
            selector,
        } => run_mutate(&config_path, selector, |config, selector| {
            commands::restart(config, selector, wait_ms, failed)
        }),
        Commands::Logs { id, lines } => run_logs(&config_path, &id, lines),
        Commands::Doctor { json } => run_doctor(&config_path, json),
    }
}

// ---------------------------------------------------------------------------
// Selector resolution (pure).
// ---------------------------------------------------------------------------

/// A usage-class problem detected before any I/O — always exit `2`
/// (`docs/cli-contract.v1.md`, "Exit code table").
#[derive(Debug)]
struct UsageError(String);

/// Resolve [`SelectorArgs`] into a [`commands::Selector`], or the usage
/// error to report (`docs/cli-contract.v1.md`, "Target selectors").
///
/// `require_target` is `false` only for `status`, whose omitted target
/// defaults to [`commands::Selector::All`] (`docs/cli-contract.v1.md`,
/// "Defaults"); `start`/`stop`/`restart` pass `true` and error instead.
fn resolve_selector(
    args: SelectorArgs,
    require_target: bool,
) -> Result<commands::Selector, UsageError> {
    let provided = [args.id.is_some(), args.group.is_some(), args.all]
        .into_iter()
        .filter(|set| *set)
        .count();
    if provided > 1 {
        return Err(UsageError(
            "id, --group, and --all cannot combine (docs/cli-contract.v1.md, \
             \"Target selectors\": \"Mutual exclusion\")"
                .to_string(),
        ));
    }

    if let Some(id) = args.id {
        return Ok(commands::Selector::Id(id));
    }
    if let Some(group) = args.group {
        return Ok(commands::Selector::Group(group));
    }
    if args.all {
        return Ok(commands::Selector::All);
    }

    if require_target {
        return Err(UsageError(
            "an explicit target is required: ID, --group NAME, or --all \
             (docs/cli-contract.v1.md, \"Target selectors\": \"Defaults\")"
                .to_string(),
        ));
    }
    Ok(commands::Selector::All)
}

// ---------------------------------------------------------------------------
// status.
// ---------------------------------------------------------------------------

fn run_status(config_path: &Path, selector_args: SelectorArgs, json: bool) -> i32 {
    let selector = match resolve_selector(selector_args, false) {
        Ok(selector) => selector,
        Err(err) => return usage_exit(&err),
    };
    let config = match config::load(config_path) {
        Ok(config) => config,
        Err(err) => return config_exit(&err),
    };
    match commands::status(&config, &selector) {
        Ok(document) => {
            if !json {
                print_status_text(&document);
                return 0;
            }
            if print_status_json(&document).is_ok() {
                0
            } else {
                3
            }
        }
        Err(err) => status_error_exit(&err),
    }
}

fn status_error_exit(err: &StatusCommandError) -> i32 {
    eprintln!("error: {err}");
    match err {
        // Unknown id/group: usage (`docs/cli-contract.v1.md`, "Exit code
        // table": `2`).
        StatusCommandError::Select(_) => 2,
        // A config-valid id failing `model::unit_name` is "practically
        // unreachable" (`src/commands/status.rs`'s own doc comment) — a
        // data/config problem, not an environmental one.
        StatusCommandError::UnitName { .. } => 2,
        // Anything that stops `status` from reaching a Unit at all (bus
        // down, or a malformed systemd response) is a "hard dependency
        // failure that prevents producing status"
        // (`docs/cli-contract.v1.md`, "`status`").
        StatusCommandError::Supervisor { .. } => 3,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum OutputCapError {
    CapExceeded { cap: usize },
}

fn write_status_json<W: std::io::Write>(
    document: &StatusDocument,
    mut writer: W,
) -> Result<(), OutputCapError> {
    let text = serde_json::to_string_pretty(document).expect("StatusDocument always serializes");
    let bytes = text.as_bytes();
    if bytes.len() <= STATUS_MAX_BYTES {
        writer.write_all(bytes).expect("stdout write succeeds");
        assert!(
            bytes.len() <= STATUS_MAX_BYTES,
            "stdout output invariant violated"
        );
        Ok(())
    } else {
        eprintln!(
            "error: status document exceeds maximum allowed output size ({STATUS_MAX_BYTES} bytes)"
        );
        Err(OutputCapError::CapExceeded {
            cap: STATUS_MAX_BYTES,
        })
    }
}

fn print_status_json(document: &StatusDocument) -> Result<(), OutputCapError> {
    write_status_json(document, std::io::stdout().lock())
}

fn print_status_text(document: &StatusDocument) {
    println!(
        "{} running, {} starting, {} error, {} stopped (of {})",
        document.running, document.starting, document.error, document.stopped, document.total
    );
    for row in &document.connections {
        let detail = row
            .error
            .as_ref()
            .map_or("-", |error| error.detail.as_str());
        println!(
            "{:<20} {:<9} {:<6} {:<5} {}",
            row.id,
            state_label(row.state),
            row.port,
            source_label(row.source),
            detail,
        );
    }
}

// ---------------------------------------------------------------------------
// start / stop / restart.
// ---------------------------------------------------------------------------

fn run_mutate(
    config_path: &Path,
    selector_args: SelectorArgs,
    action: impl FnOnce(&Config, &commands::Selector) -> Result<BatchOutcome, SelectError>,
) -> i32 {
    let selector = match resolve_selector(selector_args, true) {
        Ok(selector) => selector,
        Err(err) => return usage_exit(&err),
    };
    let config = match config::load(config_path) {
        Ok(config) => config,
        Err(err) => return config_exit(&err),
    };
    match action(&config, &selector) {
        Ok(outcome) => {
            print_batch(&outcome);
            exit_for_batch(&outcome)
        }
        Err(err) => select_error_exit(&err),
    }
}

fn select_error_exit(err: &SelectError) -> i32 {
    eprintln!("error: {err}");
    2
}

fn print_batch(outcome: &BatchOutcome) {
    for target in &outcome.targets {
        print_target_outcome(target);
    }
    println!(
        "{} succeeded, {} skipped, {} failed",
        outcome.succeeded_count(),
        outcome.skipped_count(),
        outcome.failed_count() + outcome.dependency_count(),
    );
}

fn print_target_outcome(target: &TargetOutcome) {
    match &target.result {
        TargetResult::Succeeded => println!("{}: ok", target.id),
        TargetResult::SkippedDisabled => {
            // `docs/config.v1.md`, "v1 rule (normative)": disabled connections
            // skipped in a multi-target selector are a **stderr** warning, not
            // stdout — the operator still gets a clean success line count on
            // stdout for what actually ran.
            eprintln!("{}: skipped (disabled)", target.id);
        }
        TargetResult::RefusedDisabled => {
            eprintln!("{}: refused — connection is disabled", target.id);
        }
        TargetResult::Failed { code, message } => {
            eprintln!(
                "{}: failed ({}): {message}",
                target.id,
                error_code_label(*code)
            );
        }
        TargetResult::Dependency { message } => {
            eprintln!("{}: dependency error: {message}", target.id);
        }
    }
}

/// Turns a whole [`BatchOutcome`] into the process exit code
/// (`docs/cli-contract.v1.md`, "Exit code table") — the one step
/// `commands::mutate` deliberately leaves to `cli`
/// (`src/commands/mutate.rs`'s own module doc comment).
///
/// **Pick:** any [`TargetResult::Dependency`] wins over
/// [`TargetResult::Failed`], even mixed with successes. **Why:**
/// `docs/cli-contract.v1.md` says "prefer `3` when failure is
/// environmental rather than per-id" without a mixed-outcome carve-out.
/// **Discarded:** counting dependency failures as ordinary failures for
/// the `1`/`4` split. **Unchanged:** a single disabled id refused before
/// any I/O ([`TargetResult::RefusedDisabled`]) stays a usage error (`2`),
/// checked first.
fn exit_for_batch(outcome: &BatchOutcome) -> i32 {
    let refused_disabled = outcome
        .targets
        .iter()
        .any(|target| target.result == TargetResult::RefusedDisabled);
    if refused_disabled {
        return 2;
    }

    if outcome.dependency_count() > 0 {
        return 3;
    }

    if outcome.failed_count() == 0 {
        return 0;
    }

    if outcome.succeeded_count() > 0 {
        1
    } else {
        4
    }
}

// ---------------------------------------------------------------------------
// logs.
// ---------------------------------------------------------------------------

fn run_logs(config_path: &Path, id: &str, lines: u32) -> i32 {
    if lines < 1 {
        eprintln!("error: --lines must be an integer >= 1");
        return 2;
    }
    let config = match config::load(config_path) {
        Ok(config) => config,
        Err(err) => return config_exit(&err),
    };
    match commands::logs(&config, id, lines) {
        Ok(Dump::Empty) => {
            // `docs/logs.v1.md`, "Empty journal / never started": exactly
            // one hint line on stderr, empty stdout, exit 0. `commands::logs`
            // only reaches `Ok` after its own `resolve_unit` already called
            // `model::unit_name(id)` successfully (`docs/modules.v1.md`,
            // "model": one owner for the unit-name string) — reuse that same
            // call instead of a second, fallback copy of the format.
            match model::unit_name(id) {
                Ok(unit) => eprintln!(
                    "no journal entries for unit {unit} (never started, vacuumed, or empty)"
                ),
                Err(_) => {
                    eprintln!("no journal entries for `{id}` (never started, vacuumed, or empty)")
                }
            }
            0
        }
        Ok(Dump::Bytes(bytes)) => {
            use std::io::Write as _;
            let _ = std::io::stdout().write_all(&bytes);
            0
        }
        Err(err) => {
            print_logs_error(&err);
            logs_error_exit(&err)
        }
    }
}

/// `docs/logs.v1.md`, exit `3` row: "Message should suggest
/// `cloud-sql-tracker doctor`" — only the dependency-class
/// [`LogsCommandError::Journal`] case gets that suggestion; the usage-class
/// (`2`) cases do not.
fn print_logs_error(err: &LogsCommandError) {
    match err {
        LogsCommandError::Journal(_) => {
            eprintln!("error: {err} (run `cloud-sql-tracker doctor` for details)");
        }
        LogsCommandError::UnknownId(_) | LogsCommandError::UnitName { .. } => {
            eprintln!("error: {err}");
        }
    }
}

fn logs_error_exit(err: &LogsCommandError) -> i32 {
    match err {
        LogsCommandError::UnknownId(_) => 2,
        LogsCommandError::UnitName { .. } => 2,
        LogsCommandError::Journal(_) => 3,
    }
}

// ---------------------------------------------------------------------------
// doctor.
// ---------------------------------------------------------------------------

fn run_doctor(config_path: &Path, json: bool) -> i32 {
    // `commands::doctor` never fail-fasts on a bad config
    // (`docs/doctor.v1.md`, "Config load path"): it takes the path, not an
    // already-loaded `Config`.
    let report = commands::doctor(config_path);
    if !json {
        print_doctor_text(&report);
        return if report.ok { 0 } else { 3 };
    }
    if print_doctor_json(&report).is_err() {
        return 3;
    }
    if report.ok {
        0
    } else {
        3
    }
}

fn write_doctor_json<W: std::io::Write>(
    report: &DoctorReport,
    mut writer: W,
) -> Result<(), OutputCapError> {
    let text = serde_json::to_string_pretty(report).expect("DoctorReport always serializes");
    let bytes = text.as_bytes();
    if bytes.len() <= DOCTOR_MAX_BYTES {
        writer.write_all(bytes).expect("stdout write succeeds");
        assert!(
            bytes.len() <= DOCTOR_MAX_BYTES,
            "stdout output invariant violated"
        );
        Ok(())
    } else {
        eprintln!(
            "error: doctor report exceeds maximum allowed output size ({DOCTOR_MAX_BYTES} bytes)"
        );
        Err(OutputCapError::CapExceeded {
            cap: DOCTOR_MAX_BYTES,
        })
    }
}

fn print_doctor_json(report: &DoctorReport) -> Result<(), OutputCapError> {
    write_doctor_json(report, std::io::stdout().lock())
}

fn print_doctor_text(report: &DoctorReport) {
    for check in &report.checks {
        let status = match check.status {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        };
        match &check.hint {
            Some(hint) => println!("[{status}] {}: {} (hint: {hint})", check.id, check.detail),
            None => println!("[{status}] {}: {}", check.id, check.detail),
        }
    }
    println!("{}", if report.ok { "ok" } else { "FAILED" });
}

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

fn usage_exit(err: &UsageError) -> i32 {
    eprintln!("error: {}", err.0);
    2
}

fn config_exit(err: &ConfigError) -> i32 {
    // Missing/invalid config file: usage/config class
    // (`docs/config.v1.md`, "Load failure -> exit 2").
    eprintln!("error: {err}");
    2
}

/// Human `status`/`doctor` text uses these instead of the wire's
/// lowercase/snake_case `Serialize` output — a presentation concern local
/// to this module, not a reason to add `Display` to `model` types
/// (`docs/modules.v1.md`, "model": "shallow ... shared vocabulary, not
/// behavior").
fn state_label(state: HealthState) -> &'static str {
    match state {
        HealthState::Stopped => "stopped",
        HealthState::Starting => "starting",
        HealthState::Running => "running",
        HealthState::Error => "error",
    }
}

fn source_label(source: Source) -> &'static str {
    match source {
        Source::Unit => "unit",
        Source::None => "none",
    }
}

fn error_code_label(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::BinMissing => "bin_missing",
        ErrorCode::PortInUse => "port_in_use",
        ErrorCode::ExecFailed => "exec_failed",
        ErrorCode::UnitFailed => "unit_failed",
        ErrorCode::StartTimeout => "start_timeout",
        ErrorCode::Auth => "auth",
        ErrorCode::Config => "config",
        ErrorCode::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selector_args(id: Option<&str>, group: Option<&str>, all: bool) -> SelectorArgs {
        SelectorArgs {
            id: id.map(str::to_string),
            group: group.map(str::to_string),
            all,
        }
    }

    // -- resolve_selector ----------------------------------------------------

    #[test]
    fn resolve_selector_with_an_id_resolves_to_selector_id() {
        let selector = resolve_selector(selector_args(Some("fe-dev"), None, false), true)
            .expect("a lone id resolves");
        assert_eq!(selector, commands::Selector::Id("fe-dev".to_string()));
    }

    #[test]
    fn resolve_selector_with_a_group_resolves_to_selector_group() {
        let selector = resolve_selector(selector_args(None, Some("fe"), false), true)
            .expect("a lone group resolves");
        assert_eq!(selector, commands::Selector::Group("fe".to_string()));
    }

    #[test]
    fn resolve_selector_with_all_resolves_to_selector_all() {
        let selector =
            resolve_selector(selector_args(None, None, true), true).expect("a lone --all resolves");
        assert_eq!(selector, commands::Selector::All);
    }

    #[test]
    fn resolve_selector_defaults_to_all_when_not_required_and_omitted() {
        let selector = resolve_selector(selector_args(None, None, false), false)
            .expect("status defaults to All");
        assert_eq!(selector, commands::Selector::All);
    }

    #[test]
    fn resolve_selector_errors_when_required_and_omitted() {
        resolve_selector(selector_args(None, None, false), true)
            .expect_err("start/stop/restart require an explicit target");
    }

    #[test]
    fn resolve_selector_errors_when_id_and_all_both_given() {
        resolve_selector(selector_args(Some("fe-dev"), None, true), true)
            .expect_err("id and --all cannot combine");
    }

    #[test]
    fn resolve_selector_errors_when_id_and_group_both_given() {
        resolve_selector(selector_args(Some("fe-dev"), Some("fe"), false), true)
            .expect_err("id and --group cannot combine");
    }

    #[test]
    fn resolve_selector_errors_when_group_and_all_both_given() {
        resolve_selector(selector_args(None, Some("fe"), true), true)
            .expect_err("--group and --all cannot combine");
    }

    // -- exit_for_batch --------------------------------------------------------

    fn outcome(results: Vec<TargetResult>) -> BatchOutcome {
        BatchOutcome {
            targets: results
                .into_iter()
                .enumerate()
                .map(|(index, result)| TargetOutcome {
                    id: format!("id-{index}"),
                    result,
                })
                .collect(),
        }
    }

    #[test]
    fn exit_for_batch_is_zero_when_everything_succeeds() {
        let outcome = outcome(vec![TargetResult::Succeeded, TargetResult::Succeeded]);
        assert_eq!(exit_for_batch(&outcome), 0);
    }

    #[test]
    fn exit_for_batch_is_zero_for_an_empty_batch() {
        assert_eq!(exit_for_batch(&outcome(vec![])), 0);
    }

    #[test]
    fn exit_for_batch_is_zero_when_only_skips_and_successes_mix() {
        let outcome = outcome(vec![TargetResult::Succeeded, TargetResult::SkippedDisabled]);
        assert_eq!(exit_for_batch(&outcome), 0);
    }

    #[test]
    fn exit_for_batch_is_one_when_some_succeed_and_some_fail() {
        let outcome = outcome(vec![
            TargetResult::Succeeded,
            TargetResult::Failed {
                code: ErrorCode::Unknown,
                message: "boom".to_string(),
            },
        ]);
        assert_eq!(exit_for_batch(&outcome), 1);
    }

    #[test]
    fn exit_for_batch_is_four_when_every_attempted_target_fails() {
        let outcome = outcome(vec![TargetResult::Failed {
            code: ErrorCode::Unknown,
            message: "boom".to_string(),
        }]);
        assert_eq!(exit_for_batch(&outcome), 4);
    }

    #[test]
    fn exit_for_batch_is_two_for_a_single_refused_disabled_target() {
        let outcome = outcome(vec![TargetResult::RefusedDisabled]);
        assert_eq!(exit_for_batch(&outcome), 2);
    }

    #[test]
    fn exit_for_batch_prefers_three_for_any_dependency_failure() {
        let outcome = outcome(vec![
            TargetResult::Succeeded,
            TargetResult::Dependency {
                message: "no bus".to_string(),
            },
        ]);
        assert_eq!(exit_for_batch(&outcome), 3);
    }

    // -- error_code_label / state_label / source_label ------------------------

    #[test]
    fn error_code_label_matches_the_status_document_wire_catalog() {
        // `docs/status-document.v1.md`, "error object": the same
        // snake_case tokens `model::ErrorCode`'s `Serialize` produces.
        assert_eq!(error_code_label(ErrorCode::PortInUse), "port_in_use");
        assert_eq!(error_code_label(ErrorCode::StartTimeout), "start_timeout");
        assert_eq!(error_code_label(ErrorCode::UnitFailed), "unit_failed");
        assert_eq!(error_code_label(ErrorCode::ExecFailed), "exec_failed");
        assert_eq!(error_code_label(ErrorCode::BinMissing), "bin_missing");
        assert_eq!(error_code_label(ErrorCode::Auth), "auth");
        assert_eq!(error_code_label(ErrorCode::Config), "config");
        assert_eq!(error_code_label(ErrorCode::Unknown), "unknown");
    }

    #[test]
    fn state_label_matches_the_status_document_wire_catalog() {
        assert_eq!(state_label(HealthState::Stopped), "stopped");
        assert_eq!(state_label(HealthState::Starting), "starting");
        assert_eq!(state_label(HealthState::Running), "running");
        assert_eq!(state_label(HealthState::Error), "error");
    }

    #[test]
    fn source_label_matches_the_status_document_wire_catalog() {
        assert_eq!(source_label(Source::Unit), "unit");
        assert_eq!(source_label(Source::None), "none");
    }

    #[test]
    fn write_status_json_rejects_document_over_max_bytes_and_writes_zero_bytes() {
        let over_cap_doc = StatusDocument {
            version: 1,
            ts: "2026-08-24T12:00:00Z".to_string(),
            cli_version: "0.1.0".to_string(),
            running: 0,
            starting: 0,
            error: 0,
            stopped: 0,
            total: 0,
            groups: std::collections::BTreeMap::new(),
            connections: vec![model::StatusRow {
                id: "a".to_string(),
                name: "A".to_string(),
                group: "g".to_string(),
                instance: "p:r:i".to_string(),
                address: "127.0.0.1".to_string(),
                port: 15432,
                private_ip: false,
                enabled: true,
                state: HealthState::Error,
                source: Source::None,
                pid: None,
                unit: None,
                port_open: false,
                uptime_sec: None,
                error: Some(model::StatusError {
                    code: ErrorCode::Unknown,
                    // Direct construction bypassing clamp for test of stdout guard
                    detail: "x".repeat(300_000),
                }),
            }],
        };

        let mut buf = Vec::new();
        let res = write_status_json(&over_cap_doc, &mut buf);
        assert_eq!(
            res,
            Err(OutputCapError::CapExceeded {
                cap: STATUS_MAX_BYTES
            })
        );
        assert!(
            buf.is_empty(),
            "rejected over-cap document must write zero bytes to supplied writer"
        );
    }

    #[test]
    fn write_doctor_json_rejects_report_over_max_bytes_and_writes_zero_bytes() {
        let over_cap_report = DoctorReport {
            version: 1,
            cli_version: "0.1.0".to_string(),
            ok: true,
            checks: vec![model::CheckRow {
                id: "test".to_string(),
                status: model::CheckStatus::Pass,
                // Direct construction bypassing clamp for test of stdout guard
                detail: "d".repeat(70_000),
                hint: None,
            }],
        };

        let mut buf = Vec::new();
        let res = write_doctor_json(&over_cap_report, &mut buf);
        assert_eq!(
            res,
            Err(OutputCapError::CapExceeded {
                cap: DOCTOR_MAX_BYTES
            })
        );
        assert!(
            buf.is_empty(),
            "rejected over-cap report must write zero bytes to supplied writer"
        );
    }
}
