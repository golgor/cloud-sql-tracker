# Agent notes — cloud-sql-tracker

This repo is the **control plane CLI** for multiple Google Cloud SQL Auth Proxy processes. The Omarchy bar plugin is a **separate** repo: `cloud-sql-tracker-oma-plugin`.

## Read before changing contracts

| Doc | Why |
|-----|-----|
| [`CONTEXT.md`](./CONTEXT.md) | Domain language (Connection, Status document, Health state, Orphan, …) |
| [`docs/DESIGN.md`](./docs/DESIGN.md) | Product decisions |
| [`docs/adr/`](./docs/adr/) | Hard-to-reverse choices |
| [`docs/status-document.v1.md`](./docs/status-document.v1.md) | **Full field-by-field meaning** of `status --json` |
| [`schemas/status.v1.json`](./schemas/status.v1.json) | Machine-readable Status document schema |
| [`examples/status.v1.json`](./examples/status.v1.json) | Golden snapshot |
| [`docs/research/`](./docs/research/) | systemd / port / journal research |

## Status document (critical)

- **Only** `cloud-sql-tracker status --json` produces the Status document.
- Schema id is integer field `version` (currently `1`). Binary semver is `cli_version`.
- Do **not** invent a second parallel JSON shape for “list” that duplicates status. Plugin consumes **status only**.
- Prefer **additive** optional fields; bump `version` only for breaking shape/meaning changes.
- When you touch status fields: update **prose + JSON Schema + golden example** together, and say so in the PR/commit.

If a JSON field is unclear, **open `docs/status-document.v1.md`** — that file exists specifically so agents and humans are not left guessing from a bare example.

## Implementation preferences

- Stateless CLI; long-lived work is `cloud-sql-proxy` under `systemd --user`.
- ADC is a hard requirement ([ADR 0002](./docs/adr/0002-adc-only-auth.md)).
- v1 health = unit/orphan + local TCP accept ([ADR 0003](./docs/adr/0003-local-health-signals.md)).
- Prefer native Linux I/O over scraping `ss`/`pgrep` ([ADR 0004](./docs/adr/0004-rust-toolchain-and-linux-io.md)).

## Wayfinder

Planning decisions live on GitHub issues (map label `wayfinder:map`). Do not re-litigate closed tickets without an explicit reopen.
