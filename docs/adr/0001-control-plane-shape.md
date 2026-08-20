# Control plane is a stateless CLI over systemd --user + stock cloud-sql-proxy

We own lifecycle and status only: a short-lived Rust CLI that starts/stops Google’s `cloud-sql-proxy` as transient `systemd --user` units and reports a versioned Status document. We do not reimplement the tunnel, run a long-lived tracker daemon, or put process ownership inside the Omarchy/QML shell. That split keeps the bar thin, survives shell restarts, and gives terminal and plugin one seam (`argv` + JSON).
