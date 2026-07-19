# box-gantry — guide for AI coding assistants

box-gantry is a **Rust engine** that generates Box API SDKs — Go, Apex, Rust,
TypeScript, and Java — from the Box OpenAPI specification. One typed IR feeds
five backends; per-language behavior is driven by a **capability manifest**,
never by branching on the language name.

## Build & verify — run before every change

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

These are exactly what CI runs. The `cargo test` gate includes each backend's
verification step, which compiles the generated SDK with that target's real
toolchain (Go 1.23+, Node 22+ with TypeScript 7, JDK 26, and the Salesforce CLI
for Apex). A gate whose toolchain is absent skips cleanly, so you can work on one
backend without installing all five.

Generate an SDK locally:

```sh
cargo run -p gantry-cli -- generate --target rust --out /tmp/sdk \
  fixtures/specs/openapi.json fixtures/specs/openapi-v2025.0.json fixtures/specs/openapi-v2026.0.json
```

## Conventions — do not violate

- **Deterministic output.** Same specs in → same files out, sorted by path.
  Tests enforce byte-for-byte reproducibility.
- **No semantics in strings.** Optionality, references, and operation kinds are
  structured data — never parsed out of a name.
- **Loud, never silent.** An unclassifiable shape is an error that names the file
  and JSON path, not a silent fallback.
- **No new runtime dependencies** in the generated SDKs or the hand-written
  runtimes without discussion — they're deliberately dependency-light.
- **Match the surrounding code** — its naming, comment density, and idioms.

## Where things live

- **`ARCHITECTURE.md`** — the pipeline (ingest → IR → semantics → manifest +
  runtime contract → backend lowering + printer) and directory map. Read first.
- **`NEW_ENGINE_REQUIREMENTS.md`** — the normative requirements (`FR`/`NF`/`TR`/
  `VR`/`G-` identifiers).
- **`DECISIONS.md`** — design decisions as dated `D-###` entries; code comments
  cite them. Add one when you make a non-obvious choice.
- **`CONTRIBUTING.md`** — setup, the gate suite, and the PR workflow.

## Pull requests

Branch off `main`, keep the change focused, keep the gates green, add or adjust
tests (a new generated shape needs a compile gate; a behavioral change needs a
round-trip test), and cite any relevant `D-###` / requirement IDs.
