# Design — cloud-sql-tracker

Decisions from a design grill (2026-08-20). Implementation details may evolve; product decisions below are the contract.

Deeper tradeoff research (supervision model, language, proxy gotchas): [RESEARCH.md](./RESEARCH.md).

## Problem

Track and control several Google Cloud SQL Auth Proxy processes from the desktop (and later an Omarchy bar plugin). Today this is a handful of one-line shell scripts with no ports, no status, and no safe concurrency against local Docker Postgres on 5432.

## Architecture

```
UI / scripts / future tools
        │  argv + stdout JSON only
        ▼
cloud-sql-tracker   (this repo — stateless CLI)
        │
        ├── ~/.config/cloud-sql-tracker/connections.json
        ├── systemd --user transient units
        └── exec → cloud-sql-proxy (Google binary) × N
```

- **Data plane:** Google's `cloud-sql-proxy` — we never reimplement the tunnel.
- **Control plane:** this CLI — config, lifecycle, health aggregation, stable JSON.
- **View:** separate repo `cloud-sql-tracker-oma-plugin` — Omarchy bar only; no direct filesystem access to config.

The CLI is **stateless and short-lived**: each invocation does one job and exits. Long-lived processes are only the proxies (under systemd user units).

## Companion plugin

- Repo: https://github.com/golgor/cloud-sql-tracker-oma-plugin
- Plugin id: `io.github.golgor.cloud-sql-tracker`
- Contract: `status --json` schema `version` field + `cloud-sql-tracker --version` minimum check.

## Config

- Path: `~/.config/cloud-sql-tracker/connections.json`
- v1: hand-edited (seed from `examples/connections.json`)
- Later: `cloud-sql-tracker config ...` CRUD so all writers go through the CLI

### Connection fields (v1)

| Field | Required | Notes |
|-------|----------|--------|
| `id` | yes | stable key, unit name suffix |
| `name` | yes | display label |
| `group` | yes | e.g. `backend`, `fe`, `iot` |
| `instance` | yes | `project:region:instance` |
| `port` | yes | fixed local port; **never** default 5432 |
| `address` | no | default `127.0.0.1` |
| `private_ip` | no | default `false` |
| `auto_iam_authn` | no | default `false` |
| `extra_args` | no | passthrough to proxy |
| `enabled` | no | default `true` |

Global `defaults` merge under each connection.

## Ports (initial Toolsense map)

| id | port |
|----|------|
| backend-dev | 15432 |
| backend-prod | 15433 |
| fe-dev | 15434 |
| fe-prod | 15435 |
| fe-rw-prod | 15436 |
| iot-dev | 15437 |
| iot-prod | 15438 |

## Process model

- One `cloud-sql-proxy` process per connection, fixed `--port`.
- Start via `systemd-run --user` transient unit `cloud-sql-proxy-<id>.service`.
- Stop via systemd (SIGTERM then SIGKILL).
- Detect orphans (old bash scripts): cmdline + port match → report `running`; stop kills that PID; start is no-op if already healthy.
- Do **not** enable shared default HTTP health/admin ports (9090/9091) per instance without unique ports.

## Auth (hard requirement)

Operators **must** have working [Application Default Credentials (ADC)](https://docs.cloud.google.com/docs/authentication/provide-credentials-adc) for Google Cloud before proxies will stay healthy.

- We do **not** implement alternate auth paths in v1 (no SA JSON management UI, no `--token`, no embedding keys).
- `cloud-sql-proxy` uses ADC by default; the control plane only ensures the Unit can *see* ADC (forward `HOME`, and `GOOGLE_APPLICATION_CREDENTIALS` when set; absolute `proxy_bin`).
- `doctor` should fail hard when ADC is missing/unusable (exact checks: freeze on doctor ticket).
- Colleagues are expected to run `gcloud auth application-default login` (or equivalent ADC setup) once per machine/user.

## Health states (v1)

`stopped` | `starting` | `running` | `error`

v1 `running` ≈ unit active (or adopted PID) **and** local port accepting TCP. That is **local listener health**, not “Cloud SQL is reachable.”

### Deferred research — proxy HTTP health-check

Do **not** lose this thread (not in v1 destination):

- `cloud-sql-proxy --health-check` exposes `/startup`, `/liveness`, `/readiness` on `--http-address`/`--http-port` (default `localhost:9090`).
- **Collision:** one default HTTP port cannot be shared across N concurrent proxies — each Connection would need a unique `http_port` (config field) or health-check stays off.
- Useful later for distinguishing “port open” vs “ADC/API/private-IP path broken” (stronger than TCP connect).
- Tracked: [Deferred research: multi-proxy `--health-check` strategy](https://github.com/golgor/cloud-sql-tracker/issues/15) and map **Out of scope / stretch**.
- Until then: never enable default 9090/9091 health/admin ports on every instance.

## Status document (frozen v1)

Machine contract for `status --json`:

- Prose (field meanings): [`docs/status-document.v1.md`](./status-document.v1.md)
- JSON Schema: [`schemas/status.v1.json`](../schemas/status.v1.json)
- Golden example: [`examples/status.v1.json`](../examples/status.v1.json)

Plugin and tests bind to schema `version: 1`. See that doc for `list` vs `status` vs future `config`.

## CLI surface (frozen v1)

Full argv, version, and exit-code contract: [`docs/cli-contract.v1.md`](./cli-contract.v1.md).

```
cloud-sql-tracker [--config PATH] status [--json] [id | --group G | --all]
cloud-sql-tracker start    [--wait-ms N] <id | --group G | --all>
cloud-sql-tracker stop     [--wait-ms N] <id | --group G | --all>
cloud-sql-tracker restart  [--wait-ms N] [--failed] <id | --group G | --all>
cloud-sql-tracker logs     <id> [--lines N]
cloud-sql-tracker doctor   [--json]
cloud-sql-tracker --version   # bare semver from Cargo.toml only
```

- **No `list` in v1** — runtime view is `status`; config inventory is later `config list`.
- **No rollback** on partial multi-target failure (exit `1`); successes stay applied.
- **`restart --failed`** — only cycle connections currently in Health state `error`.
- Later (stretch): `config init|list|add|set|remove`.

## Non-goals (v1)

- Connection strings / DBeaver integration
- Autostart on login
- Long-lived tracker daemon
- Rewriting or bundling `cloud-sql-proxy`
- Plugin writing config files directly
- Alternate Google auth to ADC (keys-in-config, impersonation UX, etc.)
- Proxy HTTP `--health-check` / admin ports as part of Health state (deferred research)
- `logs --follow` streaming (dump-only first; see research brief)

## Build slices

1. Config load + validate + `status --json` (stopped-only if nothing running)
2. `start` / `stop` via systemd --user + port/pid reconcile
3. Orphan adopt + `doctor` + `logs`
4. Group targeting polish + wait/starting timeouts → `error`
5. `config` subcommands

## XDG paths

| Kind | Path |
|------|------|
| Config | `~/.config/cloud-sql-tracker/connections.json` |
| Runtime locks | `$XDG_RUNTIME_DIR/cloud-sql-tracker/` |
| Optional file logs | `~/.local/state/cloud-sql-tracker/logs/` (prefer journald) |
