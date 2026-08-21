# Research: supervisor systemd I/O path for v1

**Ticket:** [#31 — Research systemd I/O for supervisor](https://github.com/golgor/cloud-sql-tracker/issues/31)  
## Summary

Use **only zbus on the user/session bus in v1**. Do not shell out to `systemd-run` / `systemctl`. Do not implement both paths. Do not add `trait Supervisor`.

Human override (2026-08-21): the operator chose D-Bus over subprocess. That matches [ADR 0004](../adr/0004-rust-toolchain-and-linux-io.md) (prefer user D-Bus for units). The argv tables below stay as **discarded evidence**, not the v1 adapter.

Same facts either way (Unit present, ActiveState, MainPID, start/stop, `reset-failed`). One pipe into frozen `supervisor`.

## Findings

1. **Recommendation — zbus in v1 (human decision).** `docs/modules.v1.md` hides either transport inside one concrete `supervisor` adapter and forbids a speculative supervisor trait. The research default was argv (smaller crate graph, docs already list `systemd-run` flags). The operator chose D-Bus so the Control plane does not parse `systemctl` text. Cost: add `zbus` (blocking API; default features include async/`block_on`). [zbus blocking API](https://docs.rs/zbus/latest/zbus/blocking/index.html) [zbus features](https://docs.rs/crate/zbus/latest/features) [ADR 0004](../adr/0004-rust-toolchain-and-linux-io.md) [local module freeze](../modules.v1.md)

2. **zbus is functionally complete, but should not be used alongside argv.** On the user/session bus, systemd exposes `StartTransientUnit`, `GetUnit`, `StopUnit`, and `ResetFailedUnit` on `org.freedesktop.systemd1.Manager`. Unit objects expose generic Unit state and service-specific process-result properties. zbus provides `Connection::session()`, blocking proxies, typed method calls, and D-Bus property reads, so it can implement every frozen function. Implementing both transports would create the second adapter the freeze explicitly does not call for, double failure handling and tests, and create pressure for the forbidden trait. [systemd D-Bus API](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html) [zbus blocking connection](https://docs.rs/zbus/latest/zbus/blocking/connection/struct.Connection.html) [zbus blocking proxy](https://docs.rs/zbus/latest/zbus/blocking/proxy/struct.Proxy.html)

3. **The shell path adds executable and parsing failures, but they are bounded.** `supervisor` must distinguish: child spawn failure (`systemd-run` or `systemctl` missing/not executable), nonzero command status, malformed/missing required `show` properties, and user-bus failure reported on stderr. It must not invoke a shell or build a command string: pass every argument through `std::process::Command`. Parse named `KEY=value` lines rather than positional `--value` output, split only on the first `=`, reject duplicate required keys, and validate required keys. `systemctl show` may succeed for a not-found unit and may omit empty properties unless `--all` is used, so **presence comes from `LoadState=not-found`, not exit status**. [systemctl(1)](https://www.freedesktop.org/software/systemd/man/latest/systemctl.html) [upstream explanation of show/not-found semantics](https://github.com/systemd/systemd/issues/1105)

4. **A down user bus fails both approaches; there must be no fallback process spawn.** `zbus::blocking::Connection::session()` fails while connecting/handshaking when no session bus is available; the argv tools fail nonzero with diagnostics such as failure to connect to the bus. `systemd_user_check` should exercise the user manager itself, not merely find an environment variable or connect to the bus broker. Use manager property `Version`; do not use `is-system-running`, because an unrelated failed unit can make a reachable manager degraded. Any down-bus result becomes the frozen doctor `systemd_user` failure/check row with actionable detail/hint. Lifecycle calls return a typed supervisor I/O error. Never fall back to `Command::spawn(proxy)` outside systemd. [zbus connection docs](https://docs.rs/zbus/latest/zbus/blocking/connection/struct.Connection.html) [systemd manager D-Bus API](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html) [existing local research](./systemd-user-units.md)

5. **Use monotonic start time for Reconcile.** Request `ExecMainStartTimestampMonotonic`, an integer number of microseconds, rather than parsing the locale-formatted `ExecMainStartTimestamp`. A zero value means unknown/not started. Compare it with the caller's monotonic observation time and pass an age/timestamp representation into the frozen pure Reconcile input. This avoids wall-clock jumps and locale parsing. The service D-Bus interface defines both real-time and monotonic start timestamps as `t` (`u64`). [systemd D-Bus API](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html) [systemd time units](https://www.freedesktop.org/software/systemd/man/latest/systemd.time.html)

## Discarded v1 argv (not the adapter)

Kept so implementers can see the subprocess alternative. **Do not call these from `supervisor`.** The following are argument vectors, not shell strings.

### `show(unit) -> Result<UnitSnapshot>`

```text
systemctl
  --user
  --no-pager
  --all
  --property=LoadState
  --property=ActiveState
  --property=SubState
  --property=MainPID
  --property=Result
  --property=ExecMainCode
  --property=ExecMainStatus
  --property=ExecMainStartTimestampMonotonic
  show
  --
  UNIT
```

Map properties as follows:

| `UnitSnapshot` need | `systemctl show` property | Mapping |
|---|---|---|
| loaded/missing | `LoadState` | `not-found` means absent; `loaded` means present; preserve other load errors as observation/error detail rather than pretending absent |
| high-level state | `ActiveState` | map known values needed by Reconcile (`inactive`, `activating`, `active`, `deactivating`, `failed`); retain unknown text for conservative typed handling because systemd states may grow |
| service substate | `SubState` | retain/map inside `supervisor`; do not leak raw property strings into `reconcile` |
| live PID | `MainPID` | unsigned PID; `0` means none |
| service outcome | `Result` | string such as `success`, `exit-code`, `signal`, `timeout`, `core-dump`, `oom-kill` |
| exit-vs-signal discriminator | `ExecMainCode` | signed SIGCHLD `si_code`; `1` (`CLD_EXITED`) means `ExecMainStatus` is an exit status, while killed/dumped codes mean it is a signal number; `0` means no result yet |
| exit status or signal | `ExecMainStatus` | interpret only together with `ExecMainCode`/`Result`; this supplies the frozen clean SIGTERM patterns (`15`, `143`, or exit `0`) |
| start age | `ExecMainStartTimestampMonotonic` | microseconds since boot; `0` means unknown |

Do not request a nonexistent separate `Signal` property: systemd encodes signal information in the `ExecMainCode` + `ExecMainStatus` pair. Do not infer missing unit from command exit status.

### `start_transient(connection, proxy_bin, env) -> Result<()>`

```text
systemd-run
  --user
  --unit=UNIT
  --description=Cloud SQL Auth Proxy (ID)
  --property=Type=exec
  --property=KillMode=control-group
  --property=TimeoutStopSec=30
  --property=Restart=no
  --expand-environment=no
  --setenv=HOME=HOME_VALUE
  [--setenv=GOOGLE_APPLICATION_CREDENTIALS=ADC_VALUE]
  --
  ABSOLUTE_PROXY_BIN
  --address=ADDRESS
  --port=PORT
  [--private-ip]
  [EXTRA_PROXY_ARG ...]
  --exit-zero-on-sigterm
  INSTANCE
```

Rules inherited unchanged from the frozen contract:

- Use the model-owned `cloud-sql-proxy-<sanitized-id>.service` unit name.
- `ABSOLUTE_PROXY_BIN` must be absolute.
- `--exit-zero-on-sigterm` is unconditional and appears exactly once on proxy argv.
- Do not pass `--scope`, `--collect`, `--wait`, `--pty`, or a shell wrapper.
- Do not pass `--no-block`: the default job wait plus `Type=exec` lets `systemd-run` report an `execve` failure; it does not wait for the long-running proxy to exit.
- Capture stdout/stderr and require exit status 0. Readiness remains the frozen port/Reconcile flow, not the `systemd-run` return alone.

`systemd-run` documents that transient services run detached under the manager, that the default service type is `simple`, and that `Type=exec` delays successful start reporting until the command has been executed. [systemd-run(1)](https://www.freedesktop.org/software/systemd/man/latest/systemd-run.html)

### `stop(unit) -> Result<()>`

```text
systemctl --user --no-pager stop -- UNIT
systemctl --user --no-pager reset-failed -- UNIT
```

Only the expected managed Unit is named; never stop or kill by PID. Treat a confirmed absent Unit as the frozen idempotent no-op. After an actual stop, run `reset-failed` best-effort as frozen: failure to clear stale failure metadata should not undo a successful stop, but should remain diagnosable. Restart uses the same stop/reset sequence before `systemd-run`.

`reset-failed` clears the failed state and recorded exit status as well as restart/start-rate counters. [systemctl(1)](https://www.freedesktop.org/software/systemd/man/latest/systemctl.html)

### `systemd_user_check() -> CheckRow`

```text
systemctl --user --no-pager --property=Version --value show
```

With no unit argument, `show` reads manager properties. Require status 0 and a nonempty Version value. This one probe verifies all v1 prerequisites relevant to this adapter: `systemctl` can execute, the user bus is reachable, and `org.freedesktop.systemd1` answers. Preserve a bounded stderr excerpt in doctor detail and use the already-frozen doctor hint policy. Do not classify a reachable but globally `degraded` user manager as unavailable.

## v1 D-Bus calls (normative)

This is the adapter. Do not also spawn `systemctl` / `systemd-run`.

Common connection and manager proxy:

```text
zbus::blocking::Connection::session()
destination = org.freedesktop.systemd1
path        = /org/freedesktop/systemd1
interface   = org.freedesktop.systemd1.Manager
```

| Frozen operation | D-Bus calls |
|---|---|
| `start_transient` | `StartTransientUnit(UNIT, "fail", properties: a(sv), aux: empty a(sa(sv))) -> job: o` |
| `show` | `GetUnit(UNIT) -> object_path`; missing-unit error means absent. On that object path, `org.freedesktop.DBus.Properties.GetAll("org.freedesktop.systemd1.Unit")` and `GetAll("org.freedesktop.systemd1.Service")` |
| `stop` | `StopUnit(UNIT, "replace") -> job: o`, then best-effort `ResetFailedUnit(UNIT)` |
| `systemd_user_check` | connect to session bus, create manager proxy, read manager `Version: s` (connection alone proves only the bus, not the systemd user manager) |

Required `StartTransientUnit` properties and D-Bus signatures:

```text
("Description",     variant s   "Cloud SQL Auth Proxy (ID)")
("Type",            variant s   "exec")
("KillMode",        variant s   "control-group")
("TimeoutStopUSec", variant t   30_000_000)
("Restart",         variant s   "no")
("Environment",     variant as  ["HOME=...", "GOOGLE_APPLICATION_CREDENTIALS=..."])
("ExecStart",       variant a(sasb) [
    (ABSOLUTE_PROXY_BIN,
     [ABSOLUTE_PROXY_BIN, PROXY_ARG..., "--exit-zero-on-sigterm", INSTANCE],
     false)
])
```

Required object properties/types:

```text
org.freedesktop.systemd1.Unit:
  LoadState s, ActiveState s, SubState s

org.freedesktop.systemd1.Service:
  MainPID u, Result s,
  ExecMainCode i, ExecMainStatus i,
  ExecMainStartTimestampMonotonic t
```

The D-Bus path avoids PATH lookup for `systemctl`/`systemd-run`, subprocess startup, text parsing, and lossy stderr classification. v1 costs are zbus plus transitive/default async-I/O features, blocking-runtime behavior, D-Bus `Variant`/`OwnedValue` conversion for `a(sv)` and `a(sasb)`, two interface property maps, and D-Bus-specific error mapping. Those costs are accepted for v1.

## Failure classification

| Failure | v1 handling |
|---|---|
| `systemd-run`/`systemctl` cannot be spawned | typed adapter/tool-unavailable error; doctor `systemd_user` fails; no bare-process fallback |
| user bus missing/refused/disconnected | typed user-systemd-unavailable error; doctor fails with bounded diagnostic/hint |
| systemd manager responds with command/job failure | typed operation error retaining bounded stderr |
| `show` returns `LoadState=not-found` | successful absent `UnitSnapshot`, not an I/O error |
| `show` omits/duplicates/malforms a required property | typed parse/protocol error; do not synthesize a healthy snapshot |
| unknown future state/result string | retain raw value for detail and map conservatively; never silently classify healthy |
| stop succeeds, best-effort reset fails | stop remains successful under frozen semantics; retain diagnostic where the command layer can surface/log it |

## Sources

- **Kept:** [systemctl(1)](https://www.freedesktop.org/software/systemd/man/latest/systemctl.html) — primary definition of machine-readable `show`, `stop`, `reset-failed`, properties, and user-manager selection.
- **Kept:** [systemd-run(1)](https://www.freedesktop.org/software/systemd/man/latest/systemd-run.html) — primary transient-service argv and `Type=exec` behavior.
- **Kept:** [org.freedesktop.systemd1](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html) — primary D-Bus methods, object interfaces, signatures, and properties.
- **Kept:** [systemd.time(7)](https://www.freedesktop.org/software/systemd/man/latest/systemd.time.html) — primary explanation of normalized microsecond D-Bus properties.
- **Kept:** [zbus blocking API](https://docs.rs/zbus/latest/zbus/blocking/index.html), [Connection](https://docs.rs/zbus/latest/zbus/blocking/connection/struct.Connection.html), and [Proxy](https://docs.rs/zbus/latest/zbus/blocking/proxy/struct.Proxy.html) — first-party crate API and blocking-runtime behavior.
- **Kept:** [`docs/research/systemd-user-units.md`](./systemd-user-units.md) — existing project research and already-frozen unit/argv decisions.
- **Kept:** [`docs/modules.v1.md`](../modules.v1.md) and [`docs/reconcile.v1.md`](../reconcile.v1.md) — frozen local interface and observation requirements only.
- **Dropped:** blogs and generic “manage systemd from Rust” examples — redundant and weaker than systemd/zbus primary documentation.
- **Dropped:** alternative Rust systemd crates — ticket compares the already-documented argv path with zbus only.
- **Dropped:** old project `docs/RESEARCH.md` sketches — explicitly superseded by the module freeze.

## Gaps and residual risks

1. **Live Omarchy smoke test remains required.** Verify D-Bus behavior for: absent transient unit, bad proxy binary under `Type=exec`, SIGTERM clean stop fields, down user bus, and post-stop `reset-failed` on the target systemd version.
2. **User bus is a runtime prerequisite.** Doctor `systemd_user` must fail clearly when the session bus or user manager is down. Do not require `systemctl` / `systemd-run` on `PATH` for the adapter (journal still uses `journalctl`).
3. **`ExecMainCode` constants should be named/tested against libc SIGCHLD semantics.** At minimum test exited (`CLD_EXITED`), killed (`CLD_KILLED`), dumped (`CLD_DUMPED`), and zero/no-result; never interpret status `15` without the code/result discriminator.

## Decision gist for issue #31

**Use one concrete zbus adapter in v1** (user/session bus: `StartTransientUnit`, `GetUnit` + properties, `StopUnit`, `ResetFailedUnit`, manager `Version`). Do not shell out to `systemd-run`/`systemctl`. Do not ship both. Do not add `trait Supervisor`.
