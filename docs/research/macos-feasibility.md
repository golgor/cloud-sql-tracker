# macOS feasibility — run, cross-compile, or CI

Research brief for [issue #67](https://github.com/golgor/cloud-sql-tracker/issues/67).

The question: can `cloud-sql-tracker` run on macOS, or is v1 permanently Linux-only? If a Mac
story exists, what is the smallest useful slice, and does cross-compile or GitHub Actions help?

Crate, tool, runner, and price numbers in this brief are **snapshots**. Compile numbers were
measured on 2026-08-28 against crate version `0.1.1` and Rust `1.97.1`. Documentation numbers
were read in 2026-08. Re-check every number before you act on it.

Terms follow [`CONTEXT.md`](../../CONTEXT.md): Connection, Proxy process, Control plane, Status
document, Health state, Reconcile, Source, Foreign process, Group, Unit, Supervisor.

---

## The Pick

**Pick:** v1 stays **Linux-only**. Add a Darwin **compile gate** and document the macOS
options. Do not build a macOS Supervisor now.

**Why:** the two operator pains need a real Supervisor on the Mac, and the only supervisor
macOS offers is launchd. A launchd adapter is a new map, not a slice.

**Discarded:** the "degraded Mac control plane" of issue #67 mode 3. It costs almost as much as
a full launchd adapter and it cannot solve the second operator pain at all. Section
"Mode 3 is dominated" gives the evidence.

**Unchanged:** [ADR 0004](../adr/0004-rust-toolchain-and-linux-io.md), every frozen contract,
every schema, every golden file, the Reconcile truth table, and the close criteria of
[map #28](https://github.com/golgor/cloud-sql-tracker/issues/28). The Linux path does not
change. `cargo test` on Linux stays at 314 passing tests.

The compile gate lands as [issue #100](https://github.com/golgor/cloud-sql-tracker/issues/100).
The launchd question and the macOS runtime question become follow-up research tickets. A Mac
owner can then take them with a branch that already builds on his machine.

---

## The operator, and the two pains

One colleague runs an ARM Mac. He reports two problems:

1. He juggles many Proxy processes by hand. Terminal tabs, forgotten processes, port
   collisions, and no overview of what runs.
2. A Proxy process dies without warning and nothing restarts it.

Pain 1 is a **control plane** problem. Pain 2 is a **Supervisor** problem. The distinction
decides everything below, because macOS gives us the first for free and makes the second hard.

---

## Cross-compile is not macOS support

A Darwin **build** of this crate costs about twenty lines. A Darwin **binary** does not work.

The single compile blocker is the `procfs` crate. A build script is a small program that Cargo
runs before it compiles the crate. The `procfs` build script stops the build on purpose:

```
error: failed to run custom build command for `procfs v0.18.0`
  Building procfs on an for a unsupported platform.
  Currently only linux and android are supported
  (Your current target_os is macos)
```

Move `procfs` under a `[target.'cfg(target_os = "linux")'.dependencies]` table and gate the
`procfs`-backed private functions in `src/port.rs`. A `#[cfg]` attribute is a compile-time
switch. The compiler drops the item when the condition is false. After that change,
`cargo check --target aarch64-apple-darwin --all-targets` passes with **zero errors**. The
measured spike was **15 insertions and 1 deletion across 2 files**. Three dead-code warnings
need gates too, so the real size is about **twenty lines**.

No other dependency blocks Darwin. `zbus` 5.19.0, `rustix`, `clap` 4.6.6, `serde`, `serde_json`,
and `thiserror` all compile clean for `aarch64-apple-darwin`. The `zbus` result is the
surprising one, and it is also the trap. **zbus compiles, and no systemd user bus exists on
macOS.** Every Supervisor call therefore fails at run time.

That is the whole point. Twenty lines buy a green compile. They buy no working `start`.

| Feature | Compiles for Darwin | Works on macOS |
|---|---|---|
| `clap` argv, exits, `--version` | yes | yes |
| `config` parse and validation | yes | yes |
| `reconcile` truth table (pure) | yes | yes |
| TCP liveness probe | yes | yes |
| `supervisor` start, stop, show | **yes** | **no.** No systemd. |
| `journal` logs | **yes** | **no.** No `journalctl` binary. |
| `port` attribution (holder PID and name) | after gating | **no.** No `/proc`. |

The Linux dependencies survive a Darwin compile because they are shell-outs, D-Bus calls, and
plain file reads. The compiler cannot see a missing binary, an absent bus, or an absent path.

---

## Per-operating-system reality

| Capability | Linux today | macOS, compile gate only (#100) | macOS, launchd adapter |
|---|---|---|---|
| `cargo check`, `cargo clippy` | yes | **yes** | yes |
| `cargo test` | 314 pass, 0 ignored | compiles. Pass count **not measured**. | needs new tests |
| `--version`, `--help`, argv errors | yes | yes | yes |
| `config` validation and exit `2` | yes | yes | yes |
| `status --json` shape and schema | yes | yes, but every row reads broken | yes |
| `start` a Connection | yes | **no** | yes |
| `stop` a Connection | yes | **no** | yes |
| Restart a dead Proxy process | yes, systemd | **no** | yes, `KeepAlive` |
| `logs <id>` | yes, `journalctl` | **no** | file dump, reduced |
| Port liveness | yes | **yes** | yes |
| Port holder PID and name | yes, procfs | no | yes, `libproc` |
| `doctor` hard checks pass | yes | **no.** `systemd_user` and `journal_user` fail. | needs redefinition |
| Dogfood checklist | full | none | most, with gaps |

Nobody measured how many of the 314 tests pass on a Mac, because no Mac was available. Do not
guess the number. Measure it on the first real Mac run.

---

## Mode 3 is dominated

Issue #67 offered mode 3 as a middle option: spawn `cloud-sql-proxy` as a child process, with
no systemd. Research shows mode 3 is not a cheap version of the launchd mode. It costs almost
the same and it delivers half the value.

The Control plane is a **stateless CLI**. It exits after each command. On Linux, systemd is the
thing that outlives that exit, owns the transient Unit, restarts the Proxy process, and records
why it died. A child process has nobody to do that job.

Evidence, from Apple's own documentation:

1. **A child of an exited process is reparented.** The macOS `_exit(2)` man page states: "The
   parent process-ID of all of the calling process's existing child processes are set to 1, the
   initialization process". PID 1 on macOS is launchd
   ([`_exit(2)`](https://manp.gs/mac/2/_exit),
   [`launchd(8)`](https://keith.github.io/xcode-man-pages/launchd.8.html)).
2. **Reparenting is not supervision.** launchd manages only jobs that `launchctl` and a plist
   told it about. No documented key, subcommand, or API hands a running PID to launchd for
   restart or for exit-status bookkeeping. This is an absence of a documented feature, so treat
   it as strongly indicated and not as a positive citation.
3. **Apple calls the POSIX route deprecated.** "On Darwin operating systems, the canonical way
   to launch a daemon is through `launchd` as opposed to traditional POSIX and POSIX-like
   mechanisms ... These alternate methods should be considered deprecated and not suitable for
   new projects" ([`launchd(8)`](https://keith.github.io/xcode-man-pages/launchd.8.html)).
   Mode 3 is exactly the traditional POSIX mechanism.
4. **Nobody reaps the exit status.** PID 1 reaps the Proxy process. Our Control plane already
   exited, so it never calls `wait`. Nothing keeps "exited 143 after SIGTERM" apart from
   "exited 2 on a crash".

One refinement to the issue wording: the reparented Proxy process **does keep running**. Mode 3
delivers a working tunnel. What mode 3 cannot deliver is a **restart** and a **recorded reason
for death**.

The effect on Reconcile is the serious part. Mode 3 has no `ActiveState`, no `Result`, no
`ExecMainStatus`, and no `ExecMainCode`. The truth-table rows that produce `unit_failed` and
`exec_failed` become unreachable, and the whole clean-stop branch table loses its input. Every
dead Proxy process collapses into one row: port closed, no live process, `stopped`. **A
silently dead proxy reads as a clean stop.** `restart --failed` filters Health state `error`,
so it would select nothing. That command becomes unimplementable, not merely awkward.

| | Mode 3, child processes | Mode 4, launchd agents |
|---|---|---|
| Pain 1, juggling proxies | solved | solved |
| **Pain 2, silent death and restart** | **impossible** | solved, `KeepAlive` |
| Modules changed | same as mode 4, **plus a new PID-state module** | `supervisor`, `journal`, `port`, `model`, `commands`, `cli` |
| Needs a second adapter, so reopens the no-traits freeze | yes | yes |
| Reopens a frozen non-goal | yes. [`reconcile.v1.md`](../reconcile.v1.md) lists a state file of start deadlines under Non-goals. | no |
| Breaks the `stop` contract | yes. Forces kill-by-PID, which [`modules.v1.md`](../modules.v1.md) forbids: "**Our Unit only** — never kill-by-PID". | no. `launchctl bootout`. |
| Test gap | **green suite, unreachable behaviour** | medium. One output parser. |
| Confidence | high on the pain 2 conclusion | medium |

Mode 3 pays the price of a second adapter and gets one pain solved. Reject it.

---

## What a launchd adapter would need

This section is a sketch for a follow-up ticket. Nothing here is a commitment.

### Supervisor

A **user agent** is a launchd service that runs for one user, not system-wide. It is the
closest match to a `systemd --user` Unit. A **plist** is Apple's XML configuration file, and one
launchd service is one plist file in `~/Library/LaunchAgents`
([`launchd.plist(5)`](https://keith.github.io/xcode-man-pages/launchd.plist.5.html)).

Note one difference against `CONTEXT.md` immediately: our Unit is **transient**. A plist file is
on-disk state. A launchd adapter changes the definition of Unit.

| launchd key | Documented meaning | Use for one Proxy process |
|---|---|---|
| `Label` | Uniquely identifies the job. | Replaces `cloud-sql-proxy-<id>.service`. |
| `ProgramArguments` | Argument vector for the spawned process. | Proxy binary plus Connection flags. |
| `EnvironmentVariables` | Variables set before the job runs. | ADC forwarding. |
| `KeepAlive` | Keeps the job running. A dictionary selects conditions such as `SuccessfulExit` and `Crashed`. | **This is the restart feature. It answers pain 2.** |
| `ThrottleInterval` | Overrides the throttle policy. By default a job does not spawn more than once every 10 seconds. | Bounds restart storms. |
| `ExitTimeOut` | Wait between SIGTERM and SIGKILL. | Matches `TimeoutStopSec`. |
| `StandardOutPath`, `StandardErrorPath` | Map stdout and stderr to files. | The only way to keep Proxy process output. |

There is **no transient equivalent of `systemd-run`**. `launchctl submit` exists and the man
page marks it as a legacy subcommand. A launchd adapter writes plist files.

### Reading state back — the real risk

`supervisor::show` reads typed D-Bus properties today. launchd offers text instead, and Apple
disclaims that text. `launchctl print` carries this warning in Apple's own man page:

> *IMPORTANT*: This output is *NOT* API in any sense at all. Do *NOT* rely on the structure or
> information emitted for *ANY* reason. It may change from release to release without warning.

`launchctl list` has a documented three-column format that includes the last exit status, where
a negative number is the negative of the stopping signal. That gives a genuine clean-stop
versus crash signal, which is what the Reconcile branches need. But Apple points away from
`list` toward `print`, and `print` has the warning above.

Also, **launchd reports no start timestamp**. Status `uptime_sec` and the Reconcile start window
need another source.

`SMAppService` is the modern supported API, and it does not help. Its `status` property is an
enum of registration states such as `notRegistered` and `requiresApproval`, not a run state and
not an exit status
([`SMAppService.Status`](https://developer.apple.com/documentation/servicemanagement/smappservice/status-swift.enum)).
It also expects plists inside a code-signed application bundle, which a `cargo install` CLI does
not have.

**Pick for a future adapter:** read the exit status from the `launchctl list <label>`
three-column output. Read the start time from the kernel for the reported PID.
**Why:** the three-column format is the only launchd output with a documented field layout.
**Discarded:** parsing `launchctl print`, because Apple states its output is not an API.
**Unchanged:** on Linux, `supervisor::show` keeps reading typed D-Bus properties, and
`UnitSnapshot` stays the seam.

One more warning. `ThrottleInterval` defaults to one spawn per ten seconds. `AGENTS.md` already
separates two clocks, the Reconcile `START_WINDOW` and the CLI `--wait-ms`. macOS adds a
**third** clock that neither of them knows about. A `start` that hits the throttle can look
like `start_timeout` for a reason the Control plane cannot see.

### Port attribution

TCP liveness needs no change. `TcpStream::connect_timeout` is portable.

Attribution needs a replacement for procfs. `libproc` is the Darwin library for asking the
kernel about a process. `proc_pidfdinfo` with the `PROC_PIDFDSOCKETINFO` flavor maps an open
socket to a PID, and `proc_pidinfo` returns a BSD info block that carries the process start
time. An Apple engineer recommended `lsof` first on the developer forums. The same engineer added
that if you do not want a shell-out, "yes, libproc is the right answer"
([Apple developer forums](https://developer.apple.com/forums/thread/728731)).

Snapshot, 2026-02:

| Crate | Version | What it gives |
|---|---|---|
| [`libproc`](https://docs.rs/libproc/latest/libproc/) | 0.14.11 | `proc_pidinfo`, `proc_pidfdinfo`, `pidpath`, and the Darwin `bsd_info` block with the process start time |
| [`netstat2`](https://docs.rs/crate/netstat2/latest) | 0.11.2 | One cross-platform socket-to-PID call. Uses `proc_pidfdinfo` on macOS. |

**Pick:** `libproc`. **Why:** it answers both the holder-PID question and the start-timestamp
question that launchd does not answer. **Discarded:** `netstat2`, because it solves only the
socket half and adopting it would rewrite the working Linux path for no gain.
**Unchanged:** the Linux adapter keeps `procfs`, `PortObservation` stays the seam, and the TCP
probe stays authoritative on both platforms.

On privileges: the kernel checks a security policy on this path. An unprivileged caller gets
file-descriptor information for its **own** processes. Every Proxy process we manage runs as the
operator's own user, so the common case needs no elevated privileges. A Foreign process owned by
another user degrades to "port open, holder unknown". Nobody verified this on a current macOS
release, so treat it as strongly indicated and not as measured.

### What "no attribution at all" costs

Issue #100 ships exactly this fallback, so the cost is worth stating precisely. It is small.

| What is lost | Where |
|---|---|
| Telling "our Proxy process holds the port" apart from "a Foreign process holds the port" **when a Unit is active**. That row would read `running` instead of Health state `error` with `error.code: port_in_use`. | Reconcile PID attribution table |
| The holder identity in `error.detail`. It degrades to "held by unknown process". | Reconcile holder identity in errors |
| The holder name and PID in the Doctor `ports` row. | [`doctor.v1.md`](../doctor.v1.md) |

Note the shape of the loss. Only the "Unit active and Foreign process on the port" row goes
wrong. Port open with no Unit stays `error` and `port_in_use` whether or not we know the holder.
So this is one wrong row plus two poorer messages, not a hole across the board.

### Logs

[`logs.v1.md`](../logs.v1.md) promises a thin dump of our Unit's journal lines. macOS has no
`journalctl`. Two options exist, and neither keeps every promise. Capturing Proxy process stdout
and stderr through `StandardOutPath` gives real proxy output and loses journal metadata,
rotation, and the `--since` semantics. Apple unified logging through `log show` covers the
system, and it does not capture the stdout of a plain child process. A launchd mode should
redefine the `logs` promise, not pretend to keep it.

### Data plane

This half is fine. `cloud-sql-proxy` ships a `darwin.arm64` build, and Application Default
Credentials work the same way on macOS. The `doctor` rows `proxy_bin` and `adc` keep their
meaning. The rows `systemd_user` and `journal_user` do not, and
[`doctor.v1.md`](../doctor.v1.md) makes both hard, so `doctor` exits `3` on a Mac until those
rows are redefined.

### Dogfood delta

Against [`verification.v1.md`](../verification.v1.md), a launchd mode would keep most items.
These change:

- `doctor` hard checks cannot pass until `systemd_user` and `journal_user` are redefined.
- `logs <id>` is not applicable as written.
- `status --json` keeps its shape, and the `unit` field names something that is not a transient
  Unit.
- The seven golden Connections on ports 15432 to 15438 need no change. No port there is
  reserved on macOS.

---

## A working answer for the operator today

The colleague does not have to wait for a launchd adapter, and he does not have to give up
macOS. He can run a small **ARM Linux virtual machine beside macOS**. Lima, OrbStack, and UTM
all do this on Apple silicon. macOS stays exactly as it is. The virtual machine is an
application.

Inside that Linux virtual machine, `systemd --user` exists, so `cloud-sql-tracker` works the way
it works on any Linux host. Both pains go away with **no new code and no second adapter**. The
virtual machine forwards the proxy ports to the Mac. A client on the Mac such as DBeaver or
`psql` then connects to `localhost:15432` as normal.

Two things to verify on the first try, because nobody tested them here:

1. Port forwarding from the virtual machine to the Mac for every port in the Group range.
2. That `systemd --user` has a session bus in the chosen virtual machine image. A minimal or
   SSH-only image may not start a user session bus. See
   [`systemd-user-units.md`](./systemd-user-units.md).

This route is a workaround and not a product promise. It is the cheapest way to learn whether
the Control plane model helps him at all, before anybody funds a launchd adapter.

---

## Build, CI, and artifacts

### GitHub Actions

**Pick:** no `macos-latest` job on pull requests. Add `cargo check --target
aarch64-apple-darwin` as one extra step in the existing `ubuntu-latest` job.
**Why:** `cargo check` does not link. The free Linux runner therefore proves the same compile
fact that a Mac runner would prove, in one step instead of one job.
**Discarded:** a `runs-on` matrix over `ubuntu-latest` and `macos-latest` on every pull request.
It doubles the job count to assert one compile fact the Linux runner can assert.
**Unchanged:** the single `ci` job, the `RUST_VERSION` pin, `cargo test` on Linux, and the
Layer 1 contract script.

Be exact about what such a step can assert:

- `cargo check` and `cargo clippy` for Darwin are **meaningful**. They prove the target gates
  are complete, that no new dependency re-broke Darwin, and that no new `/proc` call slipped in
  outside a gated function. That is a real regression guard for the twenty-line split.
- `cargo test` on a Mac is **not meaningful today**. The test binaries compile. They fail at run
  time because the Linux dependencies are shell-outs, D-Bus calls, and file reads. A
  `macos-latest` job that runs `cargo test` would go red for reasons that are not regressions.
  Do not add it until a mode with a real Mac Supervisor exists.

On cost: this repository is **public**, and standard GitHub-hosted runners are free on public
repositories. `macos-latest` is a standard runner, so a macOS job here costs no money. Two
cautions. Larger runners such as `macos-latest-large` are never free. If the repository ever
becomes private, a macOS job consumes plan minutes at about ten times the Linux rate. Snapshot
2026-08-28, from [Actions runner pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing):
Linux x64 is 0.006 USD per minute and macOS is 0.062 USD per minute.

So the real cost of a macOS job here is wall-clock time and queue time, not money. That is still
a reason to prefer one extra step on the Linux runner. Keep a real `macos-latest` job on
`workflow_dispatch` only, for a human who wants to see what happens on a Mac.

### Host to target matrix

| Host | Target | Works | Recommend |
|---|---|---|---|
| `ubuntu-latest` | `x86_64-unknown-linux-gnu` | yes, today | keep |
| `ubuntu-latest` | `aarch64-apple-darwin`, `cargo check` only | **yes, measured** | **yes** |
| `ubuntu-latest` | `aarch64-unknown-linux-gnu` | yes, with `cross` or `cargo-zigbuild` | no, no ARM Linux operator exists |
| `macos-latest` | `aarch64-apple-darwin`, native | yes | `workflow_dispatch` only |
| `ubuntu-latest` | `aarch64-apple-darwin`, linked | blocked by the Apple SDK license | **no** |

Two notes. GitHub now offers native ARM64 Linux runners such as `ubuntu-24.04-arm`. So `cross`
and `cargo-zigbuild` are no longer the cheapest route to ARM Linux if that need appears. A
Darwin **link** from Linux needs an Apple SDK, which is a license problem and was not attempted.
`cargo check` from Linux works only because `check` does not link.

### Target-gating style

**Pick:** one target table in `Cargo.toml`, plus one platform seam inside `port` as two sibling
child modules, each behind a single `#[cfg]`.
**Why:** the reader sees the platform split in one place instead of a condition on every
function.
**Discarded:** scattered `#[cfg(target_os = "linux")]` attributes on each import and each
private function. The measured spike did exactly that and left three dead-code warnings, and
each fix adds another condition.
**Unchanged:** the `observe` signature, `PortObservation` in `model`, Reconcile purity, and the
no-traits rule in [`modules.v1.md`](../modules.v1.md).

The `Cargo.toml` half is not a choice. `procfs` must move under
`[target.'cfg(target_os = "linux")'.dependencies]`, because its build script fails before any
gate in our source is read.

Do **not** add a `trait PortAttribution`. There is one real attribution adapter and one stub
that returns nothing. That is one adapter, not two.

### Artifacts

**Pick:** do not publish a Darwin archive. **Why:** it would hold a binary whose `start`,
`stop`, and `logs` all fail on the operator's machine. That creates support load instead of a
working install. **Discarded:** attaching an `aarch64-apple-darwin` archive beside the Linux
one, because a green Darwin compile is a build fact and not a runtime fact.
**Unchanged:** [`release.md`](./release.md) stands. Dogfood stays
`cargo install --path --locked` on the operator's own operating system.

---

## Follow-up tickets

The Pick above closes the research question. These tickets carry the rest. A Mac owner can take
the runtime ones, because they need a Mac and this brief does not.

| Ticket | Kind | Depends on |
|---|---|---|
| [#100](https://github.com/golgor/cloud-sql-tracker/issues/100) Target-gate `procfs` for a Darwin compile | task | none, ready |
| Measure real macOS behaviour: run the built binary and the test suite on a Mac, record what fails and why | research, needs a Mac | #100 |
| Research a launchd Supervisor adapter: plist generation, `launchctl` state read-back, the third clock | research, needs a Mac | the measurement ticket |
| Research macOS logs: redefine the `logs` promise for a file-based source | research | the launchd ticket |
| Research a graphical user interface for the Control plane | research | out of scope of this brief and of this repository. The bar plugin lives in a separate repository. |

---

## Gaps — what nobody measured

State these plainly. Do not let a later reader treat them as settled.

- How many of the 314 tests pass on a Mac. No Mac was available.
- Line counts for a launchd adapter. Both need a design decision first, so any number would be
  invented.
- Whether a Darwin **link** from Linux works. Only `cargo check` was measured.
- The exact fields that `launchctl list <label>` prints on a current macOS release. Apple
  disclaims the format.
- The `libproc` privilege behaviour on a current macOS release.
- Whether port forwarding and a `systemd --user` session bus work in a chosen ARM Linux virtual
  machine image.
