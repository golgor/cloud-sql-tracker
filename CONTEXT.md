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
One of `stopped` | `starting` | `running` | `error` for a Connection, produced by Reconcile.
_Avoid_: Status (ambiguous with Status document), phase

**Reconcile**:
The read-only mapping, for one Connection at one moment, from config identity plus observed signals (Unit, local port liveness, Proxy process / Orphan attribution) to a Health state and the related Status document fields.
_Avoid_: Desired-state loop, adopt, heal, sync engine, continuous controller

**Source**:
Who owns the Proxy process behind a Connection’s Health state: our Unit, an Orphan, or none.
_Avoid_: Origin, provider, parent (ambiguous)

**Orphan**:
A Proxy process that matches a Connection (instance/port/cmdline) but was not started under this control plane’s supervisor.
_Avoid_: Foreign process, leaked proxy (unless stop failed)

**Foreign process**:
A process holding a Connection’s local port that is not this control plane’s Unit and not a matching Orphan.
_Avoid_: Orphan (Orphan is a recognized Proxy process), stranger

**Group**:
A display and bulk-action label on Connections (e.g. `fe`, `backend`, `iot`).
_Avoid_: Environment, project (GCP project is part of instance name)

**Unit**:
The systemd --user transient unit that supervises one managed Proxy process (`cloud-sql-proxy-<id>.service`).
_Avoid_: Service (alone), job

**Supervisor**:
systemd --user (via `systemd-run` / `systemctl --user`) as the process owner for managed Proxy processes.
_Avoid_: Our CLI (the CLI is not long-lived)

**ADC** (Application Default Credentials):
Google’s standard credential discovery used by `cloud-sql-proxy` (typically `gcloud auth application-default login`). Hard requirement for operators; not optional in v1.
_Avoid_: “gcloud login” alone (that is user credentials for gcloud CLI, not always ADC)
