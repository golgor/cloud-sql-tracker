//! Non-Linux listener attribution stub (`docs/research/macos-feasibility.md`).
//! macOS (and any other non-Linux target) has no `/proc`, so there is no
//! socket-inode-to-PID lookup here. Attribution is diagnostic only —
//! `port::observe`'s `probe` result is unaffected — so returning "unknown"
//! is correct, not a lie.
pub(super) fn attribute_listener(
    _address: std::net::IpAddr,
    _port: u16,
) -> (Option<u32>, Option<String>) {
    (None, None)
}
