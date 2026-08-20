//! cloud-sql-tracker — stateless control plane for cloud-sql-proxy processes.
//!
//! This binary does **not** implement the Cloud SQL tunnel. It starts, stops,
//! and reports on Google's `cloud-sql-proxy`, driven by a user config file and
//! (planned) systemd --user transient units.
//!
//! See README.md and docs/DESIGN.md.

fn main() {
    eprintln!(
        "cloud-sql-tracker {}: scaffold only — not implemented yet.\n\
         See https://github.com/golgor/cloud-sql-tracker",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2);
}
