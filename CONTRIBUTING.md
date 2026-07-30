# Contributing to box-gantry

Thanks for your interest in box-gantry — a Rust engine that generates Box API
SDKs (Go, Apex, Rust, TypeScript, Java) from the Box OpenAPI specification.

## Ground rules

- **One focused change per pull request.** Small, reviewable PRs land faster and
  are easier to reason about.
- **Every PR keeps CI green.** The full gate suite (below) runs on every push and
  pull request; a red build blocks the merge.
- **Deterministic output.** Generated code must be byte-for-byte reproducible —
  same specs in, same files out (sorted by path). Tests enforce this.
- **No new runtime dependencies** in the generated SDKs or the hand-written
  runtimes without discussion — the SDKs are deliberately dependency-light
  (pure-stdlib where the platform allows).

## Development setup

The Rust toolchain is pinned by `rust-toolchain.toml`, so `rustup` installs the
right version automatically. That alone is enough to build the engine and run its
unit tests.

The **verification gates** that compile generated SDKs with their real toolchains
additionally need those toolchains installed (the same versions CI uses):

| Target | Toolchain |
|---|---|
| Go | Go 1.23+ |
| TypeScript | Node 22+ and TypeScript 7 (`npm i -g typescript@7`) |
| Java | JDK 26 (Amazon Corretto) |
| Apex | Salesforce CLI (`sf`) + a Dev Hub, for the scratch-org gates |

A gate that can't find its toolchain skips cleanly, so you can work on one backend
without installing all five.

## Build, test, and lint

Run these before opening a PR — they are exactly what CI runs:

```sh
cargo fmt --all --check                 # formatting
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                  # engine + every backend's verification gate
```

The CLI has five subcommands — `check`, `generate`, `verify`, `conform`, and
`diff`. To generate an SDK locally:

```sh
cargo run -p gantry-cli -- generate --target rust --out /tmp/sdk \
  fixtures/specs/openapi.json \
  fixtures/specs/openapi-v2025.0.json \
  fixtures/specs/openapi-v2026.0.json
```

## How it works

The pipeline is: ingest → typed IR → semantics → per-language manifest + runtime
contract → backend lowering + printer. Every backend consumes the same IR;
language-specific behavior is driven by a **capability manifest**, never by
branching on the language name. The [README](./README.md#how-it-works) has a
stage-by-stage overview; the crate layout under `crates/` mirrors it.

## Pull request workflow

1. Branch off `main`.
2. Make your change; keep it focused and keep the gates green.
3. Add or update tests — new generated shapes need a compile gate, and behavioral
   changes need a round-trip or behavioral test.
4. Open the PR against `main`. Describe *what* and *why*.
5. Address review feedback, keep CI green, and a maintainer will merge.

## Code style

Match the surrounding code — its naming, comment density, and idioms. Rust is
`rustfmt`-clean and `clippy -D warnings`-clean by construction; generated code
for each target must satisfy that target's formatter/linter too.
