# Research: Omarchy Cloud SQL Tracker Architecture

Architecture research brief that informed the design grill. Product decisions that stuck are summarized in [DESIGN.md](./DESIGN.md); this file keeps the fuller tradeoff trail (CLI vs QML vs systemd, Rust vs Go, cloud-sql-proxy gotchas).

## Recommendation

Use a **stateless Rust companion CLI** that the Quickshell/Omarchy bar plugin drives via short-lived process invocations (`status --json` poll + `start`/`stop` actions), and supervise each `cloud-sql-proxy` as a **named transient `systemd --user` service** created with `systemd-run --user`. Do **not** put process lifecycle inside QML, and do **not** run a long-lived tracker daemon for v0. One proxy process per configured connection (fixed non-default local port + instance connection name), with config at `~/.config/cloud-sql-tracker/connections.json`. The CLI owns start/stop/adopt/status aggregation; the bar only renders JSON. Rust matches the user's preference, keeps a small fast binary for frequent polling, and is a good fit for procfs/port checks and clean subprocess orchestration on Arch/Omarchy.

## Alternatives rejected and why

1. **Pure QML `Process` management of proxies** — Rejected. Quickshell can run external commands and parse JSON well, but QML is a bad process supervisor: shell restart kills children tied to the bar, detach/adopt/pid races are painful in QML, and lifecycle logic belongs outside UI code. Community patterns poll external CLIs (`Process` + `JSON.parse` / `JsonPoll`), not supervise multi-hour sidecars in-process.

2. **Static/template systemd user units only (no CLI)** — Rejected as the sole interface. Persistent `~/.config/systemd/user/*.service` units are excellent for always-on services, but a bar dropdown over ~7 optional, group-toggled proxies wants dynamic start/stop, JSON status, adopt-orphans, and a single config file. Generating units is possible later; v0 should wrap `systemd-run` + `systemctl --user` behind a CLI contract the plugin can call.

3. **Long-lived tracker daemon + Unix socket IPC (cliamp-style)** — Rejected for v0. Cliamp needs a daemon because it owns playback state. Here the real long-lived processes are the proxies (or their systemd units). A tracker daemon adds crash surface, socket path management, and “who supervises the supervisor?” without buying much for ~7 processes and 1–2s poll intervals.

4. **Double-fork / nohup / raw setsid daemons + pidfiles as primary** — Rejected as primary supervision. Classic daemonization works but reimplements what user systemd already does: cgroup tracking, journal logs, SIGTERM on stop, surviving compositor/shell restart, and clean unit naming. Pidfiles alone race; cmdline matching alone collides. Prefer systemd as source of truth, with proc/port checks as secondary adopt/health signals.

5. **One multi-instance `cloud-sql-proxy` process for all connections** — Rejected for this UX. v2 supports multiple instances and per-instance `?port=` query params, but the bar needs independent start/stop and per-connection health/error by fe/backend/iot group. One process per connection maps cleanly to units and failure isolation.

6. **Bash/Python as the companion** — Rejected as the maintained end state (fine for a throwaway prototype). Bash is what is being retired; brittle JSON, weak structured error handling. Python is fine operationally but heavier cold start for bar polls and weaker single-binary distribution than Rust/Go on Arch.

7. **Go over Rust** — Acceptable alternative, not preferred here. Go is excellent for CLIs and process control; cloud-sql-proxy itself is Go. For a tiny local supervisor with frequent `status --json` polls, Rust’s smaller stripped binary and user preference win. Startup latency is negligible either way at human/UI timescales.

## Architecture (concrete)

```
┌─────────────────────────┐     spawn/poll      ┌──────────────────────────┐
│ Omarchy Quickshell bar  │ ──────────────────► │ cloud-sql-tracker (Rust) │
│ dropdown widget (QML)   │ ◄──── JSON/status ──│ stateless per invocation │
└─────────────────────────┘                     └────────────┬─────────────┘
                                                             │
                    systemd-run / systemctl --user / busctl
                                                             ▼
                                              ┌──────────────────────────────┐
                                              │ systemd --user               │
                                              │ cloud-sql-proxy@<id>.service │
                                              │ (transient, one per conn)    │
                                              └──────────────┬───────────────┘
                                                             │
                                                             ▼
                                              cloud-sql-proxy <instance>?port=N
                                              (+ optional --private-ip, etc.)
```

**Split of responsibilities**

| Layer | Owns |
|-------|------|
| QML plugin | UI, grouping, poll timer, buttons → CLI |
| CLI | Config load, unit naming, start/stop/adopt, status JSON, exit codes |
| systemd --user | Process lifetime across shell restart, logs, signals |
| cloud-sql-proxy | Auth, TLS to Cloud SQL, local listen port |
| DBeaver | DB credentials/connection strings (unchanged) |

**Why CLI + JSON is the right split for this use case**

- Matches established Quickshell patterns (external command → stdout JSON → bind UI).
- Proxies survive Omarchy/Quickshell restart because they are children of `user@.service`, not the bar.
- Plugin can “adopt” by calling `status --json`, which reconciles systemd units + listening ports + leftover processes.
- Retires one-liner bash scripts behind one stable contract.

## Language choice

**Rust (recommended)**

| Criterion | Rust | Go | Bash/Python |
|-----------|------|-----|-------------|
| Binary size (stripped small CLI) | Typically smaller (~1–3MB class) | Larger defaults (~5–15MB) | N/A / interpreter |
| Status poll startup | Fine (ms) | Fine (ms) | Python slower/heavier |
| Arch distro | `cargo build --release`, optional AUR later | same | already there |
| JSON + clap UX | Excellent | Excellent | awkward / ok |
| Spawn + signal + `/proc` | Excellent | Excellent | ok |
| User preference | Preferred | Available | retiring bash |
| Pidfile/locking | nix/fs2 patterns; less needed if systemd is SoT | go-daemon etc.; less needed | flock hacks |

**Rationale:** tiny local supervisor, frequent polls, no cloud RPC in the tracker itself, user already leans Rust. Go remains plan B if the implementer wants faster coding of subprocess/systemd wrappers.

**v0 dependency posture:** `clap`, `serde`/`serde_json`, `which` or manual path search, thin wrappers around `systemctl`/`systemd-run` (avoid deep zbus unless needed). Optional later: direct D-Bus for lower latency.

## Process lifecycle sketch

### Unit and process model

- **One connection → one transient user service**, name derived from stable config `id` (not display name):
  - `cloud-sql-proxy-<id>.service` (sanitize id to `[A-Za-z0-9_-]`)
- Prefer **service** units over **scope** units so `systemctl --user stop` tracks the main PID cleanly.
- Create with `systemd-run --user`:
  - `--unit=cloud-sql-proxy-<id>.service`
  - `--collect` (GC failed/inactive units; avoid unit clutter)
  - `--property=Type=exec` (start fails if binary missing / exec fails; default `simple` can report success before `execve`)
  - `--property=Restart=no` (bar/CLI decides restarts; avoid surprise flaps on auth errors)
  - `--property=StandardOutput=append:…` and `StandardError=append:…` **or** journal-only (`journalctl --user -u …`)
  - Working directory irrelevant; pass full absolute path to `cloud-sql-proxy` when possible
- **Do not** enable lingering units for login-boot autostart unless the user explicitly wants “always up”; default is on-demand from the bar.

### Start sequence (`tracker start <id>|--group <g>|--all`)

1. Load `connections.json`; resolve target set.
2. For each target, run reconcile/adopt first (see below). If already **running**, no-op success.
3. Preflight:
   - `cloud-sql-proxy` on `PATH` (or configured `bin`)
   - local port free (`ss`/`netstat`/bind probe on `127.0.0.1:port`)
   - optional: config flags present (`private_ip`, extra args)
4. Mark logical state **starting** (status can show this if a small state file is written under runtime dir; optional for v0 if start is synchronous enough).
5. `systemd-run --user --unit=… --collect --property=Type=exec -- …/cloud-sql-proxy [flags] 'project:region:instance'`
   - Prefer **separate process per connection** with `--port <fixed>` (not multi-instance increment).
   - Equivalent query form also valid: `instance?port=55432` (and `?address=127.0.0.1`, `?private-ip=true` as needed).
6. Wait for readiness (bounded, e.g. 3–10s):
   - unit active **and**
   - TCP listen on configured host/port **and/or**
   - optional: if started with `--health-check` and a unique `--http-port`, GET `/startup` then `/readiness`
7. On failure: capture last log lines, set **error** with reason (`bin_missing`, `port_in_use`, `exec_failed`, `auth`, `timeout`, `unit_failed`), ensure unit stopped/reset-failed.

### Detach model

| Method | Use? | Notes |
|--------|------|-------|
| `systemd-run --user` transient service | **Yes (primary)** | Survives shell restart; journal; clean stop |
| double-fork + setsid | No (primary) | Reinvents systemd; harder adopt |
| `nohup` | No | Weak supervision |
| QML parent process | No | Dies with bar/shell |
| Persistent unit files | Optional later | Good for autostart subsets |

### Stop sequence (`tracker stop …`)

1. `systemctl --user stop cloud-sql-proxy-<id>.service`
2. systemd sends **SIGTERM** to the main process; proxy supports graceful drain via `--max-sigterm-delay` / `--min-sigterm-delay` if ever needed (default usually fine for desktop).
3. If unit stuck past timeout (e.g. 5–10s): `systemctl --user kill -s SIGKILL …` or `stop` force path.
4. Alternative if you opted into admin server: start proxy with `--quitquitquit` and unique `--admin-port`, then `cloud-sql-proxy shutdown --admin-port …`. **Not required** if systemd is PID owner; adds port bookkeeping. Skip for v0 unless you want HTTP quit without going through systemd.

### Adopt / reconcile (critical for shell restart)

On every `status` and before `start`/`stop`:

1. **systemd**: `systemctl --user show cloud-sql-proxy-<id>.service -p ActiveState -p SubState -p MainPID -p Result`
2. **Port**: is `127.0.0.1:port` listening? which PID?
3. **Proc cmdline**: if PID known, read `/proc/<pid>/cmdline`; require `cloud-sql-proxy` and instance connection name (and ideally port) match.
4. Cases:
   - unit active + port listen + cmdline match → **running**
   - unit active + no listen yet → **starting** (or **error** if aged out)
   - unit failed/inactive + port held by matching proxy → **running (orphaned)**; optionally `systemd-run` is not owning it — either `stop` via signal to PID, or offer `adopt` by… (true adopt into systemd is awkward for existing PIDs; practical approach: **track foreign matching PIDs as running**, stop via SIGTERM to that PID, prefer always starting under systemd going forward)
   - unit inactive + no process + port free → **stopped**
   - port held by non-matching process → **error: port_in_use**
   - unit inactive + stale runtime state → clear state → **stopped**

**Cmdline detection** is a good secondary key (instance name is unique on the argv), but not sufficient alone if multiple tools embed the same string. Combine: **unit name (primary) + listen port + cmdline match**.

### Pid / state file locations (XDG)

| Data | Path | Why |
|------|------|-----|
| Config | `$XDG_CONFIG_HOME/cloud-sql-tracker/connections.json` → `~/.config/cloud-sql-tracker/connections.json` | User-edited, persistent |
| Runtime ephemeral (optional lock/state) | `$XDG_RUNTIME_DIR/cloud-sql-tracker/` → `/run/user/$UID/cloud-sql-tracker/` | Lifetime bound to login; correct for sockets/locks/pid-like files |
| Logs (if not journal-only) | `$XDG_STATE_HOME/cloud-sql-tracker/logs/<id>.log` → `~/.local/state/cloud-sql-tracker/logs/` | Persists across restarts; appropriate for logs per XDG state |
| Cache (optional) | `$XDG_CACHE_HOME/cloud-sql-tracker/` | Not needed v0 |

**Guidance:** Prefer **journald via systemd** as primary logs (`journalctl --user -u cloud-sql-proxy-<id>.service`). If file logs are desired for easy UI “copy last error”, append under **state**, not config, not runtime-only (runtime is cleared on logout).

If you keep any pidfiles: store under `$XDG_RUNTIME_DIR`, use atomic create + pid liveness check. With systemd MainPID, pidfiles are mostly redundant.

### Start failure detection matrix

| Failure | Detect how | Health |
|---------|------------|--------|
| Binary missing | `Type=exec` fail / `which` preflight | `error` / `bin_missing` |
| Port in use | preflight bind/`ss` + post | `error` / `port_in_use` |
| ADC / auth failure | unit exits; logs contain refresh/credential errors; readiness never OK | `error` / `auth` |
| API/quota 403/429 | logs; process may die or fail readiness | `error` / `api` |
| Private IP unreachable | listen may come up; first DB connect or `--health-check` `/readiness` fails | `running` vs `degraded`/`error` (see gotchas) |
| Wrong flags | immediate exit | `error` |

### Health states for the bar

Recommended enum:

- `stopped` — no unit, no matching process, port free  
- `starting` — start requested / unit activating / wait for listen  
- `running` — unit active (or adopted PID) **and** local port listening  
- `degraded` — optional: listening but readiness/dial failed (private IP/auth refresh)  
- `error` — failed start/stop or crashed unit; include `error_code` + `detail`  
- `stopping` — optional transient during stop  

Aggregate: `running_count`, `error_count`, per-group counts.

## CLI command contract sketch

Binary name: `cloud-sql-tracker` (repo: `omarchy-cloud-sql-tracker`).

```text
cloud-sql-tracker list [--json]
cloud-sql-tracker status [id|--group G|--all] [--json]
cloud-sql-tracker start <id|--group G|--all> [--wait-ms N]
cloud-sql-tracker stop  <id|--group G|--all> [--wait-ms N]
cloud-sql-tracker restart <id|--group G|--all>
cloud-sql-tracker logs <id> [--lines N]   # journalctl wrapper or tail state log
cloud-sql-tracker doctor [--json]         # bin, ADC hint, port conflicts, unit bus
```

### Human vs JSON

- Default: short human text for terminal use.
- `--json`: stable machine schema for Quickshell.
- Plugin should call `status --json` on a timer (1–2s while dropdown open; 5–10s when closed is enough).

### Example `status --json`

```json
{
  "version": 1,
  "ts": "2026-04-08T12:00:00+02:00",
  "running": 3,
  "total": 7,
  "groups": {
    "fe": { "running": 1, "total": 2 },
    "backend": { "running": 2, "total": 3 },
    "iot": { "running": 0, "total": 2 }
  },
  "connections": [
    {
      "id": "fe-dev",
      "name": "FE Dev",
      "group": "backend",
      "instance": "acme-dev:europe-west1:frontend-db-1",
      "address": "127.0.0.1",
      "port": 55432,
      "private_ip": false,
      "state": "running",
      "pid": 12345,
      "unit": "cloud-sql-proxy-fe-dev.service",
      "uptime_sec": 3600,
      "error": null
    }
  ]
}
```

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success; for `status`/`list`, success even if some connections are in `error` (state is reported in payload) |
| 1 | Partial failure on multi-target start/stop (some ids failed) |
| 2 | Usage / unknown id / bad config JSON |
| 3 | Dependency failure (no systemd user bus, proxy binary missing on start, etc.) |
| 4 | All requested operations failed |

Idempotency: `start` on already running → 0; `stop` on already stopped → 0.

### Daemon or not?

**Stateless CLI per invocation is enough for v0–v1.**  
No long-lived tracker daemon. systemd owns lifetime; CLI is a pure control plane.

## cloud-sql-proxy v2 gotchas that shape health states

1. **Per-instance ports** — Use fixed `--port` per process, or `instance?port=N`. Multi-instance single process increments ports from `--port`; bad fit for independent toggles. Never rely on default 5432 if local Postgres exists; user’s non-default ports are correct.

2. **Public vs private IP** — Default is public IP. Private instances need `--private-ip` (or query param). Proxy **does not** create network path; laptop needs VPN/bastion/VPC reachability. Symptom: proxy may listen locally while upstream dial fails → expose `degraded`/`error` via logs or `--health-check` `/readiness`, not only “port open”.

3. **ADC failures** — Desktop auth is typically Application Default Credentials (`gcloud auth application-default login`). Missing/expired ADC → start fail or runtime cert refresh errors. Surface as `auth` in status; `doctor` should check `gcloud auth application-default print-access-token` or clear error strings from logs. Note: `gcloud auth login` alone is not always enough; ADC is the ADC store.

4. **Health HTTP ports** — `--health-check` serves `/startup`, `/readiness`, `/liveness` on `--http-port` (default 9090). **Only one proxy can own 9090.** If enabling health checks per connection, assign unique `http_port` in config or skip HTTP health and use listen+unit state only (simpler v0).

5. **Admin / quitquitquit ports** — `--quitquitquit` admin default 9091 similarly conflicts across instances. Prefer systemd SIGTERM over per-proxy admin ports unless necessary.

6. **Readiness vs private IP** — There have been readiness quirks around IP type reporting with `--private-ip`; don’t treat a single readiness implementation bug as sole truth—combine with unit state + local listen + log tail.

7. **IAM** — Caller needs `cloudsql.instances.connect` + `get` (e.g. role Cloud SQL Client). Failures look like API errors in logs.

8. **Auto IAM auth** — `--auto-iam-authn` is a separate concern from proxy ADC; only enable if DB users are IAM-based. Put it in per-connection config flags.

9. **Signals** — Stop via systemd SIGTERM; optional `--max-sigterm-delay` if desktop clients keep connections open and slow shutdowns annoy.

10. **One listener address** — Default `127.0.0.1`; keep it that way for a desktop companion (do not bind `0.0.0.0` unless intentional).

## Config sketch (`~/.config/cloud-sql-tracker/connections.json`)

```json
{
  "proxy_bin": "cloud-sql-proxy",
  "connections": [
    {
      "id": "fe-dev",
      "name": "FE Dev",
      "group": "backend",
      "instance": "acme-dev:europe-west1:frontend-db-1",
      "port": 55432,
      "address": "127.0.0.1",
      "private_ip": false,
      "auto_iam_authn": false,
      "extra_args": []
    }
  ]
}
```

Manual edit OK. Groups: `fe` | `backend` | `iot` (free strings; bar groups by field).

## Repo layout: `omarchy-cloud-sql-tracker`

```text
omarchy-cloud-sql-tracker/
├── README.md                 # install, config schema, bar integration notes
├── Cargo.toml
├── cargo-dist / justfile     # optional
├── src/
│   ├── main.rs               # clap entry
│   ├── config.rs             # load/validate connections.json
│   ├── model.rs              # Connection, HealthState, StatusDocument
│   ├── systemd.rs            # run/show/stop/logs via systemctl/systemd-run
│   ├── proc.rs               # cmdline + listen port inspect (adopt)
│   ├── lifecycle.rs          # start/stop/restart/reconcile
│   ├── status.rs             # aggregate JSON
│   └── doctor.rs
├── examples/
│   └── connections.json
├── quickshell/               # optional sample widget (or separate dotfiles)
│   └── CloudSqlTracker.qml   # Process + Timer + JSON.parse pattern
└── dist/
    └── PKGBUILD              # optional Arch package later
```

Install: `cargo install --path .` or copy release binary to `~/.local/bin`. Plugin invokes absolute path or PATH lookup.

### Quickshell integration pattern

- `Timer` + `Process { command: ["cloud-sql-tracker", "status", "--json"] }`
- On finish: `JSON.parse(stdout)` → model for dropdown
- Button handlers: fire-and-forget `start`/`stop`, then refresh status
- Show running count on bar; color by any `error`

No need to embed connection strings; labels + state only (DBeaver already configured).

## Risks / open implementation hazards

1. **Orphan proxies from old bash scripts** — First run must detect cmdline+port matches and either report them as running or offer stop-by-PID; otherwise port conflicts on start.
2. **systemd user bus availability** — Headless SSH without user session may lack `systemd --user`; on Omarchy desktop login it should exist. `doctor` must fail clearly if `XDG_RUNTIME_DIR` or user bus missing.
3. **`Type=simple` false success** — Always use `Type=exec` (or verify listen) so missing binary is an error.
4. **Port-only health is optimistic** — Local accept ≠ Cloud SQL reachable (VPN/private IP/ADC expiry mid-run). Consider optional deeper check later (proxy readiness or trivial TCP+log heuristic).
5. **HTTP health/admin port collisions** — Easy footgun if copying k8s examples; default off for multi-proxy desktop.
6. **Transient unit name collisions / failed units** — Use `--collect` and `systemctl --user reset-failed` on restart paths.
7. **Race: double start from double-click** — Serialize per-id with `flock` on `$XDG_RUNTIME_DIR/cloud-sql-tracker/<id>.lock` during start/stop.
8. **ADC expiry while running** — Proxy may keep listening but fail new DB connects; bar may still show `running`. Document “reconnect/restart proxy after `gcloud auth application-default login`”.
9. **Security** — Keep listeners on `127.0.0.1`; tracker does not store DB passwords.
10. **Scope creep** — Don’t build connection-string UI, multi-user support, or autostart until the bar loop is solid.

## v0 command set (ship this first)

1. `list --json`
2. `status --json` (reconcile + aggregate)
3. `start <id|--group|--all>`
4. `stop <id|--group|--all>`
5. `logs <id>`
6. `doctor`

Human-readable output optional but cheap with the same structs.

## Summary answers to research questions

1. **CLI + JSON vs QML vs systemd:** Hybrid — **CLI + JSON for UX/control**, **systemd --user for supervision**. Not pure QML; not systemd-only without CLI.
2. **Language:** **Rust** primary; Go fine alternative; bash retire; Python not preferred.
3. **Supervision:** **systemd-run --user transient services**; runtime dir for locks; state/journal for logs; cmdline+port adopt; SIGTERM then SIGKILL via systemctl.
4. **CLI UX:** start/stop/status/list/logs/doctor + `--json`; exit codes as above; **no long-lived daemon**.
5. **Proxy v2 gotchas:** fixed ports per process; private-ip/VPN; ADC; unique health ports if used; port-open ≠ healthy upstream.
6. **Concrete:** Rust repo `omarchy-cloud-sql-tracker`, layout above, config path as specified, Quickshell polls `status --json`.

## Sources consulted

- https://github.com/GoogleCloudPlatform/cloud-sql-proxy/blob/main/README.md — multi-instance, ports, private IP, health, admin/quitquitquit
- https://github.com/GoogleCloudPlatform/cloud-sql-proxy/blob/main/docs/cmd/cloud-sql-proxy.md — flags, health endpoints, ADC notes, sigterm delays
- https://github.com/GoogleCloudPlatform/cloud-sql-proxy/blob/main/docs/cmd/cloud-sql-proxy_shutdown.md — shutdown subcommand requires quitquitquit admin
- https://github.com/GoogleCloudPlatform/cloud-sql-proxy/blob/main/migration-guide.md — v1→v2 invocation
- https://docs.cloud.google.com/sql/docs/postgres/connect-auth-proxy — ADC (`application-default login`), private IP requirements, multi-port examples, troubleshooting
- https://github.com/GoogleCloudPlatform/cloud-sql-proxy/issues/1360 — readiness/private-IP quirk
- https://man.archlinux.org/man/systemd-run.1.en — `--user`, `--unit`, `--collect`, `Type=exec`, remain-after-exit
- https://wiki.archlinux.org/title/Systemd/User — user manager lifecycle on Arch
- https://specifications.freedesktop.org/basedir-spec/latest/ — XDG config/state/runtime split
- https://quickshell.org/docs/guide/introduction/ — Process + StdioCollector pattern
- https://github.com/ORFLEM/just_enough_shell/blob/main/.local/JES/quickshell/helpers/JsonPoll.qml — JSON poll helper pattern
- https://github.com/bjarneo/cliamp/blob/main/docs/remote-control.md — status --json IPC inspiration (daemon not required here)
- https://besterry.com/posts/rust-vs-go-for-cli-tools/ — practical Rust/Go CLI size/startup tradeoffs

## Gaps

- Exact Omarchy/cliamp widget code in the user’s tree was not inspected; integration assumes generic Quickshell `Process` polling.
- No benchmark of `systemctl --user show` vs D-Bus latency on the user’s machine (unlikely to matter at 1–2s poll).
- Whether user instances are public IP, private IP, or PSC should be confirmed per connection in config (`private_ip` flag).
- True “adopt into systemd” of an already-running non-systemd PID is not cleanly supported; design treats foreign PIDs as manageable orphans instead.

## Supervisor coordination

None required; research complete, artifact written to `/home/golgor/Code/Personal/research.md`.
