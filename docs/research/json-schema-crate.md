# Research: Rust JSON Schema validation for CI Layer 2

## Summary

Use **`jsonschema` 0.50 as a test-only Rust dependency**, with runtime schema compilation and `default-features = false`. It supports the repository’s declared JSON Schema Draft 2020-12, automatically detects that dialect from each committed `$schema`, produces iterable errors with instance and schema locations, is actively maintained, and its Rust 1.85 MSRV exactly matches this repository’s current `Cargo.toml`/ADR 0004 baseline.

Do not use the compile-time macro or a separate CLI for Layer 2. Runtime loading tests the exact checked-in `schemas/*.v1.json` files against real built-binary stdout and config inputs, while avoiding macro and external-tool machinery.

**Snapshot, not a second pin.** `jsonschema` 0.50 / 0.50.0 and MSRV 1.85 are what we looked up when researching. When Layer 2 lands, pin the then-current compatible crate in `Cargo.lock` and edit this file if the number moved. Revisit on crate major/API breaks or if `Cargo.toml` and this brief disagree.

## Recommendation

Add only when issue #23 Layer 2 tests are implemented:

```toml
[dev-dependencies]
jsonschema = { version = "0.50", default-features = false }
```

The three schemas declare `https://json-schema.org/draft/2020-12/schema` and use Draft 2020-12 features such as `$defs`. `jsonschema::validator_for` detects the dialect from `$schema`; its Draft 2020-12 module and conformance badge explicitly cover this dialect. Internal fragment references such as `#/$defs/connection` do not require the crate’s HTTP/file resolver features, so disabling defaults avoids `reqwest`, `rustls`, and an unnecessary TLS provider. [Repository README](https://github.com/Stranger6667/jsonschema#supported-drafts) [crate features](https://github.com/Stranger6667/jsonschema/blob/master/crates/jsonschema/Cargo.toml)

### Usage sketch (~10 lines)

```rust
fn assert_schema(path: &str, instance: &serde_json::Value) {
    let text = std::fs::read_to_string(path).expect("read schema");
    let schema = serde_json::from_str(&text).expect("parse schema");
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    let errors = validator.iter_errors(instance)
        .map(|e| format!("{}: {e}", e.instance_path()))
        .collect::<Vec<_>>();
    assert!(errors.is_empty(),
        "{path} rejected instance:\n{}", errors.join("\n"));
}
```

Call this helper with parsed stdout from the **built** `status --json` and `doctor --json` commands, not only with in-process serialized fixtures. For config, test the golden plus representative invalid documents through both schema validation and `config::parse`; parser-only or serde-only proof does not close issue #23.

## Findings

1. **Draft match — strong.** `schemas/status.v1.json`, `schemas/doctor.v1.json`, and `schemas/config.v1.json` all declare Draft 2020-12. The selected crate explicitly supports Draft 2020-12, has a dedicated `draft202012` API, and `validator_for` automatically detects the draft from `$schema`. This covers the committed keywords (`$defs`, `$ref`, `const`, `not`, unions in `type`, patterns, and object/array constraints). [jsonschema supported drafts](https://github.com/Stranger6667/jsonschema#supported-drafts) [draft202012 docs](https://docs.rs/jsonschema/0.50.0/jsonschema/draft202012/) [validator_for docs](https://docs.rs/jsonschema/0.50.0/jsonschema/fn.validator_for.html)

2. **Runtime compilation is the simpler and more direct contract proof.** The crate compiles a parsed schema into a reusable `Validator`; invalid schemas or unresolved references make construction fail. Its optional `#[jsonschema::validator]` macro generates a validator at Rust compile time and is faster, but these tiny CI fixtures do not need that optimization. Runtime loading makes the file under `schemas/` visibly the authority, avoids one generated type per schema and the `macros` feature, and permits one small private test helper. [Validator docs](https://docs.rs/jsonschema/0.50.0/jsonschema/struct.Validator.html) [macro docs](https://docs.rs/jsonschema/0.50.0/jsonschema/attr.validator.html)

3. **Errors are appropriate for `cargo test`.** `validate` returns the first error; `iter_errors` returns all failures. `ValidationError` implements `Display` and exposes the failing instance JSON Pointer, canonical keyword location, evaluation path, and absolute keyword URI. Collecting all errors into the assertion message gives useful failures such as the instance path plus the violated requirement, rather than a bare boolean. [Validator error iteration](https://docs.rs/jsonschema/0.50.0/jsonschema/struct.Validator.html) [ValidationError docs](https://docs.rs/jsonschema/0.50.0/jsonschema/error/struct.ValidationError.html)

4. **MSRV matches exactly; maintenance is strong.** The current crate release is 0.50.0, declares Rust 1.85, and was published 2026-08-20. The project’s `Cargo.toml` declares `rust-version = "1.85"`; ADR 0004 requires current stable with an explicit MSRV. crates.io reports over 82 million total downloads, and the upstream repository was receiving maintainer commits on 2026-08-21, is unarchived, and has 805 stars. These are stronger maintenance signals than the alternatives, though release upgrades should still be reviewed because the crate has had frequent API changes. [crates.io](https://crates.io/crates/jsonschema) [upstream repository metadata](https://api.github.com/repos/Stranger6667/jsonschema) [upstream commits](https://api.github.com/repos/Stranger6667/jsonschema/commits?per_page=5) [local ADR 0004](../adr/0004-rust-toolchain-and-linux-io.md)

5. **A separate CLI is unnecessary for Layer 2.** `jsonschema-cli` exists, and issue #23 mentions `check-jsonschema`/`ajv-cli` as possible Layer 1 golden checks. Layer 2 already needs Rust tests to spawn the built binary and exercise `config::parse`; embedding the validator keeps `cargo test` as the documented local and CI command, pins the validator through `Cargo.lock`, and avoids an independently installed tool. [jsonschema CLI note](https://github.com/Stranger6667/jsonschema#jsonschema) [issue #23](https://github.com/golgor/cloud-sql-tracker/issues/23)

6. **Alternatives are viable but inferior here.** `boon` 0.6.1 supports Draft 2020-12 and hierarchical errors, but its latest crates.io release is from 2025-01-07, it does not declare an MSRV, and it has far lower adoption (about 448 thousand total downloads). It remains maintained, but offers no project-specific advantage over `jsonschema`. `valico` 4.0.0 targets Draft 7 rather than the committed Draft 2020-12 and was last released in 2023, so it is not a match. [boon docs](https://docs.rs/boon/latest/boon/) [boon crates.io API](https://crates.io/api/v1/crates/boon) [boon repository](https://github.com/santhosh-tekuri/boon) [valico crates.io](https://crates.io/crates/valico)

7. **`format` needs deliberate interpretation.** `status.v1.json` contains `"format": "date-time"`. Under Draft 2020-12, format is annotation-oriented unless format assertions are enabled; `jsonschema` exposes `should_validate_formats(true)` for an opt-in assertion policy. The Layer 2 helper should initially follow the committed schema’s normal dialect semantics rather than silently add policy. If CI must reject malformed timestamps solely through JSON Schema, freeze that intent explicitly and configure the validator consistently in every schema-validation path. [jsonschema format configuration](https://docs.rs/jsonschema/0.50.0/jsonschema/struct.ValidationOptions.html) [Draft 2020-12 validation specification](https://json-schema.org/draft/2020-12/json-schema-validation#name-vocabularies-for-semantic-c)

8. **Schema validation is necessary but does not replace domain validation.** `schemas/config.v1.json` can reject unknown keys, missing required properties, reserved ports, bad patterns, and bad primitive types. It cannot express the repository’s cross-item uniqueness rules for `id`, `port`, and `instance`, nor all defaults-merge behavior. Layer 2 therefore must compare representative schema accept/reject cases with `config::parse` and retain parser tests for semantic rules. `docs/verification.v1.md` likewise says in-process Status/Doctor serialization is necessary but does not replace validation of real CLI stdout. [issue #23 acceptance](https://github.com/golgor/cloud-sql-tracker/issues/23) [`docs/verification.v1.md`](../verification.v1.md)

## Sources

- Kept: [jsonschema docs](https://docs.rs/jsonschema/0.50.0/jsonschema/) — primary API, draft, validator, macro, and error behavior.
- Kept: [jsonschema crates.io](https://crates.io/crates/jsonschema) — release, MSRV, downloads, and feature metadata.
- Kept: [Stranger6667/jsonschema](https://github.com/Stranger6667/jsonschema) — maintenance, supported drafts, CLI, and compile-time/runtime positioning.
- Kept: [boon docs](https://docs.rs/boon/latest/boon/) and [crates.io API](https://crates.io/api/v1/crates/boon) — credible Draft 2020-12 alternative and comparison data.
- Kept: [issue #23](https://github.com/golgor/cloud-sql-tracker/issues/23) and [`docs/verification.v1.md`](../verification.v1.md) — exact Layer 2 proof boundary.
- Dropped: `jsonschema_valid` — no advantage over the selected crate and reported local-reference limitations make it a poor fit for these `$defs` schemas.
- Dropped: SEO/tutorial comparisons — no authority over draft compliance, MSRV, releases, or API behavior.

## Gaps and residual risks

- This research did not execute a proof-of-concept because the ticket explicitly forbids implementing tests. The implementation PR should first compile the sketch against the then-selected 0.50.x lockfile and validate all three goldens.
- `jsonschema` 0.50.0 is very recent and upstream has frequent releases/migrations. Keep it a private dev-dependency, pin through `Cargo.lock`, and review upgrades rather than exposing its types in project APIs.
- The correct `format` assertion policy remains a contract interpretation risk; default Draft 2020-12 behavior may accept a malformed `date-time` string. Resolve explicitly when implementing Layer 2 rather than enabling it accidentally in only one test path.
