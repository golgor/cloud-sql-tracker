# CLI contract — v1

Frozen user-facing argv, version output, and exit codes for the control plane binary `cloud-sql-tracker`.

| Related | |
|---------|--|
| Status document (JSON body of `status --json`) | [`status-document.v1.md`](./status-document.v1.md) |
| Wayfinder ticket | [#4](https://github.com/golgor/cloud-sql-tracker/issues/4) |
| Design summary | [`DESIGN.md`](./DESIGN.md) |

This document is the **argv contract**. Field meanings inside `status --json` live only in the Status document docs.

---

## Binary identity

- Command name: `cloud-sql-tracker`
- Install: on `PATH` (e.g. `~/.local/bin` or `cargo install`)

---

## Version (single source of truth)

| Surface | Value |
|---------|--------|
| `Cargo.toml` `[package].version` | **Only** place humans bump the release version |
| `cloud-sql-tracker --version` / `-V` | Prints that version as a **bare semver line** |
| Status document `cli_version` | Same string |

### Output format

```text
$ cloud-sql-tracker --version
0.1.0
```

- Exactly one line on stdout, no `v` prefix, no binary name.
- Plugin / scripts: parse the whole line as semver (trim whitespace).
- Implementation: compile-time `env!("CARGO_PKG_VERSION")` (or equivalent). **Do not** maintain a second version constant in source.

### Releases (intent)

- GitHub Releases tag (e.g. `v0.1.0`) should match `Cargo.toml`.
- Preferred flow: one bump of `Cargo.toml` (and lockfile if needed) → tag → release assets. Tools like `cargo release` / release-please that **only touch package version once** are fine.
- Avoid workflows that require editing a separate `VERSION` file or hard-coded string in `main.rs`.

---

## Global options

Available before the subcommand (clap-style global):

| Flag | Meaning |
|------|---------|
| `--config PATH` | Config file override. Default: `$XDG_CONFIG_HOME/cloud-sql-tracker/connections.json` if `XDG_CONFIG_HOME` set, else `~/.config/cloud-sql-tracker/connections.json`. |
| `-h` / `--help` | Help |
| `-V` / `--version` | Bare semver (see above) |

---

## Subcommands (v1)

| Command | Role |
|---------|------|
| `status` | Reconcile and report (Status document with `--json`) |
| `start` | Start target Connection(s) |
| `stop` | Stop target Connection(s) |
| `restart` | Stop then start targets (optional `--failed`) |
| `logs` | Journal dump for one Connection |
| `doctor` | Environment/config sanity checks |

### Not in v1

| Command | Fate |
|---------|------|
| `list` | **Dropped.** Use `status` for runtime view; later `config list` for config inventory. |
| `config …` | Stretch / later map — CLI-mediated config editing |

---

## Target selectors

Used by `status`, `start`, `stop`, `restart`.

Exactly one of:

| Form | Meaning |
|------|---------|
| `ID` | Single Connection id (positional) |
| `--group NAME` | All connections in that group |
| `--all` | Every connection in the config document |

**Mutual exclusion:** id, `--group`, and `--all` cannot combine. Violation → exit `2`.

### Defaults

| Command | Target omitted |
|---------|----------------|
| `status` | Treated as **all** connections (same as `--all`) |
| `start` / `stop` / `restart` | **Error** exit `2` — mass actions require an explicit target (`--all` if intentional) |

Unknown `ID` or unknown `--group` → exit `2`.

---

## Per-command argv

### `status`

```text
cloud-sql-tracker status [--json] [ID | --group NAME | --all]
```

| Flag | Meaning |
|------|---------|
| `--json` | Stdout = one Status document (schema v1). No extra prose on stdout. |

Without `--json`: human-readable summary (table or lines: id, state, port, source, short error). Suitable for terminal dogfood.

Exit `0` even when some connections have `state: error` (errors are data). Exit `2`/`3` for usage or hard dependency failures that prevent producing status.

### `start`

```text
cloud-sql-tracker start [--wait-ms N] <ID | --group NAME | --all>
```

| Flag | Meaning |
|------|---------|
| `--wait-ms N` | Optional max wait for each target to become `running` (port open) before counting that id as failed. Default: **10000** (10s). Same numeric ceiling as Reconcile’s start window; see [`reconcile.v1.md`](./reconcile.v1.md). |

**Idempotency:** if a target is already `running` (managed unit, `source: unit`) → **success no-op** for that id (exit contribution: success). Port held without our unit (`port_in_use`) is **not** a successful no-op — start fails for that id until the operator frees the port.

### `stop`

```text
cloud-sql-tracker stop [--wait-ms N] <ID | --group NAME | --all>
```

**Idempotency:** already `stopped` → success no-op.

### `restart`

```text
cloud-sql-tracker restart [--wait-ms N] [--failed] <ID | --group NAME | --all>
```

| Flag | Meaning |
|------|---------|
| `--failed` | Restrict the selected set to connections currently in Health state **`error`** only. If the selector is empty after filtering, exit `0` (nothing to do). |
| (default) | Restart every connection in the selector (stop then start each). |

Semantics: for each target, `stop` then `start` (no deeper magic). `--failed` is the “repair the broken ones without cycling healthy proxies” switch.

### `logs`

```text
cloud-sql-tracker logs <ID> [--lines N]
```

| Flag | Default | Meaning |
|------|---------|---------|
| `--lines N` | `100` | Journal line count (integer ≥ 1) |

- Single id only (no `--group` / `--all` in v1).
- No `--follow` in v1. No `--json` on `logs` (plugin uses `status` / `doctor` JSON only).
- Stdout: plain journal text via `journalctl --user` (see [`logs.v1.md`](./logs.v1.md)).
- Empty journal → exit `0` + short **stderr** hint (stdout empty). Missing `journalctl` / unusable user journal → exit `3`. Same `0`/`2`/`3` family as the [exit code table](#exit-code-table).
- Full behavior, argv template, exit codes: [`docs/logs.v1.md`](./logs.v1.md). Sample transcript: [`examples/logs.v1.txt`](../examples/logs.v1.txt).

### `doctor`

```text
cloud-sql-tracker doctor [--json]
```

- Always runs full environment/config checks (no target selector).
- `--json`: one **Doctor report** on stdout — contract: [`doctor.v1.md`](./doctor.v1.md), [`schemas/doctor.v1.json`](../schemas/doctor.v1.json).
- Without `--json`: human checklist (same checks).
- Exit: `0` if no hard failures (`ok: true`, warns allowed); `2` usage only; `3` if any check fails (`status: "fail"`, including invalid config discovered *as* the `config` check). Doctor does **not** fail-fast on bad config before the checklist — see doctor.v1.md.

---

## Multi-target execution and exit codes

### No transactional rollback

`start --all` / `stop --group` / etc. apply **per Connection independently**.

- If 6 of 7 starts succeed and one fails: the 6 **stay** started. There is **no** automatic stop of successes.
- Exit code reflects the **batch result**, not a rolled-back world.

### Exit code table

| Code | Name | When |
|------|------|------|
| `0` | Success | All requested work succeeded, including idempotent no-ops. `status` produced a document. `restart --failed` with zero matches. `logs` with journalctl success (including **empty** journal). |
| `1` | Partial failure | Multi-target batch: **at least one** target succeeded and **at least one** failed. Survivors keep their new state. **Not used by `logs`** (single-id only). |
| `2` | Usage / config | Bad argv, missing/invalid config file, unknown id/group, mutual exclusion violation, invalid `--lines`. |
| `3` | Dependency | Cannot operate: e.g. no systemd user bus, proxy binary unresolved when required for the whole command, **`journalctl` missing or user journal unusable** (`logs`). Prefer `3` when failure is environmental rather than per-id. |
| `4` | Total failure | Every target in the batch failed, **or** a single-id mutating command failed after attempting the operation. **Not used by `logs`** (empty journal is `0`; journal access problems are `3`). |

#### Examples

| Scenario | Exit |
|----------|------|
| `start --all`, 7/7 ok | `0` |
| `start --all`, 6 ok 1 fail | `1` (6 remain up) |
| `start --all`, 0 ok 7 fail | `4` |
| `start fe-dev` fails | `4` |
| `start fe-dev` already running | `0` |
| `start nope` unknown id | `2` |
| `status --json` with 2 connections in error | `0` |
| `start --all` but no user systemd | `3` |
| `logs fe-dev` with lines or empty journal | `0` |
| `logs nope` unknown id / `--lines 0` | `2` |
| `logs fe-dev` but `journalctl` missing / user journal unusable | `3` |

### Stdio conventions

| Stream | Use |
|--------|-----|
| stdout | Human summary **or** pure JSON when `--json` (never mix) |
| stderr | Warnings, per-id errors, diagnostics |

---

## Plugin minimum surface

The Omarchy plugin must only rely on:

```text
cloud-sql-tracker --version
cloud-sql-tracker [--config PATH] status --json
cloud-sql-tracker start  <ID | --group NAME | --all>
cloud-sql-tracker stop   <ID | --group NAME | --all>
```

Optional later: `restart`, `restart --failed`, `logs`, `doctor`.

---

## Human output (required in v1)

The CLI is not JSON-only. Terminal dogfood requires readable `status`, `start`/`stop`/`restart` progress lines, `doctor` text, and `logs` text. Exact table layout is implementation detail; stability guarantee is **JSON + exit codes + argv**, not column widths.

---

## Implementation notes (non-normative)

- Parse with `clap` (derive), version from `CARGO_PKG_VERSION`.
- Prefer stable flag names above; aliases only if documented here first.
- Shell completion is optional and out of contract v1.
