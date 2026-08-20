# Cloud SQL Tracker

Control plane for multiple Google Cloud SQL Auth Proxy processes on a developer machine. Owns config, lifecycle, and a versioned status snapshot — never the SQL tunnel itself.

## Language

**Connection**:
One configured Cloud SQL instance plus its fixed local listen endpoint (id, name, group, instance, port, flags).
_Avoid_: Service, database (the remote DB), tunnel

**Proxy process**:
A running Google `cloud-sql-proxy` binary dedicated to one Connection.
_Avoid_: Daemon (reserved for misunderstanding our CLI), tracker process

**Control plane**:
This CLI (`cloud-sql-tracker`) — stateless, short-lived invocations that start/stop/report on Proxy processes.
_Avoid_: Agent, service manager (too broad), wrapper script

**Status document**:
The versioned JSON snapshot of all Connections’ health and aggregates returned by `status --json`.
_Avoid_: State file, report, healthcheck response

**Health state**:
One of `stopped` | `starting` | `running` | `error` for a Connection at reconcile time.
_Avoid_: Status (ambiguous with Status document), phase

**Orphan**:
A Proxy process that matches a Connection (instance/port/cmdline) but was not started under this control plane’s supervisor.
_Avoid_: Foreign process, leaked proxy (unless stop failed)

**Group**:
A display and bulk-action label on Connections (e.g. `fe`, `backend`, `iot`).
_Avoid_: Environment, project (GCP project is part of instance name)

**Unit**:
The systemd --user transient unit that supervises one managed Proxy process (`cloud-sql-proxy-<id>.service`).
_Avoid_: Service (alone), job

**Supervisor**:
systemd --user (via `systemd-run` / `systemctl --user`) as the process owner for managed Proxy processes.
_Avoid_: Our CLI (the CLI is not long-lived)
