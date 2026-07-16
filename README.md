# box-gantry

A net-new SDK generator, written in Rust, that consumes the Box OpenAPI
specification and generates Box SDKs — **Go** (v1), **Salesforce Apex**
(v2), **Rust** (v3), and **TypeScript** (v4). No dependencies on existing SDKs
or existing developers; box-codegen is consultable prior art only.

## Quickstart

Rust only (the toolchain is pinned by `rust-toolchain.toml`):

```sh
cargo test --workspace          # engine tests, incl. ingesting the real Box specs
cargo run -p gantry-cli -- check \
  fixtures/specs/openapi.json \
  fixtures/specs/openapi-v2025.0.json \
  fixtures/specs/openapi-v2026.0.json
```

The `gantry` CLI has five subcommands: `check` (ingest + validate a spec
set), `generate` (emit an SDK), `verify` (generate then compile with the
target toolchain), `conform` (the R§1 capability checklist), and `diff` (the
breaking-change report between two spec sets). The **Go SDK (v1) is shipped**;
Apex (v2) is essentially complete; **Rust (v3) has reached full capability
parity** with Go (`verify --target rust` gates the toolchain — rustfmt +
`cargo check` + clippy + `cargo test` — while `conform --target rust` reports
9/9 and gates the CI release check); and TypeScript (v4) is next — see
[`PROGRESS.md`](./PROGRESS.md).

## Generating SDKs

`generate` emits **one** target per run — `--target` takes a single manifest
key and `--out` the output directory. Three targets are implemented in the CLI
today: `go` (shipped), `apex` (near-complete), and `rust` (capability parity);
`typescript` (v4) is planned and not yet selectable.
The trailing arguments are the spec set: the base spec plus each versioned
overlay, ingested together.

```sh
# One SDK — Apex into ./out/apex
cargo run -p gantry-cli -- generate --target apex --out out/apex \
  fixtures/specs/openapi.json \
  fixtures/specs/openapi-v2025.0.json \
  fixtures/specs/openapi-v2026.0.json

# Another target — Go into ./out/go (same spec set)
cargo run -p gantry-cli -- generate --target go --out out/go \
  fixtures/specs/openapi.json \
  fixtures/specs/openapi-v2025.0.json \
  fixtures/specs/openapi-v2026.0.json
```

Build **several targets at once** by passing a comma-separated list (or
repeating `--target`); use `all` for the whole fleet. With more than one target
each SDK lands in its own `<out>/<target>/` subdirectory (`out/go`, `out/apex`):

```sh
# A subset — Go and Apex into out/go and out/apex
cargo run -p gantry-cli -- generate --target go,apex --out out \
  fixtures/specs/openapi.json \
  fixtures/specs/openapi-v2025.0.json \
  fixtures/specs/openapi-v2026.0.json

# Everything
cargo run -p gantry-cli -- generate --target all --out out \
  fixtures/specs/openapi.json \
  fixtures/specs/openapi-v2025.0.json \
  fixtures/specs/openapi-v2026.0.json
```

To generate **and compile** the output with the target's real toolchain, use
`verify` instead (Go today; Apex compiles on the Salesforce platform via the
`apex-scratch` workflow, VR-1.3):

```sh
cargo run -p gantry-cli -- verify --target go \
  fixtures/specs/openapi.json \
  fixtures/specs/openapi-v2025.0.json \
  fixtures/specs/openapi-v2026.0.json
```

## Documents

Start here:

| Doc | Role |
|---|---|
| [`ARCHITECTURE.md`](./ARCHITECTURE.md) | Components, the pipeline, directory breakdown |
| [`PROGRESS.md`](./PROGRESS.md) | Effort-weighted percentage breakdown |
| [`SCOPE.md`](./SCOPE.md) | One-page orientation and hard boundaries |
| [`NEW_ENGINE_REQUIREMENTS.md`](./NEW_ENGINE_REQUIREMENTS.md) | Normative requirements, estimates, acceptance criteria |
| [`REWRITE_ASSESSMENT.md`](./REWRITE_ASSESSMENT.md) | Why net-new, why Rust, lessons learned |
| [`PLAN.md`](./PLAN.md) | Milestones, verification cadence, risks |
| [`DECISIONS.md`](./DECISIONS.md) | Decision records |
| [`ISSUES.md`](./ISSUES.md) | Engineering issue log |
