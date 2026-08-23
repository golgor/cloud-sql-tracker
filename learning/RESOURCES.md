# cloud-sql-tracker Resources

## Knowledge

### Primary Codebase & Docs (High Trust)
- [Control Plane Architecture — docs/DESIGN.md](../docs/DESIGN.md)
  Primary product decisions, stateless CLI model, systemd user units, Omarchy bar plugin boundary.
- [Domain Language — CONTEXT.md](../CONTEXT.md)
  Authoritative domain glossary: Connection, Proxy process, Control plane, Status document, Health state, Reconcile, Source, Foreign process, Group, Unit, Supervisor.
- [Module Seams & Layers — docs/modules.v1.md](../docs/modules.v1.md)
  Module boundaries, pure vs I/O separation, dependency direction, and error propagation rules.
- [Reconcile Truth Table — docs/reconcile.v1.md](../docs/reconcile.v1.md)
  Pure reconciliation function logic, Health state mapping, start window math, and failure signal classification.
- [CLI Contract — docs/cli-contract.v1.md](../docs/cli-contract.v1.md)
  Subcommand specifications, target selectors, exit codes 0–4, and human vs JSON output rules.
- [Status Document Contract — docs/status-document.v1.md](../docs/status-document.v1.md)
  Field-by-field specification and schema invariants for `status --json`.
- [Doctor Contract — docs/doctor.v1.md](../docs/doctor.v1.md)
  Preflight checks catalog (`config`, `proxy_bin`, `systemd_user`, `adc`, `journal_user`, `ports`).
- [Verification Strategy — docs/verification.v1.md](../docs/verification.v1.md)
  Unit tests, Layer 1 schema validation, Layer 2 binary stdout proof, and human dogfooding checklist.
- [Architecture Decision Records — docs/adr/](../docs/adr/)
  ADR 0001 (Control plane shape), ADR 0002 (ADC-only auth), ADR 0003 (Local health signals), ADR 0004 (Rust toolchain & Linux I/O).

### External Primary Sources
- [Google Cloud SQL Auth Proxy Documentation](https://cloud.google.com/sql/docs/postgres/sql-proxy)
  Official proxy flags, IAM authn, ADC, and connection lifecycle.
- [systemd.service & systemd.exec Documentation](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html)
  Transient unit configuration, `Type=exec`, `KillMode=control-group`, and `TimeoutStopUSec`.
- [D-Bus Specification & zbus crate docs](https://docs.rs/zbus/latest/zbus/)
  Asynchronous and synchronous D-Bus IPC on the user bus (`org.freedesktop.systemd1`).

## Wisdom (Communities & Practice)

- [Arch Linux User Repository (AUR) & Systemd User Guidelines](https://wiki.archlinux.org/title/Systemd/User)
  Best practices for user session services on Arch/Omarchy Linux desktop environments.
