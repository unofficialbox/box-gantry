# 🏗️ New Codegen Engine — Requirements

Requirements for the net-new SDK generator described in
[`REWRITE_ASSESSMENT.md`](./REWRITE_ASSESSMENT.md). This document is the seed
specification for the new project's repository; box-codegen itself is frozen
reference material.

**Scope assumptions:** zero existing users; zero obligations to the six
legacy SDK targets; SDK targets are **Go** (first), **Salesforce Apex**
(second), **Rust** (third); the engine is implemented in **Rust**
(assessment §6) and lives in its own repository.

## 🔑 Legend

| Symbol / Notation | Meaning |
|---|---|
| ✅ | Satisfied — a directly portable artifact already exists (code, suite, or procedure carries over nearly verbatim) |
| 🔶 | Partial — proven design or reference implementation exists in box-codegen but must be re-expressed in the new engine |
| ❌ | Not started — net-new work |
| **Est. hrs 👤 (🤖)** | Estimated effort in **human engineer-hours**, with **AI-agent-driven hours** in parentheses (agent-assisted delivery observed at roughly 3× throughput on the Go target) |
| **% Complete** | Section score: ✅ = 100, 🔶 = 50, ❌ = 0 per row, averaged (salvage-adjusted — credits reusable box-codegen assets, not new-repo code, which is 0 lines today) |
| MUST / SHOULD / MAY | RFC 2119 |
| (G-n / D-n / §n) | Source lesson: box-codegen issue, decision record, or assessment section |

## 📊 Roll-up

| Section | Est. hrs 👤 (🤖) | % Complete |
|---|---|---|
| FR-1 Spec ingestion | 280 (85) | 25% |
| FR-2 Intermediate representation | 240 (70) | 21% |
| FR-3 Semantic analysis | 160 (50) | 0% |
| FR-4 Capability manifest | 40 (12) | 0% |
| FR-5 Runtime contract | 100 (30) | 17% |
| FR-6 Backend infrastructure | 120 (36) | 30% |
| FR-7 Feature synthesis | 400 (120) | 44% |
| FR-8 CLI / driver | 60 (18) | 0% |
| NF Non-functional | 140 (42) | 14% |
| VR Verification | 220 (66) | 19% |
| TR-Go | 200 (60) | 58% |
| TR-Apex | 460 (140) | 0% |
| TR-Rust | 260 (78) | 0% |
| **TOTAL** | **2,680 (807)** | **~21%** |

> Hours are effort, not calendar: with agent-driven parallelism the calendar
> path is the assessment §7 plan (~5–7 months to v1). The salvage-adjusted
> ~21% reflects that the Go lowering logic, fixture suite, verification
> loop, and feature designs (D-003…D-013) already exist and port — the new
> repository itself starts at zero.

---

## 1. 📦 Product scope

The engine consumes the Box OpenAPI specification (base + versioned) and
produces, per target, a complete SDK with **functional parity to the Box SDK
capability contract**. Byte- or structure-level parity with any existing SDK
is a **non-goal** (§9 assumptions).

| Capability | Detail | Release |
|---|---|---|
| Resource managers | ~86, aggregated by a single client entry point | v1 → v3 |
| Auth flows | Developer Token, CCG, JWT, OAuth 2.0 auth-code — token storage, refresh, revoke, downscope | v1 → v3 |
| Network layer | Retrying: backoff + jitter, 401 refresh, `Retry-After` | v1 → v3 |
| Uploads / downloads | Multipart + chunked/session uploads; streaming downloads where the platform allows | v1 → v3 |
| Pagination | Marker- and offset-based, idiomatic per-language iteration | v1 → v3 |
| Versioned APIs | e.g. `2025.0`, `2026.0` schema/parameter sets | v1 → v3 |
| Polymorphism | `oneOf` + extensible enums; unknown values round-trip (G-10, G-11, D-012) | v1 → v3 |
| Generated tests & docs | Per-manager tests and reference docs | v1 → v3 |

## 2. ⚙️ Functional requirements

### FR-1 — 📥 Spec ingestion — 280 hrs 👤 (85 🤖) — 25% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-1.1 | MUST | Ingest OpenAPI 3.x, including multiple documents per run (base + versioned specs), into one merged, version-aware model | G-9 | 🔶 | 80 (25) |
| FR-1.2 | MUST | All naming rules (operation names, manager grouping, schema names, casing) applied in the ingestion layer; impossible for a driver/CLI to bypass (the `PostFolders` failure) | G-18 | ❌ | 60 (18) |
| FR-1.3 | MUST | Box-specific quirks (base-URL mapping, multipart/octet-stream bodies, binary responses, extensible-enum markers) handled at ingestion, represented structurally in the IR | G-2, G-7, G-11 | 🔶 | 100 (30) |
| FR-1.4 | MUST | Ingestion errors report the offending spec path and fail the run; no silent partial ingestion | §2 | ❌ | 40 (12) |

### FR-2 — 🧬 Intermediate representation — 240 hrs 👤 (70 🤖) — 21% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-2.1 | MUST | Closed set of typed node kinds (Rust enums); adding a kind is a compile error in every backend lowering that misses it | §6 | ❌ | 60 (18) |
| FR-2.2 | MUST | No semantics in strings — optionality, chains, force-unwraps, package paths, operation kinds are structured data (the `"…downloadUrl!"` pattern) | §2 | ❌ | 40 (12) |
| FR-2.3 | MUST | Optionality is a type constructor (`Optional<T>`), lowered per target (Go pointers / Apex nullable / Rust `Option<T>`) | D-004 | 🔶 | 20 (6) |
| FR-2.4 | MUST | References are resolved links to declarations; an unresolved ref is a semantic error, never a backend concern | §2 | ❌ | 30 (9) |
| FR-2.5 | MUST | Unions carry discriminator field, per-variant values, and open/closed extensibility — rich enough for Rust enums, Go variant structs, Apex parse dispatch | G-10, G-11, D-012 | 🔶 | 40 (12) |
| FR-2.6 | MUST | Modules/namespaces modeled as a rich concept; Apex lowers it, Go/Rust use it directly; Apex's flat namespace never shapes the IR | §8 | ❌ | 30 (9) |
| FR-2.7 | MUST | One error-model abstraction spanning `(T, error)`, exceptions, and `Result<T, E>` | D-003 | 🔶 | 20 (6) |

### FR-3 — 🔎 Semantic analysis — 160 hrs 👤 (50 🤖) — 0% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-3.1 | MUST | Exactly one semantic pass between ingestion and backends, producing a complete, queryable type environment (every expression typed, every ref bound) | §3 | ❌ | 80 (25) |
| FR-3.2 | MUST | Backends receive only verified programs; an unbound ref or untyped expression at backend time fails loudly, never emits placeholders | §2 | ❌ | 20 (6) |
| FR-3.3 | MUST | Semantic errors carry a spec-level location and an actionable message | §3 | ❌ | 30 (10) |
| FR-3.4 | MUST | No implicit order-sensitive pipeline; if multiple passes exist, each declares requires/provides and the chain is validated at startup | §2 | ❌ | 30 (9) |

### FR-4 — 🗂️ Capability manifest — 40 hrs 👤 (12 🤖) — 0% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-4.1 | MUST | One declarative manifest per language: module system, generics, error model, async model, streaming, callout/transaction limits, test-coverage mandates | §3, §4 | ❌ | 24 (7) |
| FR-4.2 | MUST | Feature synthesis keys off manifest entries only; comparing against a language name outside the manifest is prohibited (the 31 `=== 'CSharp'` sites) | §2 | ❌ | 16 (5) |

### FR-5 — 🔌 Runtime contract — 100 hrs 👤 (30 🤖) — 17% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-5.1 | MUST | Hand-written runtime surface declared machine-readably: name, arity, types, error behavior, context/cancellation threading | §3 | ❌ | 40 (12) |
| FR-5.2 | MUST | Generated code calls only through the contract; generation fails on signature drift | §2 | ❌ | 30 (9) |
| FR-5.3 | MUST | Compilable runtime stubs per target live in the engine repo, so output compile-verifies without the real SDK runtime | G-1 | 🔶 | 30 (9) |

### FR-6 — 🖨️ Backend infrastructure — 120 hrs 👤 (36 🤖) — 30% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-6.1 | MUST | Backends ship v1 Go → v2 Apex → v3 Rust; adding one touches only manifest + lowering + printer, never ingestion/semantics | §7 | 🔶 | 30 (9) |
| FR-6.2 | MUST | Deterministic output: identical inputs → byte-identical output; no timestamps or environment content | §3 | ❌ | 20 (6) |
| FR-6.3 | MUST | Every generated file carries a "generated — do not edit" header + engine/spec version ids | NF-7 | 🔶 | 10 (3) |
| FR-6.4 | MUST | Output is canonical-format clean (gofmt / rustfmt / Prettier-Apex) by construction or built-in post-step | G-17 | ❌ | 30 (9) |
| FR-6.5 | MUST | Never emit constructs the toolchain rejects: empty-module files, unused imports, unreachable code (the G-1 fix classes) | G-1 | 🔶 | 30 (9) |

### FR-7 — 🧩 Feature synthesis — 400 hrs 👤 (120 🤖) — 44% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-7.1 | MUST | Managers per spec tag + client entry point with fluent `With*` decorators that preserve sibling state | G-3 | 🔶 | 50 (15) |
| FR-7.2 | MUST | All four auth flows wired per the capability contract, with pluggable token storage | §1 | 🔶 | 60 (18) |
| FR-7.3 | MUST | Marker/offset operations gain idiomatic paged surfaces (Go `iter.Seq2`; Apex transaction-bounded continuations; Rust `Stream`/iterator), manifest-driven | G-8, D-013 | 🔶 | 60 (18) |
| FR-7.4 | MUST | Multipart + chunked-session uploads; streaming bodies where the platform allows, buffered fallbacks where not (Apex heap) | G-7 | 🔶 | 60 (18) |
| FR-7.5 | MUST | Versioned schema/parameter sets generate into distinct idiomatic namespaces without base-spec collisions | G-9 | 🔶 | 40 (12) |
| FR-7.6 | MUST | `oneOf`: exactly-one-variant semantics, discriminator dispatch on deserialize, unknown values retained and round-tripped | D-012, G-11 | 🔶 | 40 (12) |
| FR-7.7 | MUST | Per-manager reference docs + cross-cutting guides (auth, pagination, errors) generated from the same IR | §1 | ❌ | 50 (15) |
| FR-7.8 | MUST | Per-manager tests generated from shared, language-agnostic test semantics; ship-blocking for Apex (75% gate) | G-18, §4 | 🔶 | 40 (12) |

### FR-8 — ⌨️ CLI / driver — 60 hrs 👤 (18 🤖) — 0% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-8.1 | MUST | One CLI: `generate <spec…> --target <lang> --out <dir>`, plus `check` (ingest + semantics) and `verify` (generate + compile) | §3 | ❌ | 30 (9) |
| FR-8.2 | MUST | CLI is a thin argument parser over the engine library; business logic in the driver prohibited | FR-1.2 | ❌ | 10 (3) |
| FR-8.3 | MUST | Exit codes distinguish spec errors, engine bugs, verification failures | NF-3 | ❌ | 20 (6) |

## 3. 🛡️ Non-functional requirements — 140 hrs 👤 (42 🤖) — 14% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| NF-1 | MUST | **No silent failure**: unhandled IR shapes are compile errors; unresolvable constructs are loud errors; anything skipped appears in the run summary. Emitting less than expected while exiting 0 is a critical bug | §2 | ❌ | 30 (9) |
| NF-2 | SHOULD | Feedback-loop speed: `check` on the full real spec in seconds; per-target generation well under a minute; unit-level probes (single-node lowering) exist | §2 | ❌ | 30 (9) |
| NF-3 | MUST | Every user-facing error answers what / where (spec path or IR node) / what to do; internal invariant violations name the responsible component | §3 | ❌ | 20 (6) |
| NF-4 | MUST | A fourth language requires only: manifest + runtime contract/stubs + lowering/printer + harness entry, with the compiler enumerating the lowering work list | FR-2.1 | ❌ | 10 (3) |
| NF-5 | MUST | Agent/newcomer maintainability: all contracts explicit in types or manifests; adopt the ISSUES/DECISIONS/PLAN/SCOPE docs regime from day one | §7 | ✅ | 10 (3) |
| NF-6 | MUST | Reproducible from clean checkout: pinned toolchain + deps; no network during generation (specs are inputs) | §3 | ❌ | 20 (6) |
| NF-7 | MUST | Generated output embeds spec hash + engine version; every SDK release traceable to its inputs | §3 | ❌ | 20 (6) |

## 4. 🔬 Verification requirements — 220 hrs 👤 (66 🤖) — 19% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| VR-1.1 | MUST | Go: generate full real spec (base + versioned) → `go build ./...` + `go vet ./...` clean in CI, from the backend's first week | G-1 | ✅ | 30 (9) |
| VR-1.2 | MUST | Rust: `cargo check` + `clippy` clean in CI | §4 | ❌ | 20 (6) |
| VR-1.3 | MUST | Apex: syntax check (apex-parser / Code Analyzer) per commit; full `sf project deploy validate` (compile + tests) against a scratch org at least per merge | §4 | ❌ | 60 (18) |
| VR-2 | MUST | Per-node lowering fixtures (IR fragment → expected source) per backend; the 54-case box-codegen Go suite ports as the initial Go set | §7 | 🔶 | 40 (12) |
| VR-3 | MUST | §1 capability contract as a machine-checkable per-target checklist (operation/manager counts, auth flows, paged surfaces), reported every CI run | §2 | ❌ | 30 (9) |
| VR-4 | MUST | Round-trip tests: optional presence/absence, known/unknown discriminators, open enums, versioned payloads | G-10, G-11 | ❌ | 20 (6) |
| VR-5 | MUST | Determinism: CI generates twice and diffs for byte-identity | FR-6.2 | ❌ | 10 (3) |
| VR-6 | MUST | No silent caps: generation-time skips/fallbacks appear in a summary CI asserts against an allowlist | §2 | ❌ | 10 (3) |

## 5. 🎯 Per-target requirements

### TR-Go 🐹 (v1) — 200 hrs 👤 (60 🤖) — 58% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| TR-Go.1 | MUST | `(T, error)` error model; pointer optionals with nilable types (slices, maps, interfaces) unwrapped | D-003, D-004, G-1 | 🔶 | 30 (9) |
| TR-Go.2 | MUST | No generated per-model serializers — struct tags + `encoding/json`; generated `MarshalJSON`/`UnmarshalJSON` only for `oneOf` variant structs | D-005, D-012 | 🔶 | 30 (9) |
| TR-Go.3 | MUST | `context.Context` first parameter on every I/O method | D-003 | 🔶 | 10 (3) |
| TR-Go.4 | MUST | Pagination via stdlib `iter.Seq2[*T, error]` (Go ≥ 1.23) | D-013 | 🔶 | 30 (9) |
| TR-Go.5 | MUST | Package layout `client` / `managers` / `schemas` (+`vNrM`) / `parameters/vNrM` / `networking` / `serialization` / `internal/utils`; gofmt-clean | G-9, G-17 | 🔶 | 40 (12) |
| TR-Go.6 | MUST | Functional baseline: box-codegen Go output's API surface, diffed structurally (not byte-wise) | §7 | ✅ | 60 (18) |

### TR-Apex ☁️ (v2) — 460 hrs 👤 (140 🤖) — 0% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| TR-Apex.1 | MUST | Flat-namespace layout: outer classes as grouping, deterministic name mangling within identifier limits; no reliance on packages/modules | §4 | ❌ | 80 (24) |
| TR-Apex.2 | MUST | No user generics: pagination/shared containers lower to per-type code or typed `Object` wrappers — decided by the manifest, never ad hoc | §4 | ❌ | 80 (24) |
| TR-Apex.3 | MUST | Governor-limit-aware shapes: callout budgets and heap/CPU bounds inform pagination (transaction-bounded, `Queueable` continuations), chunked upload part sizes, retry policy | §4 | ❌ | 120 (36) |
| TR-Apex.4 | MUST | `oneOf`/polymorphic deserialization via generated `JSON.deserializeUntyped` dispatch | §4 | ❌ | 80 (24) |
| TR-Apex.5 | MUST | Generated test classes clear the 75% coverage deployment gate and ship with the SDK | §4 | ❌ | 100 (32) |

### TR-Rust 🦀 (v3) — 260 hrs 👤 (78 🤖) — 0% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| TR-Rust.1 | MUST | `oneOf` as native enums with serde tagged/adjacent representations; unknown discriminators retained via catch-all variant | §4 | ❌ | 60 (18) |
| TR-Rust.2 | MUST | `Result<T, BoxError>` error model; `Option<T>` optionals | §4 | ❌ | 40 (12) |
| TR-Rust.3 | MUST | Async-first (`reqwest` + `tokio`); owned types in all public signatures (no lifetime params); builders for optional-heavy request structs | §4 | ❌ | 120 (36) |
| TR-Rust.4 | MUST | `cargo check` + `clippy` clean; rustfmt-clean output | §4 | ❌ | 40 (12) |

## 6. 🚫 Non-goals

| # | Non-goal |
|---|---|
| 1 | Compatibility with, or migration of, the six legacy box-codegen SDK targets |
| 2 | Byte- or structure-parity with any existing SDK's output |
| 3 | A plugin system for third-party languages (revisit after v3; NF-4 keeps the door open) |
| 4 | Supporting non-Box OpenAPI documents beyond what the Box specs exercise |

## 7. 🚀 Releases & acceptance criteria

| Release | Scope | Acceptance criteria | Est. hrs 👤 (🤖) | % Complete |
|---|---|---|---|---|
| **v1 — Go SDK** 🐹 | Engine core (FR-1…FR-8, NF), Go verification (VR-1.1, VR-2, VR-3, VR-4, VR-5), TR-Go | Full real spec (base + 2025.0 + 2026.0) generates; `go build` + `go vet` clean; ported fixture suite green; conformance checklist ≥ box-codegen Go baseline; round-trip + determinism green; smoke app executes one call per auth flow | 1,860 (560) | ~25% |
| **v2 — Apex SDK** ☁️ | TR-Apex, VR-1.3 harness | Full scratch-org validation green **including generated tests** (coverage gate); conformance parity with v1 minus manifest-documented platform exclusions | 530 (160) | 0% |
| **v3 — Rust SDK** 🦀 | TR-Rust, VR-1.2 harness | `cargo check` + `clippy` clean; conformance parity with v1; round-trip suite green incl. unknown-discriminator retention | 290 (87) | 0% |
| **TOTAL** | | | **2,680 (807)** | **~21%** |
