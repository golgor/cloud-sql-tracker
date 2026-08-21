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
The read-only mapping, for one Connection at one moment, from config identity plus observed signals (Unit, local port liveness, listener attribution) to a Health state and the related Status document fields.
_Avoid_: Desired-state loop, adopt, heal, sync engine, continuous controller

**Source**:
Whether the Health state is backed by our Unit (`unit`) or by no managed process (`none`).
_Avoid_: Origin, provider, parent, orphan (not a v1 Source value)

**Foreign process**:
A process holding a Connection’s local port that is not this control plane’s Unit (including leftover hand-started proxies).
_Avoid_: Orphan (not a v1 concept — port conflicts are errors, not adopted runtimes), stranger

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
Google’s standard credential discovery used by `cloud-sql-proxy` (typically `gcloud auth application-default login`). Libraries look for `GOOGLE_APPLICATION_CREDENTIALS` if set, otherwise the default file under the user home (e.g. `~/.config/gcloud/application_default_credentials.json`). Hard requirement for operators; not optional in v1.
_Avoid_: “gcloud login” alone (that is user credentials for gcloud CLI, not always ADC); inventing a separate product term for the env var
