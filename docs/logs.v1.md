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
  journalctl --user --unit=… --no-pager -n N -o short-iso
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
  -n <N> \
  -o short-iso
```

| Piece | Why |
|-------|-----|
| `--user` | Units are systemd --user |
| `--unit=cloud-sql-proxy-<id>.service` | Same naming as supervisor research / Status `unit` field |
| `--no-pager` | Non-interactive CLI |
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

| Condition | Exit | stdout | stderr |
|-----------|------|--------|--------|
| `journalctl` exits 0 with **no** matching lines (never started, vacuumed, GC’d transient unit, …) | **0** | empty (or only journalctl chrome if any) | One short hint, e.g. `no journal entries for unit cloud-sql-proxy-<id>.service (never started, vacuumed, or empty)` |
| `journalctl` exits 0 with lines | **0** | those lines | no extra success noise |

Implementers may detect “zero lines” by capturing stdout when a hint is required, or by equivalent means — keep stdout free of control-plane banners when lines exist so `logs id | less` stays clean.

---

## Exit codes

| Code | When |
|------|------|
| `0` | `journalctl` succeeded, including **zero** matching lines |
| `2` | Usage / config: bad argv, unknown id, `--lines` missing/invalid (non-integer or &lt; 1), config load/validate failure |
| `3` | Dependency: `journalctl` not found/executable, or user journal clearly unusable (same class as doctor `journal_user` / no user session). Message should suggest `cloud-sql-tracker doctor`. |
| other | If `journalctl` fails for a real error after spawn, map to **3** when it is environmental/access; otherwise treat as command failure — prefer **3** for journal access problems so scripts can distinguish from usage (**2**). Do **not** use exit **1**/**4** multi-target batch codes (logs is single-id only). |

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
