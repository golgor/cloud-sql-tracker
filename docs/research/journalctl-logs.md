# Research: journalctl --user access pattern for `logs`

Issue: #8  
Slug: `research/journalctl-logs`  
Scope: v1 `cloud-sql-tracker logs <id>` only (no UI log viewer, no product code).  
Locked context: Rust stateless CLI; Google `cloud-sql-proxy` data plane; `systemd --user` supervisor; Arch/Omarchy.

## Summary

**Recommendation:** implement `logs` as a thin, non-interactive wrapper around:

```bash
journalctl --user --unit=cloud-sql-proxy-<id>.service --no-pager -n <N> -o short-iso
```

Default to a bounded plain-text dump (not follow, not journal JSON). Prefer journald as the sole v1 log store; do **not** dual-write XDG state file logs unless journal access proves broken on target machines. Empty journals (never started / vacuumed / GC’d transient unit) are a normal soft outcome, not a hard dependency failure.

## Question (from #8)

How should `cloud-sql-tracker logs <id>` read logs for a unit on `systemd --user`?

Must cover:

1. Stable `journalctl --user -u cloud-sql-proxy-<id>.service` flags (`-n`, `-o`, follow or not for v1)
2. Whether JSON output from journal is worth it vs plain text for humans
3. Behavior when unit never started / vacuumed / permission denied
4. Whether file logs under XDG state are needed for v1 if journal works

## Recommendation (v1)

### Access pattern

| Item | Choice |
|------|--------|
| Source | User journal only (`--user`) |
| Unit filter | `--unit=cloud-sql-proxy-<id>.service` **with** `--user` (equivalent to `--user-unit=…` on current systemd) |
| Unit name | Same as start path: `cloud-sql-proxy-<id>.service` (sanitize `id` to `[A-Za-z0-9_-]`) |
| Pager | Always `--no-pager` (CLI must not block in `less`) |
| Line bound | `-n` / `--lines=N`; CLI flag `--lines N` (DESIGN); **default 100** (journalctl’s own default is 10 — too small for proxy auth failures) |
| Output | `-o short-iso` for humans (locale-independent timestamps; good for paste into bugs) |
| Follow | **Off by default** for v1; optional later `--follow` / `-f` |
| Quiet chrome | Optional `-q` to hide “Journal begins at …” noise; nice-to-have, not required |
| Boot filter | Do **not** force `-b` in v1 — keep history across reboot if journal retained |
| Privilege | Run as the invoking user; never `sudo` |

Canonical invocation:

```bash
journalctl \
  --user \
  --unit="cloud-sql-proxy-${id}.service" \
  --no-pager \
  --lines="${lines:-100}" \
  --output=short-iso
```

Equivalent older/explicit form (same matches on modern systemd):

```bash
journalctl --user-unit="cloud-sql-proxy-${id}.service" --no-pager -n 100 -o short-iso
```

Prefer `--user --unit=` in code comments and docs because:

- DESIGN/`RESEARCH.md` already document `journalctl --user -u …`
- Current man page states: with `--user`, all `--unit=` args are converted as if `--user-unit=` were used
- systemd commit `52051dd` explicitly made `--user --unit=` mean user-unit matching (the combination users expect)

### CLI surface (v1)

Align with DESIGN:

```text
cloud-sql-tracker logs <id> [--lines N]
```

Suggested behavior:

- Resolve `<id>` from config (unknown id → exit 2, same as other commands).
- Do **not** require the unit to be active.
- Spawn `journalctl` with stdout/stderr inherited (or piped-through unchanged).
- Exit code:
  - `0` — journalctl succeeded (including zero matching lines)
  - `2` — unknown id / bad usage
  - `3` — `journalctl` missing, or clearly not a user-session / journal access hard failure
  - pass through non-zero from journalctl when it is a real failure (after classifying empty vs error)

Optional v1.1 flags (document, do not require for first slice):

```text
cloud-sql-tracker logs <id> [--lines N] [--follow] [--since SPEC] [--output short|short-iso|cat]
```

### Why plain text, not journal JSON, for v1

| Mode | Use in v1? | Why |
|------|------------|-----|
| `short` / `short-iso` | **Yes (default `short-iso`)** | Human terminal; timestamps; no parse step |
| `cat` | Optional later | Message-only; loses time — worse for auth/retry timelines |
| `json` / `json-pretty` | **No as default** | No UI log viewer; noisy for humans; field quirks (non-unique fields → arrays, large fields → null unless `--all`) |
| `json` internal helper | Optional later | Useful only if CLI wants structured “last error lines” inside `status --json` / start failure payloads |

v1 `logs` is a terminal affordance. Keep the CLI stateless: stream journalctl’s text, do not re-encode.

If a future machine API is needed, add `logs --json` that either:

1. runs `journalctl … -o json` and rewrites a **stable** array of `{ts, priority, message, pid}`, or  
2. returns `{ "unit": "…", "lines": ["…"] }` as plain captured text  

Do not expose raw journald schema as the product contract.

### Follow mode

**v1: no follow by default.**

Reasons:

- CLI is short-lived and stateless; DESIGN non-goals include long-lived tracker daemons.
- Bar/plugin is not a log viewer in v1.
- `--follow` implies a long-running child until SIGINT; complicates testing and exit codes.
- journalctl already implies `--lines` when `--follow` is set (tails recent then streams).

Ship dump-only first. If users ask, add `--follow` as exec/inherit of:

```bash
journalctl --user -u cloud-sql-proxy-<id>.service --no-pager -f -o short-iso
```

### XDG file logs

**Not needed for v1** if units log to the journal (systemd default for service stdout/stderr).

DESIGN already lists optional:

`~/.local/state/cloud-sql-tracker/logs/` — “prefer journald”.

Rejected for v1:

- `StandardOutput=append:…` / `StandardError=append:…` dual-write
- app-level log files beside journal

Revisit only if:

- `doctor` finds user journal unreadable on Omarchy/Arch installs, or  
- orphan (non-unit) proxies become common and users need a file fallback

For orphans **not** under systemd: `logs` should print a clear message that journal unit logs exist only for systemd-managed starts; point at `status` / adopt path. Do not invent file capture in v1.

## Concrete command / Rust sketches

### Shell equivalent (operator mental model)

```bash
# last 100 lines
cloud-sql-tracker logs fe-dev

# last 500 lines
cloud-sql-tracker logs fe-dev --lines 500

# manual escape hatch (document in --help)
journalctl --user -u cloud-sql-proxy-fe-dev.service -n 100 -o short-iso --no-pager
```

### Rust v1 (spawn, inherit stdio)

```rust
// Pseudocode — not product code
fn logs(id: &str, lines: u32) -> Result<i32> {
    let unit = format!("cloud-sql-proxy-{id}.service");
    // id already validated / sanitized against config
    let status = std::process::Command::new("journalctl")
        .args([
            "--user",
            "--unit", &unit,
            "--no-pager",
            "--lines", &lines.to_string(),
            "--output", "short-iso",
        ])
        .status()
        .map_err(|e| /* exit 3: journalctl missing or exec failed */ e)?;

    Ok(status.code().unwrap_or(3))
}
```

Notes:

- Prefer **inherit** stdio over capturing for human `logs` (colors may still apply when stdout is a TTY; journalctl colors by priority on TTY).
- If capturing for tests, add `--no-pager` and possibly set env to disable color if needed (`SYSTEMD_COLORS=0` on newer systemd).
- Do **not** pull `libsystemd` / crates that open journal files directly in v1 — `journalctl` matches the existing `systemctl`/`systemd-run` subprocess posture in RESEARCH.md.

### Optional internal “last error snippet” (start failure path)

When `start` fails, a **captured** call is useful:

```bash
journalctl --user -u cloud-sql-proxy-<id>.service --no-pager -n 20 -o cat -q
```

Use `-o cat` here to keep `error.detail` compact. Still journal-backed; no file log.

### systemctl is not enough

`systemctl --user status cloud-sql-proxy-<id>.service` embeds only a short tail. Fine for humans peeking at status; insufficient as the `logs` implementation.

## Behavior matrix

| Situation | journalctl behavior | CLI should |
|-----------|---------------------|------------|
| Unit running, healthy | Prints proxy stdout/stderr lines tagged to user unit | Exit 0; stream text |
| Unit failed (auth/exec) | Still has entries from the failed invocation | Exit 0; show lines (primary debug path) |
| Unit never started | Often **no entries**, exit 0 | Exit 0; if zero bytes/lines, print one CLI note: `No journal entries for cloud-sql-proxy-<id>.service (unit may never have started).` |
| Transient unit collected (`--collect`) after stop | Historical entries often **remain** in journal until vacuum | Show history; do not require unit file to still exist |
| Journal vacuumed / retention expired | Empty match | Same as never started; message may mention vacuum/retention |
| `Storage=volatile` and no persistent user journal | Man page warns `--user` needs persistent storage on some setups; empty or “No journal files were found” | Exit 3 or structured doctor hint; `doctor` should check `journalctl --user -n 0` / presence of user journal |
| Permission denied / wrong user | Uncommon for own user journal; more common if reading another uid | Exit 3; tell user to run as the desktop session user |
| Orphan proxy (no unit) | No `_SYSTEMD_USER_UNIT` match for our name | Explain: logs only for systemd-managed units; stop/start under tracker to get journal |
| `journalctl` not installed | Exec failure | Exit 3 (dependency) |
| Unknown config id | N/A | Exit 2 before calling journalctl |

### Empty output detection

journalctl commonly returns **0** with empty stdout when the filter matches nothing. Do not treat that as exit 3.

Heuristic:

1. Run journalctl.
2. If status ≠ 0 → map to dependency/failure.
3. If status == 0 and captured mode saw no lines → print the friendly empty message (only if CLI captured; if inherit-only, optional second probe with `-n 1 -o cat` is usually unnecessary — operators understand empty dumps).

For inherit-without-capture v1, simplest UX: always run journalctl; document that empty output means no entries. A one-line preface to stderr is optional:

```text
# cloud-sql-proxy-fe-dev.service (journalctl --user)
```

### Persistence on Arch/Omarchy

- Arch defaults to persistent journal (`/var/log/journal/`, `Storage=persistent` effective via package layout).
- User services log to the user journal; `journalctl --user` is the supported read path.
- UIDs &lt; 1000 do not get separate user journals (irrelevant for desktop users).
- `doctor` (later) can verify: `journalctl --user -n 1 --no-pager` succeeds.

## Rejected alternatives

1. **Default `--output=json` for `logs`** — Rejected for v1 humans. Verbose, unstable as a UX contract, unnecessary without a log UI.
2. **Direct journal API (`sd-journal`, `sdjournal` crate, etc.)** — Rejected for v1. Heavier deps, more failure modes, inconsistent with “thin wrapper around systemctl/systemd-run”.
3. **XDG state file logs as primary** (`~/.local/state/.../logs/<id>.log`) — Rejected for v1. Duplicates journal, needs rotation, breaks if process not started with append properties, worse for multi-invocation transient units.
4. **`StandardOutput=file:` / `append:` only (no journal)** — Rejected. Loses `journalctl -u` integration and cgroup-correlated metadata.
5. **`systemctl --user status` as logs** — Rejected. Truncated; not a log API.
6. **Follow-by-default** — Rejected. Conflicts with stateless short-lived CLI.
7. **`--user-unit` without documenting `--user -u`** — Not wrong, but less aligned with DESIGN wording; either works on modern systemd. Pick one in code: `--user` + `--unit`.
8. **Merging system+user journal without `--user`** — Rejected. Wider match surface; user units need user-unit field matching (`_SYSTEMD_USER_UNIT` + `_UID`), which `--user --unit` / `--user-unit` set up correctly.
9. **Requiring unit to be active before logs** — Rejected. Failed starts are exactly when logs matter most.

## Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Older mental model `journalctl --user -u` vs `--user-unit` confusion | Empty logs on very old systemd if combo mishandled | Use current systemd (`--user` converts `--unit`); Arch/Omarchy is new enough; document both |
| Man-page note: `--user` needs persistent `Storage=` | Empty journals on exotic minimal images | Arch OK by default; `doctor` checks; rare on Omarchy desktops |
| Transient `--collect` units disappear from `list-units` | Users think logs are gone | Journal history usually remains; logs by unit **name**, not by live unit file |
| Orphan processes | `logs` empty while port is live | Message + `status` shows orphan; start path should prefer systemd |
| Proxy logs may be sparse at default log level | “Empty” while running | Document `cloud-sql-proxy --debug-logs` via `extra_args` for deep debug; not tracker’s job to enable by default |
| Pager accidents | CLI hangs in less | Always `--no-pager` |
| Color codes in captured logs | Dirty `status` error snippets | Use `-o cat` / `SYSTEMD_COLORS=0` when capturing |
| Vacuum retention | Lost post-mortem after days | Accept journal policy; optional later `--since today`; no private log island in v1 |

## Alignment with existing design docs

- DESIGN CLI: `logs <id> [--lines N]` — keep; no `--json` required in v1.
- DESIGN XDG: optional file logs under state — mark **deferred**; journal primary.
- RESEARCH process model: `journalctl --user -u cloud-sql-proxy-<id>.service` — **confirmed** as correct primary.
- RESEARCH start properties: journal-only stdout/stderr — **confirmed** for v1 (no append files).

## Implementation checklist (for a later code ticket; not this research)

- [ ] Validate id from config; build unit name identically to `start`/`stop`
- [ ] `Command::new("journalctl")` with flags above; default `--lines=100`
- [ ] `--no-pager` always
- [ ] Inherit stdio; no JSON parse
- [ ] Map exec failures → exit 3
- [ ] Help text shows the underlying journalctl escape hatch
- [ ] `doctor` (separate): verify user journal readable
- [ ] Tests: mock/`PATH` stub for journalctl argv assertion; no live journald required in CI

## Sources

- [journalctl(1) — man7](https://man7.org/linux/man-pages/man1/journalctl.1.html) — `--user` / `--unit` / `--user-unit`, `--lines`, `--follow`, `--output`, `--no-pager`
- [journalctl(1) — Arch man](https://man.archlinux.org/man/journalctl.1.html) — same; notes `--user` + persistent storage
- [systemd commit 52051dd](https://github.com/systemd/systemd/commit/52051dd84c45c745ca877d8893be6f71aa27bf97) — `--user --unit=` treated as `--user-unit=`
- [systemd#26742](https://github.com/systemd/systemd/issues/26742) — historical confusion between `--user --unit` and `--user-unit`
- [ArchWiki: systemd/Journal](https://wiki.archlinux.org/title/Systemd/Journal) — Arch persistent journal default
- [ArchWiki: systemd/User](https://wiki.archlinux.org/title/Systemd/User) — `journalctl --user` for user units
- [journald.conf(5)](https://freedesktop.org/software/systemd/man/latest/journald.conf.html) — `Storage=` volatile/persistent/auto
- [Google cloud-sql-proxy cmd docs](https://github.com/GoogleCloudPlatform/cloud-sql-proxy/blob/main/docs/cmd/cloud-sql-proxy.md) — `--debug-logs` when proxy output is too quiet
- In-repo: [docs/DESIGN.md](../DESIGN.md), [docs/RESEARCH.md](../RESEARCH.md) — unit naming, `logs` CLI, prefer journald

## Gaps

- No live `journalctl --user` probe was run in this research agent session (no shell). Parent should smoke-test on Omarchy: start a throwaway `systemd-run --user` unit, confirm `--user -u` returns lines, stop/`--collect`, confirm history still readable.
- Exact journalctl exit codes for “no matches” vs “no journal files” can vary by version; classify empirically in `doctor` tests on the target machine.
- Whether Omarchy customizes `journald.conf` away from Arch defaults was not verified here (low risk).

## Decision gist

**v1 `logs` = `journalctl --user --unit=cloud-sql-proxy-<id>.service --no-pager -n N -o short-iso`; plain text; no follow; no XDG file logs; empty journal is OK.**
