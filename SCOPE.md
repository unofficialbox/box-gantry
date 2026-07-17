# 🎯 box-gantry — Scope

One-page orientation; [`NEW_ENGINE_REQUIREMENTS.md`](./NEW_ENGINE_REQUIREMENTS.md)
is normative.

## What this is

A net-new SDK generator, written in **Rust**, that consumes the Box OpenAPI
specification (base + versioned documents) and generates complete Box SDKs
with functional parity to the Box SDK capability contract (requirements
R§1): ~86 resource managers, four auth flows, retrying network layer,
multipart + chunked uploads, streaming downloads, idiomatic pagination,
versioned APIs, `oneOf`/extensible-enum round-tripping, and generated tests
and docs.

## Targets

| Order | Target | Release |
|---|---|---|
| 1 | Go | v1 |
| 2 | Salesforce Apex | v2 |
| 3 | Rust | v3 |
| 4 | TypeScript | v4 |
| 5 | Java 26 | v5 |

## Hard boundaries

- **No dependency on existing SDKs or existing developers.** Zero users,
  zero obligations to the six legacy box-codegen targets; no byte- or
  structure-parity with any existing SDK's output.
- **box-codegen is consult-only** (D-102): prior art to learn from, never a
  baseline to match or code to port verbatim.
- No third-party-language plugin system before v3 retrospective (NF-4
  keeps the door open).
- No support for non-Box OpenAPI documents beyond what the Box specs
  exercise.

## Sibling documents

| File | Role |
|---|---|
| [`NEW_ENGINE_REQUIREMENTS.md`](./NEW_ENGINE_REQUIREMENTS.md) | Normative requirements, estimates, acceptance criteria |
| [`REWRITE_ASSESSMENT.md`](./REWRITE_ASSESSMENT.md) | Rationale: why net-new, why Rust, lessons from box-codegen |
| [`PLAN.md`](./PLAN.md) | Milestones, verification cadence, risk register |
| [`DECISIONS.md`](./DECISIONS.md) | Decision records (D-101+) |
| [`ISSUES.md`](./ISSUES.md) | Engineering issue log (BG-n) |
