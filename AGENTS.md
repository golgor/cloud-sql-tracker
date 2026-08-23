# Agent notes — cloud-sql-tracker

This repo is the **control plane CLI** for multiple Google Cloud SQL Auth Proxy processes. The Omarchy bar plugin is a **separate** repo: `cloud-sql-tracker-oma-plugin`.

## Audience

The operator is a **senior developer**. Treat design and trade-offs at that level.

Experience that is already there:

- Linux as a daily OS (Arch / Omarchy): packages, systemd as a user, files, permissions.
- Containers and scripts. Not native Linux services.

Experience that is **not** there:

- Rust beyond basics (ownership, traits, Cargo features, MSRV).
- Talking to the kernel or systemd from a program (`/proc`, D-Bus, zbus, transient Units, procfs).

The operator wants to **learn** that native layer. When a Linux or Rust term first appears in chat, a research brief, or a PR comment, add **one short sentence** of what it is and why this Control plane uses it. Link a primary source (man page, systemd docs, The Rust Book, crate docs) when that helps. Do not skip D-Bus, Unit, `/proc`, clippy, or MSRV. Do not teach `pacman` or “what is a process.”

## Writing (chat, issues, docs, PR comments)

Use terms from [`CONTEXT.md`](./CONTEXT.md). Write in **[ASD-STE100](https://www.asd-ste100.org/) Simplified Technical English** (controlled English for technical docs: short sentences, one idea each, same word for the same thing):

- Use short sentences. Put one idea in each sentence.
- Use the same word for the same thing.
- Use active voice. Do not use slang or idioms.

When you state a **choice**, use this order:

1. **Pick** — what we use.
2. **Why** — one reason.
3. **Discarded** — what we do not use, and why (one line).
4. **Unchanged** — what this choice does not change.

Start with the Pick. Wrong: “Not nextest; no cargo-watch; mise wraps cargo.” Right:

> **Pick:** mise tasks wrap cargo. **Why:** one toolchain and the same commands locally and in CI. **Discarded:** nextest (crate is small). **Unchanged:** `cargo test` stays the bar.

**Research briefs** live in [`docs/research/`](./docs/research/). They are human prose. Do not put subagent `acceptance-report` JSON or `/tmp` paths in them. Crate and toolchain numbers in a brief are **snapshots**. On the **implementation ticket**, under `## Question`, put:

- **Brief:** path to `docs/research/…`
- **Gist:** Pick in one line
- **Pin:** the crate or toolchain version this slice locks (or “pin then-current X.Y when landing”)

Do not point only at a closed GitHub issue.

**HITL / dogfood tickets:** the issue body must list **checkbox steps** the human can tick. Linking [`docs/verification.v1.md`](./docs/verification.v1.md) alone is not enough. Normative text stays in the doc; the issue is the working checklist. Attest on the **implementation map**, not a closed spec map.

## Git workflow (PRs)

- Do **not** commit product/docs freezes straight to `main` when collaborating via history-friendly flow.
- Branch from latest `main`: `wayfinder/<ticket>-short-slug` or `feat/…` / `docs/…` / `fix/…`.
- Open a **Pull Request** into `main`; link the Wayfinder issue (`Fixes #N` / `Closes #N` when the PR fully resolves it).
- Prefer one logical decision or slice per PR so `main` history stays reviewable.
- After merge, update that map’s **Decisions so far** in the **same session**. GitHub may auto-close a parent map when the last child closes — still edit the body.
- Parallel PRs that both touch `src/lib.rs`, `src/commands.rs`, or `Cargo.lock`: merge **one at a time**; rebase the rest onto `main` (keep every `mod` line).
- Implement on a **git worktree**. After merge or abandon: `git worktree remove --force <path>`, then `git branch -D`. `branch -D` fails while a worktree still holds the branch.
- Pushing `.github/workflows/**` needs the **`workflow` OAuth scope**. The `gh` HTTPS credential-helper token does not have it, and `url.https://github.com/.insteadof git@github.com:` rewrites SSH back to HTTPS, so plain `git push git@…` still fails. Push those branches over SSH explicitly: `git push ssh://git@github.com/golgor/cloud-sql-tracker.git HEAD:refs/heads/<branch>`.

## Read before changing contracts

| Doc | Why |
|-----|-----|
| [`CONTEXT.md`](./CONTEXT.md) | Domain language (Connection, Status document, Health state, Foreign process, …) |
| [`docs/DESIGN.md`](./docs/DESIGN.md) | Product decisions |
| [`docs/adr/`](./docs/adr/) | Hard-to-reverse choices |
| [`docs/cli-contract.v1.md`](./docs/cli-contract.v1.md) | **Argv, --version, exit codes** |
| [`docs/config.v1.md`](./docs/config.v1.md) | **connections.json validation + defaults merge** |
| [`schemas/config.v1.json`](./schemas/config.v1.json) | Machine-readable config schema |
| [`examples/connections.json`](./examples/connections.json) | Golden config |
| [`docs/status-document.v1.md`](./docs/status-document.v1.md) | **Full field-by-field meaning** of `status --json` |
| [`schemas/status.v1.json`](./schemas/status.v1.json) | Machine-readable Status document schema |
| [`examples/status.v1.json`](./examples/status.v1.json) | Golden snapshot |
| [`docs/reconcile.v1.md`](./docs/reconcile.v1.md) | **Health state transitions** (pure Reconcile truth table) |
| [`docs/doctor.v1.md`](./docs/doctor.v1.md) | **Doctor** preflight checks + `doctor --json` |
| [`schemas/doctor.v1.json`](./schemas/doctor.v1.json) | Machine-readable Doctor report schema |
| [`examples/doctor.v1.json`](./examples/doctor.v1.json) | Golden doctor snapshot |
| [`docs/logs.v1.md`](./docs/logs.v1.md) | **`logs` subcommand** (journalctl dump UX) |
| [`examples/logs.v1.txt`](./examples/logs.v1.txt) | Sample plain-text logs transcript |
| [`docs/modules.v1.md`](./docs/modules.v1.md) | **Rust module seams** (`src/` layout, pure vs I/O) |
| [`docs/verification.v1.md`](./docs/verification.v1.md) | **Test + dogfood strategy** (cargo bar, human gate, next map) |
| [`docs/research/`](./docs/research/) | Adapter I/O, CI/dev-loop, crates. **Read the matching brief** before working on that implementation ticket. |

## Contract artifacts (keep in sync)

Frozen product surfaces use a small artifact set. **Same PR** must update every applicable piece — never prose-only or schema-only drift.

| Kind | When | Artifacts |
|------|------|-----------|
| **JSON contract** | Machine JSON in or out (`status --json`, `doctor --json`, `connections.json`) | **Prose** (`docs/…v1.md`) + **JSON Schema** (`schemas/…`) + **golden example** (`examples/…json`) |
| **Plain-text / argv UX** | Human stdout or argv-only (`logs`, much of CLI contract) | **Prose** + **golden sample** when useful (e.g. `examples/logs.v1.txt`) — **no** JSON Schema |
| **Rules / tables** | Pure decision tables (`reconcile`, module seams, verification) | **Prose** (normative tables); schema only if a JSON document is defined |

### Current JSON trios

| Prose | Schema | Golden |
|-------|--------|--------|
| [`docs/status-document.v1.md`](./docs/status-document.v1.md) | [`schemas/status.v1.json`](./schemas/status.v1.json) | [`examples/status.v1.json`](./examples/status.v1.json) |
| [`docs/config.v1.md`](./docs/config.v1.md) | [`schemas/config.v1.json`](./schemas/config.v1.json) | [`examples/connections.json`](./examples/connections.json) |
| [`docs/doctor.v1.md`](./docs/doctor.v1.md) | [`schemas/doctor.v1.json`](./schemas/doctor.v1.json) | [`examples/doctor.v1.json`](./examples/doctor.v1.json) |

### Rules

- Touching a JSON field, enum, or validation rule → update **prose + schema + golden** together.
- Prefer **additive** optional fields; bump document schema `version` only for breaking shape/meaning changes (see each prose doc).
- New JSON contracts must add the full trio and a row in the table above.
- CI: goldens must validate against schemas **and**, once the binary exists, CLI JSON (`status --json`, `doctor --json`) and config parse must match those schemas — [issue #23](https://github.com/golgor/cloud-sql-tracker/issues/23). Do not close #23 on golden-only checks **or** in-process serde alone.

## Status document (critical)

- **Only** `cloud-sql-tracker status --json` produces the Status document.
- Schema id is integer field `version` (currently `1`). Binary semver is `cli_version`.
- There is **no `list` command** in v1; do not invent a parallel status JSON. Plugin consumes **status only** (not `logs`, not doctor as bar state).
- Field meanings: [`docs/status-document.v1.md`](./docs/status-document.v1.md). Sync rule: **Contract artifacts** above.

If a JSON field is unclear, **open the status prose** — do not guess from the golden alone.

## Version string (single source)

- Bump **`Cargo.toml` `[package].version` only** for releases.
- Regenerate **`Cargo.lock` in the same commit** (build once, or `cargo update -p cloud-sql-tracker`). `release.yml` runs `cargo test --locked`; a stale lock fails the Release job **after** the tag is already pushed ([#79](https://github.com/golgor/cloud-sql-tracker/issues/79)).
- `--version` / `-V` and Status `cli_version` must use `CARGO_PKG_VERSION` (or equivalent) — never a second hard-coded version in `main.rs`.
- GitHub Release tags should match that package version (`v0.1.0` ↔ `0.1.0`).

## CLI argv

- Contract: [`docs/cli-contract.v1.md`](./docs/cli-contract.v1.md).
- Multi-target start/stop is **not transactional**: partial failure leaves successes running (exit `1`).
- `restart --failed` only targets Health state `error`.
- **Two clocks:** Reconcile `START_WINDOW` (10s) classifies one observation for **status** (`starting` vs `start_timeout`). CLI `--wait-ms` (default 10s) is how long **start/restart** may poll for `running`. They are independent. Do not abort `--wait-ms` only because one poll returned Reconcile `start_timeout`.

## Config file

- Contract: [`docs/config.v1.md`](./docs/config.v1.md).
- **Reject unknown JSON keys** (exit 2). Do not “ignore extras.”
- Unique `id`, `port`, and `instance`. Reserved ports: 5432, 3306, 1433 (+ all 1–1023).
- Sync rule: **Contract artifacts** (prose + schema + golden).

## Code style

Follow the **[Zen of Python](https://peps.python.org/pep-0020/)** in Rust too. **Readability counts.** Maintainability and ease of understanding beat clever, nested, or micro-efficient code.

- Explicit is better than implicit.
- Simple is better than complex. Complex is better than complicated.
- Flat is better than nested.
- Sparse is better than dense.
- If the implementation is hard to explain, it is a bad idea.

Prefer a short named function over a clever one-liner, a generic, or a macro. Name types from [`CONTEXT.md`](./CONTEXT.md). Reviewers reject “smart” code that is hard to read.

## Layers (do not confuse)

| Layer | What it is | Examples |
|-------|------------|----------|
| `commands` | Library seam: selector, status assemble, mutate, doctor, logs | issues #42–#44 |
| `cli` | clap argv, exits `0`–`4`, print; `main` → `cli::run()` (**no argv**) | issue #45 |
| Layer 1 contracts | Goldens vs `schemas/*.json` (script / CI) | issue #34 |
| Layer 2 contracts | Spawn **built binary**; validate **stdout** vs schemas | issue #47 |
| Dogfood | Human gate on the implementation map | issue #46 |

In-process serde is not Layer 2. Goldens-only is not Layer 2. Landing `commands` does not make `-h` / `status` work until `cli` exists.

## Implementation preferences

- Stateless CLI; long-lived work is `cloud-sql-proxy` under `systemd --user`.
- ADC is a hard requirement ([ADR 0002](./docs/adr/0002-adc-only-auth.md)).
- v1 health = **our Unit** + local TCP accept; no Orphan adopt ([ADR 0003](./docs/adr/0003-local-health-signals.md), [`reconcile.v1.md`](./docs/reconcile.v1.md)).
- Prefer native Linux I/O over scraping `ss`/`pgrep` ([ADR 0004](./docs/adr/0004-rust-toolchain-and-linux-io.md)). v1 Supervisor I/O is **zbus** on the user bus ([`docs/research/supervisor-io.md`](./docs/research/supervisor-io.md)) — not `systemctl` / `systemd-run`. `logs` still uses `journalctl`.
- Module seams: [`docs/modules.v1.md`](./docs/modules.v1.md) — clap stays in `cli`; Reconcile is pure; no traits until a second adapter exists.
- Test / dogfood: [`docs/verification.v1.md`](./docs/verification.v1.md) — required `cargo test` list + human dogfood; implementation map inherits this; do not close [#23](https://github.com/golgor/cloud-sql-tracker/issues/23) on golden-only **or** in-process serde alone.
- State latency in **measured ms**, not adjectives. `doctor` is the plugin's panel-open preflight and `status` is bar-polled, so overhead is user-visible. Benchmark before and after, and put the numbers in the PR.

## Wayfinder

- Spec map [#2](https://github.com/golgor/cloud-sql-tracker/issues/2) is **closed**. Implementation map is [#28](https://github.com/golgor/cloud-sql-tracker/issues/28). Do not reopen #2 as parent.
- Planning decisions live on GitHub issues (map label `wayfinder:map`). Do not re-litigate closed tickets without an explicit reopen.
- **One map, one job.** Spec freeze ≠ cargo-test-green. Proof lives on the implementation map ([`docs/verification.v1.md`](./docs/verification.v1.md)).

## Implement / review loop

- **Implement** with a Sonnet-5 subagent on a worktree. Parent does not write product code.
- When the implementer is done: dual GitHub review — **chatgpt-sol** (spec vs frozen contracts) and **Opus-5** (module seams / depth / **Code style**). Give both the branch as a **worktree** (next bullet), not only a pasted diff.
- Reviewers and researchers usually have **no shell**. Give them a **git worktree** of the branch plus evidence files inside it: `git worktree add ../cloud-sql-tracker-<n> <branch>`, then write `REVIEW_DIFF.patch` and the issue body there. A pasted diff alone blocks them as soon as they need to read a neighbouring file. Do not commit those scratch files.
- Measurements are the **parent's or implementer's** job. A research subagent cannot run `cargo`; hand it the numbers to write up.
- Must-fix → Sonnet-5 again on the same branch. Repeat until **both** reviewers and the coder are satisfied.
- Reviewers are read-only. A human performs the squash-merge.
- When a freeze requires tests, name the **pure fn** (do not invite a test trait).
- Judge review comments against the **current branch tip**. Bot or stale comments may describe code that a later commit already fixed.
- **Verify a must-fix's premise before applying it.** A review claimed that fsync plus atomic rename removed a test `ETXTBSY` flake. Measurement said otherwise: `main` was 10/10 clean and the change was 5/10 failing, so the real cause was fork/exec contention across test threads ([#82](https://github.com/golgor/cloud-sql-tracker/issues/82)). Keep the reviewer's **goal**; fix the **reason**. Escalate — do not silently comply, and do not silently ignore.
- **One mitigation per problem.** When two guards address one cause, keep the one that holds and delete the other. This applies to test code too.
- **Test-only workarounds stay in `#[cfg(test)]`.** Production must not carry error handling shaped by test setup.

### Bugfix (product)

1. Build a **tight repro** that fails on the operator symptom (script or test). Show red before fixing.
2. Open a **GitHub issue** (symptom, repro, expected, non-goals). Link the map when the bug is on-map.
3. Branch `fix/…` or `wayfinder/<n>-…`; PR with `Fixes #N`.
4. Prefer a **pure seam** regression test when the bug is policy; keep a live repro for I/O timing when needed.
5. Use the same dual-review loop when the change is non-trivial.
