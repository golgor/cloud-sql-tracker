# Research: local development loop and CI

**Issue:** [#29](https://github.com/golgor/cloud-sql-tracker/issues/29)  
**Map:** [#28](https://github.com/golgor/cloud-sql-tracker/issues/28)  
**Context:** small Rust control-plane CLI; mise is given; optional real-systemd tests must remain ignored in CI.

## Summary

Use **mise as the documented local entry point**, but keep every task a thin, visible wrapper around standard Cargo commands. Put fast formatting in `pre-commit`, put Clippy, default `cargo test`, and contract validation in `pre-push`, and run the same checks in one straightforward GitHub Actions workflow. Keep `cargo test` rather than nextest until measured test duration justifies another tool; do not adopt dormant cargo-watch.

Pin one numbered Rust toolchain and set the same number as `package.rust-version`. Validate issue #23 Layer 1 through a small repository script invoked both by mise and the same CI workflow, so the schema/golden pair list has one source of truth.

**Snapshot, not a second pin.** Toolchain numbers in this brief were a research-time lookup. [Land toolchain, hk, and GitHub Actions](https://github.com/golgor/cloud-sql-tracker/issues/34) pins **1.97.1** in `mise.toml`, `Cargo.toml` `rust-version`, and CI together. Revisit if those three and this brief disagree.

## Evidence

### mise should be the front door, not a second build system

Mise tasks are intended for project build, test, and lint commands and run with the tools and environment declared in `mise.toml`. They can express task dependencies and parallel execution, but those features are unnecessary for the initial small CLI loop. [Mise tasks](https://mise.jdx.dev/tasks/) [Running tasks](https://mise.jdx.dev/tasks/running-tasks.html)

Define thin tasks whose command is obvious:

| Task | Underlying command |
| --- | --- |
| `mise run fmt` | `cargo fmt` |
| `mise run fmt-check` | `cargo fmt --check` |
| `mise run lint` | `cargo clippy --all-targets --all-features -- -D warnings` |
| `mise run test` | `cargo test` |
| `mise run contracts` | `scripts/validate-contracts.sh` |
| `mise run check` | fmt-check, lint, test, contracts |

The Cargo and Clippy books describe `cargo fmt`, `cargo clippy`, and `cargo test` as the standard interfaces; mise should not hide flags or introduce different semantics. `cargo fmt` and Clippy are rustup components rather than Cargo built-ins, so the pinned toolchain must include `rustfmt` and `clippy`. [cargo fmt](https://doc.rust-lang.org/cargo/commands/cargo-fmt.html) [cargo clippy](https://doc.rust-lang.org/cargo/commands/cargo-clippy.html) [Clippy usage](https://doc.rust-lang.org/clippy/usage.html)

Raw Cargo remains acceptable for focused work (`cargo test reconcile`, for example). Documentation and contributor guidance should prefer mise task names for the full checks because mise also provisions the agreed versions.

### hk: cheap commit hook, complete push hook

Recommended hooks:

| Hook | Steps | Rationale |
| --- | --- | --- |
| `pre-commit` | `cargo fmt` through hk's cargo-fmt step, with `fix = true` and `stash = "git"` | Formatting is fast and deterministic; fix it before the commit rather than waiting for CI. |
| `pre-push` | `mise run fmt-check`, `mise run lint`, `mise run test`, `mise run contracts` | This is the complete local gate and matches CI. Do not run ignored tests. |

Hk documents that a pre-commit hook runs before the commit, supports a check/fix pair for cargo-fmt, and can stash unstaged work before fixes. With `stash = "git"`, hk temporarily removes unstaged changes, runs against the staged state, then restores unstaged work; this protects partially staged files but adds a stash/restore operation and can require conflict resolution when a formatter and unstaged work touch the same lines. That cost is justified only for the cheap formatter. Keep pre-push checks non-mutating. [hk hooks](https://hk.jdx.dev/hooks.html) [hk configuration](https://hk.jdx.dev/configuration.html)

Use `mise.toml` as the tool-version source and hk only for Git lifecycle. Set `HK_MISE=1` and install hooks with `hk install --mise`; hk then invokes hooks through `mise x`, so mise-provisioned tools are on `PATH` even when the shell has not activated mise. Hk's built-ins configure invocations but do not themselves download third-party tools. [hk mise integration](https://hk.jdx.dev/mise_integration.html) [hk install](https://hk.jdx.dev/cli/install.html) [hk built-ins](https://hk.jdx.dev/builtins.html)

Do not include pitchfork: it is not part of formatting, linting, or test execution and issue #29 explicitly excludes it.

### GitHub Actions: one simple workflow using standard commands

Start with one `ci.yml` workflow and one Ubuntu job, in this order:

1. checkout;
2. install the exact pinned Rust toolchain with `rustfmt` and `clippy`;
3. restore Cargo cache keyed by OS, toolchain, and `Cargo.lock`;
4. `cargo fmt --check`;
5. `cargo clippy --all-targets --all-features -- -D warnings`;
6. `cargo test`;
7. `scripts/validate-contracts.sh`.

A single sequential job minimizes workflow YAML, runner startup, duplicate tool installation, and cache complexity; Clippy and tests can reuse compiled artifacts in `target`. Split jobs only after timings show that parallel failure reporting is worth the duplication. GitHub's Rust guide explicitly uses ordinary Cargo commands and documents caching Cargo registries, git dependencies, and `target` with a `Cargo.lock`-based key. [GitHub Actions: building and testing Rust](https://docs.github.com/actions/tutorials/build-and-test-code/building-and-testing-rust) [Dependency caching](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching)

Use `-D warnings` for Clippy. On an unpinned floating toolchain, new lints can create surprise failures; the numbered toolchain pin removes that churn from routine runs. `--all-targets --all-features` makes the lint gate cover tests and feature combinations as the crate grows. Clippy documents that its default groups already cover correctness, suspicious, style, complexity, and performance lints. [Clippy lint groups](https://doc.rust-lang.org/clippy/)

The test command must remain exactly the default behavior required by `docs/verification.v1.md`: **never add `--include-ignored`** (nor nextest's equivalent `--run-ignored all`). Optional real-systemd or proxy tests marked `#[ignore]` are human/local-only and are not a CI gate. Nextest also excludes ignored tests by default, but there is no reason to change runners merely for this property. [Nextest test selection](https://nexte.st/docs/selecting/)

### Keep `cargo test`; defer nextest

Nextest builds test binaries, lists tests, and then runs each test in a separate process in parallel. It offers better per-test isolation, richer reporting, retries, and can be faster for large suites. It also has a thicker interface and fewer stability guarantees than Cargo, and it currently does not run doctests without a separate `cargo test --doc` step. [How nextest works](https://nexte.st/docs/design/how-it-works/) [Nextest](https://nexte.st/)

For this small CLI, those benefits do not yet offset installation/versioning, configuration, a changed execution model, and a second doctest command. `docs/verification.v1.md` already names default `cargo test` as the automated bar. Adopt nextest only if recorded CI timings show test execution—not compilation or dependency download—is a meaningful bottleneck, or if per-test retries/timeouts/JUnit output become a concrete requirement.

### Do not add cargo-watch

Cargo-watch can rerun `check` or `test` on file changes, but its maintainers declare version 8.5.3 the final release and the project dormant. It can also conflict with rust-analyzer. [Cargo Watch 8.5.3](https://watchexec.github.io/downloads/cargo-watch/8.5.3/index.html)

Do not provision it or make it a project task. Editors and direct `mise run test` are sufficient for a small crate. If a watch loop becomes desirable, mise already supports watching tasks with declared sources, avoiding a dormant extra tool; add that only from demonstrated developer demand. [Mise task sources](https://mise.jdx.dev/tasks/toml-tasks.html)

### Pin a numbered MSRV/toolchain

Cargo defines `package.rust-version` as the minimum Rust version supported by the package and requires a bare version number. It also uses that field when selecting compatible dependencies. [Cargo rust-version](https://doc.rust-lang.org/cargo/reference/rust-version.html)

ADR 0004 requires modern stable Rust **and** an explicit MSRV. Implement that as follows:

- choose the current stable release at the time the implementation PR lands;
- record its exact number in both `Cargo.toml` `rust-version` and mise's Rust tool version;
- use that exact version in CI, not the moving string `stable`;
- upgrade deliberately in a small PR when a newer compiler is needed.

The repository now pins `rust-version = "1.97.1"` in `Cargo.toml`, `mise.toml`, and CI together. Upgrade that number deliberately in a small PR; do not leave Cargo, mise, and CI on different versions. A separate floating-stable CI lane is unnecessary for this application CLI unless the project later chooses to test future compiler compatibility.

### Issue #23 Layer 1: script called locally and by the same workflow

Create one small `scripts/validate-contracts.sh` that invokes a mise-pinned `check-jsonschema` (or equivalent) for exactly these pairs:

- `schemas/status.v1.json` ↔ `examples/status.v1.json`;
- `schemas/config.v1.json` ↔ `examples/connections.json`;
- `schemas/doctor.v1.json` ↔ `examples/doctor.v1.json`.

Call the script from `mise run contracts`, pre-push, and the same GitHub Actions job. The script—not duplicated YAML—is the registry of contract pairs and makes the check equally easy locally. Do not include `examples/logs.v1.txt`, which is plain text. This is only Layer 1; it must not close issue #23, whose Layer 2 requires real CLI stdout and config behavior to match schemas.

## Recommended implementation sketch

The follow-up implementation ticket should land only:

1. a `mise.toml` pinning Rust, hk, and the JSON Schema validator and defining the thin tasks above;
2. an `hk.pkl` with formatter-only pre-commit and complete non-mutating pre-push hooks;
3. `scripts/validate-contracts.sh` with the three Layer 1 pairs;
4. one `.github/workflows/ci.yml` running fmt-check, Clippy with denied warnings, default `cargo test`, and the contract script;
5. brief contributor instructions for `mise install`, hook installation, `mise run check`, and focused raw Cargo commands.

Do not add nextest, cargo-watch, pitchfork, `--include-ignored`, product-contract edits, or multiple workflows in the initial slice.

## Risks and follow-up triggers

1. **Formatter plus partial staging:** hk's stash/restore protects unstaged work, but overlapping formatter changes can still need manual conflict handling. If this is frequent, change pre-commit to check-only rather than removing formatting from the gate.
2. **`-D warnings` upgrades:** compiler/Clippy upgrades can expose new warnings. Keep them deliberate by pinning the toolchain and upgrade in a focused PR.
3. **Cache staleness or size:** caching `target` is simple and officially documented, but measure workflow time and cache usage; remove `target` from the cache if restore/save cost exceeds compilation savings.
4. **Nextest revisit:** reconsider only when test execution is demonstrably slow or richer test isolation/reporting is needed.
5. **Layer 2 remains:** the contract script proves only committed examples against schemas. Issue #23 remains open until running CLI output and config parsing are also covered.

## Sources

- Kept: [Issue #29](https://github.com/golgor/cloud-sql-tracker/issues/29) — required decisions and scope.
- Kept: [Issue #23](https://github.com/golgor/cloud-sql-tracker/issues/23) — exact Layer 1 pairs and Layer 2 boundary.
- Kept: [mise tasks](https://mise.jdx.dev/tasks/) and [running tasks](https://mise.jdx.dev/tasks/running-tasks.html) — task purpose, environment, dependencies, and execution.
- Kept: [hk hooks](https://hk.jdx.dev/hooks.html), [configuration](https://hk.jdx.dev/configuration.html), and [mise integration](https://hk.jdx.dev/mise_integration.html) — hook events, stash/fix behavior, and tool provisioning.
- Kept: [Cargo fmt](https://doc.rust-lang.org/cargo/commands/cargo-fmt.html), [Clippy](https://doc.rust-lang.org/clippy/usage.html), and [Cargo rust-version](https://doc.rust-lang.org/cargo/reference/rust-version.html) — canonical commands and MSRV semantics.
- Kept: [GitHub Actions Rust guide](https://docs.github.com/actions/tutorials/build-and-test-code/building-and-testing-rust) and [dependency caching](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching) — workflow and cache mechanics.
- Kept: [Nextest design](https://nexte.st/docs/design/how-it-works/) and [test selection](https://nexte.st/docs/selecting/) — execution model and ignored-test defaults.
- Kept: [Cargo Watch final release](https://watchexec.github.io/downloads/cargo-watch/8.5.3/index.html) — upstream dormancy notice.
- Dropped: blog roundups and third-party workflow templates — secondary or add unnecessary actions.
- Dropped: pitchfork — explicitly out of scope and not a test runner.

## Gaps

No workflow timing exists because implementation has not started. The one-job recommendation and the decision to defer nextest should be revisited from measured CI timing after the required test suite lands, not from synthetic benchmarks.

---
