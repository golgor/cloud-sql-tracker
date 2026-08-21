# Logs subcommand — contract v1

**Canonical product contract** for `cloud-sql-tracker logs`.

| Artifact | Path |
|----------|------|
| This prose | `docs/logs.v1.md` |
| Golden plain-text sample | [`examples/logs.v1.txt`](../examples/logs.v1.txt) |
| CLI argv summary | [`docs/cli-contract.v1.md`](./cli-contract.v1.md) |
| Journal research | [`docs/research/journalctl-logs.md`](./research/journalctl-logs.md) |
| Wayfinder freeze | [issue #12](https://github.com/golgor/cloud-sql-tracker/issues/12) |

**No JSON Schema.** v1 `logs` is human-oriented plain text on stdout (optionally piped to `less`). Machine interfaces for the Omarchy plugin remain **`status --json`** and **`doctor --json`** only — the plugin is not intended to parse journal dumps.

---

## Mental model

```text
logs <id> [--lines N]
        │
        ▼
  resolve id from config → unit name cloud-sql-proxy-<id>.service
        │
        ▼
  journalctl --user --unit=… --no-pager --quiet -n N -o short-iso
        │
        ▼
  stdout: journal lines (pass-through)
  stderr: our hints only when useful (empty journal, hard errors)
```

| Is | Is not |
|----|--------|
| Thin dump of **our Unit’s** user-journal lines | Live Health / Status document |
| Short-lived CLI helper for dogfood/debug | Plugin API |
| Read-only | `--follow` stream (v1 out of scope) |
| journald only | XDG file log store / dual-write |

---

## CLI

```text
cloud-sql-tracker logs <ID> [--lines N]
cloud-sql-tracker --config PATH logs <ID> [--lines N]
```

| Item | Rule |
|------|------|
| Target | **Single** Connection `ID` (positional). No `--group` / `--all`. |
| `--lines N` | Max journal lines. Default **`100`**. Must be an integer **≥ 1**. |
| `--follow` | **Not in v1** (map out of scope / stretch). |
| `--json` | **Not supported** on `logs`. |
| Config | Required (fail-fast like other non-doctor commands): missing/invalid → exit **2**. |
| Unit active? | **Not required.** Logs may be read after stop or if never successfully started (may be empty). |
| `enabled: false` | **Allowed** — historical unit journal still useful. |

Unknown `ID` → exit **2** (same family as other commands).

---

## Implementation: shell out to `journalctl`

v1 **spawns** `journalctl` (absolute path resolution via `PATH` is fine). No Rust journal crate and no private log files required.

### Normative argv template

```text
journalctl \
  --user \
  --unit=cloud-sql-proxy-<id>.service \
  --no-pager \
  --quiet \
  -n <N> \
  -o short-iso
```

| Piece | Why |
|-------|-----|
| `--user` | Units are systemd --user |
| `--unit=cloud-sql-proxy-<id>.service` | Same naming as supervisor research / Status `unit` field |
| `--no-pager` | Non-interactive CLI |
| `--quiet` / `-q` | **Required.** Suppress journalctl chrome (`Journal begins at …`, etc.) so empty vs lines is unambiguous |
| `-n <N>` | Bound output; default N=100 (journalctl’s own default 10 is too small for proxy auth noise) |
| `-o short-iso` | Readable timestamps for humans |

`<id>` is the Connection id as used in unit naming (sanitization rules from systemd research: charset/length safe for unit names). Invalid ids are already rejected at config validation time for configured connections.

Do **not** pass `--follow` in v1.

### Process / stdio

- Prefer running `journalctl` with **stdout** connected to the CLI’s stdout (pass-through of journal lines).
- **stderr:** journalctl’s own diagnostics may pass through; the control plane may add **its own** short hints on stderr (see empty journal).
- Do not rewrite or JSON-wrap journal lines on stdout.

---

## Empty journal / never started

Soft success — **not** a hard failure.

Because argv includes **`--quiet`**, journalctl chrome is not part of stdout. **Empty** means: `journalctl` exit 0 **and** captured stdout has **no non-whitespace bytes** (no log lines).

| Condition | Exit | stdout | stderr |
|-----------|------|--------|--------|
| Empty (never started, vacuumed, GC’d transient unit, unit never existed, …) | **0** | **empty** (no chrome, no banner) | Exactly one hint line, e.g. `no journal entries for unit cloud-sql-proxy-<id>.service (never started, vacuumed, or empty)` |
| At least one log line | **0** | those lines, unchanged | **no** control-plane hint |

**How to detect empty:** capture `journalctl` stdout (required for this decision). If empty → print the hint on **stderr**, nothing on stdout. If non-empty → write captured bytes to stdout as-is. Do not treat whitespace-only chrome as “has logs”; with `--quiet` there should be none.

Do **not** put the hint on stdout (keeps `logs id | less` / pipes clean when there *are* lines, and when empty the hint stays on stderr).

---

## Exit codes

| Code | When |
|------|------|
| `0` | `journalctl` succeeded, including **empty** stdout (hint on stderr). Same as CLI table **Success**. |
| `2` | Usage / config: bad argv, unknown id, `--lines` missing/invalid (non-integer or < 1), config load/validate failure. Same as CLI table **Usage / config**. |
| `3` | Dependency: `journalctl` not found/executable, or user journal clearly unusable (doctor `journal_user` / no user session). Same as CLI table **Dependency**. Message should suggest `cloud-sql-tracker doctor`. If `journalctl` exits non-zero after spawn for access/environment reasons, map to **3** (do not pass through raw journalctl codes). |

**Do not use `1` or `4` for `logs`** (those are multi-target / mutating-command batch codes). Canonical table: [`cli-contract.v1.md`](./cli-contract.v1.md) exit codes.

---

## Non-goals (v1)

- `--follow` / streaming  
- `--json` or journal `-o json` as a product API  
- `--group` / `--all`  
- XDG state file logs or dual-write with journald  
- Parsing or summarizing logs for the plugin  
- Showing logs for processes that were never under our Unit (no unit journal) — operator uses `status` / OS tools; leftover proxies are `port_in_use`, not log targets  

Stretch (later maps): `--follow`, `--since`, alternate `-o` formats.

---

## Relationship to other commands

| Command | Role |
|---------|------|
| `status --json` | Health snapshot (plugin) |
| `doctor --json` | Preflight including `journal_user` |
| `logs` | Human dump of unit journal after start/failures |

---

## Golden example

[`examples/logs.v1.txt`](../examples/logs.v1.txt) is an **illustrative stdout-only** transcript (not machine-validated JSON). No header comments in the file — pure sample lines. Timestamps and PIDs are fictional; format matches `-o short-iso` style lines. Caption: `cloud-sql-tracker logs fe-dev --lines 5`.

---

## Implementer checklist (non-normative)

1. Resolve config + id → unit name; build argv as above.  
2. `Command::new("journalctl")` (or absolute); clear env only if needed — usually inherit.  
3. Empty-line hint on stderr; exit 0.  
4. Missing `journalctl` → exit 3 + doctor hint.  
5. Integration smoke on Omarchy: start one unit, `logs`, stop, `logs` still works.  
6. No JSON schema for this command.
