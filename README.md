# box-gantry

A net-new SDK generator, written in Rust, that consumes the Box OpenAPI
specification and generates Box SDKs — **Go** (v1), **Salesforce Apex**
(v2), **Rust** (v3). No dependencies on existing SDKs or existing
developers; box-codegen is consultable prior art only.

## Quickstart

Rust only (the toolchain is pinned by `rust-toolchain.toml`):

```sh
cargo test --workspace          # engine tests, incl. ingesting the real Box specs
cargo run -p gantry-cli -- check \
  fixtures/specs/openapi.json \
  fixtures/specs/openapi-v2025.0.json \
  fixtures/specs/openapi-v2026.0.json
```

`gantry check` ingests and validates a spec set; `generate` and `verify`
arrive with the Go backend (see `PLAN.md`, milestone M3).

## Documents

Start here:

| Doc | Role |
|---|---|
| [`SCOPE.md`](./SCOPE.md) | One-page orientation and hard boundaries |
| [`NEW_ENGINE_REQUIREMENTS.md`](./NEW_ENGINE_REQUIREMENTS.md) | Normative requirements, estimates, acceptance criteria |
| [`REWRITE_ASSESSMENT.md`](./REWRITE_ASSESSMENT.md) | Why net-new, why Rust, lessons learned |
| [`PLAN.md`](./PLAN.md) | Milestones, verification cadence, risks |
| [`DECISIONS.md`](./DECISIONS.md) | Decision records |
| [`ISSUES.md`](./ISSUES.md) | Engineering issue log |
