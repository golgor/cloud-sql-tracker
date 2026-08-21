# Reconcile — Health state rules v1

**Canonical product contract** for how the control plane maps machine observations to a Connection’s Health state and related Status document fields.

| Artifact | Path |
|----------|------|
| This prose | `docs/reconcile.v1.md` |
| Domain terms | [`CONTEXT.md`](../CONTEXT.md) (Reconcile, Health state, Source, Foreign process, …) |
| Status field meanings | [`docs/status-document.v1.md`](./status-document.v1.md) |
| CLI wait flag | [`docs/cli-contract.v1.md`](./cli-contract.v1.md) (`--wait-ms`) |
| Unit / port research | [`docs/research/systemd-user-units.md`](./research/systemd-user-units.md), [`docs/research/port-liveness.md`](./research/port-liveness.md) |
| Wayfinder freeze | [issue #9](https://github.com/golgor/cloud-sql-tracker/issues/9) |

**v1 does not adopt Orphans.** A Proxy process (or any other process) on the configured port **without** our Unit is always a conflict (`error` / `port_in_use`), never `running`. See [Non-goals](#non-goals-v1).

---

## Mental model

**Reconcile is read-only classification**, not a desired-state controller and not process takeover.

```text
config identity  +  Unit snapshot  +  port probe  +  listener PID  +  clock
        │
        ▼
   reconcile()     ← pure: same inputs ⇒ same outputs (unit-testable)
        │
        ▼
 Health state, Source, pid, port_open, uptime_sec, error → Status document row
```

| Does | Does not |
|------|----------|
| Observe Unit / port / listener PID | Start or stop processes |
| Assign `stopped` \| `starting` \| `running` \| `error` | Run a background loop |
| Fill Source (`unit` \| `none`), pid, error, uptime when known | Treat non-unit proxies as healthy / adopt into systemd |
| Stay free of hidden global CLI state | Require a state file for start deadlines |

Lifecycle commands (`start` / `stop` / `restart`) **change** the world, then may call the same function to **read** it. Plain `status` only reads.

v1 Health is **local** (our Unit + TCP accept), not Cloud SQL upstream readiness ([ADR 0003](./adr/0003-local-health-signals.md)).

---

## Pure function shape

Normative intent (names may differ in code):

```text
reconcile(connection_identity, observation, now) -> status_row_fields
```

- **No I/O inside** the pure core: callers gather Unit properties, port probe, listener PID, then pass structs in.
- **`now`** is an explicit input (for start-window age), not an implicit `SystemTime::now()` buried in the core — tests inject a fixed clock.
- **`enabled`** from config is **not** an input to Health math (see below).

### Outputs (Status row)

Reconcile is responsible for:

| Field | Rule sketch |
|-------|-------------|
| `state` | Health state enum |
| `source` | `unit` \| `none` only |
| `pid` | Unit `MainPID` when known and relevant; else `null` |
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
| **Listener PID** | Best-effort PID owning the listen socket (may be unknown) — for MainPID match and `port_in_use` detail |
| **Clock** | `now` for age vs start window |

No cmdline / instance “Orphan match” input in v1.

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

Lookup: listener PID + `/proc/<pid>/comm` (and optionally exe basename). **Never fail Reconcile** if name lookup fails. A leftover `cloud-sql-proxy` from an old script is still `port_in_use` — name in detail only.

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

## Authority: truth table vs priority summary

| Artifact | Role |
|----------|------|
| **Truth table** (next section) | **Normative** for implementers and tests — single source of row outcomes |
| **Conflict priority** (below) | **Derived reading guide** only — same rules, ordered for humans when signals disagree |

If prose and table ever drift, **fix the table** and then reword the summary. Do not maintain two independent rule sets.

### Conflict priority (summary of the table)

When scanning observations by hand, highest matching idea wins:

1. Port open without our healthy unit ownership (Foreign / mismatched / no unit) → `error` / `port_in_use`
2. True unit failure (not clean stop) → `error` / `exec_failed` or `unit_failed`
3. Unit up + port open + PID OK/unknown → `running` / `unit`
4. Start window + port closed → `starting`
5. Activating past window, port closed → `start_timeout`
6. Active past window, port closed → `unit_failed`
7. Idle (inactive, port closed) → `stopped`

### PID attribution (also in the table)

| Listener PID | Interpretation |
|--------------|----------------|
| Unknown + unit active + port open | Trust unit → `running` / `unit` |
| Equals `MainPID` | `running` / `unit` |
| Known ≠ `MainPID` | `error` / `port_in_use` |
| No unit (inactive/not-found) + port open | `error` / `port_in_use` (any holder, including cloud-sql-proxy) |

---

## Truth table (normative)

Unit column is simplified from systemd `ActiveState` (+ failed/clean handling). “Start window” as defined above. **This table is authoritative.**

| Unit (simplified) | port_open | Process signal | Start window | → state | source | error.code |
|-------------------|-----------|----------------|--------------|---------|--------|------------|
| none / inactive / dead | no | none | — | `stopped` | `none` | — |
| none / inactive | yes | any holder (known or unknown) | — | `error` | `none` | `port_in_use` |
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
| `port_in_use` | Port open but not owned by our Unit (no unit, Foreign holder, or listener PID ≠ MainPID) |
| `start_timeout` | Still `activating` and port closed after start window |
| `unit_failed` | Unit failed unexpectedly, or `active` with port closed outside start window |
| `exec_failed` | Unit/exec failed before a steady proxy process (binary/args) |
| `bin_missing` | When observation/start path surfaces missing proxy binary (may also be start/doctor) |
| `auth` | When control plane can attribute ADC/auth failure (best-effort) |
| `unknown` | Fallback |

`detail` is human-readable and may change wording; `code` is the stable token.

---

## Clean stop vs failed unit

Managed Proxy processes **always** receive `--exit-zero-on-sigterm` on the unit argv we construct, so intentional stops should exit **0** and usually leave the unit `inactive`/`dead` rather than `failed`.

### Lifecycle (not pure Reconcile)

After `systemctl --user stop` (or equivalent): best-effort **`reset-failed`** on that unit name so a later `status` sees `not-found`/`inactive` instead of a sticky `failed` row.

**`stop` only stops our Unit** — it does **not** SIGTERM/SIGKILL Foreign processes. Clearing a leftover listener is manual (pid in `error.detail` / doctor).

### Pure Reconcile — when `ActiveState=failed` still means `stopped`

Use this only as a **fallback** if `reset-failed` was skipped or raced. Classify as **`stopped` / `source: none`** when **all** of the following hold:

1. `port_open === false`
2. No live process under our Unit (`MainPID` is 0/absent)
3. Unit properties match a **clean termination** pattern (any one branch):

| Branch | systemd fields (typical) | Meaning |
|--------|--------------------------|--------|
| A — preferred with our argv | `Result=success`, or `Result=exit-code` and `ExecMainStatus=0` | Proxy honored `--exit-zero-on-sigterm` |
| B — signal stop without exit-zero | `Result=signal` and `ExecMainStatus=15` (**SIGTERM**) | Killed by default `KillSignal` |
| C — proxy default SIGTERM exit | `Result=exit-code` and `ExecMainStatus=143` (128+15) | Process exited 143 after SIGTERM |

Also require `ExecMainCode` consistent with exit/killed (implementers: treat unknown code + the Result/Status pairs above as sufficient when in doubt).

### Still `error` (not clean stop)

| Pattern | Typical fields | code |
|---------|----------------|------|
| Exec / binary failure | `Result=exit-code` or `exec-condition` / failed before running; often `ExecMainStatus≠0` early | `exec_failed` |
| Crash / nonzero exit | `Result=exit-code`, `ExecMainStatus` not in `{0, 143}` | `unit_failed` |
| SIGKILL / stop timeout | `Result=signal`, `ExecMainStatus=9` (**SIGKILL**), or `Result=timeout` | `unit_failed` |
| OOM / core | `Result=core-dump` or oom-adjacent | `unit_failed` |

Do **not** treat SIGKILL (9) as clean stop: that usually means stop timeout or manual kill -9.

---

## `enabled: false`

Config rule ([config v1](./config.v1.md)): disabled Connections still appear in the Status document; explicit single-id `start`/`restart` is rejected (exit 2); multi-target start **skips** them.

**Reconcile ignores `enabled`.** Health reflects the real machine. A disabled Connection whose Unit is still up is honestly `running` / `unit`. Disable gates **start policy**, not truthfulness of status.

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
| No attributed managed process | `none` |
| `port_in_use` (no our unit owning the port) | `none` |
| Unit exists but error without process | `unit` or `none` depending on MainPID |

Source is **orthogonal** to Health state (e.g. `running`+`unit`, `error`+`none`).

---

## Relationship to `start` / `--wait-ms`

| Concern | Owner |
|---------|--------|
| Whether to create / stop a Unit | `start` / `stop` / `restart` |
| How long CLI blocks waiting for `running` | `--wait-ms` (default **10000**) |
| Whether a row is `starting` vs `start_timeout` / `unit_failed` on any command | **This document** + unit timestamps |
| Idempotent start when already `running` | Only managed `running` (`source: unit`); port conflict is **not** success |
| Port held without our unit | `start` fails for that id (`port_in_use`); operator frees the port manually |

---

## Non-goals (v1)

- Continuous reconcile loop or in-process daemon  
- State file of start deadlines  
- Proxy HTTP `--health-check` / upstream readiness in Health state ([#15](https://github.com/golgor/cloud-sql-tracker/issues/15))  
- **Orphan happy path:** cmdline match, `source: orphan`, `running` without Unit, start no-op on foreign proxy, stop-by-PID / adopt-into-systemd  
- New Status schema fields for holder pid/comm (detail string only)  
- Per-Connection configurable start timeout in `connections.json`  
- Painting disabled-but-running as automatic `error`  
- CLI killing Foreign processes on `stop`

Migration from old hand-started proxies: stop those processes once manually (pid often in `error.detail`), then `start` under systemd.

---

## Expected iteration

Truth table and code choice for “active + port closed after a long healthy run” (`unit_failed`) vs rare edge cases may be refined after dogfood. Prefer **additive** codes and clearer `detail` before breaking Status `version`. Re-introducing Orphan support would be an explicit product decision (new ticket), not a silent implementer choice.

---

## Implementer checklist (non-normative)

1. Structs for observation + pure `reconcile` with injected `now`.  
2. Table-driven unit tests per truth-table row (no systemd in the pure tests).  
3. Integration: gather Unit via `systemctl --user show` / D-Bus; port via `connect_timeout`; listener PID via listeners/procfs for match + detail.  
4. Unit template: `Type=exec`, forward ADC env, **`--exit-zero-on-sigterm`**, `reset-failed` on stop/restart paths.  
5. Wire `status --json` aggregates only from reconciled `state` values.  
6. Do **not** implement Orphan match or kill-by-PID in v1.
