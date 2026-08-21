//! `cli`'s one public seam (`docs/modules.v1.md`, "commands — one public
//! command seam"). `select`, `status`, `mutate`, `doctor`, and `logs` are
//! internal files for readability, not a second layer of public modules.
//!
//! [#42](https://github.com/golgor/cloud-sql-tracker/issues/42) landed
//! selector expansion and `status`. [#44](https://github.com/golgor/cloud-sql-tracker/issues/44)
//! landed `doctor` and `logs`. [#43](https://github.com/golgor/cloud-sql-tracker/issues/43)
//! added `start`/`stop`/`restart`. This ticket
//! ([#45](https://github.com/golgor/cloud-sql-tracker/issues/45)) is the
//! first caller of this whole seam: `cli`.

mod doctor;
mod logs;
mod mutate;
mod select;
mod status;

pub(crate) use doctor::doctor;
pub(crate) use logs::{logs, LogsCommandError};
pub(crate) use mutate::{
    restart, start, stop, BatchOutcome, TargetOutcome, TargetResult, DEFAULT_WAIT_MS,
};
// `filter_failed` stays internal to `commands::mutate`'s own
// `restart --failed` gather step (`super::select::filter_failed`) — nothing
// outside `commands` calls it, so it is not re-exported here.
pub(crate) use select::{SelectError, Selector};
pub(crate) use status::{status, StatusCommandError};
