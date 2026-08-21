//! `cli`'s one public seam (`docs/modules.v1.md`, "commands — one public
//! command seam"). `select`, `status`, `doctor`, and `logs` are internal
//! files for readability, not a second layer of public modules.
//!
//! This ticket ([#44](https://github.com/golgor/cloud-sql-tracker/issues/44))
//! lands `doctor` and `logs`. `start`/`stop`/`restart` (#43) land in a
//! later ticket on this same public seam.

mod doctor;
mod logs;
mod select;
mod status;

// Re-exported for `cli` (#45), which does not exist yet — each item is
// already exercised through its own module's tests.
#[allow(unused_imports)]
pub(crate) use doctor::doctor;
#[allow(unused_imports)]
pub(crate) use logs::{logs, LogsCommandError};
#[allow(unused_imports)]
pub(crate) use select::{filter_failed, SelectError, Selector};
#[allow(unused_imports)]
pub(crate) use status::{status, StatusCommandError};
