# cloud-sql-tracker

Stateless CLI control plane for multiple [Google Cloud SQL Auth Proxy](https://github.com/GoogleCloudPlatform/cloud-sql-proxy) processes.

This tool does **not** replace `cloud-sql-proxy`. It starts, stops, and reports on it — with a fixed local port per database, groups, and machine-readable status for UIs (including an [Omarchy](https://omarchy.org/) bar plugin).

```
cloud-sql-tracker status --json
cloud-sql-tracker start fe-dev
cloud-sql-tracker stop --group backend
```

## Status

**Contracts locking via PRs.** Binary still a stub (`0.1.0` in `Cargo.toml` only — single version source).

| | |
|--|--|
| Design | [docs/DESIGN.md](docs/DESIGN.md) |
| Status document v1 | [docs/status-document.v1.md](docs/status-document.v1.md) · [schema](schemas/status.v1.json) · [example](examples/status.v1.json) |
| CLI contract v1 | [docs/cli-contract.v1.md](docs/cli-contract.v1.md) |
| Module seams v1 | [docs/modules.v1.md](docs/modules.v1.md) |
| Verification v1 | [docs/verification.v1.md](docs/verification.v1.md) |
| Config v1 | [docs/config.v1.md](docs/config.v1.md) · [schema](schemas/config.v1.json) · [example](examples/connections.json) |
| Agent guide | [AGENTS.md](AGENTS.md) |
| ADRs | [docs/adr/](docs/adr/) |
| Research (tradeoffs) | [docs/RESEARCH.md](docs/RESEARCH.md) |
| Research (systemd/ports/logs) | [docs/research/](docs/research/) |
| Example config | [examples/connections.json](examples/connections.json) |
| Omarchy plugin | [cloud-sql-tracker-oma-plugin](https://github.com/golgor/cloud-sql-tracker-oma-plugin) |

## Why

One-liner scripts like `cloud-sql-proxy project:region:instance` all default to Postgres **5432**, collide with each other and with local Docker Compose, and give you no shared status surface. This CLI owns:

- explicit ports (e.g. 15432+)
- start/stop (planned: `systemd --user` transient units)
- `status --json` for bars and scripts
- port-conflict detection when something else already holds a Connection’s port

## Install (planned)

```bash
# once implemented
cargo install --path .
# or download a release binary to ~/.local/bin
```

Requires:

- `cloud-sql-proxy` on `PATH`
- systemd user session (typical desktop login on Arch/Omarchy)
- Application Default Credentials (`gcloud auth application-default login`) for the proxy itself

## Config

```bash
mkdir -p ~/.config/cloud-sql-tracker
cp examples/connections.json ~/.config/cloud-sql-tracker/connections.json
# edit ports/instances to match your project and DBeaver hosts
```

Full rules: [docs/config.v1.md](docs/config.v1.md). Ports must not be 5432 / 3306 / 1433 (or 1–1023). Unknown JSON keys are errors.

## Commands (v1 contract)

See [docs/cli-contract.v1.md](docs/cli-contract.v1.md).

```
cloud-sql-tracker --version          # bare semver from Cargo.toml
cloud-sql-tracker status --json
cloud-sql-tracker start  <id|--group G|--all>
cloud-sql-tracker stop   <id|--group G|--all>
cloud-sql-tracker restart [--failed] <id|--group G|--all>
cloud-sql-tracker logs <id>
cloud-sql-tracker doctor [--json]
```

No `list` in v1. Later: `config` subcommands so UIs never need filesystem access to the config file.

## Omarchy bar

The bar widget lives in a **separate** repository so `omarchy plugin add` only clones QML + manifest:

```bash
omarchy plugin add https://github.com/golgor/cloud-sql-tracker-oma-plugin.git --enable
```

The plugin expects this binary on `PATH` and speaks only via CLI JSON (no direct config file access).

## License

MIT
