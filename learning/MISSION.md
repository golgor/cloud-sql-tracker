# Mission: Master the cloud-sql-tracker Architecture and Design

## Why
Understand how a modern, production-grade, stateless Rust control plane CLI is designed, implemented, and verified on Arch/Linux. Learn how to architect low-footprint Linux desktop tools using systemd --user transient units, D-Bus (zbus), contract-first schemas, pure reconciliation logic, and layered verification.

## Success looks like
- Explain the high-level architecture and 4 ADR decisions (stateless CLI, ADC-only auth, local unit+TCP health, native Linux I/O).
- Trace any request from CLI argv (`clap`) through commands, selectors, reconciliation logic, and systemd/D-Bus adapters.
- Understand how `reconcile()` uses pure functions and observation inputs to compute health states without I/O or state files.
- Navigate and modify the contract trios (`docs/*v1.md`, `schemas/*v1.json`, `examples/*`) without breaking schema validation.

## Constraints
- Reader has basic Rust knowledge (ownership, `Result`, `match`, traits) but zero prior knowledge of this specific project, Cloud SQL, systemd, or D-Bus.
- Ground all explanations in real codebase snippets (`src/*.rs`, `docs/*.md`, `schemas/*.json`).

## Out of scope
- Implementing a Cloud SQL tunnel from scratch (Google's `cloud-sql-proxy` binary handles tunneling).
- macOS / Windows / non-Linux OS architectures.
