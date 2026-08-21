//! `cli`'s one public seam (`docs/modules.v1.md`, "commands — one public
//! command seam"). `select`, `status`, `mutate`, `doctor`, and `logs` are
//! internal files for readability, not a second layer of public modules.
//!
//! [#42](https://github.com/golgor/cloud-sql-tracker/issues/42) landed
//! selector expansion and `status`. [#44](https://github.com/golgor/cloud-sql-tracker/issues/44)
//! landed `doctor` and `logs`. This ticket
//! ([#43](https://github.com/golgor/cloud-sql-tracker/issues/43)) adds
//! `start`/`stop`/`restart` on the same seam, reusing `status`'s
//! Observation-gather + Reconcile round trip
//! (`src/commands/status.rs::observe_and_reconcile`) for idempotency
//! checks and wait loops.

mod doctor;
mod logs;
mod mutate;
mod select;
mod status;

// Re-exported for `cli` (#45), which does not exist yet — each item is
// already exercised through its own module's tests.
#[allow(unused_imports)]
pub(crate) use doctor::doctor;
#[allow(unused_imports)]
pub(crate) use logs::{logs, LogsCommandError};
#[allow(unused_imports)]
pub(crate) use mutate::{restart, start, stop, BatchOutcome, TargetOutcome, TargetResult};
#[allow(unused_imports)]
pub(crate) use select::{filter_failed, SelectError, Selector};
#[allow(unused_imports)]
pub(crate) use status::{status, StatusCommandError};
