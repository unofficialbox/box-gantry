# 🏗️ box-gantry — New Codegen Engine Requirements

Requirements for **box-gantry**, the net-new SDK generator. This document is
the normative specification for this repository. box-codegen is **consultable
prior art**: its designs and
lessons inform this spec, but no requirement or acceptance criterion depends
on its code or its output.

**Scope assumptions:** zero existing users; zero obligations to the six
legacy SDK targets; SDK targets are **Go** (first), **Salesforce Apex**
(second), **Rust** (third), **TypeScript** (fourth, D-143), and **Java 26**
(fifth, D-164); the engine is implemented in **Rust** (assessment §6, D-101)
and lives in this repository.

## 🔑 Legend

| Symbol / Notation | Meaning |
|---|---|
| ✅ | Satisfied — exists in this repository today |
| 🔶 | Partial — a proven design or reference implementation exists in box-codegen to consult; the work must be re-expressed here (nothing is treated as verbatim-portable) |
| ❌ | Not started — net-new work with no meaningful prior art |
| **Est. hrs 👤 (🤖)** | Estimated effort in **human engineer-hours**, with **AI-agent-driven hours** in parentheses. The 🤖 column is derived mechanically at the ~3× throughput observed on the box-codegen Go target — it is not an independent estimate |
| **% Complete** | Section score: ✅ = 100, 🔶 = 50, ❌ = 0 per row, averaged. 🔶 credit reflects consultable designs, not code — this repository is 0 lines of engine code today |
| MUST / SHOULD / MAY | RFC 2119 |
| G-n / D-n | box-codegen's `ISSUES.md` / `DECISIONS.md` entries (prior art, in that repository; this repo's own decisions start at D-101 in [`DECISIONS.md`](./DECISIONS.md)) |
| §n / R§n | Assessment section / section of this document |

## 📊 Roll-up

| Section | Est. hrs 👤 (🤖) | % Complete |
|---|---|---|
| FR-1 Spec ingestion | 280 (85) | 25% |
| FR-2 Intermediate representation | 240 (72) | 21% |
| FR-3 Semantic analysis | 160 (50) | 0% |
| FR-4 Capability manifest | 40 (12) | 0% |
| FR-5 Runtime contract | 100 (30) | 17% |
| FR-6 Backend infrastructure | 120 (36) | 30% |
| FR-7 Feature synthesis | 400 (120) | 44% |
| FR-8 CLI / driver | 60 (18) | 0% |
| FR-9 Spec evolution | 60 (18) | 0% |
| NF Non-functional | 170 (51) | 13% |
| VR Verification | 280 (84) | 11% |
| TR-Go | 280 (84) | 50% |
| TR-Apex | 580 (176) | 0% |
| TR-Rust | 360 (108) | 0% |
| TR-TypeScript | 300 (90) | 0% |
| TR-Java | 360 (108) | 0% |
| **TOTAL** | **3,790 (1,142)** | **~14%** |

> Hours are effort, not calendar: with agent-driven parallelism the calendar
> path was ~6–8 months to v1. This roll-up is the project-start
> snapshot; the ~16% reflected that the Go
> lowering design, fixture semantics, verification-loop procedure, and
> feature designs (D-003…D-013) exist in box-codegen as prior art to
> consult — the code in this repository starts at zero lines, and nothing
> is credited as directly portable.

---

## 1. 📦 Product scope

The engine consumes the Box OpenAPI specification (base + versioned) and
produces, per target, a complete SDK with **functional parity to the Box SDK
capability contract**. Byte- or structure-level parity with any existing SDK
is a **non-goal** (assessment, operating assumptions; R§6).

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

### FR-2 — 🧬 Intermediate representation — 240 hrs 👤 (72 🤖) — 21% complete

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
| FR-7.2 | MUST | All four auth flows wired per the capability contract, with pluggable token storage | R§1 | 🔶 | 60 (18) |
| FR-7.3 | MUST | Marker/offset operations gain idiomatic paged surfaces (Go `iter.Seq2`; Apex transaction-bounded continuations; Rust `Stream`/iterator), manifest-driven | G-8, D-013 | 🔶 | 60 (18) |
| FR-7.4 | MUST | Multipart + chunked-session uploads; streaming bodies where the platform allows, buffered fallbacks where not (Apex heap) | G-7 | 🔶 | 60 (18) |
| FR-7.5 | MUST | Versioned schema/parameter sets generate into distinct idiomatic namespaces without base-spec collisions | G-9 | 🔶 | 40 (12) |
| FR-7.6 | MUST | `oneOf`: exactly-one-variant semantics, discriminator dispatch on deserialize, unknown values retained and round-tripped | D-012, G-11 | 🔶 | 40 (12) |
| FR-7.7 | MUST | Per-manager reference docs + cross-cutting guides (auth, pagination, errors) generated from the same IR | R§1 | ❌ | 50 (15) |
| FR-7.8 | MUST | Per-manager tests generated from shared, language-agnostic test semantics; ship-blocking for Apex (75% gate) | G-18, §4 | 🔶 | 40 (12) |

### FR-8 — ⌨️ CLI / driver — 60 hrs 👤 (18 🤖) — 0% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-8.1 | MUST | One CLI: `generate <spec…> --target <lang> --out <dir>`, plus `check` (ingest + semantics) and `verify` (generate + compile) | §3 | ❌ | 30 (9) |
| FR-8.2 | MUST | CLI is a thin argument parser over the engine library; business logic in the driver prohibited | FR-1.2 | ❌ | 10 (3) |
| FR-8.3 | MUST | Exit codes distinguish spec errors, engine bugs, verification failures | NF-3 | ❌ | 20 (6) |

### FR-9 — 🔄 Spec evolution — 60 hrs 👤 (18 🤖) — 0% complete

The engine's whole premise is ongoing regeneration as Box updates the spec;
these requirements make that a first-class workflow rather than an afterthought.

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| FR-9.1 | MUST | Spec-diff report between any two ingested spec sets: operations, schemas, parameters, and enum values added / removed / changed, each classified breaking vs non-breaking | R§7 | ❌ | 40 (12) |
| FR-9.2 | MUST | Regeneration is the release workflow: a documented semver policy maps breaking-change classes to SDK version bumps, and the diff report suggests the bump | R§7 | ❌ | 20 (6) |

## 3. 🛡️ Non-functional requirements — 170 hrs 👤 (51 🤖) — 13% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| NF-1 | MUST | **No silent failure**: unhandled IR shapes are compile errors; unresolvable constructs are loud errors; anything skipped appears in the run summary. Emitting less than expected while exiting 0 is a critical bug | §2 | ❌ | 30 (9) |
| NF-2 | SHOULD | Feedback-loop speed: `check` on the full real spec in seconds; per-target generation well under a minute; unit-level probes (single-node lowering) exist | §2 | ❌ | 30 (9) |
| NF-3 | MUST | Every user-facing error answers what / where (spec path or IR node) / what to do; internal invariant violations name the responsible component | §3 | ❌ | 20 (6) |
| NF-4 | MUST | A fourth language requires only: manifest + runtime contract/stubs + lowering/printer + harness entry, with the compiler enumerating the lowering work list | FR-2.1 | ❌ | 10 (3) |
| NF-5 | MUST | Agent/newcomer maintainability: all contracts explicit in types or manifests; the ISSUES/DECISIONS/PLAN/SCOPE docs regime adopted from day one (seeded in this repo) | §7 | ✅ | 10 (3) |
| NF-6 | MUST | Reproducible from clean checkout: pinned toolchain + deps; no network during generation (specs are inputs) | §3 | ❌ | 20 (6) |
| NF-7 | MUST | Generated output embeds spec hash + engine version; every SDK release traceable to its inputs | §3 | ❌ | 20 (6) |
| NF-8 | MUST | Each release defines its ship artifact and the pipeline produces it: tagged Go module; publishable crate (`cargo publish --dry-run` clean); Apex deployable source, with the packaging decision (unlocked package vs deployable source) recorded in `DECISIONS.md` | R§7 | ❌ | 30 (9) |

## 4. 🔬 Verification requirements — 260 hrs 👤 (78 🤖) — 11% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| VR-1.1 | MUST | Go: generate full real spec (base + versioned) → `go build ./...` + `go vet ./...` clean in CI, from the backend's first week (the G-1 loop, rebuilt here) | G-1 | 🔶 | 30 (9) |
| VR-1.2 | MUST | Rust: `cargo check` + `clippy` clean in CI | §4 | ❌ | 20 (6) |
| VR-1.3 | MUST | Apex: syntax check (apex-parser / Code Analyzer) per commit; full `sf project deploy validate` (compile + tests) against a scratch org at least per merge | §4 | ❌ | 60 (18) |
| VR-2 | MUST | Per-node lowering fixtures (IR fragment → expected source) per backend; the 54-case box-codegen Go suite's case list and expected semantics inform the initial Go set, authored fresh against the new IR | §7 | 🔶 | 40 (12) |
| VR-3 | MUST | R§1 capability contract as a machine-checkable per-target checklist (operation/manager counts, auth flows, paged surfaces), reported every CI run | §2 | ❌ | 30 (9) |
| VR-4 | MUST | Round-trip tests: optional presence/absence, known/unknown discriminators, open enums, versioned payloads | G-10, G-11 | ❌ | 20 (6) |
| VR-5 | MUST | Determinism: CI generates twice and diffs for byte-identity | FR-6.2 | ❌ | 10 (3) |
| VR-6 | MUST | No silent caps: generation-time skips/fallbacks appear in a summary CI asserts against an allowlist | §2 | ❌ | 10 (3) |
| VR-7 | MUST | Live smoke suite per target: one call per auth flow plus upload, download, and paginate against a Box developer account; runs per release and on demand, never in the generation path | R§7 | ❌ | 40 (12) |

## 5. 🎯 Per-target requirements

### TR-Go 🐹 (v1) — 280 hrs 👤 (84 🤖) — 50% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| TR-Go.1 | MUST | `(T, error)` error model; pointer optionals with nilable types (slices, maps, interfaces) unwrapped | D-003, D-004, G-1 | 🔶 | 30 (9) |
| TR-Go.2 | MUST | No generated per-model serializers — struct tags + `encoding/json`; generated `MarshalJSON`/`UnmarshalJSON` only for `oneOf` variant structs | D-005, D-012 | 🔶 | 30 (9) |
| TR-Go.3 | MUST | `context.Context` first parameter on every I/O method | D-003 | 🔶 | 10 (3) |
| TR-Go.4 | MUST | Pagination via stdlib `iter.Seq2[*T, error]` (Go ≥ 1.23) | D-013 | 🔶 | 30 (9) |
| TR-Go.5 | MUST | Package layout `client` / `managers` / `schemas` (+`vNrM`) / `parameters/vNrM` / `networking` / `serialization` / `internal/utils`; gofmt-clean | G-9, G-17 | 🔶 | 40 (12) |
| TR-Go.6 | MUST | Functional baseline: the R§1 capability contract, checked via the VR-3 conformance checklist across the full spec surface; box-codegen's Go output MAY be consulted as an informal comparison, never as an acceptance oracle | R§1 | 🔶 | 60 (18) |
| TR-Go.7 | MUST | Hand-written Go runtime (networking with retry/backoff, auth token management, serialization helpers) implemented in this repo against the FR-5 contract and shipped with the generated SDK; the existing box-go-sdk runtime consulted, not vendored | FR-5 | 🔶 | 80 (24) |

### TR-Apex ☁️ (v2) — 580 hrs 👤 (176 🤖) — 0% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| TR-Apex.1 | MUST | Flat-namespace layout: outer classes as grouping, deterministic name mangling within identifier limits; no reliance on packages/modules | §4 | ❌ | 80 (24) |
| TR-Apex.2 | MUST | No user generics: pagination/shared containers lower to per-type code or typed `Object` wrappers — decided by the manifest, never ad hoc | §4 | ❌ | 80 (24) |
| TR-Apex.3 | MUST | Governor-limit-aware shapes: callout budgets and heap/CPU bounds inform pagination (transaction-bounded, `Queueable` continuations), chunked upload part sizes, retry policy | §4 | ❌ | 120 (36) |
| TR-Apex.4 | MUST | `oneOf`/polymorphic deserialization via generated `JSON.deserializeUntyped` dispatch | §4 | ❌ | 80 (24) |
| TR-Apex.5 | MUST | Generated test classes clear the 75% coverage deployment gate and ship with the SDK | §4 | ❌ | 100 (32) |
| TR-Apex.6 | MUST | Hand-written Apex runtime: `Http`-based networking with retry within callout budgets, Crypto-based JWT (RS256) signing with org key storage, pluggable token storage, multipart body assembly | FR-5, §4 | ❌ | 120 (36) |

### TR-Rust 🦀 (v3) — 360 hrs 👤 (108 🤖) — 0% complete

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| TR-Rust.1 | MUST | `oneOf` as native enums with serde tagged/adjacent representations; unknown discriminators retained via catch-all variant | §4 | ❌ | 60 (18) |
| TR-Rust.2 | MUST | `Result<T, BoxError>` error model; `Option<T>` optionals | §4 | ❌ | 40 (12) |
| TR-Rust.3 | MUST | Async-first (`reqwest` + `tokio`); owned types in all public signatures (no lifetime params); builders for optional-heavy request structs | §4 | ❌ | 120 (36) |
| TR-Rust.4 | MUST | `cargo check` + `clippy` clean; rustfmt-clean output | §4 | ❌ | 40 (12) |
| TR-Rust.5 | MUST | Hand-written Rust runtime: `reqwest`+`tokio` networking with retry/backoff, auth token management, streaming body support; implemented against the FR-5 contract | FR-5, §4 | ❌ | 100 (30) |

### TR-TypeScript 🟦 (v4) — 300 hrs 👤 (90 🤖) — 0% complete

TypeScript is the fourth target (D-143). It is the strongest structural fit for
the IR of any target so far — the type system expresses the IR's own shapes
almost directly — so the work concentrates on the runtime and the module/packaging
surface, not on bridging a type-system mismatch. The verification gate is the
**TypeScript 7 native (Go-ported) compiler** (`tsc --noEmit`), whose ~10× speedup
makes a full-spec type-check a fast per-commit signal, the TS analogue of `go build`
(VR-1.5).

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| TR-TS.1 | MUST | `oneOf`/polymorphic types as discriminated unions (`{ type: 'a' } \| { type: 'b' }`); unknown discriminators retained via a catch-all member; open enums as string-literal unions widened with `(string & {})` | §4 | ❌ | 60 (18) |
| TR-TS.2 | MUST | Tri-state optionality mapped to the type system directly: absent → `field?: T`, explicit null → `T \| null`, so the absent-vs-null distinction needs no wrapper type | §4 | ❌ | 40 (12) |
| TR-TS.3 | MUST | `Promise`-based async API; `Error`-subclass error model (`BoxApiError` carrying status + parsed body); optional-heavy requests take an options object, not positional params | §4 | ❌ | 60 (18) |
| TR-TS.4 | MUST | ESM output; `tsc --noEmit` clean under `strict`; formatter-clean (Prettier) by construction; ships type declarations (`.d.ts`) and both ESM/CJS entry points | §4 | ❌ | 40 (12) |
| TR-TS.5 | MUST | Hand-written TypeScript runtime: `fetch`-based networking (Node ≥ 20 / undici) with retry/backoff + `401` refresh, auth token management, streaming request/response bodies, multipart upload assembly; implemented against the FR-5 contract | FR-5, §4 | ❌ | 100 (30) |

### TR-Java ☕ (v5) — 360 hrs 👤 (108 🤖) — 0% complete

Java is the fifth target (D-164), positioned after TypeScript (v5 / M7).
**Java 26** is chosen deliberately: with records, sealed interfaces + `permits`,
record patterns, and pattern matching for `switch` all finalized, the IR's
shapes map cleanly — `oneOf` → a sealed interface over record variants dispatched
by an exhaustive `switch`, structs → immutable records — without the
type-erasure gymnastics an older Java would force. The runtime uses the JDK's
built-in `java.net.http.HttpClient` (no third-party HTTP dependency), and the
verification gate is `javac` (compile-clean under `-Xlint:all`) plus a
formatter (`google-java-format` / Spotless) — the Java analogue of `go build`
/ `cargo check` (VR-1.6). One place Java is *less* direct than TypeScript or
Rust: it has no native absent-vs-null distinction, so the tri-state needs a
wrapper (like Go's `Nullable[T]`), documented as the platform shape.

**Java 25/26 features we leverage** (see D-164 for the rationale): HTTP/3 (QUIC)
in `HttpClient` for the runtime transport (Java 26); the standard **PEM
Encodings** API to parse the JWT RSA key with no third-party crypto dep (Java 26
preview); **Structured Concurrency** for the chunked-upload fan-out (Java 26
preview); **Scoped Values** for request/auth context across virtual threads
(Java 25); **Module Import Declarations** to shrink generated imports (Java 25);
**Flexible Constructor Bodies** so records/config fail loudly at construction
(Java 25); **primitive patterns in `switch`** for union dispatch (Java 26
preview); and **Compact Source Files + instance `main`** for the generated
live-smoke entry point (Java 25).

| ID | Level | Requirement | Source | Status | Hrs 👤 (🤖) |
|---|---|---|---|---|---|
| TR-Java.1 | MUST | `oneOf`/polymorphic types as a sealed interface over record variants, dispatched by pattern-matching `switch` (primitive patterns where a discriminator is primitive, Java 26); unknown discriminators retained via a catch-all record; open enums as an `enum` plus an unknown-value carrier so round-tripping never drops an unrecognized value | §4 | ❌ | 60 (18) |
| TR-Java.2 | MUST | Tri-state optionality via an explicit wrapper (absent vs explicit-null vs value), since Java has no native distinction, validating in a flexible constructor body (Java 25) so a bad value fails at construction; unchecked `BoxApiException` error model carrying status + parsed body | §4 | ❌ | 40 (12) |
| TR-Java.3 | MUST | Immutable records for models; builder pattern for optional-heavy requests; a blocking API with an async (`CompletableFuture`) variant over `java.net.http`; module import declarations (Java 25) to keep generated imports compact | §4 | ❌ | 120 (36) |
| TR-Java.4 | MUST | `javac` clean under `-Xlint:all`; formatter-clean (`google-java-format` / Spotless) output | §4 | ❌ | 40 (12) |
| TR-Java.5 | MUST | Hand-written Java runtime: `java.net.http.HttpClient` networking (HTTP/3/QUIC where negotiable, Java 26) with retry/backoff + `401` refresh, Scoped-Value auth/request context, structured-concurrency chunked-upload fan-out, PEM-decoded JWT keys (no third-party crypto dep), streaming request/response bodies, multipart upload assembly; implemented against the FR-5 contract | FR-5, §4 | ❌ | 100 (30) |

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
| **v1 — Go SDK** 🐹 | Engine core (FR-1…FR-9, NF), verification harnesses (VR-1.1, VR-2…VR-7), TR-Go | Full real spec (base + 2025.0 + 2026.0) generates; `go build` + `go vet` + gofmt clean; per-node fixture suite green; generated per-manager tests compile and pass; reference docs generated for every manager plus the auth/pagination/errors guides; VR-3 conformance checklist covers the full R§1 contract; round-trip + determinism green; VR-7 live smoke green (one call per auth flow + upload/download/paginate); FR-9 spec-diff runs across the versioned specs; ship artifact: tagged Go module (NF-8) | 2,090 (630) | ~23% |
| **v2 — Apex SDK** ☁️ | TR-Apex, VR-1.3 harness | Full scratch-org deploy validation green **including generated tests** (75% coverage gate); conformance parity with v1 minus manifest-documented platform exclusions, each recorded in `DECISIONS.md`; VR-7 live smoke green from a scratch org; packaging decision recorded and ship artifact produced (NF-8) | 640 (194) | 0% |
| **v3 — Rust SDK** 🦀 | TR-Rust, VR-1.2 harness | `cargo check` + `clippy` + rustfmt clean; conformance parity with v1; round-trip suite green incl. unknown-discriminator retention; generated tests pass and docs generated (same bar as v1); VR-7 live smoke green; `cargo publish --dry-run` clean (NF-8) | 380 (114) | 0% |
| **v4 — TypeScript SDK** 🟦 | TR-TypeScript, VR-1.5 harness | `tsc --noEmit` clean under `strict` + Prettier-clean; conformance parity with v1; round-trip suite green incl. unknown-discriminator retention; generated tests pass and docs generated (same bar as v1); VR-7 live smoke green; `npm publish --dry-run` clean + shipped `.d.ts`/dual ESM-CJS, with a package-import smoke loading the built package through **both** its ESM (`import`) and CJS (`require`) entry points (NF-8) | 320 (96) | 0% |
| **v5 — Java SDK** ☕ | TR-Java, VR-1.6 harness | `javac` clean under `-Xlint:all` + formatter-clean (`google-java-format` / Spotless); conformance parity with v1; round-trip suite green incl. unknown-discriminator retention; generated tests pass and docs generated (same bar as v1); VR-7 live smoke green; Maven Central publish dry-run clean with shipped sources + Javadoc JARs (NF-8) | 360 (108) | 0% |
| **TOTAL** | | | **3,790 (1,142)** | **~14%** |

> The release rows decompose exactly: v1 = FR-1…FR-9 + NF + TR-Go +
> VR minus {VR-1.2, VR-1.3, VR-1.5, VR-1.6}; v2 = TR-Apex + VR-1.3; v3 = TR-Rust +
> VR-1.2; v4 = TR-TypeScript + VR-1.5; v5 = TR-Java + VR-1.6. The VR-7 live-smoke
> harness is built in v1 and reused (with per-target entries) in v2, v3, v4, and v5.
