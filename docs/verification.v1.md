# Test and dogfood verification — v1

**Canonical strategy** for what must be proven, and where. This freeze is **prose only** (no JSON Schema, no test suite in this PR).

| Artifact | Path |
|----------|------|
| This prose | `docs/verification.v1.md` |
| Module seams | [`docs/modules.v1.md`](./modules.v1.md) |
| Reconcile truth table | [`docs/reconcile.v1.md`](./reconcile.v1.md) |
| CLI argv / exits | [`docs/cli-contract.v1.md`](./cli-contract.v1.md) |
| Config | [`docs/config.v1.md`](./config.v1.md) |
| Status JSON | [`docs/status-document.v1.md`](./status-document.v1.md) |
| Doctor JSON | [`docs/doctor.v1.md`](./doctor.v1.md) |
| Canonical Status snapshot | [`examples/status.v1.json`](../examples/status.v1.json) |
| CI layers (goldens + CLI vs schemas) | [issue #23](https://github.com/golgor/cloud-sql-tracker/issues/23) |
| Wayfinder freeze | [issue #13](https://github.com/golgor/cloud-sql-tracker/issues/13) |

The binary remains a stub until the **implementation map**. Tests listed here are written **with the code** (TDD), not in this freeze.

---

## Two maps

| Map | Job | Closes when |
|-----|-----|-------------|
| **[#2](https://github.com/golgor/cloud-sql-tracker/issues/2) — spec** | Freeze product contracts + module seams + this strategy | **#13 merged** and Decisions so far updated |
| **Implementation map** (next session: `/wayfinder`) | Build the CLI, land the tests below, dogfood | Required `cargo test` green **and** dogfood attested on **that** map |

Do **not** keep #2 open as a parent of the implementation map.

---

## Automated

Hit the **same interfaces** callers use ([`modules.v1.md`](./modules.v1.md)). No public test traits. One adapter per I/O kind.

### Required `cargo test` (no systemd, no Cloud SQL)

| Surface | Must prove |
|---------|------------|
| `config::parse` | Golden [`examples/connections.json`](../examples/connections.json) loads. Unknown keys, duplicate `id`/`port`/`instance`, reserved ports (5432/3306/1433 and 1–1023) **reject**. |
| `reconcile` | **Every** truth-table row in [`reconcile.v1.md`](./reconcile.v1.md) with injected `now`. |
| Status / Doctor JSON | Serialize fixture rows → validate against [`schemas/status.v1.json`](../schemas/status.v1.json) and [`schemas/doctor.v1.json`](../schemas/doctor.v1.json). Construct **Observation** in-process (no real units). This **is** [issue #23](https://github.com/golgor/cloud-sql-tracker/issues/23) **Layer 2** in-process. |
| Selector | `id` / `--group` / `--all`; `enabled: false` skipped on multi-target start; **`--failed` is an error-state filter**, not a fourth selector. Empty after filter = success. |
| `model::unit_name` | `cloud-sql-proxy-<id>.service` (single owner). |
| `cli` smokes | `--version` / `-V` prints bare `CARGO_PKG_VERSION`. A **couple** of usage failures exit `2` (e.g. unknown id, missing start target, id+`--all`). **Not** a full argv matrix. |

`cargo test` (default) is the automated bar. GitHub Actions that run it belong to implementation / [#23](https://github.com/golgor/cloud-sql-tracker/issues/23), not this spec map.

### Adapters

| Adapter | Unit tests |
|---------|------------|
| `port`, `env` | Allowed and cheap: localhost sockets, temp ADC file. |
| `supervisor`, `journal` | **Not required** as unit tests. |

Do **not** add a `trait Supervisor` (or Clock, or Journal) for tests. A second real adapter is the only reason to introduce a trait.

### Optional `#[ignore]` (real systemd + `cloud-sql-proxy`)

Allowed later on the implementation map. **CI must never run them** (`cargo test` default; do not pass `--include-ignored` in workflows). Not a gate for either map.

---

## Dogfood (human gate — implementation map)

Operator config is expected to match the seven Connections in the golden (ports **15432–15438**, groups `backend` / `fe` / `iot`). Attest with a **comment on the implementation map** when every **required** item is true. Do not treat this file as a living checkbox list. Do not attest on [#2](https://github.com/golgor/cloud-sql-tracker/issues/2) (that map will already be closed).

**Required**

- `doctor` hard checks pass (`ok: true`; port warns allowed)
- Start all 7 Connections (`start --all` or per-group) → Health `running`, local port open
- `status --json` looks right (schema `version` 1; ids/ports/groups match)
- DBeaver or `psql` on **at least one port per group**
- `logs <id>` dumps journal lines (or empty + stderr hint)
- `stop --all`
- Old one-liner proxy scripts **retired**

**Optional**

- `restart --failed` after staging a Health `error` (skip if staging a failed unit is annoying)

---

## Plugin fixtures

**Not this work.** [`examples/status.v1.json`](../examples/status.v1.json) is the canonical Status snapshot. The plugin map may copy or vendor it later. Do not dual-maintain extra Status JSON here “for the plugin.”

---

## Leftover issues

| Issue | After spec map closes |
|-------|------------------------|
| [#23](https://github.com/golgor/cloud-sql-tracker/issues/23) CI | **Stays open.** Spec map does **not** wait. Implementation map **adopts** it. Layer 1 (goldens vs schemas) may merge whenever. **Do not close #23 on Layer 1 alone.** Layer 2 = the Status/Doctor/config cargo tests above. |
| [#15](https://github.com/golgor/cloud-sql-tracker/issues/15) proxy HTTP health | **Stays open**, **not** on the implementation map (stretch / future product). |

---

## Handoff — implementation map (`/wayfinder`, next session)

Destination sketch (edit when the map is created):

- Dogfoodable v1 CLI per frozen contracts.
- Inherit **this file** as the proof bar.
- Research crates as needed (JSON Schema validator, systemd/zbus vs `systemd-run`, procfs/listeners) **inside** adapter modules — do not reopen seams.
- Adopt **#23**. Leave **#15** alone.
- TDD: required tests land with the code.
- Close that map when required `cargo test` is green **and** dogfood is attested there.

---

## Out of scope for this freeze

- Writing the tests or implementing the CLI
- GitHub Actions workflows
- `SOFTWARE_DESIGN.md`
- Splitting this ticket into “strategy” vs “DoD”
- Re-litigating status / CLI / config / reconcile / doctor / logs / module seams
