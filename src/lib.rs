//! cloud-sql-tracker library crate.
//!
//! `src/main.rs` stays a thin binary; behavior lives in these modules.
//! See `docs/modules.v1.md` for the frozen module seams.

mod config;
mod env;
mod model;
mod reconcile;
