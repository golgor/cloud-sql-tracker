# 0001: Established Baseline Architecture Understanding

Established the core architecture of `cloud-sql-tracker` v0.1.1: a stateless Rust CLI control plane managing Google Cloud SQL Auth Proxy processes as transient `systemd --user` units (`Type=exec`), communicating over D-Bus via `zbus`. The codebase is structured with pure reconciliation logic (`src/reconcile.rs`), contract-first schema validation (`schemas/*.json`), and layered verification (Layer 1 schema checks, Layer 2 binary stdout tests, and human dogfooding).
