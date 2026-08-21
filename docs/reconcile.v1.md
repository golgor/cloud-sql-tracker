# Reconcile — Health state rules v1

**Canonical product contract** for how the control plane maps machine observations to a Connection’s Health state and related Status document fields.

| Artifact | Path |
|----------|------|
| This prose | `docs/reconcile.v1.md` |
| Domain terms | [`CONTEXT.md`](../CONTEXT.md) (Reconcile, Health state, Source, Orphan, Foreign process, …) |
| Status field meanings | [`docs/status-document.v1.md`](./status-document.v1.md) |
| CLI wait flag | [`docs/cli-contract.v1.md`](./cli-contract.v1.md) (`--wait-ms`) |
| Unit / port research | [`docs/research/systemd-user-units.md`](./research/systemd-user-units.md), [`docs/research/port-liveness.md`](./research/port-liveness.md) |
| Wayfinder freeze | [issue #9](https://github.com/golgor/cloud-sql-tracker/issues/9) |

Orphan **match keys** and start/stop behavior toward orphans: [issue #10](https://github.com/golgor/cloud-sql-tracker/issues/10) (consumes “orphan yes/no + pid” as an input here).

---

## Mental model

**Reconcile is read-only classification**, not a desired-state controller and not process takeover.

```text
config identity  +  Unit snapshot  +  port probe  +  listener/Orphan signals  +  clock
        │
        ▼
   reconcile()     ← pure: same inputs ⇒ same outputs (unit-testable)
        │
        ▼
 Health state, Source, pid, port_open, uptime_sec, error → Status document row
```

| Does | Does not |
|------|----------|
| Observe Unit / port / process attribution | Start or stop processes |
| Assign `stopped` \| `starting` \| `running` \| `error` | Run a background loop |
| Fill Source, pid, error, uptime when known | “Adopt” an Orphan into a Unit (lifecycle / #10) |
| Stay free of hidden global CLI state | Require a state file for start deadlines |

Lifecycle commands (`start` / `stop` / `restart`) **change** the world, then may call the same function to **read** it. Plain `status` only reads.

v1 Health is **local** (Unit/Orphan + TCP accept), not Cloud SQL upstream readiness ([ADR 0003](./adr/0003-local-health-signals.md)).

---

## Pure function shape

Normative intent (names may differ in code):

```text
reconcile(connection_identity, observation, now) -> status_row_fields
```

- **No I/O inside** the pure core: callers gather Unit properties, port probe, listener PID, Orphan match, then pass structs in.
- **`now`** is an explicit input (for start-window age), not an implicit `SystemTime::now()` buried in the core — tests inject a fixed clock.
- **`enabled`** from config is **not** an input to Health math (see below).

### Outputs (Status row)

Reconcile is responsible for:

| Field | Rule sketch |
|-------|-------------|
| `state` | Health state enum |
| `source` | `unit` \| `orphan` \| `none` |
| `pid` | MainPID, orphan pid, or `null` |
| `port_open` | boolean (see probe mapping) |
| `uptime_sec` | From unit `ExecMainStartTimestamp` or process start time when known; else `null` |
| `error` | `null` unless `state === "error"`; then `{ code, detail }` |
| `unit` | Expected unit name from Connection id (always when id is valid) |

Identity fields (`id`, `name`, `group`, `instance`, `address`, `port`, `private_ip`) are pass-through from config.

---

## Observation inputs

| Input | Meaning |
|-------|---------|
| **Unit snapshot** | Load/presence, `ActiveState`, `SubState`, `MainPID`, `Result`, `ExecMainStatus` / signal info, `ExecMainStartTimestamp` (or equivalent age) |
| **Port probe** | TCP connect to configured `address:port` → Open / Closed / Unreachable |
| **Listener PID** | Best-effort PID owning the listen socket (may be unknown) |
| **Orphan match** | Boolean + pid: matching Proxy process **not** under our Unit (match algorithm frozen in #10) |
| **Foreign holder** | When port is open and the holder is neither MainPID nor a matching Orphan — treat as Foreign process |
| **Clock** | `now` for age vs start window |

### `port_open` mapping

| Probe | `port_open` |
|-------|-------------|
| Open | `true` |
| Closed | `false` |
| Unreachable (timeout / filtered) | `false` |

Unreachable is rare on loopback; v1 does **not** add a third Health path for it. Detail text may mention a probe timeout only when already in `error` for another reason.

### Holder identity in errors

When reporting `port_in_use` (and similar), put best-effort holder identity in **`error.detail` only** (v1 Status shape stays `{ code, detail }`):

- Prefer: `port 15432 held by docker-proxy (pid 1234)`
- Fallback: `held by pid 1234` / `held by unknown process`

Lookup: listener PID + `/proc/<pid>/comm` (and optionally exe basename). **Never fail Reconcile** if name lookup fails.

---

## Start window (no state file)

`starting` exists without a daemon or deadline file.

**Constant:** `START_WINDOW_MS = 10_000` (10 seconds).

Same numeric default as CLI `--wait-ms` when the flag is omitted ([cli-contract](./cli-contract.v1.md)). `--wait-ms` only controls how long **start/restart block** waiting for `running`; Reconcile uses the constant (and unit timestamps) on every invocation, including plain `status`.

**Not** a per-Connection config field in v1.

A Connection is **inside the start window** when either:

1. Unit `ActiveState` is `activating`, and age since start attempt ≤ `START_WINDOW_MS`, or  
2. Unit is `active` (e.g. `running`), `port_open === false`, and process/unit start age ≤ `START_WINDOW_MS`.

Age comes from systemd start timestamp (or process start time) relative to `now`. Cold start is typically ~1s; 10s is a ceiling, not a deliberate UX delay on the happy path.

After the window, do **not** leave the row in eternal `starting`.

---

## Conflict priority

When signals disagree, apply **highest matching rule** (top wins):

1. **Port held by Foreign process** (or listener PID **known** and ≠ Unit `MainPID` while unit claims the connection) → `error` / `port_in_use`
2. **Unit truly failed** (exec/crash; not clean SIGTERM stop — see below) → `error` / `exec_failed` or `unit_failed`
3. **Unit active (or activating with port already open) + port open + MainPID owns port or listener PID unknown** → `running` / `unit`
4. **No active unit (inactive / not-found / clean failed-as-stopped) + matching Orphan + port open** → `running` / `orphan`
5. **Inside start window + port closed** → `starting`
6. **Past start window, still activating, port closed** → `error` / `start_timeout`
7. **Unit active, port closed, outside start window** → `error` / `unit_failed` (listener not accepting)
8. **Inactive/not-found, port closed, no Orphan** → `stopped` / `none`

### PID attribution soft rule

| Listener PID | Interpretation |
|--------------|----------------|
| Unknown (lookup failed) + unit active + port open | **Trust** unit → `running` / `unit` (do not false-alarm) |
| Known and equals `MainPID` | `running` / `unit` |
| Known and ≠ `MainPID` (Foreign or other proxy) | `error` / `port_in_use` — two owners is not “healthy Orphan while unit active” |
| No unit + Orphan match + port open | `running` / `orphan` |
| No unit + port open + not Orphan | `error` / `port_in_use` |

---

## Truth table (normative sketch)

Unit column is simplified from systemd `ActiveState` (+ failed/clean handling). “Orphan” means match already true (#10). “Start window” as defined above.

| Unit (simplified) | port_open | Process signal | Start window | → state | source | error.code |
|-------------------|-----------|----------------|--------------|---------|--------|------------|
| none / inactive / dead | no | none | — | `stopped` | `none` | — |
| none / inactive | yes | Orphan match | — | `running` | `orphan` | — |
| none / inactive | yes | Foreign / unknown non-orphan | — | `error` | `none` | `port_in_use` |
| activating | no | — | yes | `starting` | `unit` or `none` | — |
| activating | no | — | no (past T) | `error` | `unit` | `start_timeout` |
| activating | yes | MainPID / unknown OK | — | `running` | `unit` | — |
| active | yes | MainPID match or PID unknown | — | `running` | `unit` | — |
| active | yes | listener PID ≠ MainPID | — | `error` | `unit` | `port_in_use` |
| active | no | — | yes | `starting` | `unit` | — |
| active | no | — | no (past T) | `error` | `unit` | `unit_failed` |
| deactivating | yes | — | — | `running` | `unit` | — |
| deactivating | no | — | — | `stopped` | `unit` or `none` | — |
| failed (crash / exec / OOM) | * | — | — | `error` | `unit` or `none` | `unit_failed` / `exec_failed` |
| failed but clean SIGTERM-equivalent + port closed + no process | no | none | — | `stopped` | `none` | — |

No fifth Health state (no `stopping` / `degraded`) in v1.

---

## Error codes produced by Reconcile

Subset of the Status catalog; new codes remain additive for consumers.

| code | When |
|------|------|
| `port_in_use` | Port open but holder is Foreign, or listener PID known ≠ MainPID |
| `start_timeout` | Still `activating` and port closed after start window |
| `unit_failed` | Unit failed unexpectedly, or `active` with port closed outside start window |
| `exec_failed` | Unit/exec failed before a steady proxy process (binary/args) |
| `bin_missing` | When observation/start path surfaces missing proxy binary (may also be start/doctor) |
| `auth` | When control plane can attribute ADC/auth failure (best-effort) |
| `unknown` | Fallback |

`detail` is human-readable and may change wording; `code` is the stable token.

---

## Clean stop vs failed unit

Managed Proxy processes **always** receive `--exit-zero-on-sigterm` on the unit argv we construct, so intentional stops should not look like crashes.

Belt and suspenders:

1. **Stop path (lifecycle, not pure Reconcile):** after `systemctl --user stop`, best-effort `reset-failed` on that unit name.  
2. **Pure Reconcile:** if unit shows `failed` but result is consistent with clean SIGTERM-style termination, port is closed, and no matching process remains → treat as **`stopped`**, not `error`.

True crashes (nonzero exit status, OOM, exec failure) stay `error`.

Orphans / hand-started proxies may lack `--exit-zero-on-sigterm`; stop-by-PID timing is #10.

---

## `enabled: false`

Config rule ([config v1](./config.v1.md)): disabled Connections still appear in the Status document; explicit single-id `start`/`restart` is rejected (exit 2); multi-target start **skips** them.

**Reconcile ignores `enabled`.** Health reflects the real machine. A disabled Connection that still has a Proxy process up is honestly `running` (unit or orphan). Disable gates **start policy**, not truthfulness of status.

---

## Deactivating (stop in progress)

| Unit | port_open | state |
|------|-----------|--------|
| `deactivating` | true | `running` / `unit` (still accepting clients) |
| `deactivating` | false | `stopped` (source `unit` if MainPID still set, else `none`) |

Brief flicker is acceptable; no `stopping` enum value.

---

## Source assignment (typical)

| Situation | source |
|-----------|--------|
| Health from our Unit’s process | `unit` |
| Health from matching Orphan | `orphan` |
| No attributed Proxy process | `none` |
| `port_in_use` with no our proxy | often `none` |
| Unit exists but error without process | `unit` or `none` depending on MainPID |

Source is **orthogonal** to Health state (e.g. `running`+`orphan`, `error`+`unit`).

---

## Relationship to `start` / `--wait-ms`

| Concern | Owner |
|---------|--------|
| Whether to create a Unit / kill a process | `start` / `stop` / #10 |
| How long CLI blocks waiting for `running` | `--wait-ms` (default **10000**) |
| Whether a row is `starting` vs `start_timeout` / `unit_failed` on any command | **This document** + unit timestamps |
| Idempotent start when already `running` (unit or healthy orphan) | CLI contract; classification from Reconcile |

---

## Non-goals (v1)

- Continuous reconcile loop or in-process daemon  
- State file of start deadlines  
- Proxy HTTP `--health-check` / upstream readiness in Health state ([#15](https://github.com/golgor/cloud-sql-tracker/issues/15))  
- Orphan cmdline match algorithm and stop-by-PID details ([#10](https://github.com/golgor/cloud-sql-tracker/issues/10))  
- New Status schema fields for holder pid/comm (detail string only)  
- Per-Connection configurable start timeout in `connections.json`  
- Painting disabled-but-running as automatic `error`

---

## Expected iteration

Truth table and code choice for “active + port closed after a long healthy run” (`unit_failed`) vs rare edge cases may be refined after dogfood. Prefer **additive** codes and clearer `detail` before breaking Status `version`.

---

## Implementer checklist (non-normative)

1. Structs for observation + pure `reconcile` with injected `now`.  
2. Table-driven unit tests per truth-table row (no systemd in the pure tests).  
3. Integration: gather Unit via `systemctl --user show` / D-Bus; port via `connect_timeout`; PID via listeners/procfs.  
4. Unit template: `Type=exec`, forward ADC env, **`--exit-zero-on-sigterm`**, `reset-failed` on stop/restart paths.  
5. Wire `status --json` aggregates only from reconciled `state` values.
