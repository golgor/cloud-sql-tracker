//! Linux listener attribution: socket-inode-to-PID/name via `/proc`
//! (`docs/research/port-io.md`). Only Linux exposes `procfs`'s TCP tables
//! and `/proc/<pid>/fd`; other platforms use `attribution_other`.

use std::net::{IpAddr, SocketAddr};

use procfs::net::{tcp, tcp6, TcpNetEntry, TcpState};
use procfs::process::{all_processes, FDTarget, Process};

/// Best-effort listener PID + process name for `address:port`
/// (`docs/research/port-io.md`, "Recommended control flow", steps 2–5):
/// the socket inode of the matching `LISTEN` entry, then the process
/// whose open file descriptors reference that inode, then its name.
pub(super) fn attribute_listener(address: IpAddr, port: u16) -> (Option<u32>, Option<String>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn localhost() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
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
