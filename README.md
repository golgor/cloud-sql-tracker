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

## Develop

Needs [mise](https://mise.jdx.dev/). Pin is Rust **1.97.1** (same number in `mise.toml`, `Cargo.toml` `rust-version`, and CI).

```bash
mise install          # rust + rustfmt + clippy, hk, check-jsonschema; installs git hooks
mise run check        # fmt-check, clippy, cargo test, Layer 1 contracts
mise run fmt          # cargo fmt
mise run test         # cargo test (never --include-ignored)
mise run build-release  # cargo build --release --locked (same profile as GitHub Release)
mise run install-local  # release build + symlink ~/.local/bin/cloud-sql-tracker -> target/release/…
```

`install-local` is for dogfood on PATH: rebuild with the same command after code changes (the symlink stays put). Prefer a debug `cargo build` loop only when you need faster compile cycles and do not care about release size/speed.

Focused Cargo commands stay valid (`cargo test reconcile`). Prefer `mise run …` for the full gate so tool versions match. Hooks: `hk install --mise` (also the mise `postinstall` hook). Pre-commit formats Rust. Pre-push runs the same checks as CI.

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

## Install

### From source (dogfood / development)

```bash
cargo install --path . --locked
```

Pinned tag (after a release exists):

```bash
cargo install --git https://github.com/golgor/cloud-sql-tracker \
  --tag v0.1.0 --locked
```

### From a GitHub Release (Linux x86_64)

Each tag `vX.Y.Z` (matching `Cargo.toml` `[package].version`) publishes a Release asset:

- `cloud-sql-tracker-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS`

```bash
# example for v0.1.0 — replace the version to match the Release
VERSION=0.1.0
ARCHIVE=cloud-sql-tracker-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz

curl -fsSL -O "https://github.com/golgor/cloud-sql-tracker/releases/download/v${VERSION}/${ARCHIVE}"
curl -fsSL -O "https://github.com/golgor/cloud-sql-tracker/releases/download/v${VERSION}/SHA256SUMS"
sha256sum -c SHA256SUMS --ignore-missing

tar -xzf "$ARCHIVE"
install -Dm755 cloud-sql-tracker ~/.local/bin/cloud-sql-tracker
cloud-sql-tracker --version   # bare X.Y.Z
```

Requires:

- `cloud-sql-proxy` on `PATH`
- systemd user session (typical desktop login on Arch/Omarchy)
- Application Default Credentials (`gcloud auth application-default login`) for the proxy itself

Release build notes: [docs/research/release-build.md](docs/research/release-build.md).

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
