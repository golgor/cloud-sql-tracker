# Doctor report — schema version 1

**Canonical product contract** for `cloud-sql-tracker doctor` and `doctor --json`.

| Artifact | Path |
|----------|------|
| This prose | `docs/doctor.v1.md` |
| JSON Schema | [`schemas/doctor.v1.json`](../schemas/doctor.v1.json) |
| Golden example | [`examples/doctor.v1.json`](../examples/doctor.v1.json) |
| CLI argv / exit codes | [`docs/cli-contract.v1.md`](./cli-contract.v1.md) |
| Wayfinder freeze | [issue #11](https://github.com/golgor/cloud-sql-tracker/issues/11) |

Doctor is **not** a Status document. Live Connection Health stays `status --json` ([status-document.v1.md](./status-document.v1.md)).

---

## Mental model

**Doctor = preflight / setup checklist** for one machine and one config path.

```text
doctor [--json]
   │
   ├─ try load+validate config          → check `config`
   ├─ resolve + version-probe proxy bin → check `proxy_bin`
   ├─ probe systemd --user bus          → check `systemd_user`
   ├─ locate ADC credentials (local)    → check `adc`
   ├─ smoke user journal                → check `journal_user`
   └─ scan configured ports (if config) → check `ports` (warn-only conflicts)
```

| Is | Is not |
|----|--------|
| “Can this environment run the control plane?” | A second Status UI |
| Read-only observation + hints | Auto-fix, `gcloud login`, killing processes |
| Stable check `id`s for scripts | Cloud SQL / IAM reachability probe (v1) |

---

## CLI

```text
cloud-sql-tracker doctor [--json]
cloud-sql-tracker --config PATH doctor [--json]
```

- No target selector (`id` / `--group` / `--all`).
- **`--json`:** one Doctor report object on stdout (this schema). No extra prose on stdout.
- **Human (default):** one line per check, e.g. `PASS config — …` / `WARN ports — …` / `FAIL adc — …` plus `hint` on warn/fail when present. Final summary line optional (implementation detail).

### Exit codes

| Code | When |
|------|------|
| `0` | No check has `status: fail` (warns allowed). `ok === true`. |
| `2` | Usage only (bad global flags / unknown args). **Not** used for invalid config discovered *inside* doctor. |
| `3` | At least one check has `status: fail`, or doctor cannot produce a report for an environmental reason after starting. |

Invalid config on `start`/`status`/… remains exit **2** (fail-fast). Invalid config on **doctor** is check `config` = `fail` and overall exit **3** with a full report when possible.

---

## Config load path (important)

Most subcommands: parse argv → **load+validate config or exit 2** → run command.

**Doctor does not fail-fast before the checklist.** Config load+validate is **check id `config`**, using the **same** validation rules as [config.v1.md](./config.v1.md).

| Command | Config |
|---------|--------|
| `status`, `start`, `stop`, `restart`, `logs` | Required up front; errors → exit **2** |
| `doctor` | Attempted as a check; failure → `config`/`fail`, other independent checks still run |
| `--version` / `--help` | No config |

Shared library validator; doctor maps `Err` → a check row. No second rule set.

If config fails to load: still run `proxy_bin`, `systemd_user`, `adc`, `journal_user`. Set `ports` to **`pass`** with detail that the scan was skipped, **or** omit port details — preferred: **`ports` = `pass`**, detail `skipped: config not loaded` (not a fail; nothing to scan). Do **not** invent fake port conflicts.

---

## Output Size Cap and Backstop

The maximum output size for `doctor --json` is **64 KiB** (65,536 bytes).

- The checklist is fixed at maximum **6** checks (`config`, `proxy_bin`, `systemd_user`, `adc`, `journal_user`, `ports`).
- `detail` and `hint` strings are clamped at production seams to at most **512 UTF-8 bytes** (up to 3072 bytes in JSON output when escaped).
- As a final backstop before stdout, `doctor --json` serializes the report in memory and checks its byte length against 65,536 bytes. If it exceeds 65,536 bytes, the CLI writes **no JSON** to stdout, prints an error to stderr, and exits **3**.

---

## JSON document

### Top-level

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `version` | integer | yes | Schema id of **this** Doctor report. Always `1` for this contract. |
| `cli_version` | string | yes | Binary semver (`CARGO_PKG_VERSION`). |
| `ok` | boolean | yes | `true` iff no check has `status === "fail"`. Warns do not clear `ok`. |
| `checks` | array | yes | Ordered list of check objects (stable order recommended below). |

**Invariant:** `ok === false` ⇔ some check has `status === "fail"`.

Versioning: bump integer `version` only for breaking shape/meaning changes (same spirit as Status). Additive new check `id`s and optional fields do not require a bump. Pre-consumer freezes may still amend v1 in place.

### `checks[]` element

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `id` | string | yes | Stable machine token (catalog below). |
| `status` | string enum | yes | `pass` \| `warn` \| `fail` |
| `detail` | string | yes | Human-readable outcome (wording may change). |
| `hint` | string \| null | yes | Optional fix guidance / doc link; `null` when none. |

Unknown future `id` values: consumers should tolerate and display them.

### Recommended check order

1. `config`  
2. `proxy_bin`  
3. `systemd_user`  
4. `adc`  
5. `journal_user`  
6. `ports`  

---

## Check catalog (v1)

### `config` — hard (`fail` on problems)

- Resolve path: `--config` or default XDG path ([config.v1.md](./config.v1.md)).
- File missing, unreadable, invalid JSON, schema/validation failure (unknown keys, duplicates, reserved ports, …) → **`fail`**.
- `detail` should include JSON path when possible (same spirit as config errors).
- `hint`: e.g. point at example config / config docs path.

### `proxy_bin` — hard

- Resolve `proxy_bin` from merged config defaults when config loaded; else built-in default name `cloud-sql-proxy` on `PATH`.
- Must exist and be executable → else **`fail`** (resolve stage; no spawn).
- After a successful resolve, spawn the resolved path with argv **`-v`** only (same as `cloud-sql-proxy --version`). Wait at most **2 seconds**.
- **Pass** when the process exits 0 and stdout or stderr contains a line with `cloud-sql-proxy version <token>` **anywhere in it** (a log-prefixed line still matches; example: `cloud-sql-proxy version 2.25.2+linux.amd64`).
- **Pass `detail` format:** `{resolved_path} ({version_token})` — example: `/usr/bin/cloud-sql-proxy (2.25.2+linux.amd64)`. Prefer an absolute path when resolved.
- **Fail** when resolve fails, spawn fails, the process times out, exit status is non-zero, output is empty, or the identity line does not match. `hint` should point operators at installing `cloud-sql-proxy` or fixing `proxy_bin` in config.
- Doctor owns this probe. **Start** still only resolves the path at mutate time; it does not run `-v` on the happy path.
- v1 does **not** enforce a minimum proxy semver.

### `systemd_user` — hard

- User systemd bus usable (e.g. `systemctl --user show-environment` or equivalent D-Bus ping succeeds) → else **`fail`**.
- `hint`: graphical login / `XDG_RUNTIME_DIR` / linger docs as appropriate — wording free.

### `adc` — hard (local only)

Application Default Credentials presence for libraries/`cloud-sql-proxy` — **no network**, no token fetch, no `gcloud` invocation required.

| Priority | Rule |
|----------|------|
| 1 | If `GOOGLE_APPLICATION_CREDENTIALS` is set in the doctor process environment → that path must exist and be readable. |
| 2 | Else → default file `$HOME/.config/gcloud/application_default_credentials.json` (or equivalent under resolved `HOME`) must exist and be readable. |

Optional light sanity: non-empty file; if content is clearly not JSON, **`fail`** or **`warn`** — prefer **`fail`** only when unreadable/missing; corrupt JSON may **`fail`** with detail “unreadable/invalid ADC file”.

**Normal healthy case:** env var **unset**, default file present (typical after `gcloud auth application-default login`).

On **`fail`**, `hint` **must** include guidance to set up ADC and a link to Google’s ADC documentation, e.g.:

- https://cloud.google.com/docs/authentication/provide-credentials-adc  
- and/or mention: `gcloud auth application-default login`

Doctor **never** runs login or writes credential files.

### `journal_user` — hard

- Smoke that the user journal is usable for `logs` (e.g. `journalctl --user -n 0 --no-pager` or equivalent succeeds).
- Failure → **`fail`** with detail; doctor **still completes** other checks.
- Hard means severity of this row, not “abort the whole doctor process mid-flight.”

### `ports` — warn only (never hard in v1)

Requires a successfully loaded config. Otherwise skipped as noted above (`pass` + skipped detail).

For each Connection’s `address:port` after merge:

| Situation | Contribution |
|-----------|----------------|
| Nothing listening | OK |
| Listener is our Unit `MainPID` for that Connection | OK |
| Anything else holds the port (Foreign process, including leftover `cloud-sql-proxy`) | **Conflict** |

- If one or more conflicts → check `status: **warn**`, `detail` lists them (id, port, holder name/pid when known).
- If none → `pass`.
- Never `fail` for ports in v1 (status/`start` already enforce; doctor is advisory for migration).

---

## Severity and `ok`

| status | Counts as hard failure? | Effect on `ok` / exit |
|--------|-------------------------|------------------------|
| `pass` | no | — |
| `warn` | no | `ok` stays true if no fails; exit can still be `0` |
| `fail` | yes | `ok: false`, exit `3` |

---

## Human output (normative enough)

- One check per line; include `id` and `status` clearly.
- Print `hint` on the following line or same line for `warn`/`fail`.
- Do not print secrets or full credential file contents.

Example (illustrative):

```text
PASS  config — /home/you/.config/cloud-sql-tracker/connections.json (7 connections)
PASS  proxy_bin — /usr/bin/cloud-sql-proxy (2.25.2+linux.amd64)
PASS  systemd_user — user bus ok
PASS  adc — ~/.config/gcloud/application_default_credentials.json
PASS  journal_user — journalctl --user ok
WARN  ports — fe-dev:15434 held by cloud-sql-proxy (pid 12002)
      hint: stop the leftover process before start, or free the port
```

---

## Non-goals (v1)

- Cloud SQL Admin API / instance reachability / IAM on the instance  
- Running `gcloud auth application-default print-access-token` by default  
- Mutating system state (login, kill PIDs, write config)  
- Embedding a Status document inside doctor JSON  
- Per-Connection Health states (use `status`)  
- Plugin polling doctor instead of status  

---

## Implementer checklist (non-normative)

1. Shared `load_config` / validate used by doctor check and by other commands.  
2. Doctor subcommand entry does not `exit 2` solely because config invalid.  
3. Table-driven tests for `ok` aggregation and ADC path priority (`GOOGLE_APPLICATION_CREDENTIALS` vs default ADC file).  
4. Golden `examples/doctor.v1.json` validates against `schemas/doctor.v1.json`.  
5. Hints for ADC include the Google ADC documentation URL.
