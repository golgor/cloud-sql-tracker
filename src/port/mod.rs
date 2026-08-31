//! TCP liveness probe + best-effort listener attribution for one
//! Connection's local `address:port` (`docs/modules.v1.md`, "port —
//! liveness + attribution"). Crate choice: `docs/research/port-io.md`
//! (`std::net::TcpStream::connect_timeout` for liveness, `procfs` for
//! socket-inode-to-PID/name attribution — not `listeners`, not `ss`).
//!
//! `commands::status` (#42) composes [`observe`]'s `model::PortObservation`
//! with `supervisor::show` into Reconcile's `Observation`, and
//! `commands::doctor`'s (#44) `ports` check will reuse this same probe.
//!
//! Listener attribution needs `/proc`, so it lives behind a platform seam:
//! Linux reads it for real, every other target (`docs/research/macos-feasibility.md`)
//! reports "unknown" rather than pull in `procfs`, which does not build there.

use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::model::{PortObservation, PortProbe};

#[cfg(target_os = "linux")]
mod attribution_linux;
#[cfg(target_os = "linux")]
use attribution_linux::attribute_listener;

#[cfg(not(target_os = "linux"))]
mod attribution_other;
#[cfg(not(target_os = "linux"))]
use attribution_other::attribute_listener;

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

/// Finds a closed TCP port on `127.0.0.1` outside the kernel's ephemeral port range.
///
/// Reads `/proc/sys/net/ipv4/ip_local_port_range` at test time to find a free port
/// at or above 1024 that is outside the ephemeral range.
/// Prefers ports above the ephemeral range over ports below it to avoid common local developer services.
/// Candidate calculation uses `u32` range bounds to prevent `u16` overflow when `high` is 65535.
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

    let above_start = high.saturating_add(1).max(1024);
    let above = above_start..=65535;
    let below = 1024..low;

    if above.is_empty() && below.is_empty() {
        panic!("no non-ephemeral port candidates available outside range {low}..={high} (must be >= 1024)");
    }

    for port_u32 in above.chain(below) {
        let port = u16::try_from(port_u32).expect("candidate port is in range 1024..=65535");
        if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            drop(listener);
            return port;
        }
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
}
