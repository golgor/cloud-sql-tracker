# Research: Localhost port liveness detection (Rust / Linux)

**Issue:** #7  
**Slug:** `research/port-liveness`  
**Context:** Stateless Rust CLI control plane; Google `cloud-sql-proxy` data plane; `systemd --user` supervisor; Arch/Omarchy.  
**Question:** How should the control plane reliably detect that a Connection’s local `address:port` is accepting connections, without root, suitable for frequent `status` polls?

---

## Recommendation (v1)

**Primary (accepting / “live” for status):** timed TCP connect probe to the configured bind address (default `127.0.0.1`) with a short timeout via `std::net::TcpStream::connect_timeout`.

**Primary (listener PID / Orphan matching):** Linux socket table + inode→fd walk (prefer crate `listeners` or thin `procfs` + `/proc/*/fd` logic), **not** shelling out to `ss`/`lsof`.

**Optional deep signal (not required for v1 port liveness):** if the proxy unit is started with `--health-check`, HTTP GET `/startup` (and optionally `/readiness`) on the proxy HTTP port (default `localhost:9090`). This answers “proxy finished startup / can reach Cloud SQL,” which is **stricter** than “TCP listener is open.”

**Fallback:** none required for the connect path (stdlib only). If PID attribution fails (permissions, race), report `port_open: true/false` from connect and `listener_pid: null` rather than failing the whole status.

### Why this split

| Concern | Best signal |
|--------|-------------|
| “Can a client open a TCP session to this Connection’s port?” | Connect probe |
| “Is *our* proxy (or an orphan) the one bound there?” | Socket→PID attribution |
| “Is the proxy process alive under systemd?” | `systemctl --user` unit state (separate research) |
| “Has the proxy finished Cloud SQL dial / cert setup?” | `--health-check` `/startup` or `/readiness` |

A listening socket can exist before the proxy is fully ready to serve DB traffic; a connect success only proves the **data-plane listener** accepted TCP. That matches the issue wording (“accepting connections”) and is what DBeaver/psql need first. Unit state + optional health HTTP cover process and upstream readiness.

---

## Comparison matrix

| Method | Accuracy / false positives | Cost @ 1–2s poll / every `status` | PID attribution | Root? | Rust approach |
|--------|----------------------------|-------------------------------------|-----------------|-------|---------------|
| **TCP `connect_timeout`** | High for “accepting”; **false positive** if *another* process bound the port | Very low on localhost (RST or accept in ≪1ms; timeout only if filtered) | No | No | `std` only |
| **`/proc/net/tcp{,6}` parse** | High for “something in LISTEN on addr:port”; must match bind addr (v4/v6/dual-stack) | Low–medium (read+parse tables); scales with connection count | Inode only → need fd walk for PID | No (own netns) | `procfs` or manual parse |
| **`ss -ltnp` / shell** | Good if `-p` works; brittle parse; PATH/`ss` version drift | Medium (fork+exec every poll) | Yes with `-p` when permitted | No | `std::process::Command` — **avoid** |
| **Netlink `SOCK_DIAG` / `tcp_diag`** | Best kernel source (kernel docs deprecate `/proc/net/tcp` in favor of diag) | Low if filtered query | Inode/sk; PID still via userspace mapping | No | `nlink`/custom — more code |
| **Crate `listeners`** | Designed for port→process | Benchmarked; fine for desktop status | Yes (`get_process_by_port`) | No | One dep |
| **Bind probe (`TcpListener::bind` fails ⇒ in use)** | “Port busy,” not “accepting”; races; may confuse SO_REUSE* | Low | No | No | `std` — wrong semantic |
| **Proxy HTTP health** | Not the DB port; needs `--health-check`; `/liveness` always 200 if HTTP up | One localhost HTTP GET | No | No | `std`/`ureq`/etc. |
| **DB protocol handshake** | Strongest “really Postgres/MySQL,” heavy, needs creds | High | No | No | Out of scope for port liveness |

### False-positive notes (connect)

1. **Wrong owner:** connect succeeds if *any* listener is on that tuple — including a leftover Docker Postgres on `5432` or a stale proxy. Mitigate with PID attribution + expected unit/MainPID (and fixed high ports in config, e.g. 15432+).
2. **IPv4 vs IPv6:** proxy default address is **`127.0.0.1`** (not “all interfaces”). Probe **exactly** the configured `address` (do not only try `localhost`, which may resolve to `::1` first).
3. **Connect side effects:** each successful probe opens then closes a TCP connection. Proxy accepts it; brief ESTABLISHED blip. At 1–2s UI polls across N instances this is fine on desktop; avoid sub-100ms hammering.
4. **TOCTOU:** any check can race start/stop. Status is advisory; start/stop must still handle `EADDRINUSE` / unit failures.

### False-positive notes (LISTEN table only)

- State LISTEN without accept path (half-setup) is rare for cloud-sql-proxy but possible for other sockets.
- Without PID match, LISTEN ≡ connect for “something is there,” but connect better matches “accepting.”

---

## Concrete API sketches

### 1. Port accepting (primary liveness for Connection port)

```rust
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;
use std::io::ErrorKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortAccept {
    Open,       // TCP connect succeeded
    Closed,     // connection refused (nothing listening)
    Unreachable // timeout / filtered / other
}

pub fn probe_port_accept(addr: SocketAddr, timeout: Duration) -> PortAccept {
    // connect_timeout rejects zero Duration; use e.g. 150–300ms for localhost status.
    match TcpStream::connect_timeout(&addr, timeout) {
        Ok(_stream) => PortAccept::Open, // drop closes immediately
        Err(e) => match e.kind() {
            ErrorKind::ConnectionRefused => PortAccept::Closed,
            ErrorKind::TimedOut => PortAccept::Unreachable,
            // On some paths TimedOut is raw OS error; also map would-block-ish cases:
            _ if e.raw_os_error() == Some(110) /* ETIMEDOUT */ => PortAccept::Unreachable,
            _ if e.raw_os_error() == Some(111) /* ECONNREFUSED */ => PortAccept::Closed,
            _ => PortAccept::Unreachable,
        },
    }
}

// Usage: SocketAddr::from(([127, 0, 0, 1], conn.port))
// Prefer explicit IPv4/IPv6 from config, not "localhost" string DNS.
```

**Semantics (Linux `connect(2)`):**

- Nothing listening → **`ECONNREFUSED`** quickly on loopback.
- Listener present → handshake completes; `connect_timeout` returns `Ok`.
- Nonblocking connect in progress → `EINPROGRESS`; completion via poll + `SO_ERROR` (this is what `connect_timeout` implements internally).

**Timeout guidance:** 150–300 ms per port for interactive `status`; fail-closed to `Unreachable`/`Closed` display, never block multi-second on loopback under normal iptables.

**Batch status:** sequential probes for tens of connections are enough; optional `std::thread` scope only if N grows large. Prefer **no tokio** in v1 (CLI stays sync/stateless).

### 2. Listener PID (Orphan matching)

**Option A — crate (recommended if dep OK):**

```rust
// listeners = "0.6"
pub fn listener_pid_on_port(port: u16) -> Option<u32> {
    listeners::get_process_by_port(port).ok().flatten().map(|p| p.pid /* API: check crate version fields */)
}
```

Then compare to systemd unit `MainPID` / cgroup PIDs when classifying Managed vs Orphan vs Foreign.

**Option B — zero extra abstraction beyond `procfs`:**

1. Read `procfs::net::tcp()` and `tcp6()`; keep entries with state **LISTEN** whose local port matches and local addr matches config (`127.0.0.1`, `0.0.0.0`, `::1`, `::` as applicable).
2. Take socket **inode**.
3. Scan `/proc/[pid]/fd/*` symlinks for `socket:[inode]` (user can read own processes; may miss other users’ — acceptable on single-user desktop).
4. Optionally verify `/proc/pid/comm` or `exe` contains `cloud-sql-proxy`.

Kernel documents `/proc/net/tcp` format (local addr:port hex, state) and notes the interface is **deprecated in favor of `tcp_diag`**; still universally available and fine for v1.

**Option C — shell `ss -ltnp 'sport = :15432'`:** rejected for v1 (below).

### 3. Optional proxy HTTP readiness (deep)

Requires unit ExecStart to include `--health-check` (and stable `--http-port`, default 9090):

| Path | Meaning |
|------|---------|
| `GET /startup` | 200 after proxy finished starting; else 503 |
| `GET /readiness` | 200 if started, under max connections, not stopping; (docs also describe instance connectivity checks) |
| `GET /liveness` | **Always 200** if HTTP server responds — process liveness only |

```rust
// Pseudocode — only if health-check enabled in unit template
// GET http://127.0.0.1:9090/startup  → 200 => proxy_started
```

Official `cloud-sql-proxy wait` polls another process’s **startup** endpoint (default 30s); same mechanism, not the DB port.

**Do not** treat `/liveness` as “DB port ready.”

### 4. What not to use as sole liveness

```rust
// WRONG semantic for "accepting connections"
TcpListener::bind(( "127.0.0.1", port )).is_err(); // only "cannot bind"
```

---

## Integration with systemd --user

Recommended status fusion for one Connection:

```
unit_active     = systemctl is-active cloud-sql-tracker@<id>.service  (or transient name)
main_pid        = systemctl show -p MainPID --value ...
port_accept     = probe_port_accept(cfg.address, cfg.port)
listener_pid    = get_process_by_port(cfg.port)   // optional each poll or on anomaly
```

| unit_active | port_accept | listener_pid vs MainPID | Suggested state |
|-------------|-------------|-------------------------|-----------------|
| active | Open | match / unknown | **Up** |
| active | Closed | — | **Starting** or **Degraded** (listener not up yet) |
| active | Open | other pid | **Conflict** (foreign listener; unit confused) |
| inactive | Open | cloud-sql-proxy or unknown | **Orphan** / external |
| inactive | Closed | — | **Down** |
| failed | * | * | **Failed** (prefer unit result) |

Port probe alone must not mark **Up** if policy requires managed units — combine with unit state in reconcile rules (product policy). For pure “is the port usable?” UI badge, `port_accept == Open` is enough.

---

## Rejected alternatives

1. **Shell out to `ss`/`lsof`/`fuser` every status** — fragile parsing, fork cost, locale/version differences, harder testing. No benefit over `listeners`/`procfs` on Arch.
2. **Tokio-only async connect** — unnecessary runtime weight for a sync CLI; std `connect_timeout` is enough.
3. **Bind-as-check** — wrong meaning; flaky with reuse flags.
4. **Full DB auth + simple query** — slow, needs secrets, not “port liveness.”
5. **Rely solely on `--health-check` HTTP** — optional flag; different port; `/liveness` is weak; does not prove **configured DB port** is the one open (misconfig possible).
6. **Raw netlink-only in v1** — best long-term kernel API, more implementation risk; defer unless `procfs`/`listeners` prove insufficient.
7. **Crate `port_check` alone** — fine for free-port helpers; still connect/bind based; no PID; TOCTOU explicitly documented — OK as inspiration, not required dep.

---

## Risks

| Risk | Mitigation |
|------|------------|
| Foreign process on configured port | PID attribution + `doctor` warning; refuse start on conflict |
| Probe storms from UI | Debounce 1–2s; cache last probe timestamp in UI, not necessarily in CLI |
| IPv6/`localhost` mismatch | Store and probe numeric `address` from config; default `127.0.0.1` to match proxy |
| Connect logs / max-connections | Short-lived connect; health `/readiness` fails if maxed — separate from DB port probe |
| `/proc` fd scan cost | Only attribute PID when unit missing, conflict suspected, or `status --deep` |
| Permissions on others’ sockets | Desktop single-user OK; if pid unknown, still report port open/closed |
| TOCTOU start vs status | Idempotent start; handle bind errors; never assume check==still true |
| Dual-stack LISTEN on `::` only | If user sets `--address ::1` or `0.0.0.0`, probe that family |

---

## cloud-sql-proxy data-plane facts (relevant)

- Default bind: **`127.0.0.1`** (`--address`, default `"127.0.0.1"`).
- `--port` sets initial listener port; multiple instances increment.
- TCP clients use `127.0.0.1:PORT` (GCP docs).
- Optional `--health-check` HTTP on `--http-address`/`--http-port` (defaults `localhost` / `9090`).
- Listener “up” ≠ Cloud SQL reachable; `/readiness` / first real DB connect cover upstream.

---

## Sources

- Rust `TcpStream::connect_timeout` — https://doc.rust-lang.org/std/net/struct.TcpStream.html  
- Linux `connect(2)` (`ECONNREFUSED`, `EINPROGRESS`, `SO_ERROR`) — https://man7.org/linux/man-pages/man2/connect.2.html  
- Kernel `/proc/net/tcp` (LISTEN listing; deprecated vs `tcp_diag`) — https://docs.kernel.org/networking/proc_net_tcp.html  
- `procfs` net tables — https://docs.rs/procfs/latest/procfs/net/index.html  
- `listeners` crate (port→PID) — https://docs.rs/listeners/latest/listeners/  
- Cloud SQL Auth Proxy cmd docs (address/port, health checks) — https://github.com/GoogleCloudPlatform/cloud-sql-proxy/blob/main/docs/cmd/cloud-sql-proxy.md  
- Proxy `wait` (startup HTTP) — https://github.com/GoogleCloudPlatform/cloud-sql-proxy/blob/main/docs/cmd/cloud-sql-proxy_wait.md  
- Healthcheck handlers — https://github.com/GoogleCloudPlatform/cloud-sql-proxy/blob/main/internal/healthcheck/healthcheck.go  
- GCP connect-auth-proxy (listens on 127.0.0.1) — https://docs.cloud.google.com/sql/docs/postgres/connect-auth-proxy  
- `port_check` TOCTOU note — https://crates.io/crates/port_check  

---

## Gaps / follow-ups

- No local runtime experiment in this pass (no shell): validate 150 ms timeout vs slow proxy accept under load on the target Arch host.
- Confirm `listeners` PID field names/version when adding the dep; pin and smoke-test dual-stack.
- Unit naming / MainPID read path owned by systemd research ticket — fuse here at implement time.
- Whether v1 enables `--health-check` on all generated units (product choice); if yes, expose `proxy_startup: bool` beside `port_accept`.

---

## Implementer checklist (no product code in this ticket)

1. Add `probe_port_accept(addr, timeout)` in a small `netstatus` module (std only).
2. Config: keep explicit `address` + `port`; default address `127.0.0.1`.
3. `status --json`: include `port: { state: open|closed|unreachable, latency_ms? }`.
4. Orphan path: `listeners::get_process_by_port` or procfs inode walk; match `cloud-sql-proxy`.
5. Tests: spin `TcpListener` on `127.0.0.1:0`, probe bound port → Open; unused high port → Closed; do not require network or GCP.
