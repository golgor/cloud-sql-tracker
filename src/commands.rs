//! `cli`'s one public seam (`docs/modules.v1.md`, "commands — one public
//! command seam"). `select` and `status` are internal files for
//! readability, not a second layer of public modules.
//!
//! This ticket ([#42](https://github.com/golgor/cloud-sql-tracker/issues/42))
//! lands selector expansion and `status` only. `start`/`stop`/`restart`
//! (#43) and `doctor`/`logs` (#44) land in later tickets on this same
//! public seam.

mod select;
mod status;

// Re-exported for `cli` (#45), which does not exist yet in this ticket —
// each item is already exercised through its own module's tests.
#[allow(unused_imports)]
pub(crate) use select::{filter_failed, SelectError, Selector};
#[allow(unused_imports)]
pub(crate) use status::{status, StatusError};
