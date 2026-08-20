# cloud-sql-tracker

Stateless CLI control plane for multiple [Google Cloud SQL Auth Proxy](https://github.com/GoogleCloudPlatform/cloud-sql-proxy) processes.

This tool does **not** replace `cloud-sql-proxy`. It starts, stops, and reports on it — with a fixed local port per database, groups, and machine-readable status for UIs (including an [Omarchy](https://omarchy.org/) bar plugin).

```
cloud-sql-tracker status --json
cloud-sql-tracker start fe-dev
cloud-sql-tracker stop --group backend
```

## Status

**Scaffold / design locked.** Implementation not started.

| | |
|--|--|
| Design | [docs/DESIGN.md](docs/DESIGN.md) |
| Example config | [examples/connections.json](examples/connections.json) |
| Omarchy plugin | [cloud-sql-tracker-oma-plugin](https://github.com/golgor/cloud-sql-tracker-oma-plugin) |

## Why

One-liner scripts like `cloud-sql-proxy project:region:instance` all default to Postgres **5432**, collide with each other and with local Docker Compose, and give you no shared status surface. This CLI owns:

- explicit ports (e.g. 15432+)
- start/stop (planned: `systemd --user` transient units)
- `status --json` for bars and scripts
- orphan detection for processes started outside the tool

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

See [docs/DESIGN.md](docs/DESIGN.md) for the schema and the initial port map.

## Planned commands

```
cloud-sql-tracker list [--json]
cloud-sql-tracker status [id|--group G|--all] [--json]
cloud-sql-tracker start <id|--group G|--all>
cloud-sql-tracker stop  <id|--group G|--all>
cloud-sql-tracker restart <id|--group G|--all>
cloud-sql-tracker logs <id>
cloud-sql-tracker doctor [--json]
```

Later: `config` subcommands so UIs never need filesystem access to the config file.

## Omarchy bar

The bar widget lives in a **separate** repository so `omarchy plugin add` only clones QML + manifest:

```bash
omarchy plugin add https://github.com/golgor/cloud-sql-tracker-oma-plugin.git --enable
```

The plugin expects this binary on `PATH` and speaks only via CLI JSON (no direct config file access).

## License

MIT
