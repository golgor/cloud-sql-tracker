# Rust module seams — v1

**Canonical layout** of deep modules inside the control plane crate so implementation can proceed without redesign.

| Artifact | Path |
|----------|------|
| This prose | `docs/modules.v1.md` |
| Domain terms | [`CONTEXT.md`](../CONTEXT.md) |
| Reconcile (pure truth table) | [`docs/reconcile.v1.md`](./reconcile.v1.md) |
| CLI argv / exits | [`docs/cli-contract.v1.md`](./cli-contract.v1.md) |
| Config | [`docs/config.v1.md`](./config.v1.md) |
| Status JSON | [`docs/status-document.v1.md`](./status-document.v1.md) |
| Doctor | [`docs/doctor.v1.md`](./doctor.v1.md) |
| Logs | [`docs/logs.v1.md`](./logs.v1.md) |
| I/O posture | [`docs/adr/0004-rust-toolchain-and-linux-io.md`](./adr/0004-rust-toolchain-and-linux-io.md) |
| Wayfinder freeze | [issue #14](https://github.com/golgor/cloud-sql-tracker/issues/14) |

This freeze is **module interfaces**, not code. The binary remains a stub until after [#13](https://github.com/golgor/cloud-sql-tracker/issues/13). Product contracts above stay frozen; this document only places them in `src/`.

Use these design terms exactly: **module**, **interface**, **implementation**, **depth**, **seam**, **adapter**, **leverage**, **locality**. Domain terms stay in `CONTEXT.md` (Connection, Reconcile, Unit, Supervisor, Foreign process, …).

---

## Goal

- Callers learn a **small interface**; complexity (truth table, systemd argv, deny-unknown JSON, batch exits) stays inside the owning module.
- Tests hit the **same interface** as callers (`config::parse`, `reconcile`, later `commands`).
- One **adapter** per I/O kind until a second one exists (no speculative traits).

---

## Challenge the RESEARCH sketch

[`docs/RESEARCH.md`](./RESEARCH.md) (~lines 299–318) sketched `lifecycle.rs` + `status.rs` + `proc.rs` + clap in `main.rs`. Do **not** copy it.

| RESEARCH file | Why it is wrong for v1 |
|---------------|------------------------|
| `lifecycle.rs` mixing start/stop **and** Reconcile | Reconcile is **pure** and read-only. Mutating I/O must not sit in the same module. |
| `status.rs` as a public aggregator | Looping `reconcile` + serde is not enough depth for its own public seam. Status assembly lives under `commands`. |
| `proc.rs` “adopt” | **No Orphan adopt.** Port→PID is only for `port_in_use` / Source, not takeover. |
| `systemd.rs` / `proc.rs` as the product | Those are I/O **adapters**. Product depth is Reconcile, config validation, and command policy. |
| clap types everywhere | Argv crate is an implementation detail of `cli`. |

---

## Crate shape

One library crate + thin bin so unit tests do not go through `main`:

```text
src/main.rs                 # bin: std::process::exit(cli::run())
src/lib.rs                  # crate root; no clap *types* outside `cli`

src/cli.rs                  # clap derive, printing, exit mapping
src/model.rs                # domain / JSON DTO types
src/config.rs               # load + validate + defaults merge
src/reconcile.rs            # PURE truth table
src/supervisor.rs           # systemd --user adapter
src/port.rs                 # TCP liveness + listener PID
src/journal.rs              # journalctl adapter
src/env.rs                  # proxy binary PATH + ADC discovery

src/commands.rs             # public command seam (what cli calls)
src/commands/select.rs      # internal: selector expansion + BatchOutcome
src/commands/status.rs      # internal
src/commands/mutate.rs      # internal: start / stop / restart
src/commands/doctor.rs      # internal facade
src/commands/logs.rs        # internal facade
```

Exact file names inside `commands/` may move; the **public seam** does not.

**Visibility:** crate items used across modules are **`pub(crate)`**. This is **not** a public Rust library contract (no semver for `cloud_sql_tracker::…` outside the binary).

`cli` lives in the lib crate so tests can call `cli::run` if needed; **clap types stay inside `cli`**. `main.rs` does not parse argv itself.

**Not in v1 as public modules:** `status`, `lifecycle`, `proc`, `ops`, `trait Supervisor`, `trait Clock`, a second D-Bus supervisor adapter (zbus may replace `systemd-run` **inside** `supervisor` later). Shell completions are **out of CLI contract v1** — do not freeze them here.

---

## Dependency direction (acyclic)

```text
cli  →  commands  →  reconcile     (pure)
                  →  config
                  →  supervisor, port, journal, env
                  →  model

config     → model
reconcile  → model only     (no fs, no systemd, no clap, no clock syscalls)
supervisor / port / journal / env → model (snapshots / check rows / DTOs only)
model      → (almost) nothing
```

**Forbidden:**

- `reconcile` importing `supervisor`, `port`, `cli`, `clap`, `env`, or `std::process`
- clap types (`ArgMatches`, derive `Parser` structs) outside `cli`
- systemd property strings (`ActiveState=…`) leaking into `reconcile` — map in `supervisor` to `UnitSnapshot`
- `commands` depending on clap
- `config` owning selector expansion (`--group` / `--all` / `--failed` / disabled-skip)

---

## Modules

### `model` — types, not behavior

**Interface:** structs/enums named in CONTEXT.md and JSON contracts: `Connection`, `HealthState`, `Source` (`unit` | `none` only), Status document / row, Doctor report / check row, Status `error.code` tokens.

Also the **unit-name rule** (one owner):

```text
unit_name(id) -> UnitName   # `cloud-sql-proxy-<id>.service` (+ sanitizing)
```

`supervisor`, `journal`, Status `unit`, and Reconcile’s expected unit field all use this. Do not reimplement the string in three modules.

**Hides:** serde attributes, JSON field names vs Rust names.

**Depth:** modest / **intentionally shallow** — shared vocabulary, not behavior. Deletion test: types reappear in every module; that is acceptable. Helpers that encode **non-naming rules** belong with the module that owns the rule, not here.

**Not:** Reconcile math, file I/O, clap.

### `config` — deep validation

**Interface (`pub(crate)`):**

| Fn | Role |
|----|------|
| `parse(bytes) -> Result<Config, ConfigError>` | **Pure:** deny-unknown, uniqueness, reserved ports, defaults merge |
| `load(path) -> Result<Config, ConfigError>` | Read file then `parse` |
| `default_path() -> PathBuf` | XDG / `~/.config/cloud-sql-tracker/connections.json` |
| `by_id(cfg, id) -> Option<&Connection>` | Lookup only |

**Hides:** `deny_unknown_fields`, reserved-port table, merge algorithm.

**I/O:** only `load`. Rule tests use `parse` (no temp files).

**Not:** group/`--all` expansion, enabled-skip, batch exits. Doctor uses the same `parse`/`load`; **exit mapping is `cli` only** (doctor treats config failure as check `config` / exit 3; other commands fail-fast exit 2).

### `reconcile` — deepest pure module

**Interface:**

```text
reconcile(identity, observation, now) -> StatusRowFields
```

Matches [`docs/reconcile.v1.md`](./reconcile.v1.md). Same inputs ⇒ same outputs.

- **No I/O.**
- **`now` is a parameter** (timestamp), not `SystemTime::now()` inside the module — no `Clock` trait.
- `enabled` is **not** an input to Health math.

**Hides:** the truth table (Unit + port + listener PID + start window T=10s → Health, Source, `port_in_use`, …).

**Test surface:** table-driven tests from the normative truth table (#13).

**Deletion test:** if deleted, every caller reimplements Health.

### `supervisor` — systemd adapter (one concrete adapter)

**Interface:**

| Fn | Role |
|----|------|
| `show(unit) -> Result<UnitSnapshot>` | Load/missing, mapped ActiveState/SubState, MainPID, Result, **ExecMainStatus / ExecMainCode / signal**, start timestamp (everything Reconcile’s clean-stop vs crash table needs) |
| `start_transient(connection, proxy_bin, env) -> Result<()>` | `Type=exec`, ADC env forward, name `cloud-sql-proxy-<id>.service`, **always** `--exit-zero-on-sigterm` on proxy argv |
| `stop(unit) -> Result<()>` | **Our Unit only** — never kill-by-PID. Best-effort **`reset-failed` after stop** (and on restart) |
| `systemd_user_check() -> CheckRow` | Doctor `systemd_user` (`status` / `detail` / `hint`) |

**Hides:** `systemd-run` / `systemctl --user show` argv (or later zbus), `Type=exec`, `TimeoutStopSec`, unit-name sanitizing.

**Does not** build the Reconcile `Observation`. Returns `UnitSnapshot`; `commands` composes Observation with `port`.

**Seam discipline:** one adapter (real systemd). **No `trait Supervisor`.** Reconcile tests construct `Observation` as structs. If #13 needs an in-process fake for `commands`, add that seam **then**.

### `port` — liveness + attribution

**Interface:**

```text
observe(address, port) -> PortObservation   # probe + listener_pid together
```

`PortObservation`: TCP `Open` / `Closed` / `Unreachable` via `TcpStream::connect_timeout`, plus `listener_pid: Option<Pid>` and best-effort **holder name** (`/proc/<pid>/comm`, optionally exe basename) via procfs/`listeners` — **not** `ss`. Name lookup must never fail the probe (Reconcile `error.detail` only).

**Hides:** timeout, IPv4/IPv6, `/proc` walk.

**Not:** adopt, kill, “is this our binary”. Foreign process on port → Reconcile `error` / `port_in_use`. Doctor `ports` (warn-only) uses this adapter.

No trait.

### `journal` — logs + doctor smoke

**Interface:**

| Fn | Role |
|----|------|
| `dump(unit, lines) -> Result<Dump, JournalError>` | `Dump = Empty \| Bytes` (raw journalctl stdout, **unchanged** when non-empty) |
| `journal_user_check() -> CheckRow` | Doctor `journal_user` |

Argv (hidden): `--user --unit=… --no-pager --quiet -n N -o short-iso`. Capture stdout. Empty = exit 0 from journalctl **and** no non-whitespace bytes.

**Hides:** journalctl chrome, PATH lookup.

Hint text + which stream (stderr vs stdout) is **`cli`**. `commands` returns `Dump`; `cli` prints.

Shell-out is the interface we want ([`docs/logs.v1.md`](./logs.v1.md)). No JSON schema.

### `env` — proxy binary + ADC

Shared owner for PATH / ADC file discovery. Doctor is a **caller**, not a second implementation.

| Fn | Role |
|----|------|
| `resolve_proxy_bin(cfg_or_default) -> Result<PathBuf, ProxyBinError>` | Absolute path for **start** |
| `proxy_bin_check(cfg_or_default) -> CheckRow` | Doctor row, **built on** `resolve_proxy_bin` |
| `adc_status() -> AdcStatus` | `{ present, path, gac_env_set }` for **start** env forwarding |
| `adc_check() -> CheckRow` | Doctor row, **built on** `adc_status` |

**Hides:** `which` / PATH walk, default ADC path `~/.config/gcloud/application_default_credentials.json`.

Doctor does **not** re-implement PATH/ADC; it calls the `*_check` fns. Start calls `resolve_proxy_bin` + `adc_status` (not the CheckRow).

Different I/O than unit lifecycle or journalctl — keep out of `supervisor` / `journal`.

### `commands` — one public command seam

What `cli` calls after argv is parsed. **Internal files** for readability (`select`, `status`, `mutate`, `doctor`, `logs`); not six public modules.

**Interface (`pub(crate)`):**

| Fn | Does |
|----|------|
| `status(cfg, selector) -> Result<StatusDocument, …>` | Per Connection: gather Observation (`supervisor.show` + `port.observe`), `reconcile`, assemble document (`version`, `cli_version` from `CARGO_PKG_VERSION`, aggregates). `status` with no target ⇒ all (CLI contract). |
| `start(cfg, selector, wait_ms) -> BatchOutcome` | Expand selector; skip disabled per config rules; per id: reconcile, maybe start unit, wait window; **non-transactional** |
| `stop(cfg, selector, wait_ms) -> BatchOutcome` | Our Unit only; `reset-failed` via supervisor; already stopped = success no-op |
| `restart(cfg, selector, wait_ms, failed_only) -> BatchOutcome` | After selector expansion, **`--failed` is an error-state filter** (keep Health `error` only — not a fourth selector). Empty after filter = success |
| `doctor(cfg_path) -> DoctorReport` | Orchestrate six checks; **do not fail-fast** on bad config. Still run `proxy_bin`, `systemd_user`, `adc`, `journal_user` if config failed. |

`BatchOutcome` is part of this **public-within-crate** seam (defined next to these fns, not buried as `select`-only).
| `logs(cfg, id, lines) -> Result<Dump, …>` | Facade over `journal::dump` |

**Hides:** `--wait-ms` default **10000**, start-window interaction with Reconcile, selector expansion (`id` / `--group` / `--all` + disabled-skip), `--failed` filter, Observation gather (one **internal** helper used by status and mutate).

**Selector:** wholly in `commands` (`select.rs`). `config` only `by_id`.

**Doctor check owners** (commands only stitches the report):

| Check | Owner |
|-------|--------|
| `config` | `config::load` / `parse` |
| `proxy_bin` | `env` |
| `systemd_user` | `supervisor` |
| `adc` | `env` |
| `journal_user` | `journal` |
| `ports` | **`commands::doctor`** stitches `port::observe` + `supervisor::show` (our Unit `MainPID` vs Foreign holder). `port` owns observation only; never a `bool` “in use?” that ignores the Unit. Warn-only; skip/pass if config not loaded — see doctor.v1. |

Adapters that own a **single** doctor check return a **CheckRow** (`status`, `detail`, `hint`), not a bare `bool`. `ports` is the exception: it needs two adapters, so `commands` owns the row.

### `cli` — thin shell (clap)

**Interface to humans:** argv contract. **Interface to the crate:** none — it is the binary adapter.

- clap **derive** (already chosen).
- Map parsed command → `commands::*`.
- **Only place that chooses process exit codes** `0`/`1`/`2`/`3`/`4`.
- Human vs `--json` printing (never mix on stdout).
- `--version` / Status `cli_version` = `env!("CARGO_PKG_VERSION")` only.
- Logs: `Dump::Empty` → stderr hint, empty stdout, exit 0; `Dump::Bytes` → stdout as-is.

**Must not:** truth table, config uniqueness, systemd argv, PATH/ADC resolution. **Intentionally shallow** (binary adapter).

`src/main.rs` is only:

```rust
fn main() {
    std::process::exit(cli::run());
}
```

---

## Pure vs I/O

| Module | Pure? | Notes |
|--------|-------|--------|
| `model` | yes | types |
| `config::parse` | yes | `load` is I/O |
| `reconcile` | **yes** | clock injected as `now` |
| `supervisor` | no | systemd |
| `port` | no | TCP + procfs |
| `journal` | no | journalctl |
| `env` | no | PATH + ADC files/env |
| `commands` | no | orchestration |
| `cli` | no | argv / stdio |

---

## Traits / adapters

**No public traits in v1** for supervisor, port, journal, env, or clock.

*One adapter means a hypothetical seam. Two adapters means a real one.* Production has one systemd, one kernel, one journalctl, one ADC discovery. Reconcile is tested with structs.

---

## Errors

- I/O modules return **typed** errors (`thiserror` is fine).
- **`cli` only** maps those to process exit codes.
- JSON `error.code` strings on Status rows are **Reconcile / `model`** (contract), not systemd errno.

---

## What this freeze does *not* decide

- Exact Rust type names; `supervisor.rs` vs `supervisor/mod.rs`.
- zbus vs `systemd-run` (internal to `supervisor`).
- `listeners` crate vs hand-rolled procfs (internal to `port`).
- Test pyramid (**#13**).
- Async runtime (v1 is a sync CLI).
- `SOFTWARE_DESIGN.md` (later, after #13 if useful).

---

## Implementer checklist (non-normative)

1. `lib.rs` + modules above; `main.rs` only clap + `exit`.
2. `config::parse` + `reconcile` first (pure, TDD).
3. Adapters (`env`, `supervisor`, `port`, `journal`); `commands::status` wires them.
4. Mutating commands last; **never** start/stop from `reconcile`.
5. Do not add `source: orphan`, adopt, or stop-by-PID.
6. Do not leak clap types out of `cli`.
