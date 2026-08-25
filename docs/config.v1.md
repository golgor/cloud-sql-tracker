# Config file — schema version 1

**Canonical contract** for `connections.json` (default path and `--config`).

| Artifact | Path |
|----------|------|
| This prose | `docs/config.v1.md` |
| JSON Schema | [`schemas/config.v1.json`](../schemas/config.v1.json) |
| Golden example | [`examples/connections.json`](../examples/connections.json) |
| Wayfinder freeze | [issue #5](https://github.com/golgor/cloud-sql-tracker/issues/5) |
| CLI path / `--config` | [`cli-contract.v1.md`](./cli-contract.v1.md) |

Consumers: control plane CLI only (plugin never reads this file).

---

## Path

| Item | Rule |
|------|------|
| Default | `$XDG_CONFIG_HOME/cloud-sql-tracker/connections.json` if `XDG_CONFIG_HOME` is set; else `~/.config/cloud-sql-tracker/connections.json` |
| Override | `cloud-sql-tracker --config PATH …` |
| Format | JSON (**not** JSONC) — no comments, no trailing commas |

---

## Mental model

```
connections.json  →  load + strict validate + defaults merge  →  runtime Connection list
status --json     →  Status document (separate schema; see status-document.v1.md)
```

Config describes **desired** inventory (what can be started). Status describes **observed** health after reconcile.

---

## Versioning

| Field | Meaning |
|-------|---------|
| Top-level `version` | **Config schema** integer id. Currently must be `1`. |

Independent from Status document `version` and from binary `cli_version`.

- **Bump config `version`** when required fields change meaning, known keys are removed/renamed, or validation becomes incompatible.
- Adding a **new optional known key** in a later config schema version is a deliberate schema bump (v1 is **closed** to unknown keys — see below).
- Validation tightenings and key removals from `defaults` during pre-1.0 development stay on `version: 1` without a bump. See [Decision: stricter validation and source checks keep schema version 1](#decision-stricter-validation-and-source-checks-keep-schema-version-1) under Connection fields.

---

## Top-level object

| Field | Required | Default | Meaning |
|-------|----------|---------|---------|
| `version` | yes | — | Must be integer `1`. |
| `proxy_bin` | no | `"cloud-sql-proxy"` | Binary name on `PATH` or absolute path. Printable ASCII (`0x20`–`0x7E`), maximum **4095** bytes. |
| `defaults` | no | `{}` | Object merged under each connection (see merge). |
| `connections` | yes | — | Array of connection objects. May be empty `[]`. Maximum **32** Connections. Counts every row, including `enabled: false`. Over the limit fails the whole document (all-or-nothing), same as any other validation error. |

### Strict keys

**Unknown properties are rejected** at every object level (`top-level`, `defaults`, each connection).  
Rationale: typos must not silently degrade to defaults (e.g. `"prot": 15432`).

Validation failure → exit code **2** with a message that includes a JSON path when possible (e.g. `connections[2].prot`).

---

## Built-in defaults (before file `defaults`)

Applied as the base layer for every connection:

| Field | Built-in |
|-------|----------|
| `address` | `"127.0.0.1"` |
| `private_ip` | `false` |
| `auto_iam_authn` | `false` |
| `extra_args` | `[]` |
| `enabled` | `true` |

---

## Merge order

Each input value is validated at its source (`defaults` block and connection object).

For each element of `connections`:

1. Start with **built-in defaults** (table above).  
2. Overlay file-level **`defaults`** (object; only known keys allowed).  
3. Overlay the **connection object** (connection wins on conflict).  
4. Construct the final connection. The merge is a strict pick-one choice per field, so every field value is valid by construction (document-level uniqueness of `id`, `port`, and `instance` is checked across all connections after merge).

File `defaults` holds optional fields only (`address`, `private_ip`, `auto_iam_authn`, `extra_args`, `enabled`). Identity fields (`id`, `name`, `group`, `instance`, `port`) belong on each Connection object.

---

## Connection fields (after merge)

All config strings use **printable ASCII** bytes `0x20` through `0x7E` (`name`, `group`, `instance`, `address`, `proxy_bin`, and each `extra_args` element). `id` uses its existing stricter charset rule.

| Field | Required | Rules |
|-------|----------|--------|
| `id` | yes | `^[a-zA-Z0-9][a-zA-Z0-9_-]*$`, length 1–64 bytes. Suffix of unit name `cloud-sql-proxy-<id>.service`. |
| `name` | yes | Non-empty string (display label). Printable ASCII, maximum **64** bytes. |
| `group` | yes | Non-empty string (free text; not a fixed enum). Printable ASCII, maximum **32** bytes. The first character must not be `-`. |
| `instance` | yes | Cloud SQL instance connection name: exactly three non-empty segments separated by `:` — `project:region:instance` (regex: `^[^:\s]+:[^:\s]+:[^:\s]+$`). Printable ASCII, maximum **256** bytes. |
| `port` | yes | Integer **1024–65535**, and **not** in the reserved set (below). |
| `address` | no | Non-empty string; default `127.0.0.1`. Printable ASCII, maximum **253** bytes. |
| `private_ip` | no | Boolean; default `false`. |
| `auto_iam_authn` | no | Boolean; default `false` → proxy flag when true. |
| `extra_args` | no | Array of strings only (each arg already split; no shell parsing). Maximum **16** elements, each element printable ASCII, total merged byte length across all elements maximum **2048** bytes. Default `[]`. |
| `enabled` | no | Boolean; default `true`. |

### Decision: stricter validation and source checks keep schema version 1

Validation tightenings make these inputs newly invalid:

- `group` that starts with `-`.
- `name: ""` (empty string).
- `address: ""` (empty string), including `defaults.address: ""`.
- More than 32 Connections in the `connections` array.
- Non-printable ASCII characters in string fields. All string fields require printable ASCII (`0x20`–`0x7E`).
- String values exceeding byte caps (`id` 64 B, `name` 64 B, `group` 32 B, `instance` 256 B, `address` 253 B, `proxy_bin` 4095 B).
- `extra_args` exceeding 16 elements or 2048 total bytes across all elements.
- Identity fields (`name`, `group`, `instance`, `port`) in `defaults`.

**Pick:** validate every config value at its source (`defaults` block and connection object), remove identity fields from `defaults`, and keep config schema `version: 1`.

**Why:** each value is checked once at its file location. An error names the block that is wrong. Required identity fields cannot be set in `defaults`.

**Discarded:**

- Post-merge-only validation: hides an invalid default whenever a Connection overrides it.
- Source validation plus merged validation: validates values twice.
- Widening `RawDefaults` for identity fields: identity fields must be set per connection.
- Bumping config schema to `version: 2`: not necessary during pre-1.0 development.

**Unchanged:** the defaults merge order and field precedence do not change.

### Reserved ports (hard errors)

These ports are **never** allowed in config:

| Port | Reason |
|------|--------|
| **1–1023** | Privileged / not for unprivileged desktop listeners |
| **5432** | Default PostgreSQL (local Docker Compose, etc.) |
| **3306** | Default MySQL |
| **1433** | Default SQL Server |

Any other port in **1024–65535** is allowed (including the documented 15432+ golden connection map).

### Uniqueness (hard errors)

Across the whole file, after merge:

| Key | Rule |
|-----|------|
| `id` | Unique |
| `port` | Unique |
| `instance` | Unique |

Duplicate `instance` is forbidden in v1 (one Connection per Cloud SQL instance in this tool). Can be relaxed in a later config schema if a real need appears.

---

## `enabled: false`

| Behavior | Rule |
|----------|------|
| Present in Status document | **Yes** (not hidden); Status row carries `enabled: false` so consumers need not read this file |
| Typical `state` when idle | `stopped` |
| `start` / `restart` targeting this id | **Refuse**, exit **2**, clear message |
| `stop` | Success no-op |
| Included in `--group` / `--all` selectors | Yes for status; mutating commands that expand group/all **skip** disabled ids with a warning on stderr, or fail if the *only* matches are disabled — **prefer:** expand selector, attempt each; disabled → per-id failure contribution as usage-style failure for that id. Simpler v1 rule: **disabled id in an explicit single-id start → exit 2**; **disabled ids in `--group`/`--all` → skipped with stderr warning, not counted as start failures** so `start --all` still starts every enabled connection. |

**v1 rule (normative):**

- Single-id `start`/`restart` on disabled → exit **2**.  
- Multi-target: disabled connections are **skipped** (stderr warning), not started; they do not by themselves force exit 1/4 if every *enabled* target succeeded.

---

## `proxy_bin` resolution

| Phase | Rule |
|-------|------|
| Config load | Must be non-empty string if present (or default applied). **Do not** require the binary to exist on disk at load time. |
| `start` / `doctor` | Resolve: absolute path → must be executable file; bare name → `PATH` search. Failure → `bin_missing` / doctor hard fail (exit **3** or per-id failure as in CLI contract), not a config schema error. |

Units should be launched with the **resolved absolute path** when possible (see systemd research).

---

## Load failure → exit 2

| Situation | Exit |
|-----------|------|
| Config file missing | **2** |
| Unreadable file | **2** |
| Invalid JSON | **2** |
| `version` ≠ 1 | **2** |
| Unknown property | **2** |
| Validation / uniqueness failure | **2** |

Empty `connections: []` is **valid** (`status` → `total: 0`).

Prefer reporting **all** validation errors in one message when inexpensive; otherwise first error plus count of remaining is acceptable.

---

## Out of scope for this file (v1)

- JSONC / YAML  
- Per-connection `http_port` / health-check (deferred research #15)  
- `config` CLI CRUD (later map)  
- Storing credentials or connection strings  

---

## Consumer checklist (implementers / agents)

1. Parse JSON; reject on syntax error.
2. Reject unknown keys (`additionalProperties: false`).
3. Enforce field validation rules at each source (`defaults` block and connection object).
4. Merge built-ins → `defaults` → connection.
5. Enforce unique `id`, `port`, `instance`.
6. Keep prose, JSON Schema, and golden example in sync in the same PR when the contract changes.
