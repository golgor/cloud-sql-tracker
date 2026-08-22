# Research: release build profile and binary size

**Issue:** [#69](https://github.com/golgor/cloud-sql-tracker/issues/69) (Part A — research)  
**Map:** [#28](https://github.com/golgor/cloud-sql-tracker/issues/28)  
**Companion:** packaging and AUR path live in [`release.md`](./release.md). This brief covers **build profile, measured size, asset shape, and Part B release-job sketch** only.

## Summary

Keep the current `[profile.release]` settings (`lto = true`, `codegen-units = 1`, `strip = true`). On this research host the stripped release binary is about **2.6 MiB** (`2_711_184` bytes). That size is fine for a local desktop CLI. Do not add `opt-level = "z"`, `panic = "abort"`, or UPX for v1.

For the GitHub Release asset, build **native** `x86_64-unknown-linux-gnu` on `ubuntu-latest`, pack `cloud-sql-tracker-v{VERSION}-x86_64-unknown-linux-gnu.tar.gz` with the binary, `LICENSE`, and `README.md`, and attach a SHA-256 checksum file. Use `cargo build --release --locked` with the same Rust pin as CI. Do **not** claim bit-identical reproducible builds.

**Snapshot, not a second pin.** Sizes below are research-time measurements on an Arch `x86_64` host with `rustc 1.97.1`. A GitHub `ubuntu-latest` artifact can differ slightly (glibc, linker). When Part B lands, record the CI-built asset size once if the number matters for notes. Profile knobs stay as written in `Cargo.toml` unless a later ticket reopens them.

## Measurement host

| Item | Value |
| --- | --- |
| OS | Linux (Arch), kernel `7.1.8-arch1-3` |
| Arch | `x86_64` |
| rustc | `1.97.1 (8bab26f4f 2026-07-14)` |
| cargo | `1.97.1` |
| Target | host `x86_64-unknown-linux-gnu` (native) |
| Command base | `cargo build --release --locked` |
| Binary path | `target/release/cloud-sql-tracker` |
| `--version` | `0.1.0` (from `Cargo.toml` only) |
| Baseline file type | ELF 64-bit LSB pie executable, dynamically linked, stripped |

Baseline already includes the repo profile:

```toml
[profile.release]
lto = true
codegen-units = 1
strip = true
```

Default release `opt-level` stays `3`. Default `panic` stays `"unwind"`. [Cargo Book — Profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)

## Findings

### 1. Baseline size (current profile)

1. **Baseline binary size is `2_711_184` bytes (~2.59 MiB).** Build used the current profile with LTO, one codegen unit, and strip. The binary is dynamically linked and stripped. This is the size class to expect for a convenience GitHub asset on the same toolchain family.
2. **Sample tarball size is `1_227_641` bytes (~1.17 MiB).** Name: `cloud-sql-tracker-v0.1.0-x86_64-unknown-linux-gnu.tar.gz`. Contents: release binary + `LICENSE` + `README.md`. Research-time SHA-256: `fdc18d23617ac6c012cff3b7923576bfd0eccacaf1bff39482f51ab16bd9fe73`. Recreate checksums on the real tagged CI artifact in Part B; do not reuse this host hash as a release pin.

### 2. What the current profile knobs do

One sentence each. Primary source: [Cargo Book — Profiles](https://doc.rust-lang.org/cargo/reference/profiles.html).

1. **`lto = true` (fat LTO)** — Runs full link-time optimization across the crate graph so LLVM can inline and drop more dead code at link time; build time grows, and size or speed can improve. `true` is the same as `"fat"`.
2. **`codegen-units = 1`** — Forces a single code generation unit so the compiler does not split the crate for parallel codegen; this can produce better optimized code and often a smaller binary, at the cost of slower compiles.
3. **`strip = true`** — Strips symbols from the binary (`true` equals `strip = "symbols"`); this removes symbol-table bulk from the release artifact and is already on for this repo.

### 3. Measured size matrix

All rows start from the repo baseline unless the “profile delta” column says otherwise. Builds used temporary profile overrides and left `Cargo.toml` unchanged.

| Label | Profile delta | Bytes | ~MiB | Delta vs baseline |
| --- | --- | ---: | ---: | ---: |
| `baseline_current` | `lto=true`, `codegen-units=1`, `strip=true` | 2_711_184 | 2.59 | — |
| `opt_z` | + `opt-level = "z"` | 1_885_584 | 1.80 | −30.5% |
| `panic_abort` | + `panic = "abort"` | 2_302_736 | 2.20 | −15.1% |
| `opt_z_panic_abort` | `opt-level = "z"` + `panic = "abort"` | 1_606_800 | 1.53 | −40.7% |
| `thin_lto` | `lto = "thin"` (else same as baseline) | 2_968_632 | 2.83 | +9.5% |
| `strip_symbols` | `strip = "symbols"` | 2_711_184 | 2.59 | 0% (same as `true`) |
| `strip_none` | `strip = false` (debuginfo present) | 5_568_944 | 5.31 | +105% |
| `no_lto_defaultish` | `lto = false`, `codegen-units = 16`, `strip = true` | 3_851_904 | 3.67 | +42.1% |

**Read of the table**

- Fat LTO + one codegen unit already buys a clear win over a more default-shaped release (`no_lto_defaultish`).
- `strip = true` and `strip = "symbols"` match; leaving strip off roughly doubles the binary.
- Thin LTO was **larger** than fat LTO on this crate and host.
- `opt-level = "z"` and `panic = "abort"` shrink the binary further, alone and together.

### 4. Further size options (choices)

#### `opt-level = "z"` vs default `3`

**Pick:** keep default release `opt-level = 3` (do not set `"z"`).  
**Why:** the CLI is a local control plane; cold start and steady work should stay speed-oriented, and ~2.6 MiB is already small enough for a desktop download.  
**Discarded:** `opt-level = "z"` (and `"s"`) — measured size win (~30% here) but optimizes for size over speed; not needed for v1.  
**Unchanged:** existing LTO / codegen-units / strip settings.

#### `panic = "abort"`

**Pick:** keep default `panic = "unwind"`.  
**Why:** unwind keeps normal Rust panic backtraces and failure behavior; a control-plane CLI benefits from clearer diagnostics when something goes wrong.  
**Discarded:** `panic = "abort"` — measured ~15% size win, but aborts the process and weakens everyday panic diagnostics; tests also ignore this setting and still need unwind.  
**Unchanged:** release strip and LTO.

#### `strip = "symbols"` vs `true`

**Pick:** keep `strip = true`.  
**Why:** Cargo documents `true` as equivalent to `"symbols"`; the measured sizes match (`2_711_184` bytes).  
**Discarded:** `strip = false` for the published asset — binary roughly doubles with debuginfo left in.  
**Unchanged:** no separate post-link `strip(1)` step is required when Cargo strip is on.

#### Fat vs thin LTO

**Pick:** keep `lto = true` (fat).  
**Why:** on this crate, fat LTO produced a **smaller** binary than `lto = "thin"` (`2_711_184` vs `2_968_632`) and is already the repo setting.  
**Discarded:** `lto = "thin"` for size (larger here); `lto = false` with defaultish codegen units (much larger).  
**Unchanged:** longer link time is acceptable for rare release builds.

#### Unused dependency features

**Pick:** no new feature-trimming pass for the release binary in Part B.  
**Why:** direct deps already lean on tight features (`procfs` / `rustix` with `default-features = false`, small feature lists on `clap` / `serde`). Further cuts need a dedicated bloat pass, not a release-profile change.  
**Discarded:** drive-by feature audits in the release workflow.  
**Unchanged:** dependency versions and lockfile policy; optional later `cargo bloat` investigation if size becomes a real problem.

#### UPX packing

**Pick:** do not pack the release binary with UPX (or similar).  
**Why:** this is a security-sensitive local binary that talks to systemd and Cloud SQL proxy paths; packing hurts transparency, raises AV false-positive risk, and is a known malware packing pattern.  
**Discarded:** UPX size compression for GitHub assets.  
**Unchanged:** gzip/xz **archive** compression of the tarball is fine; that is packaging, not executable packing.

Primary context: [MITRE ATT&CK — Software Packing](https://attack.mitre.org/techniques/T1027.002/).

#### Debug info / `split-debuginfo` for crash dumps

**Pick:** ship the **stripped** release binary only in the v1 GitHub asset; do not attach a debuginfo package yet.  
**Why:** v1 dogfood does not need crash-dump symbol servers; strip keeps the asset small and simple.  
**Discarded:** `strip = false` on the public binary; separate `split-debuginfo` release assets for v1.  
**Unchanged:** developers can still build unstripped local binaries; a later ticket may add optional debuginfo artifacts if field crashes demand them.

[Cargo Book — `split-debuginfo`](https://doc.rust-lang.org/cargo/reference/profiles.html#split-debuginfo) [Cargo Book — `strip`](https://doc.rust-lang.org/cargo/reference/profiles.html#strip)

### 5. Build reproducibility-ish

**Pick:** release builds use `cargo build --release --locked` on a pinned toolchain and a fixed runner image family (`ubuntu-latest` at Part B time).  
**Why:** `--locked` forces the checked-in `Cargo.lock` resolution and fails if the lockfile would change; the Rust pin matches `mise.toml`, `Cargo.toml` `rust-version`, and CI (`1.97.1`).  
**Discarded:** claims of full bit-identical reproducible builds across hosts.  
**Unchanged:** version identity stays `Cargo.toml` `[package].version` only; tag is `v` + that version.

**What we claim**

- Same lockfile dependency set when `--locked` succeeds.
- Same intentional profile flags from `Cargo.toml`.
- Same documented toolchain version on the release job.

**What we do not claim**

- Bit-identical binaries across Arch host vs GitHub runner, or across different `ubuntu-latest` image generations.
- Deterministic timestamps, paths, or linker outputs without a dedicated reproducible-build pipeline.
- That `--locked` alone pins `rustc`, glibc, or the system linker.

[Cargo Book — `cargo build`](https://doc.rust-lang.org/cargo/commands/cargo-build.html) (see `--locked` / `--frozen`)  
[Reproducible Builds — Rust](https://reproducible-builds.org/docs/rust/)

### 6. Cross vs native GitHub Actions

**Pick:** build the v1 Linux asset **natively** on `ubuntu-latest` for `x86_64-unknown-linux-gnu`.  
**Why:** matches CI, avoids cross-linker setup, and matches the operator’s primary Arch/Omarchy x86_64 machine class.  
**Discarded:** cross-compile matrices, `cross`, and non-Linux targets in this slice.  
**Unchanged:** ARM/macOS remain out of scope per issue [#67](https://github.com/golgor/cloud-sql-tracker/issues/67) and [`release.md`](./release.md).

**glibc note (from companion brief):** an `ubuntu-latest` `gnu` binary can fail on much older glibc hosts. Test the tagged asset on the operator’s Omarchy machine before calling it supported. Musl/static is not required for v1.

### 7. Asset naming and tarball contents

**Pick:** one versioned tarball plus one checksum file.

| Piece | Recommendation |
| --- | --- |
| Archive name | `cloud-sql-tracker-v{VERSION}-x86_64-unknown-linux-gnu.tar.gz` |
| Example | `cloud-sql-tracker-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| Tarball contents | `cloud-sql-tracker` binary, `LICENSE`, `README.md` |
| Checksum | `SHA256SUMS` (or `cloud-sql-tracker-v{VERSION}-x86_64-unknown-linux-gnu.tar.gz.sha256`) with SHA-256 of the archive |
| Version source | `{VERSION}` = `Cargo.toml` `[package].version`; tag = `v{VERSION}` |

**Why:** the name states product, version, and Rust target triple; checksums give integrity checks without signing.  
**Discarded:** bare `cloud-sql-tracker` asset names; platform-less names; UPX-packed payloads; multi-arch archives for v1.  
**Unchanged:** Git install path in [`release.md`](./release.md); no AUR/`cargo-dist` in this slice.

[GitHub Docs — About releases](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases)

### 8. Final profile recommendation

**Pick:** keep the current profile exactly:

```toml
[profile.release]
lto = true
codegen-units = 1
strip = true
```

**Why:** measured baseline (~2.6 MiB binary, ~1.2 MiB tarball) is already in a good desktop range; fat LTO + single codegen unit + strip beat a defaultish release by a wide margin; further knobs trade diagnostics or speed for size the project does not need.  
**Discarded for v1:** `opt-level = "z"`, `panic = "abort"`, thin LTO, strip off, UPX, debuginfo assets.  
**Unchanged:** no product code changes in Part A; Part B should not edit the profile unless new evidence appears on the CI runner.

**Pin for implementation ticket (Part B):**

- **Brief:** `docs/research/release-build.md`
- **Gist:** keep current release profile; native ubuntu-latest tarball + SHA256SUMS on tag `v*`
- **Pin:** profile flags as above; toolchain **1.97.1** (same as `mise.toml` / `Cargo.toml` / `ci.yml`); asset name pattern `cloud-sql-tracker-v{VERSION}-x86_64-unknown-linux-gnu.tar.gz`

## Recommended Part B implementation sketch

Do **not** land `release.yml` in the Part A research PR. When Part B implements:

1. **Triggers:** `push` tags `v*`, plus `workflow_dispatch` for a controlled retry on an existing tag.
2. **Runner:** one job on `ubuntu-latest`, native build only.
3. **Toolchain:** same `RUST_VERSION` / `dtolnay/rust-toolchain` pin as [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) (`1.97.1`), with build tools only (release job does not need `rustfmt`/`clippy` if CI already gated the tag’s commit).
4. **Tests on the release job:** **re-run** `cargo test --locked` (or default `cargo test` with `--locked` if you standardize on it) before packing.  
   **Pick:** re-test on the release job. **Why:** the tag build must not trust an earlier CI green on a different commit or a force-pushed tag story. **Discarded:** “CI was green once” with no release-job test. **Unchanged:** ignored real-systemd tests stay excluded.
5. **Build:** `cargo build --release --locked`.
6. **Pack:** create the versioned `.tar.gz` with binary + `LICENSE` + `README.md`; write `SHA256SUMS`.
7. **Publish:** create the GitHub Release for the tag and upload the archive + checksum.  
   **Pick:** `softprops/action-gh-release` (current major, pin a full commit SHA like other actions) **or** `gh release create` with `GH_TOKEN` — both are acceptable; prefer **one** explicit path.  
   **Why (action):** small, common, creates release + uploads files in one step.  
   **Why (`gh`):** already on GitHub-hosted runners; fewer third-party actions.  
   **Recommendation:** use **`gh release create "${GITHUB_REF_NAME}" … --generate-notes`** if the workflow stays tiny and audit-simple; use **softprops/action-gh-release@v3** (pin SHA) if you want action-native file globs and draft controls. Either is fine; do not use both.  
   **Discarded:** deprecated `actions/upload-release-asset`; publishing from `pull_request` or untagged `main` pushes.  
   **Unchanged:** `contents: write` only on the release workflow; CI stays `contents: read`.
8. **Permissions / safety:** do not publish on PR; require the tag to match `Cargo.toml` version (simple check script or step).
9. **Non-goals for Part B:** AUR, `cargo-dist`, multi-target matrix, signing/provenance, musl, macOS/Windows.

[softprops/action-gh-release](https://github.com/softprops/action-gh-release)  
[GitHub CLI — `gh release create`](https://cli.github.com/manual/gh_release_create)  
[GitHub Docs — Workflow permissions](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)

## Sources

- Kept: [Cargo Book — Profiles](https://doc.rust-lang.org/cargo/reference/profiles.html) — authoritative `lto`, `codegen-units`, `strip`, `opt-level`, `panic`, defaults.
- Kept: [Cargo Book — `cargo build`](https://doc.rust-lang.org/cargo/commands/cargo-build.html) — `--locked` / build flags.
- Kept: [Reproducible Builds — Rust](https://reproducible-builds.org/docs/rust/) — what full reproducibility still needs beyond Cargo lockfiles.
- Kept: [GitHub Docs — About releases](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases) — tag/release/asset model.
- Kept: [GitHub CLI — `gh release create`](https://cli.github.com/manual/gh_release_create) — first-party upload path on Actions runners.
- Kept: [softprops/action-gh-release](https://github.com/softprops/action-gh-release) — common third-party release+upload action (v3 line).
- Kept: [MITRE ATT&CK — Software Packing](https://attack.mitre.org/techniques/T1027.002/) — why UPX-style packing is a bad default for a trusted local tool.
- Kept: local measurement table on Arch host, `rustc 1.97.1` — concrete sizes for this crate.
- Kept: [`release.md`](./release.md) — packaging/AUR companion; asset naming stays aligned.
- Dropped: min-sized-Rust blog checklists as authority — useful ideas, but Cargo book + local numbers decide.
- Dropped: UPX how-to guides — discarded approach.

## Gaps and residual risks

- **Medium:** Host sizes are Arch + local linker; **CI `ubuntu-latest` bytes may differ**. Part B should print `wc -c` / `sha256sum` for the CI artifact in the job log and treat those as the published numbers.
- **Medium:** glibc floor of `ubuntu-latest` vs older Linux was not re-validated here (same residual as [`release.md`](./release.md)). Dogfood the asset on Omarchy before calling it supported.
- **Low:** No runtime benchmark compared `opt-level = 3` vs `"z"` on this CLI. Size data alone drove the keep-`3` pick; revisit only if distribution size becomes painful.
- **Low:** Checksums are integrity, not publisher authentication. Signing/provenance stays deferred until distribution widens ([`release.md`](./release.md)).
- **Low:** Tag/version mismatch automation is sketched, not specified as a script contract; Part B should add a simple equality check.

## Part B accept criteria (for the implement ticket)

- [ ] Workflow runs on `v*` tags and optional `workflow_dispatch` only.
- [ ] Toolchain pin matches CI (`1.97.1` until deliberately bumped).
- [ ] `cargo test --locked` (or locked-equivalent) runs on the release job before upload.
- [ ] `cargo build --release --locked` with **unchanged** `[profile.release]` unless a new decision amends this brief.
- [ ] Asset name `cloud-sql-tracker-v{VERSION}-x86_64-unknown-linux-gnu.tar.gz` + checksum file.
- [ ] Tarball includes binary, `LICENSE`, `README.md`.
- [ ] No UPX, no multi-arch matrix, no AUR, no profile drive-by edits.
- [ ] On the implementation ticket `## Question` block: Brief / Gist / Pin as above.
