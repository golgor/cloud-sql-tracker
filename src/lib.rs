//! cloud-sql-tracker library crate.
//!
//! `src/main.rs` stays a thin binary; behavior lives in these modules.
//! See `docs/modules.v1.md` for the frozen module seams.

const _: () = assert!(
    env!("CARGO_PKG_VERSION").len() <= 64,
    "CARGO_PKG_VERSION must be at most 64 bytes"
);

pub mod cli;
mod commands;
mod config;
mod env;
mod journal;
mod model;
mod port;
mod reconcile;
mod supervisor;
