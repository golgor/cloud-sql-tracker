# Research: systemd --user transient unit contract for Proxy processes

**Issue:** [#6](https://github.com/golgor/cloud-sql-tracker/issues/6)  
**Map:** [#2](https://github.com/golgor/cloud-sql-tracker/issues/2)  
**Slug:** `research/systemd-user-units`  
**Context:** Rust stateless CLI; Google `cloud-sql-proxy` data plane; `systemd --user` supervisor; Arch/Omarchy.

## Summary

Supervise each Connection’s `cloud-sql-proxy` as a **transient user `.service`** via `systemd-run --user` (or the equivalent `StartTransientUnit` D-Bus call on the user bus), not a scope and not a static unit file. Use **`Type=exec`**, **`KillMode=control-group`**, default **`KillSignal=SIGTERM`**, a generous **`TimeoutStopSec`** (proxy drain window), **no `Restart=`** (CLI owns restart policy), and **do not** pass `--collect` while the control plane still needs failed-state inspection—prefer explicit `reset-failed` on the restart path. Unit names: `cloud-sql-proxy-<sanitized-id>.service` with `<id>` restricted to systemd unit-name chars. Discover state with `systemctl --user show` / D-Bus properties (`MainPID`, `ActiveState`, `SubState`, `Result`, `ExecMainStatus`). Environment is **not** the interactive shell: pass absolute binary path and explicitly forward ADC-related env (`HOME`, and `GOOGLE_APPLICATION_CREDENTIALS` when set).

## Recommendation

### Preferred contract

| Concern | Choice | Why |
| --- | --- | --- |
| Unit kind | Transient **`.service`** under `--user` | Detached from CLI process tree; shows in `list-units`; stop/kill/journal are first-class. Scopes keep the invoker as parent and are a poor fit for a long-lived proxy owned by a short-lived CLI. |
| Start API | `systemd-run --user` **or** D-Bus `StartTransientUnit` on session/user bus | CLI should eventually prefer D-Bus (`zbus`) for structured errors; `systemd-run` is the reference CLI and good for integration tests/docs. |
| Service type | **`Type=exec`** (`--property=Type=exec` or `--service-type=exec`) | `simple` (default) reports success after `fork()` **before** `execve()`; missing binary looks “started”. `exec` only succeeds after successful exec. |
| Collect / GC | **Omit `--collect` by default** | Without collect, failed units stay loaded until `reset-failed`—useful for `Result` / exit status. Use collect only for throwaway probe units. Equivalent property: `CollectMode=inactive-or-failed`. |
| Kill mode | **`KillMode=control-group`** (default) | Ensures all proxy children die on stop. Avoid `process`/`none`. |
| Stop signal | Default **`KillSignal=SIGTERM`** | cloud-sql-proxy drains on SIGTERM; optional `--exit-zero-on-sigterm` and `--max-sigterm-delay` on the proxy argv. |
| Stop timeout | **`TimeoutStopSec=30`** (tunable; ≥ proxy max drain) | After timeout, systemd sends `FinalKillSignal` (SIGKILL). Align with `--max-sigterm-delay` if set. |
| Restart | **`Restart=no`** (default) | Stateless CLI decides when to recreate units; avoids fighting control-plane intent. |
| RemainAfterExit | **no** (default) | Long-running proxy; unit should go inactive/failed when the process exits. |
| Logging | Default journal (`StandardOutput`/`StandardError` inherit manager defaults) | `journalctl --user -u cloud-sql-proxy-<id>.service`. |
| Slice | Optional `app-cloud-sql-tracker.slice` later | Not required for v1; keeps cgroup grouping optional. |

### Recommended unit template (CLI-equivalent)

```bash
# <id> = connection id already sanitized for unit names (see below)
# BINARY = absolute path to cloud-sql-proxy (resolve before start)
# INSTANCE = project:region:instance
# PORT = host listen port chosen by control plane

systemd-run --user \
  --unit="cloud-sql-proxy-${ID}.service" \
  --description="Cloud SQL Auth Proxy (${ID})" \
  --service-type=exec \
  --property=KillMode=control-group \
  --property=TimeoutStopSec=30 \
  --property=Restart=no \
  --expand-environment=no \
  --setenv=HOME="${HOME}" \
  ${GOOGLE_APPLICATION_CREDENTIALS:+--setenv=GOOGLE_APPLICATION_CREDENTIALS="${GOOGLE_APPLICATION_CREDENTIALS}"} \
  -- \
  "${BINARY}" \
    --port="${PORT}" \
    --address=127.0.0.1 \
    ${PRIVATE_IP:+--private-ip} \
    ${EXIT_ZERO:+--exit-zero-on-sigterm} \
    ${EXTRA_ARGS[@]} \
    "${INSTANCE}"
```

Notes:

- Put **`--`** before the proxy argv so systemd-run options cannot swallow proxy flags.
- Prefer **absolute** `BINARY` so user-manager `PATH` (often minimal) cannot miss a user-local install.
- `--expand-environment=no` avoids systemd expanding `$` inside extra args.
- Do **not** use `--scope`, `--wait`, `--pty`, or `--remain-after-exit` for the steady-state proxy.
- Do **not** enable `--quitquitquit` admin server unless the product explicitly needs HTTP shutdown; systemd SIGTERM is enough.

### Equivalent D-Bus sketch (user manager)

Bus: user/session bus → destination `org.freedesktop.systemd1`, path `/org/freedesktop/systemd1`, interface `org.freedesktop.systemd1.Manager`.

```text
StartTransientUnit(
  name  = "cloud-sql-proxy-<id>.service",
  mode  = "fail",          # or "replace" if replacing an existing job deliberately
  properties = [
    ("Description",      "s"  "Cloud SQL Auth Proxy (<id>)"),
    ("Type",             "s"  "exec"),
    ("KillMode",         "s"  "control-group"),
    ("TimeoutStopUSec",  "t"  30_000_000),   # usec; mirrors TimeoutStopSec=30
    ("Restart",          "s"  "no"),
    ("Environment",      "as" ["HOME=...", "GOOGLE_APPLICATION_CREDENTIALS=..."]),
    ("ExecStart",        "a(sasb)" [
       (binary_path, [binary_path, "--port", port, ..., instance], false)
    ]),
  ],
  aux = []
) -> job_path
```

Then:

- `GetUnit("cloud-sql-proxy-<id>.service")` → unit object path  
- On unit object / via `systemctl show`: read `ActiveState`, `SubState`, `MainPID`, `Result`, `ExecMainCode`, `ExecMainStatus`  
- Stop: `StopUnit(name, "replace")`  
- Kill (escalation): `KillUnit(name, "control-group", SIGTERM|SIGKILL)`  
- Clear failed: `ResetFailedUnit(name)` before re-`StartTransientUnit` with the same name  

Property names on the bus use systemd’s D-Bus spelling (e.g. `TimeoutStopUSec`); when shelling out, stick to unit-file names via `systemd-run -p`.

### Unit name pattern and `<id>` restrictions

Pattern:

```text
cloud-sql-proxy-<id>.service
```

systemd unit name rules ([systemd.unit(5)](https://man.archlinux.org/man/systemd.unit.5)):

- Prefix characters: ASCII letters, digits, `:`, `-`, `_`, `.`, and `\`  
- Total name including `.service` ≤ **255** characters  
- No `/`, spaces, `@` in a plain (non-template) name unless using template semantics deliberately  

**Control-plane rules for `<id>`:**

1. Start from the Connection id (UUID or slug).  
2. Allow only `[A-Za-z0-9:_.,-]` after sanitization; map everything else to `-`.  
3. Collapse repeated `-`; trim leading/trailing `.` / `-`.  
4. Reject empty result; enforce `len("cloud-sql-proxy-" + id + ".service") <= 255` (i.e. `id` max ≈ 255 − 28 = **227**).  
5. Prefer opaque UUIDs (`a1b2c3d4-...`) — already valid.  
6. Never embed raw instance connection names (`project:region:instance` is valid charset-wise but long, leaky in `list-units`, and couples naming to GCP topology). Store instance in unit Description and in app state instead.

### Safe argv construction

| Input | How to pass |
| --- | --- |
| Listen port | `--port=<n>` as a separate argv element (control plane allocates). |
| Bind address | Default `127.0.0.1` via `--address=127.0.0.1` unless product says otherwise. |
| Instance | Final positional `INSTANCE_CONNECTION_NAME` (`project:region:instance`). |
| Private IP | Presence flag `--private-ip` from Connection config. |
| Extra args | Opaque `Vec<String>` appended **before** the instance name; no shell; each flag/value its own element. |
| Credentials | Prefer ADC via env; optional `--credentials-file=PATH` only if product stores a key path. Avoid `--gcloud-auth` (legacy). |

Never build a single shell string. Never rely on systemd `$VAR` expansion inside ExecStart for user data.

### Discovery: MainPID / ActiveState

**Shell (good for tests and early CLI):**

```bash
systemctl --user show "cloud-sql-proxy-${ID}.service" \
  -p Id -p LoadState -p ActiveState -p SubState \
  -p MainPID -p Result -p ExecMainCode -p ExecMainStatus \
  -p ExecMainStartTimestamp -p NRestarts \
  --value=false
```

Interpretation sketch:

| ActiveState | SubState (typical) | Meaning for tracker |
| --- | --- | --- |
| `activating` | `start` / `start-pre` | Still starting; wait. |
| `active` | `running` | Proxy up; `MainPID > 0`. |
| `deactivating` | `stop-sigterm` / … | Drain in progress. |
| `failed` | `failed` | Dead after error; inspect `Result`, `ExecMainStatus`. |
| `inactive` | `dead` | Clean stop or never started / already GC’d. |

`LoadState=not-found` (or D-Bus GetUnit failure) ⇒ no such unit (stopped and unloaded, or never created).

**Journal:**

```bash
journalctl --user -u "cloud-sql-proxy-${ID}.service" -n 100 --no-pager
```

### Stop / restart paths

```bash
# Graceful stop (SIGTERM → wait TimeoutStopSec → SIGKILL)
systemctl --user stop "cloud-sql-proxy-${ID}.service"

# After unexpected failure, before starting the same unit name again:
systemctl --user reset-failed "cloud-sql-proxy-${ID}.service"

# Then StartTransientUnit / systemd-run again with the same --unit name.
```

Restart recipe for the CLI:

1. `stop` (ignore not-found).  
2. `reset-failed` (ignore if not failed).  
3. Start transient unit again.  
4. Poll `show` until `active/running` or terminal `failed` / timeout.

If a previous start left the unit **failed** and you skip `reset-failed`, a new start with the same name can fail or leave confusing state—always reset on the restart path.

### Environment inheritance (PATH, HOME, ADC)

Critical fact: **transient user services do not inherit the caller’s interactive shell environment**. They get the **user manager** environment:

- Set at `user@.service` start (PAM `systemd-user`, environment generators, `environment.d`).  
- Typically includes a minimal `PATH`, and `HOME`/`XDG_*` from the user manager context.  
- Does **not** include ad-hoc exports from the current terminal unless copied.

Implications for cloud-sql-proxy + gcloud ADC:

1. Resolve `cloud-sql-proxy` to an **absolute path** in the CLI before start.  
2. Always `--setenv=HOME=...` from the CLI process (ADC default path `~/.config/gcloud/application_default_credentials.json`).  
3. If `GOOGLE_APPLICATION_CREDENTIALS` is set in the CLI environment, forward it with `--setenv`.  
4. Do not assume `gcloud` on `PATH` inside the unit; the proxy uses Google auth libraries + ADC files, not a live `gcloud` CLI, when using ADC.  
5. Optional hardening: document `~/.config/environment.d/*.conf` for users who need global user-service env; still forward explicitly from the CLI for reliability.

### Failure modes: user systemd bus unavailable

| Symptom | Likely cause | CLI behavior |
| --- | --- | --- |
| `Failed to connect to bus: No such file or directory` | No `XDG_RUNTIME_DIR`, user manager not running, or SSH session without working user bus | Hard error: “user systemd unavailable”; hint login session or `loginctl enable-linger $USER` |
| Connection refused / autolaunch fails | Headless/cron context without user manager | Same; do not silently fall back to bare `Command::spawn` in v1 if the product contract is systemd supervision |
| Unit starts then dies at logout | Lingering disabled; last session ended; `KillUserProcesses` | Document linger for “survive logout” use; default Omarchy desktop login usually keeps user manager for the graphical session |
| Properties accepted but limits ignored | Some cgroup knobs on user instances historically weak / delegated | Don’t depend on `MemoryMax=` for correctness of proxy lifecycle |
| `Unit ... already exists` / job fail | Name collision or failed unit still loaded | `stop` + `reset-failed` then retry; use `mode=replace` only with care |

Linger (optional product doc, not required for “while logged into desktop”):

```bash
loginctl enable-linger "$USER"   # user manager at boot; survives logout
loginctl show-user "$USER" -p Linger
```

### cloud-sql-proxy stop semantics (data plane)

- Default stop from systemd: **SIGTERM**.  
- Proxy supports graceful drain; exit status on SIGTERM defaults to **143** unless `--exit-zero-on-sigterm`.  
- `--max-sigterm-delay` / `--min-sigterm-delay` control drain behavior—keep `TimeoutStopSec` **≥** max delay so systemd does not SIGKILL early.  
- Optional admin `/quitquitquit` is an alternate shutdown path; **not** recommended as the primary supervisor contract when systemd already owns lifecycle.

## Rejected alternatives

| Alternative | Why rejected |
| --- | --- |
| **`.scope` via `--scope`** | Invoker remains parent; start is synchronous with systemd-run’s lifetime model; worse fit for CLI that should exit while proxy stays up. |
| **`Type=simple` (default)** | Start success before `execve`; false “up” if binary/args invalid. |
| **`Type=notify` / `notify-reload`** | Proxy does not implement sd_notify readiness. |
| **`Type=oneshot` + RemainAfterExit`** | Wrong lifecycle for a long-running listener. |
| **Static `~/.config/systemd/user/*.service` files** | Stateful on disk; conflicts with stateless CLI and per-Connection dynamic ports/args. Transient units match “no daemon of our own.” |
| **`--collect` always on** | Drops failed units immediately; harder to read `Result` after crash; race with status UX. Prefer explicit GC after the CLI has recorded failure. |
| **`Restart=always` on the unit** | Duplicates control-plane policy; can restart with stale intent after user “stopped” a Connection in app state. |
| **Bare `std::process::Command` without systemd** | Loses cgroup stop, journal correlation, MainPID tracking, and logout/session integration the map chose. |
| **System-level (`--system`) units** | Requires root/polkit; wrong trust boundary for per-user ADC and desktop Omarchy workflow. |
| **Naming unit after instance connection name** | Leaks topology in unit lists; length/encoding hazards; renames on instance move. |
| **Shell-wrapped ExecStart** (`bash -c "..."`) | Injection and quoting hazards; breaks clean MainPID (becomes shell). |

## Concrete control-plane command cheat sheet

```bash
# start (see full template above)
systemd-run --user --unit="cloud-sql-proxy-${ID}.service" --service-type=exec ...

# status snapshot
systemctl --user show "cloud-sql-proxy-${ID}.service" \
  -p ActiveState -p SubState -p MainPID -p Result -p ExecMainStatus

# is-active helper (exit code)
systemctl --user is-active "cloud-sql-proxy-${ID}.service"

# stop
systemctl --user stop "cloud-sql-proxy-${ID}.service"

# clear failed before recreate
systemctl --user reset-failed "cloud-sql-proxy-${ID}.service"

# logs
journalctl --user -u "cloud-sql-proxy-${ID}.service" -f
```

## Risks

1. **User bus / linger:** SSH-only or early-boot contexts without a user manager break the supervisor; product must surface a clear error and optional linger docs.  
2. **SIGTERM exit 143:** Without `--exit-zero-on-sigterm`, clean stops may present as `Result=signal` / non-zero `ExecMainStatus`; map “SIGTERM clean stop” to success in app logic if needed.  
3. **ADC not visible to user manager:** Missing `HOME` / credentials env → proxy fails after “active” briefly; mitigate with explicit `--setenv` and absolute binary.  
4. **Name collisions:** Two Connections sanitizing to the same `<id>` overwrite each other’s unit; enforce unique ids before start.  
5. **Timeout vs drain:** `TimeoutStopSec` too low → SIGKILL mid-drain and possible client errors.  
6. **No readiness protocol:** `Type=exec` only proves exec succeeded, not that the listen socket is bound or SQL Admin API auth worked—CLI may need a short connect/probe or log scrape for true readiness (out of scope for unit contract, but a product gap).  
7. **Resource control on user instances:** Do not rely on MemoryMax/CPUQuota for correctness without verifying delegation on the target Arch/Omarchy kernel/cgroup setup.  
8. **Local machine evidence not executed in this research pass:** Commands above are from upstream/Arch man pages and proxy docs; parent should smoke-test once on Omarchy (`systemd-run --user … cloud-sql-proxy …` + `show`/`stop`).

## Implementation sketch (Rust CLI, non-normative)

```text
fn unit_name(conn_id: &str) -> Result<String> {
    let id = sanitize_unit_id(conn_id)?; // charset + length
    Ok(format!("cloud-sql-proxy-{id}.service"))
}

fn start_proxy(...) {
    // 1. which(binary) -> absolute
    // 2. reset-failed best-effort
    // 3. systemd-run or StartTransientUnit with Type=exec, env HOME[/GAC]
    // 4. poll show until active||failed||timeout
}

fn stop_proxy(unit: &str) {
    // systemctl --user stop / StopUnit
}

fn inspect(unit: &str) -> ProxyStatus {
    // parse ActiveState, MainPID, Result, ExecMainStatus
}
```

Do **not** implement in this research ticket.

## Sources

- Kept: [systemd-run(1) Arch](https://man.archlinux.org/man/systemd-run.1.en) — transient service vs scope, `--collect`, `--service-type`, `--setenv`, `--expand-environment`, linger example.  
- Kept: [systemd.service(5) Arch](https://man.archlinux.org/man/systemd.service.5.en) — `Type=simple` vs `exec`, `TimeoutStopSec`, `RemainAfterExit`, `Restart=`.  
- Kept: [systemd.kill(5) Arch](https://man.archlinux.org/man/systemd.kill.5.en) — `KillMode`, `KillSignal`, `FinalKillSignal`, SIGTERM then SIGKILL.  
- Kept: [systemd.unit(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html) — unit name charset/length; transient path under `$XDG_RUNTIME_DIR/systemd/transient`.  
- Kept: [systemd TRANSIENT-SETTINGS](https://systemd.io/TRANSIENT-SETTINGS/) — which unit settings are valid on transient units.  
- Kept: [org.freedesktop.systemd1](https://manpages.ubuntu.com/manpages/resolute/man5/org.freedesktop.systemd1.5.html) — `StartTransientUnit`, `StopUnit`, `KillUnit`, `ResetFailedUnit`, `MainPID`, `ActiveState`.  
- Kept: [loginctl(1) Arch](https://man.archlinux.org/man/loginctl.1.en) — `enable-linger` for user manager beyond login.  
- Kept: [ArchWiki systemd/user](https://wiki.archlinux.org/title/Systemd/user) — user units do not inherit shell env; `environment.d`.  
- Kept: [cloud-sql-proxy CLI docs](https://github.com/GoogleCloudPlatform/cloud-sql-proxy/blob/main/docs/cmd/cloud-sql-proxy.md) — ports, `--private-ip`, SIGTERM flags, ADC vs `--gcloud-auth`.  
- Kept: [cloud-sql-proxy SIGTERM / exit-zero PR context](https://github.com/GoogleCloudPlatform/cloud-sql-proxy/pull/1870) — exit 143 vs 0 on SIGTERM.  
- Dropped: Random blog/gist static `cloud-sql-proxy.service` examples — system-wide, static files, often v1 flag syntax; not the transient user contract.  
- Dropped: DeepWiki secondary summaries — prefer man pages and dbus XML/man.

## Gaps

1. No live smoke test on this agent host (no shell tool in the research subagent): verify `Type=exec` start failure on bad path, SIGTERM exit fields, and ADC with only forwarded `HOME` on Omarchy.  
2. Exact D-Bus property type strings for `ExecStart` should be confirmed against the running systemd version (`systemctl --version`) when implementing `zbus` bindings.  
3. Whether Omarchy sets `KillUserProcesses=` and default linger was not measured on-device.  
4. Readiness beyond `Type=exec` (TCP probe vs proxy health flags) left for a later ticket.

## Decision gist

**Use per-Connection transient `systemd --user` services named `cloud-sql-proxy-<sanitized-id>.service` with `Type=exec`, cgroup SIGTERM stop, explicit env forwarding, and `reset-failed` on restart; avoid scopes, static units, and default `Type=simple`.**
