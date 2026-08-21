# Research: crate choice for `port::observe(address, port)`

## Summary

No reviewed crate implements the frozen `observe(address, port) -> PortObservation` operation end to end. Use **`std::net::TcpStream::connect_timeout` plus `procfs` 0.18 with default features disabled**: `std` owns the authoritative Open/Closed/Unreachable probe, while `procfs` supplies best-effort, address-aware listener inode → PID → name attribution. Do not add `listeners` or `nix`, and do not add a `Port` trait.

## Findings

1. **No single crate is the product adapter.** `std::net::TcpStream::connect_timeout` opens one `SocketAddr` with a timeout and returns `io::Result<TcpStream>`; it does not identify the listener process. `listeners` and `procfs` identify/list sockets but do not perform the required accepting-connect probe. Therefore `src/port.rs` must compose liveness and attribution behind the frozen function rather than delegate the whole operation to one crate. [`TcpStream::connect_timeout`](https://doc.rust-lang.org/std/net/struct.TcpStream.html#method.connect_timeout) · [`listeners::get_process_by_port`](https://docs.rs/listeners/latest/listeners/fn.get_process_by_port.html) · [`procfs::net`](https://docs.rs/procfs/latest/procfs/net/index.html)

2. **Recommended stack: `std` + `procfs = { version = "0.18", default-features = false }`.** The `procfs` API directly exposes the pieces the frozen design names: `tcp()`/`tcp6()`, each entry's `local_address`, TCP `state`, and socket `inode`; `all_processes()`; per-process file descriptors whose `FDTarget::Socket(u64)` carries the same inode; and process `stat().comm` / `exe()`. Its own networking example demonstrates this exact socket-inode-to-process join and explicitly tolerates vanished/inaccessible processes. [`TcpNetEntry`](https://docs.rs/procfs/latest/procfs/net/struct.TcpNetEntry.html) · [`FDTarget`](https://docs.rs/procfs/latest/procfs/process/enum.FDTarget.html) · [`Process`](https://docs.rs/procfs/latest/procfs/process/struct.Process.html) · [`procfs net example`](https://docs.rs/procfs/latest/src/procfs/net.rs.html)

3. **`procfs` gives the adapter the necessary failure boundaries.** Read the TCP tables and find a LISTEN entry matching the configured address/port (including an appropriate same-family wildcard bind), then scan process FDs for its inode. A process disappearing, an unreadable FD directory, or an unreadable `comm`/`exe` must degrade only `listener_pid` or `holder_name` to `None`; it must not alter the already-computed TCP state. This follows the repository's frozen rules in [`docs/modules.v1.md`](../modules.v1.md), [`docs/reconcile.v1.md`](../reconcile.v1.md), and [`docs/research/port-liveness.md`](./port-liveness.md).

4. **`listeners` is viable but is not the best fit.** Its high-level `get_process_by_port(port, protocol)` returns PID/name/path, but it accepts no address, returns one `Process`, and reports both lookup failures and “no process found” as an error. The address-bearing alternative, `get_all()`, returns complete `Listener` records but broadens a single-target lookup into a system-wide snapshot. Those semantics make it harder to preserve the product's exact address input and its deliberate distinction between probe success and optional attribution. [`get_process_by_port`](https://docs.rs/listeners/latest/listeners/fn.get_process_by_port.html) · [`get_all`](https://docs.rs/listeners/latest/listeners/fn.get_all.html) · [`Listener`](https://docs.rs/listeners/latest/listeners/struct.Listener.html) · [`Process`](https://docs.rs/listeners/latest/listeners/struct.Process.html)

5. **`std`-only is possible but needlessly hand-rolls a kernel text format.** The kernel documents `/proc/net/tcp{,6}` fields and notes that this interface is deprecated in favor of `tcp_diag`; a std-only implementation would still need custom parsing plus `/proc/<pid>/fd` race/error handling. `procfs` isolates that parsing while leaving the small product-specific join in `port`. [Linux kernel `/proc/net/tcp` documentation](https://docs.kernel.org/networking/proc_net_tcp.html)

6. **`nix` does not close the gap.** It exposes generic socket and `NETLINK_SOCK_DIAG` primitives, but not a documented high-level inet-diag query that maps an address/port to a PID/name. Using it would require implementing netlink request/response parsing and would still require a `/proc` inode-to-PID walk. That is more code and risk than the frozen procfs route. [`nix::sys::socket`](https://docs.rs/nix/latest/nix/sys/socket/index.html) · [`SockProtocol::NetlinkSockDiag`](https://docs.rs/nix/latest/nix/sys/socket/enum.SockProtocol.html)

7. **Maintenance evidence favors `procfs` without making popularity the decision.** `procfs` 0.18.0 is Linux-specific, declares MSRV 1.70, has maintained releases through 2025, and has a long-lived public API focused on `/proc`. `listeners` is active and lightweight, but younger and cross-platform—an advantage this Linux-only adapter does not need. `nix` is highly maintained but too low-level for this operation. [`procfs` crate metadata](https://docs.rs/crate/procfs/latest) · [`procfs` releases](https://github.com/eminence/procfs/releases) · [`listeners` crate metadata](https://docs.rs/crate/listeners/latest) · [`nix` crate docs](https://docs.rs/nix/latest/nix/)

## Recommended shape

Illustrative only; this ticket must not implement `port.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortState {
    Open,
    Closed,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortObservation {
    pub(crate) state: PortState,
    pub(crate) listener_pid: Option<u32>,
    pub(crate) holder_name: Option<String>,
    // Optional internal diagnostic for error.detail; never changes `state`.
    pub(crate) attribution_detail: Option<String>,
}

pub(crate) fn observe(address: IpAddr, port: u16) -> PortObservation;
```

Recommended control flow:

1. Construct the exact configured `SocketAddr` and classify `TcpStream::connect_timeout` as Open, Closed (`ConnectionRefused`), or Unreachable (timeout/other failure). The timeout is a private `port` implementation constant; zero is invalid according to `std`.
2. Independently read `procfs::net::tcp()` and `tcp6()`, retain LISTEN entries compatible with the configured address/port, and collect candidate socket inodes.
3. Scan `all_processes()` and each readable `Process::fd()` for matching `FDTarget::Socket(inode)` values.
4. Set `listener_pid` only when attribution is unambiguous. If no process is readable, a process races away, or multiple plausible holders remain, return `None` rather than a false PID.
5. After a PID is known, try `stat().comm`; optionally fall back to the basename of `exe()`. Any name error yields `holder_name: None` and may populate `attribution_detail`; it never changes `state`, clears a known PID, or makes `observe` fail.

This shape preserves the frozen separation: connect success defines local liveness; PID/name only enrich Reconcile ownership checks and `error.detail`. It does not identify “our binary,” adopt an Orphan, stop by PID, invoke `ss`, or introduce a trait.

## Comparison

| Choice | Liveness | Address-aware PID/name | Complexity in `port` | Decision |
|---|---|---|---|---|
| `std` only | Exact frozen method | Possible via hand-parsed `/proc` | Highest parsing/maintenance burden | Reject |
| `std` + `listeners` | Exact frozen method | `get_process_by_port` is port-only; `get_all` has addresses | Small API, but weaker failure/address fit | Reject |
| **`std` + `procfs`** | Exact frozen method | Yes; explicit socket/inode/PID/name stages | Moderate, transparent, testable | **Recommend** |
| `std` + `nix` | Exact frozen method | Only after custom sock-diag and `/proc` work | Highest protocol complexity | Reject |

## Sources

- Kept: [Rust `TcpStream::connect_timeout`](https://doc.rust-lang.org/std/net/struct.TcpStream.html#method.connect_timeout) — authoritative liveness API and timeout behavior.
- Kept: [`procfs::net` API and example](https://docs.rs/procfs/latest/procfs/net/index.html) — primary API evidence for TCP tables and inode-to-process attribution.
- Kept: [`procfs::process::Process`](https://docs.rs/procfs/latest/procfs/process/struct.Process.html) — primary API evidence for FDs, `stat`, and `exe`.
- Kept: [`listeners` API](https://docs.rs/listeners/latest/listeners/) — primary evidence for its port-only and all-listeners alternatives.
- Kept: [`nix::sys::socket`](https://docs.rs/nix/latest/nix/sys/socket/index.html) — primary evidence for its low-level socket surface.
- Kept: [Linux kernel `/proc/net/tcp` documentation](https://docs.kernel.org/networking/proc_net_tcp.html) — authoritative format/deprecation context.
- Kept: [`docs/research/port-liveness.md`](./port-liveness.md), [`docs/modules.v1.md`](../modules.v1.md), and [ADR 0004](../adr/0004-rust-toolchain-and-linux-io.md) — frozen repository context.
- Dropped: crate popularity rankings and generic blog comparisons — they do not establish API fit.
- Dropped: `port_check` and async runtime crates — they do not provide the required PID/name attribution and the product method is already frozen.

## Gaps

- No runtime fixture was executed in this research-only pass. Implementation should test IPv4, IPv6, wildcard binds, a closed port, an inaccessible/vanished process, and ambiguous candidates on the target Linux environment.
- `/proc/net/tcp{,6}` is deprecated by the kernel in favor of sock_diag. That is a known future migration risk, not a v1 blocker; keeping it inside the concrete `port` module localizes replacement without a trait.
- The exact compatible-bind rule for IPv4-mapped IPv6 / `IPV6_V6ONLY` needs an implementation test. When attribution is uncertain, preserve liveness and return no PID.

## Acceptance evidence

- **Review finding (info):** `docs/research/port-io.md` recommendation is `std` + `procfs 0.18` with default features disabled; no product source implementation is proposed.
- **Residual risk:** procfs snapshots race process/socket changes and may be restricted by permissions; the recommended observation explicitly degrades attribution to `None` without failing liveness.

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Concrete crate findings, recommendation, API sketch, source links, review finding, and residual risks are recorded in /tmp/wayfinder-impl/out-port-io.md; repository target path is docs/research/port-io.md."
    }
  ],
  "changedFiles": [
    "/tmp/wayfinder-impl/out-port-io.md"
  ],
  "testsAddedOrUpdated": [],
  "commandsRun": [
    {
      "command": "Read issue #32 and required repository documents; research official std/listeners/procfs/nix/kernel sources",
      "result": "passed",
      "summary": "Compared the four requested crate stacks against the frozen method and selected std + procfs."
    },
    {
      "command": "Runtime crate fixture / cargo test",
      "result": "not-run",
      "summary": "Research-only task; no product code or Cargo dependency was changed."
    }
  ],
  "validationOutput": [
    "Issue #32 requires crate choice only, one stack, a PortObservation sketch, no trait, no port.rs implementation.",
    "Primary docs confirm std supplies connect_timeout; procfs supplies address/state/inode, process FD socket inode, comm/exe; listeners port lookup lacks address; nix exposes only low-level sock-diag primitives."
  ],
  "residualRisks": [
    "procfs snapshots are racy and PID/name visibility can be permission-limited; attribution must degrade to None without changing probe state.",
    "IPv4-mapped IPv6 and wildcard-bind matching needs target-host tests.",
    "The kernel marks /proc/net/tcp as deprecated in favor of tcp_diag, though it remains the frozen v1 route."
  ],
  "noStagedFiles": true,
  "diffSummary": "One research artifact recommends std + procfs, compares listeners/procfs/nix/std, and sketches PortObservation and failure isolation.",
  "reviewFindings": [
    "info: docs/research/port-io.md - recommend std::net::TcpStream::connect_timeout plus procfs 0.18 with default features disabled; do not add listeners or nix.",
    "no blockers: no product code, Port trait, Orphan adoption, ss invocation, or stop-by-PID behavior is proposed."
  ],
  "manualNotes": "The runtime output override limited this subagent to /tmp/wayfinder-impl/out-port-io.md; branch, commit, push, and issue-comment operations were not performed by this research subagent."
}
```
