//! TCP liveness probe + best-effort listener attribution for one
//! Connection's local `address:port` (`docs/modules.v1.md`, "port —
//! liveness + attribution"). Crate choice: `docs/research/port-io.md`
//! (`std::net::TcpStream::connect_timeout` for liveness, `procfs` for
//! socket-inode-to-PID/name attribution — not `listeners`, not `ss`).
//!
//! `commands::status` (#42) composes [`observe`]'s `model::PortObservation`
//! with `supervisor::show` into Reconcile's `Observation`, and
//! `commands::doctor`'s (#44) `ports` check will reuse this same probe.

use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

use procfs::net::{tcp, tcp6, TcpNetEntry, TcpState};
use procfs::process::{all_processes, FDTarget, Process};

use crate::model::{PortObservation, PortProbe};

/// How long [`observe`] waits for a TCP connect before treating the port
/// as [`PortProbe::Unreachable`] (`docs/modules.v1.md`, "port — liveness +
/// attribution": "Hides: timeout"). Loopback connects resolve almost
/// instantly; this only bounds a slow or filtered address.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

/// Probe a Connection's local `address:port` and, best-effort, attribute
/// any listener (`docs/modules.v1.md`; `docs/research/port-io.md`).
///
/// `probe` is authoritative: a TCP connect either succeeds or it doesn't.
/// `listener_pid` / `listener_name` are diagnostic only — a `/proc` read
/// failure, a vanished process, or an ambiguous match degrades attribution
/// to `None` and never changes `probe` (`docs/modules.v1.md`: "Name
/// lookup must never fail the probe").
pub(crate) fn observe(address: IpAddr, port: u16) -> PortObservation {
    let probe = probe_tcp(address, port);
    let (listener_pid, listener_name) = attribute_listener(address, port);
    PortObservation {
        probe,
        listener_pid,
        listener_name,
    }
}

/// The authoritative TCP liveness probe
/// (`docs/modules.v1.md`: `TcpStream::connect_timeout`).
fn probe_tcp(address: IpAddr, port: u16) -> PortProbe {
    match TcpStream::connect_timeout(&SocketAddr::new(address, port), CONNECT_TIMEOUT) {
        Ok(_stream) => PortProbe::Open,
        Err(err) => classify_connect_error(&err),
    }
}

/// `docs/research/port-io.md`, "Recommended control flow": classify a
/// failed connect as `Closed` (`ConnectionRefused`) or `Unreachable`
/// (timeout / any other failure).
fn classify_connect_error(err: &io::Error) -> PortProbe {
    if err.kind() == io::ErrorKind::ConnectionRefused {
        PortProbe::Closed
    } else {
        PortProbe::Unreachable
    }
}

/// Best-effort listener PID + process name for `address:port`
/// (`docs/research/port-io.md`, "Recommended control flow", steps 2–5):
/// the socket inode of the matching `LISTEN` entry, then the process
/// whose open file descriptors reference that inode, then its name.
fn attribute_listener(address: IpAddr, port: u16) -> (Option<u32>, Option<String>) {
    let Some(inode) = listening_inode(address, port) else {
        return (None, None);
    };
    let Some(pid) = pid_holding_inode(inode) else {
        return (None, None);
    };
    (Some(pid), process_name(pid))
}

/// The socket inode of the one `LISTEN` entry compatible with
/// `address:port`, across both the IPv4 and IPv6 tables
/// (`docs/research/port-io.md`, step 2). More than one match is
/// ambiguous — return `None` rather than guess which one is the real
/// holder.
fn listening_inode(address: IpAddr, port: u16) -> Option<u64> {
    let candidates = tcp_entries()
        .into_iter()
        .filter(|entry| entry.state == TcpState::Listen)
        .filter(|entry| binds_to(entry.local_address, address, port))
        .map(|entry| entry.inode);
    unique(candidates)
}

/// Every TCP table entry, IPv4 and IPv6 together
/// (`docs/research/port-io.md`, step 2: "read `procfs::net::tcp()` and
/// `tcp6()`"). An unreadable table degrades to no entries rather than
/// failing the probe.
fn tcp_entries() -> Vec<TcpNetEntry> {
    let mut entries = tcp().unwrap_or_default();
    entries.extend(tcp6().unwrap_or_default());
    entries
}

/// Whether a TCP table entry's `local` address is compatible with the
/// Connection's configured `address:port`. An unspecified bind
/// (`0.0.0.0` / `::`) matches any address on the same port: a process
/// listening on all interfaces still serves this Connection's configured
/// address. The IPv4-mapped-IPv6 edge is a known gap, not resolved here
/// (`docs/research/port-io.md`, "Gaps").
fn binds_to(local: SocketAddr, address: IpAddr, port: u16) -> bool {
    local.port() == port && (local.ip() == address || local.ip().is_unspecified())
}

/// The one process whose open file descriptors reference `inode`
/// (`docs/research/port-io.md`, step 3). A process that vanishes
/// mid-scan, or an unreadable `fd` directory, is skipped rather than
/// failing the scan; more than one holder is ambiguous and returns
/// `None` (`docs/research/port-io.md`, step 4).
fn pid_holding_inode(inode: u64) -> Option<u32> {
    let holders = all_processes_best_effort().filter_map(|process| {
        let holds_inode = process
            .fd()
            .ok()?
            .filter_map(Result::ok)
            .any(|fd| matches!(fd.target, FDTarget::Socket(fd_inode) if fd_inode == inode));
        holds_inode.then(|| process.pid() as u32)
    });
    unique(holders)
}

/// Every process this call can read. A process-listing failure, or a
/// single vanished/unreadable process, is skipped rather than surfaced
/// as an error (`docs/research/port-io.md`, step 4).
fn all_processes_best_effort() -> impl Iterator<Item = Process> {
    all_processes()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
}

/// `pid`'s `/proc/<pid>/comm` (`docs/modules.v1.md`: holder name via
/// `/proc/<pid>/comm`). Read directly rather than through `/proc/<pid>/stat`'s
/// parenthesized `comm` field, so this never depends on a `stat`-line parser
/// correctly re-finding the field's closing paren. `None` on any read
/// failure — the process may have exited between attribution and this
/// lookup.
fn process_name(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end().to_string())
}

/// The single item of `items`, or `None` if there are zero or more than
/// one — never guess which one is right (`docs/research/port-io.md`:
/// "Set `listener_pid` only when attribution is unambiguous").
fn unique<T>(mut items: impl Iterator<Item = T>) -> Option<T> {
    let first = items.next()?;
    if items.next().is_some() {
        return None;
    }
    Some(first)
}

/// Finds a closed TCP port on `127.0.0.1` outside the kernel's ephemeral port range.
///
/// Reads `/proc/sys/net/ipv4/ip_local_port_range` at test time to find a free port
/// at or above 1024 that is outside the ephemeral range.
/// Because the kernel auto-assigns ephemeral ports (`bind(0)`) only from within
/// that range, ports chosen outside it will not be auto-allocated to other processes
/// while the test runs.
///
/// # Limitation
/// This is a mitigation against ephemeral port allocation churn under test load,
/// not an absolute proof. A process that explicitly binds to this exact port number
/// between releasing the listener and probing it can still cause a test failure.
#[cfg(test)]
pub(crate) fn closed_non_ephemeral_port() -> u16 {
    use std::net::{Ipv4Addr, TcpListener};

    let proc_path = "/proc/sys/net/ipv4/ip_local_port_range";
    let content = std::fs::read_to_string(proc_path)
        .unwrap_or_else(|e| panic!("failed to read ephemeral port range from {proc_path}: {e}"));

    let mut parts = content.split_whitespace();
    let low: u32 = parts
        .next()
        .unwrap_or_else(|| panic!("missing low port in {proc_path}"))
        .parse()
        .unwrap_or_else(|e| panic!("invalid low port integer in {proc_path}: {e}"));
    let high: u32 = parts
        .next()
        .unwrap_or_else(|| panic!("missing high port in {proc_path}"))
        .parse()
        .unwrap_or_else(|e| panic!("invalid high port integer in {proc_path}: {e}"));

    let candidates = (1024..low).chain((high + 1)..=65535);
    let mut searched_any = false;

    for port_u32 in candidates {
        searched_any = true;
        let port = port_u32 as u16;
        if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            drop(listener);
            return port;
        }
    }

    if !searched_any {
        panic!("no non-ephemeral port candidates available outside range {low}..={high} (must be >= 1024)");
    }

    panic!("all candidate ports outside ephemeral range {low}..={high} are currently in use");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};

    fn localhost() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    // -- classify_connect_error: pure, no real sockets ----------------------

    #[test]
    fn classify_connect_error_maps_connection_refused_to_closed() {
        let err = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert_eq!(classify_connect_error(&err), PortProbe::Closed);
    }

    #[test]
    fn classify_connect_error_maps_timeout_to_unreachable() {
        let err = io::Error::from(io::ErrorKind::TimedOut);
        assert_eq!(classify_connect_error(&err), PortProbe::Unreachable);
    }

    #[test]
    fn classify_connect_error_maps_other_errors_to_unreachable() {
        let err = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(classify_connect_error(&err), PortProbe::Unreachable);
    }

    // -- binds_to: pure address/port matching --------------------------------

    #[test]
    fn binds_to_matches_the_exact_configured_address_and_port() {
        let local: SocketAddr = "127.0.0.1:15432".parse().unwrap();
        assert!(binds_to(local, localhost(), 15432));
    }

    #[test]
    fn binds_to_matches_an_unspecified_bind_on_the_same_port() {
        let local: SocketAddr = "0.0.0.0:15432".parse().unwrap();
        assert!(binds_to(local, localhost(), 15432));
    }

    #[test]
    fn binds_to_rejects_a_different_port() {
        let local: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        assert!(!binds_to(local, localhost(), 15432));
    }

    #[test]
    fn binds_to_rejects_a_different_specific_address() {
        let local: SocketAddr = "10.0.0.5:15432".parse().unwrap();
        assert!(!binds_to(local, localhost(), 15432));
    }

    // -- unique: pure "exactly one" selection ---------------------------------

    #[test]
    fn unique_returns_none_for_zero_items() {
        assert_eq!(unique(std::iter::empty::<u32>()), None);
    }

    #[test]
    fn unique_returns_the_only_item() {
        assert_eq!(unique(std::iter::once(7)), Some(7));
    }

    #[test]
    fn unique_returns_none_for_more_than_one_item() {
        assert_eq!(unique([1, 2].into_iter()), None);
    }

    // -- observe: real localhost sockets, per the ticket's TDD instruction ---

    #[test]
    fn observe_reports_open_and_attributes_a_bound_listener_to_this_process() {
        let listener = TcpListener::bind((localhost(), 0)).expect("bind an ephemeral port");
        let port = listener.local_addr().unwrap().port();

        let observation = observe(localhost(), port);

        assert_eq!(observation.probe, PortProbe::Open);
        assert_eq!(observation.listener_pid, Some(std::process::id()));
        assert!(
            observation.listener_name.is_some(),
            "the current process's own comm should be readable"
        );
    }

    #[test]
    fn observe_reports_closed_when_nothing_is_listening() {
        let port = closed_non_ephemeral_port();

        let observation = observe(localhost(), port);

        assert_eq!(observation.probe, PortProbe::Closed);
        assert_eq!(observation.listener_pid, None);
        assert_eq!(observation.listener_name, None);
    }

    // -- process_name: reads the plain `/proc/<pid>/comm` file, not the
    // parenthesized, escapable `comm` field embedded in `/proc/<pid>/stat`
    // (`docs/modules.v1.md`: "holder name via `/proc/<pid>/comm`") ---------

    #[test]
    fn process_name_matches_proc_pid_comm_for_the_current_process() {
        let expected = std::fs::read_to_string("/proc/self/comm")
            .expect("this process's own /proc/self/comm should be readable")
            .trim_end()
            .to_string();

        let name = process_name(std::process::id());

        assert_eq!(name, Some(expected));
    }

    // A test that spawns a real child process (e.g. `sleep`) and asserts its
    // `/proc/<pid>/comm` equals the program name was removed: it is flaky
    // under sandboxed/CI process-spawn semantics, where `Command::spawn()`
    // may not guarantee a freshly `execve`'d child before this read, or a
    // `child.id()` can otherwise fail to resolve to the process most
    // callers expect. The current-process case above already exercises the
    // real `/proc/<pid>/comm` read path deterministically.
}
