# Status document — schema version 1

**Canonical machine contract** for `cloud-sql-tracker status --json`.

| Artifact | Path |
|----------|------|
| This prose (read this first) | `docs/status-document.v1.md` |
| JSON Schema | [`schemas/status.v1.json`](../schemas/status.v1.json) |
| Golden example | [`examples/status.v1.json`](../examples/status.v1.json) |
| Wayfinder freeze | [issue #3](https://github.com/golgor/cloud-sql-tracker/issues/3) |

Consumers: Omarchy plugin, tests, humans debugging with `jq`.  
Producers: only the control plane CLI (`status --json`).

---

## Mental model

One **Status document** is a **point-in-time snapshot** of every configured Connection after reconcile:

- Config says what *should* exist (id, name, ports, …).
- Reconcile observes Unit / port / listener and assigns a **Health state** (rules: [`reconcile.v1.md`](./reconcile.v1.md)).
- Aggregates at the top are derived only from `connections[].state` (and group keys).

This is **not** a config file, not a log stream, and not a doctor report.

```
status --json  →  Status document (this schema)
config file    →  connections.json (separate schema; not this document)
doctor --json  →  Doctor report (separate schema; see [`doctor.v1.md`](./doctor.v1.md))
```

---

## Versioning

| Field | What it is |
|-------|------------|
| `version` | **Integer schema id** of *this JSON shape*. Currently always `1`. |
| `cli_version` | Semver string of the **binary** (same family as `cloud-sql-tracker --version`). |

### When to bump `version` (breaking)

Bump `version` to `2` (etc.) only if a consumer that implemented v1 would **misbehave** without code changes, for example:

- rename / remove a required field
- change the meaning of an existing field
- change enum membership in a breaking way (e.g. remove `running`)
- change types (string → number)

### When **not** to bump `version` (additive)

- add optional fields
- add new `error.code` string values
- add new optional `source` values only if old consumers can ignore unknowns

**Plugin rule:** require `version === 1` for this contract. Ignore unknown fields. Use `cli_version` (or `--version`) for “binary too old for feature X”, not for JSON shape.

**Why not put semver in `version`?**  
Schema compatibility and binary release cadence are different clocks. A new CLI feature that needs a *new config field* is a **config + cli_version / min plugin setting** problem, not automatically a Status document break. If that feature also adds required Status fields, then bump schema `version`.

---

## Top-level object

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `version` | integer | yes | Schema id; must be `1` for this document. |
| `ts` | string (RFC 3339 date-time) | yes | When this snapshot was built (local offset OK). |
| `cli_version` | string | yes | Binary semver, e.g. `"0.1.0"`. |
| `running` | integer ≥ 0 | yes | Count of connections with `state === "running"`. |
| `starting` | integer ≥ 0 | yes | Count with `state === "starting"`. |
| `error` | integer ≥ 0 | yes | Count with `state === "error"`. |
| `stopped` | integer ≥ 0 | yes | Count with `state === "stopped"`. |
| `total` | integer ≥ 0 | yes | `connections.length` (enabled connections included in the document). |
| `groups` | object | yes | Map of group name → group counters (see below). May be `{}` if no connections. |
| `connections` | array | yes | One element per Connection included in this snapshot (stable order: config order). |

**Invariant:** `running + starting + error + stopped === total`  
**Invariant:** each group’s counters sum the same way for connections in that group; sum of group `total` values === `total`.

### `groups[name]`

| Field | Type | Meaning |
|-------|------|---------|
| `running` | integer | |
| `starting` | integer | |
| `error` | integer | |
| `stopped` | integer | |
| `total` | integer | Connections in this group in the document |

Group names are free strings from config (`fe`, `backend`, `iot`, …).

---

## `connections[]` element

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `id` | string | yes | Stable Connection id (config). |
| `name` | string | yes | Display label. |
| `group` | string | yes | Group key. |
| `instance` | string | yes | Cloud SQL instance connection name `project:region:instance`. |
| `address` | string | yes | Local bind address (default `127.0.0.1`). |
| `port` | integer 1–65535 | yes | Local listen port. |
| `private_ip` | boolean | yes | Whether proxy should use private IP (config). |
| `state` | string enum | yes | Health state — see below. |
| `source` | string enum | yes | Process ownership signal — see below. |
| `pid` | integer \| null | yes | Main proxy PID if known; else `null`. |
| `unit` | string \| null | yes | Expected systemd unit name, e.g. `cloud-sql-proxy-fe-dev.service`. Always the *expected* name when known; `null` only if id could not form a unit name (should not happen for valid config). |
| `port_open` | boolean | yes | TCP accept probe to `address:port` succeeded. |
| `uptime_sec` | integer \| null | yes | Seconds the current Proxy process has been up, if known; else `null`. |
| `error` | object \| null | yes | `null` unless `state === "error"` (should be non-null when error). |

### `state` (Health state)

Exactly one of:

| Value | Meaning (v1) |
|-------|----------------|
| `stopped` | No managed unit active, port closed, not in a start wait. |
| `starting` | Start requested / unit activating / waiting for `port_open` within timeout window. |
| `running` | Our Unit active **and** `port_open === true` (listener is ours or PID unknown). |
| `error` | Failed start, crashed unit, port held without our Unit, start timeout, etc. |

No `degraded` in v1 (upstream Cloud SQL reachability is out of band — see deferred health-check research).

### `source` (ownership)

Orthogonal to `state`:

| Value | Meaning |
|-------|---------|
| `unit` | Process is the MainPID of our expected user Unit. |
| `none` | No managed Unit process attributed. |

There is **no** `orphan` Source in v1. A leftover proxy on the port is `error` + `port_in_use`, not `running`.

Typical combos:

- `running` + `unit` — normal managed
- `stopped` + `none` — idle
- `error` + `none` — failed start / port held by Foreign process (including hand-started proxy)
- `error` + `unit` — unit confused / failed while still loaded
- `starting` + `unit` or `none` — mid-start

### `error` object

When non-null:

| Field | Type | Meaning |
|-------|------|---------|
| `code` | string | Stable machine token (see catalog). Unknown codes must be tolerated by consumers. |
| `detail` | string | Human-readable explanation (may change wording freely). |

**Catalog (v1, non-exhaustive — new codes are additive):**

| `code` | Typical situation |
|--------|-------------------|
| `bin_missing` | `cloud-sql-proxy` not executable / not found |
| `port_in_use` | Configured port held by something that is not this Connection’s **managed Unit** (Docker, leftover script proxy, wrong PID, …) |
| `exec_failed` | Unit failed to exec proxy |
| `unit_failed` | Unit reached failed / proxy exited unexpectedly |
| `start_timeout` | Still not `port_open` after start wait |
| `auth` | ADC / credential failure detected at control plane (when we can tell) |
| `config` | Connection config inconsistent at runtime |
| `unknown` | Fallback |

When `state !== "error"`, `error` should be `null`.

---

## What is **not** in this document

- Connection strings / DB passwords  
- Full journal logs (use `logs`)  
- Doctor checks (binary on PATH, ADC probe details) — separate command  
- Config file path or raw config dump  
- HTTP health-check readiness (deferred)  

---

## `status` vs `config` (and no `list`)

| Command | Role | JSON shape |
|---------|------|------------|
| `status --json` | Reconcile + **this** Status document | schema v1 |
| `list` | **Not in v1** (dropped; see [`cli-contract.v1.md`](./cli-contract.v1.md)) | — |
| `config …` | Later map/stretch: mutate/read `connections.json` through the CLI | config schema, not Status document |

The plugin should call **`status --json` only** for bar state. Full argv contract: [`cli-contract.v1.md`](./cli-contract.v1.md).

---

## Consumer checklist (plugin / agents)

1. Parse JSON; if `version !== 1`, show “incompatible CLI / status schema” and stop trusting fields.
2. Optionally compare `cli_version` to configured minimum.
3. Bar count ← `running`; warning affordance ← `error > 0`.
4. Render `connections` grouped by `group` (or use `groups` for headers only).
5. On row: `state`, `source`, `port`, `error.detail` if any.
6. Ignore unknown fields and unknown `error.code` values.

---

## Validation

```bash
# when a validator is installed, e.g. check-jsonschema:
# check-jsonschema --schemafile schemas/status.v1.json examples/status.v1.json
```

Golden file `examples/status.v1.json` must remain valid against `schemas/status.v1.json`.
