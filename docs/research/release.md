# Research: v1 release artifacts and AUR

## Summary

For v1 dogfood, use `cargo install --path . --locked`; after the first release tag, document a pinned Git install (`cargo install --git … --tag v0.1.0 --locked`) and optionally attach one versioned Arch-compatible `x86_64-unknown-linux-gnu` binary archive plus checksum to the GitHub Release. Defer AUR until there is real demand for pacman/AUR-helper lifecycle management: it adds a second maintained repository and release chore but does not improve the CLI's runtime behavior.

Do not add `cargo-dist`, `cargo-deb`, or an in-tree `PKGBUILD` for v1. Keep the already-frozen release invariant in [`AGENTS.md`](../../AGENTS.md): `Cargo.toml` is the sole version source, tag `v0.1.0` corresponds to package version `0.1.0`, and runtime version output comes only from `CARGO_PKG_VERSION`.

## Findings

1. **Cargo install is sufficient for dogfood and a pinned source release.** Cargo officially supports local `--path` and remote `--git` sources, with Git selectors `--tag`, `--rev`, and `--branch`; installed executables go to the installation root's `bin` directory (normally `$HOME/.cargo/bin`). Cargo normally ignores a packaged lockfile for Git installs, so `--locked` is important here: it forces the checked-in `Cargo.lock` dependency set and fails if Cargo would need to change it. Recommended commands:

   ```sh
   # During implementation/dogfood
   cargo install --path . --locked

   # Reproducible-enough install of a published v1 tag
   cargo install --git https://github.com/golgor/cloud-sql-tracker \
     --tag v0.1.0 --locked
   ```

   This path requires a Rust/build toolchain and compiles locally; upgrading is an explicit reinstall rather than a pacman transaction. [Cargo Book: `cargo install`](https://doc.rust-lang.org/cargo/commands/cargo-install.html)

2. **A GitHub Release binary is a convenience layer, not a v1 prerequisite.** GitHub defines Releases as deployable iterations based on Git tags and supports attached binary files. For this personal Arch/Omarchy operator, the smallest useful asset set is a versioned archive such as `cloud-sql-tracker-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` containing the binary and license/readme, plus a SHA-256 checksum. Build and test it from the tagged commit before upload. A Release binary removes the local Rust-toolchain/build wait, but manual placement or copying still lacks pacman ownership and automatic upgrades. [GitHub Docs: About releases](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases) [GitHub Docs: Managing releases](https://docs.github.com/en/repositories/releasing-projects-on-github/managing-releases-in-a-repository)

3. **AUR buys native package lifecycle, not better dogfood coverage.** The AUR contains community-maintained `PKGBUILD` recipes, not official binary packages; users build them with `makepkg` and install the resulting package through pacman. That gives file ownership, clean uninstall/upgrade transactions, and AUR-helper update discovery. It does not change status, systemd, proxy, or TCP behavior, so skipping it does not weaken functional dogfood. The concrete cost to this operator is only manual installation/update management (and, for Cargo installation, retaining a build toolchain). A copied Release binary also needs a deliberate uninstall/update path. [ArchWiki: Arch User Repository](https://wiki.archlinux.org/title/Arch_User_Repository)

4. **AUR has ongoing maintenance cost disproportionate to one operator.** AUR content is unofficial and not thoroughly vetted. Publishing requires a separate AUR Git repository; on release, the maintainer updates `pkgver`/`pkgrel`, regenerates `.SRCINFO`, commits and pushes, checks user feedback, and keeps the recipe working. Arch's Rust guidance additionally calls for locked dependency fetch, frozen release build, and frozen tests. A stable source-built package would have no suffix; a package consuming a prebuilt GitHub asset must use `-bin`. These obligations are useful once others rely on the package, but are unnecessary to prove the v1 CLI on its author's own machine. [ArchWiki: AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines) [ArchWiki: PKGBUILD](https://wiki.archlinux.org/title/PKGBUILD) [ArchWiki: Rust package guidelines](https://wiki.archlinux.org/title/Rust_package_guidelines)

5. **No packaging generator is justified yet.** `dist` (formerly `cargo-dist`) is valuable when repeated multi-platform releases need generated CI, archives, installers, checksums, and GitHub Release upload automation. One Linux target and a low release cadence do not justify its generated workflow/configuration yet; a small explicit release workflow or manual first upload is easier to audit. `cargo-deb` creates Debian `.deb` packages and is irrelevant to an Arch/Omarchy-only v1. An in-tree `PKGBUILD` is not required by AUR, which has its own Git repository, and adding one now creates two surfaces that must stay synchronized. [`dist` project documentation](https://github.com/axodotdev/cargo-dist) [`cargo-deb` README](https://docs.rs/crate/cargo-deb/latest/source/README.md)

6. **The frozen version rule already supplies the release identity.** For every release, first set only `[package].version` in `Cargo.toml`, verify tests and `--version`, then tag the exact commit with the `v`-prefixed equivalent (`0.1.0` → `v0.1.0`). Release assets and later AUR `pkgver` should derive from that pair; do not add a second version constant. This is the repository rule in [`AGENTS.md`](../../AGENTS.md), and GitHub Releases' tag basis fits it directly.

## Recommended v1 path

1. Close implementation/dogfood without packaging work; install locally with `cargo install --path . --locked`.
2. At `0.1.0`, tag the tested commit `v0.1.0` and create a GitHub Release.
3. Initially document the pinned Git install. Add the single Linux x86_64 archive/checksum if avoiding the local Rust toolchain is useful; the Release may otherwise begin as notes plus the tag.
4. Do not add AUR files, `cargo-dist`, or `cargo-deb` in v1.
5. Revisit AUR when at least one of these becomes true: another Arch/Omarchy user requests it; pacman/AUR-helper upgrades materially reduce repeated operator friction; releases become regular enough to justify maintenance; or the operator wants package-manager-owned `/usr/bin` installation.
6. Revisit `dist` when supporting multiple OS/CPU targets, signing/provenance, installers, or enough releases that manual asset creation becomes error-prone. Consider `cargo-deb` only after Debian/Ubuntu becomes an explicit supported target.

## Sources

- Kept: [Cargo Book — `cargo install`](https://doc.rust-lang.org/cargo/commands/cargo-install.html) — authoritative install-source, lockfile, and destination behavior.
- Kept: [GitHub Docs — About releases](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases) — authoritative tag/release/asset model.
- Kept: [GitHub Docs — Managing releases](https://docs.github.com/en/repositories/releasing-projects-on-github/managing-releases-in-a-repository) — authoritative release publication and asset workflow.
- Kept: [ArchWiki — Arch User Repository](https://wiki.archlinux.org/title/Arch_User_Repository) — AUR trust, build, installation, and update model.
- Kept: [ArchWiki — AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines) — package naming and ongoing maintainer obligations.
- Kept: [ArchWiki — PKGBUILD](https://wiki.archlinux.org/title/PKGBUILD) — recipe metadata, sources, and integrity fields.
- Kept: [ArchWiki — Rust package guidelines](https://wiki.archlinux.org/title/Rust_package_guidelines) — locked/frozen Rust package build pattern.
- Kept: [`dist` documentation](https://github.com/axodotdev/cargo-dist) — primary description of generated release automation.
- Kept: [`cargo-deb` README](https://docs.rs/crate/cargo-deb/latest/source/README.md) — primary statement that it creates Debian packages.
- Dropped: search-result summaries and third-party packaging tutorials — redundant and less authoritative than Cargo, GitHub, Arch, and tool-owned documentation.

## Gaps and residual risks

- **Medium:** The final binary target and compatibility floor were not validated against the implemented dependency graph. A `x86_64-unknown-linux-gnu` asset built on a newer glibc environment can fail on older distributions; test the actual tagged artifact on the target Omarchy machine before publishing it as supported.
- **Low:** Demand for AUR cannot be established from documentation. Reassess after dogfood or user requests rather than predicting it.
- **Low:** Checksums provide integrity checking but not publisher authentication. Signing/provenance can wait until distribution expands, but should be reconsidered before serving a broader audience.

## Review findings

- **No blocker:** The recommended path is compatible with `AGENTS.md` and issue #33 and adds no packaging or version surface.
- **Advisory (`docs/research/release.md`):** Preserve the exact tag/version invariant and `--locked` examples when the research brief is copied into the repository.
- **Advisory (future release workflow):** Test the uploaded archive on the actual Arch/Omarchy host; GitHub Release publication alone does not prove runtime compatibility.

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Concrete release/AUR findings, recommended commands, target file references, severity-ranked residual risks, and review findings are recorded in /tmp/wayfinder-impl/out-release.md."
    }
  ],
  "changedFiles": [
    "/tmp/wayfinder-impl/out-release.md"
  ],
  "testsAddedOrUpdated": [],
  "commandsRun": [
    {
      "command": "Read AGENTS.md and GitHub issue #33; research Cargo, GitHub Releases, Arch AUR/PKGBUILD/Rust packaging, dist, and cargo-deb primary documentation",
      "result": "passed",
      "summary": "Primary-source evidence was retrieved and synthesized into the research brief."
    },
    {
      "command": "git branch/commit/push and gh issue comment",
      "result": "not-run",
      "summary": "This research subagent had no shell or gh tool; runtime instructions required the artifact only at /tmp/wayfinder-impl/out-release.md."
    }
  ],
  "validationOutput": [
    "Issue #33 body was fetched in full and all requested comparison points were answered.",
    "AGENTS.md version-string section was read and the recommendation preserves Cargo.toml as the single source with v0.1.0 ↔ 0.1.0.",
    "No packaging files were created."
  ],
  "residualRisks": [
    "medium: future GitHub Release binary compatibility must be tested on the target Omarchy host, especially the glibc baseline.",
    "low: AUR demand remains unknown until dogfood or user requests provide evidence.",
    "low: branch creation, commit, push, blob URL, and issue comment remain for the parent/operator because no git/gh tool was available."
  ],
  "noStagedFiles": true,
  "diffSummary": "Added one external research artifact recommending locked Cargo dogfood and a tagged GitHub release path, with AUR and packaging automation deferred.",
  "reviewFindings": [
    "no blockers: recommendation matches issue #33 and AGENTS.md version rules.",
    "advisory: docs/research/release.md should retain --locked installs and the exact v-prefixed tag mapping.",
    "advisory: test any release archive on the actual Arch/Omarchy host before calling it supported."
  ],
  "manualNotes": "The parent should persist the brief as docs/research/release.md on research/release, commit/push it, then comment on issue #33 with the gist and blob URL without closing or merging."
}
```
