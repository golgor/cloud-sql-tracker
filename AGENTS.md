# Agent notes — cloud-sql-tracker

This repo is the **control plane CLI** for multiple Google Cloud SQL Auth Proxy processes. The Omarchy bar plugin is a **separate** repo: `cloud-sql-tracker-oma-plugin`.

## Git workflow (PRs)

- Do **not** commit product/docs freezes straight to `main` when collaborating via history-friendly flow.
- Branch from latest `main`: `wayfinder/<ticket>-short-slug` or `feat/…` / `docs/…`.
- Open a **Pull Request** into `main`; link the Wayfinder issue (`Fixes #N` / `Closes #N` when the PR fully resolves it).
- Prefer one logical decision or slice per PR so `main` history stays reviewable.
- After merge, update the Wayfinder **map** Decisions-so-far if the PR closed a map ticket.

## Read before changing contracts

| Doc | Why |
|-----|-----|
| [`CONTEXT.md`](./CONTEXT.md) | Domain language (Connection, Status document, Health state, Foreign process, …) |
| [`docs/DESIGN.md`](./docs/DESIGN.md) | Product decisions |
| [`docs/adr/`](./docs/adr/) | Hard-to-reverse choices |
| [`docs/cli-contract.v1.md`](./docs/cli-contract.v1.md) | **Argv, --version, exit codes** |
| [`docs/config.v1.md`](./docs/config.v1.md) | **connections.json validation + defaults merge** |
| [`schemas/config.v1.json`](./schemas/config.v1.json) | Machine-readable config schema |
| [`examples/connections.json`](./examples/connections.json) | Golden config |
| [`docs/status-document.v1.md`](./docs/status-document.v1.md) | **Full field-by-field meaning** of `status --json` |
| [`schemas/status.v1.json`](./schemas/status.v1.json) | Machine-readable Status document schema |
| [`examples/status.v1.json`](./examples/status.v1.json) | Golden snapshot |
| [`docs/reconcile.v1.md`](./docs/reconcile.v1.md) | **Health state transitions** (pure Reconcile truth table) |
| [`docs/doctor.v1.md`](./docs/doctor.v1.md) | **Doctor** preflight checks + `doctor --json` |
| [`schemas/doctor.v1.json`](./schemas/doctor.v1.json) | Machine-readable Doctor report schema |
| [`examples/doctor.v1.json`](./examples/doctor.v1.json) | Golden doctor snapshot |
| [`docs/logs.v1.md`](./docs/logs.v1.md) | **`logs` subcommand** (journalctl dump UX) |
| [`examples/logs.v1.txt`](./examples/logs.v1.txt) | Sample plain-text logs transcript |
| [`docs/research/`](./docs/research/) | systemd / port / journal research |

## Contract artifacts (keep in sync)

Frozen product surfaces use a small artifact set. **Same PR** must update every applicable piece — never prose-only or schema-only drift.

| Kind | When | Artifacts |
|------|------|-----------|
| **JSON contract** | Machine JSON in or out (`status --json`, `doctor --json`, `connections.json`) | **Prose** (`docs/…v1.md`) + **JSON Schema** (`schemas/…`) + **golden example** (`examples/…json`) |
| **Plain-text / argv UX** | Human stdout or argv-only (`logs`, much of CLI contract) | **Prose** + **golden sample** when useful (e.g. `examples/logs.v1.txt`) — **no** JSON Schema |
| **Rules / tables** | Pure decision tables (`reconcile`) | **Prose** (normative tables); schema only if a JSON document is defined |

### Current JSON trios

| Prose | Schema | Golden |
|-------|--------|--------|
| [`docs/status-document.v1.md`](./docs/status-document.v1.md) | [`schemas/status.v1.json`](./schemas/status.v1.json) | [`examples/status.v1.json`](./examples/status.v1.json) |
| [`docs/config.v1.md`](./docs/config.v1.md) | [`schemas/config.v1.json`](./schemas/config.v1.json) | [`examples/connections.json`](./examples/connections.json) |
| [`docs/doctor.v1.md`](./docs/doctor.v1.md) | [`schemas/doctor.v1.json`](./schemas/doctor.v1.json) | [`examples/doctor.v1.json`](./examples/doctor.v1.json) |

### Rules

- Touching a JSON field, enum, or validation rule → update **prose + schema + golden** together.
- Prefer **additive** optional fields; bump document schema `version` only for breaking shape/meaning changes (see each prose doc).
- New JSON contracts must add the full trio and a row in the table above.
- CI: goldens must validate against schemas **and**, once the binary exists, CLI JSON (`status --json`, `doctor --json`) and config parse must match those schemas — [issue #23](https://github.com/golgor/cloud-sql-tracker/issues/23). Do not close #23 on golden-only checks.

## Status document (critical)

- **Only** `cloud-sql-tracker status --json` produces the Status document.
- Schema id is integer field `version` (currently `1`). Binary semver is `cli_version`.
- There is **no `list` command** in v1; do not invent a parallel status JSON. Plugin consumes **status only** (not `logs`, not doctor as bar state).
- Field meanings: [`docs/status-document.v1.md`](./docs/status-document.v1.md). Sync rule: **Contract artifacts** above.

If a JSON field is unclear, **open the status prose** — do not guess from the golden alone.

## Version string (single source)

- Bump **`Cargo.toml` `[package].version` only** for releases.
- `--version` / `-V` and Status `cli_version` must use `CARGO_PKG_VERSION` (or equivalent) — never a second hard-coded version in `main.rs`.
- GitHub Release tags should match that package version (`v0.1.0` ↔ `0.1.0`).

## CLI argv

- Contract: [`docs/cli-contract.v1.md`](./docs/cli-contract.v1.md).
- Multi-target start/stop is **not transactional**: partial failure leaves successes running (exit `1`).
- `restart --failed` only targets Health state `error`.

## Config file

- Contract: [`docs/config.v1.md`](./docs/config.v1.md).
- **Reject unknown JSON keys** (exit 2). Do not “ignore extras.”
- Unique `id`, `port`, and `instance`. Reserved ports: 5432, 3306, 1433 (+ all 1–1023).
- Sync rule: **Contract artifacts** (prose + schema + golden).

## Implementation preferences

- Stateless CLI; long-lived work is `cloud-sql-proxy` under `systemd --user`.
- ADC is a hard requirement ([ADR 0002](./docs/adr/0002-adc-only-auth.md)).
- v1 health = **our Unit** + local TCP accept; no Orphan adopt ([ADR 0003](./docs/adr/0003-local-health-signals.md), [`reconcile.v1.md`](./docs/reconcile.v1.md)).
- Prefer native Linux I/O over scraping `ss`/`pgrep` ([ADR 0004](./docs/adr/0004-rust-toolchain-and-linux-io.md)).

## Wayfinder

Planning decisions live on GitHub issues (map label `wayfinder:map`). Do not re-litigate closed tickets without an explicit reopen.
