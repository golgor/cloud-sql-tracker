//! cloud-sql-tracker — stateless control plane for cloud-sql-proxy processes.
//!
//! This binary does **not** implement the Cloud SQL tunnel. It starts, stops,
//! and reports on Google's `cloud-sql-proxy`, driven by a user config file and
//! systemd --user transient units.
//!
//! See README.md and docs/DESIGN.md. Behavior lives in the library crate
//! (`src/lib.rs`); this binary is only `exit(cli::run())`
//! (`docs/modules.v1.md`, "`cli` — thin shell (clap)").

fn main() {
    std::process::exit(cloud_sql_tracker::cli::run());
}
