# 📈 box-gantry — Progress

Effort-weighted progress toward **four shipped SDKs** (Go, Apex, Rust,
TypeScript) from one Rust engine. Weights are the human-equivalent hour
estimates from [`NEW_ENGINE_REQUIREMENTS.md`](./NEW_ENGINE_REQUIREMENTS.md)
(agent-hours in parentheses); status is tracked in [`PLAN.md`](./PLAN.md). Last
updated **2026-07-16**.

## Overall

```text
████████▊░  ~88%
```

**~88% complete by effort** — **v1 (Go SDK) is shipped**; v2 (Apex) is
essentially complete (models, native-JSON, managers/client, serialization
remap, server auth, generated tests, **VR-1.3 green** on-platform, **VR-1.4**
live smoke, **VR-3 conformance parity** (D-141), and the **NF-8 ship artifact**
— an unlocked 2GP package, defined with its CI build job wired (D-142)); v3 (Rust) and v4
(TypeScript) remain. TypeScript was added as a fourth target (D-143), which
expands total scope — so the overall figure steps down from ~84% to ~78% by
effort even though **no work was lost**. The large shared cost — the whole
engine core (ingestion, typed IR, semantics, manifest, runtime contract) and
the verification harnesses — landed with v1 and is **reused unchanged** by
v2/v3/v4, so the remainder is mostly target-specific backends + runtimes.

| | Human-equiv hrs | Agent-hrs | Complete |
|---|---:|---:|---:|
| **v1 — Go** 🐹 | 2,090 | 630 | **100%** |
| v2 — Apex ☁️ | 640 | 194 | ~95% |
| v3 — Rust 🦀 | 380 | 132 | ~72% |
| v4 — TypeScript 🟦 | 320 | 96 | ~15% |
| **Total** | **3,430** | **1,052** | **~88%** |

## By SDK target

```text
v1 — Go 🐹    ██████████  100%   SHIPPED 2026-07-13
v2 — Apex ☁️  █████████▓   ~95%   near-shipped (M4) — models, SFDX loop, JSON, managers, VR-1.3 green; naming overhaul (immediate-context, short methods, dedupe → 991 classes); SFDX scaffolding + ApexDoc + per-endpoint docs (D-129); hand-written runtime — callable SDK (D-130); governor-limit-aware pagination — documented, no extra classes (D-131); field↔wire serialization remap + union variants (D-132); generated @isTest suite for the 75% gate (D-133); CCG + JWT server-to-server auth (D-134/D-135); chunked-upload orchestrator → 1,085 classes (D-136); HttpCalloutMock coverage for the HTTP client → 1,086 classes (D-137); explicit-null (absent-vs-null) serialization via `fieldsToNull` (D-138); live smoke vs real Box (VR-1.4) + shipped Remote Site Settings (D-139); Object-field deserialization for unions/free-form JSON (D-140); target-neutral VR-3 conformance green in CI + spec-fingerprint traceability + cross-cutting docs guides → 1,087 classes (D-141); NF-8 ship artifact — unlocked 2GP package under the `unbox` namespace, defined + packaging flow documented, and the CI package-build job wired (`apex-package.yml`: create/resolve → `sf package version create` → `04t` id) (D-142); RSS source-format suffix + suppress-nulls test corrected under the strict packaging path (D-144/D-145); generated wire-hook test suite lifting on-platform coverage **56% → 99%** (D-146) → 1,087 → 1,091 classes; **promotable version minted on-platform — `04tNS000000UGfFYAW` v0.1.0.2, all namespace tests green, `HasPassedCodeCoverageCheck: true`**. NF-8 complete end-to-end.
v3 — Rust 🦀  ███████▏░░    ~72%   underway (M5) — model layer: one Rust module per IR module (collision-safe across API versions, D-147); serde structs, `Option<Option<T>>` tri-state via `double_option`, open enums as `String` newtypes + consts, closed enums, aliases; **typed discriminated unions** — hand-written `Serialize`/`Deserialize` dispatching on the tag, open-union `Unknown(Value)` retention, closed-union rejection (TR-Rust.1, D-148); **async managers/client** — one `<Name>Manager` per tag, one `async fn` per operation routing only through the runtime contract, `Client` entry point over a shared session, options structs for optional params, + the contract's Rust stub renderer so the SDK compiles without the real runtime (FR-5.3, TR-Rust.3, D-149); `generate --target rust` wired; **hand-written `reqwest`/`tokio` runtime** (`runtimes/rust/gantryruntime`, a standalone crate) — async `fetch` with idempotency-gated retries + exponential backoff/single-flight 401-refresh/Retry-After, `with_*` builders + response accessors; **all four Box auth flows** (developer-token, CCG, OAuth refresh with a durable async token store, and **JWT** signing-key server auth — encrypted-PKCS#8 key parsing + hand-signed RS256 assertions); generated SDK compiles against it (FR-5.2/TR-Rust.5, D-150/D-151); **live smoke (VR-7)** against a real Box account (`#[ignore]`d, wired into `livesmoke.yml`). **`verify --target rust`** runs the VR-1.2 toolchain gate (rustfmt + `cargo check` + clippy) as a first-class CI signal, and **`conform --target rust`** measures the R§1 checklist (a progress report — managers/operations/serialization/traceability pass; manager-docs, docs-guides, auth-flows (all docs-derived), round-trip-tests, and pagination pending — D-152). rustfmt-clean by construction + VR-1.2 on the full 185-file SDK + union round-trip (VR-4). **typed date/time** via `chrono` (D-153); **pagination** — a per-operation async `<Manager><Method>Paginator` (dependency-free `async fn next`) threading the marker/offset cursor, mirroring Go's `iter.Seq2` iterators (D-154), so `conform --target rust` now passes pagination 64/64 + operations 336/336. **Reference docs** — a `docs/` tree generated from the same IR as the code (per-manager pages with parameter tables + typed returns, an index, and the auth/pagination/errors guides with real Rust call sites), mirroring the Go backend's `docs.rs` (D-155), flipping manager-docs 85/85 + docs-guides 4/4 + auth-flows 4/4. **Generated round-trip / behavioral tests** — inline `#[cfg(test)]` modules that compile *and pass* under `cargo test`: the tri-state (absent/null/value via `double_option`) + typed `chrono` date/time, and one per-union known/unknown discriminator-dispatch test generated from the IR (27 tests on the real spec); the VR-1.2 gate learned to run `clippy --all-targets` + `cargo test` so a broken generated test fails CI (D-156). With that, **`conform --target rust` reads 9/9, 0 failing, and joins the CI release gate** alongside Go and Apex — full capability parity with the Go reference. Remaining: the NF-8 Rust ship artifact (publishable crate + release wiring). **M6 — TypeScript (v4) is now underway** (D-157)
v4 — TypeScript 🟦  █▌░░░░░░░░   ~15%   underway (M6) — **model layer**: one `.ts` module per IR module (collision-safe across API versions), structs → `export interface` with the **tri-state mapped straight onto the type system** (absent → `field?: T`, null → `T | null`, no wrapper — TR-TS.2); **`oneOf` → discriminated unions** of the variant interfaces + a `{ [key: string]: unknown }` catch-all for open unions; **open enums → string-literal unions widened with `(string & {})`**; aliases → `export type`; cross-module `import type` (NodeNext ESM) + a namespaced barrel; provenance `buildinfo` (NF-7) (TR-TS.1/.2, D-157). **`generate --target typescript`** wired; the **VR-1.5 gate** — the TypeScript 7 native (Go-ported) compiler, `tsc --noEmit` under `strict` — type-checks the full real-spec package clean, wired as `verify --target typescript` + a CI step + a backend `compile_output` test. Next: `Promise`-based managers/client + the `fetch` runtime (TR-TS.3/.5), then docs/tests + `conform --target typescript`
```

## By milestone

| Milestone | Scope | Status |
|---|---|---|
| **M0** — Bootstrap | workspace, pinned toolchain, CI, vendored specs, docs regime | ✅ 2026-07-11 |
| **M1** — Ingestion + IR (FR-1, FR-2) | multi-doc load, naming rules, typed IR, lowering | ✅ |
| **M2** — Semantics, manifest, contract (FR-3–FR-5) | one semantic pass, per-language manifests, runtime contract + drift check | ✅ |
| **M3** — Go backend, runtime, verification (FR-6–FR-9, TR-Go, VR) | lowering + printer, feature synthesis, Go runtime, full VR suite, CLI | ✅ 2026-07-13 |
| **M3.5** — Apex spike | throwaway lowering to de-risk the IR for Apex (D-108) | ✅ |
| **M4** — Apex backend + scratch-org harness (TR-Apex, VR-1.3) | flat-namespace lowering, no-generics, governor limits, Apex runtime, 75% test gate | 🔄 ~95% — models + SFDX loop + native JSON + managers/client (D-120–123); **VR-1.3 green** on-platform (D-124); immediate-context naming + dedupe (D-125–127); runtime + docs (D-129/130); pagination (D-131); serialization remap + unions (D-132); @isTest suite (D-133); CCG/JWT auth (D-134/135); chunked upload (D-136); mock coverage (D-137); explicit-null (D-138); **VR-1.4 live smoke** + Remote Site Settings (D-139); Object-field deserialization (D-140); **VR-3 conformance parity** + traceability + guides → 1,087 classes (D-141); **NF-8** unlocked-2GP ship artifact defined + CI build job (D-142); **promotable version minted on-platform** — wire-hook test suite lifted coverage **56% → 99%** (`HasPassedCodeCoverageCheck: true`), `04tNS000000UGfFYAW` v0.1.0.2, all tests green (D-144/D-145/D-146) → 1,091 classes. **NF-8 complete end-to-end.** |
| **M5** — Rust backend + runtime (TR-Rust, VR-1.2) | serde-tagged enums, `Result`/`Option`, async reqwest/tokio runtime | 🔄 ~72% — model layer (D-147); **typed discriminated unions** with tag-dispatching serde + unknown retention (TR-Rust.1, D-148); **async managers/client** routing only through the runtime contract + the contract's Rust stub renderer (FR-5.3, TR-Rust.3, D-149), `generate --target rust` wired; **hand-written `reqwest`/`tokio` runtime** — standalone crate, async `fetch` (idempotency-gated retries/backoff/401-refresh/Retry-After) + `with_*` builders + **all four auth flows** (developer-token/CCG/OAuth-with-durable-store/**JWT**); generated SDK compiles against it (FR-5.2/TR-Rust.5, D-150/D-151); **live smoke (VR-7)** vs real Box wired into `livesmoke.yml`; **`verify --target rust`** VR-1.2 CLI gate wired into CI + **`conform --target rust`** progress checklist (D-152); **typed date/time** via `chrono` (D-153); **pagination** — async paginators threading the marker/offset cursor (D-154), `conform --target rust` now 64/64 paged + 336/336 ops; **reference docs** — per-manager pages + auth/pagination/errors guides generated from the IR (D-155); **generated round-trip tests** — tri-state + typed date/time + per-union discriminator dispatch, run under `cargo test` in the VR-1.2 gate (D-156), so **`conform --target rust` is 9/9 and now gates CI** alongside Go/Apex — full capability parity; rustfmt-clean by construction + VR-1.2 gate on the full 187-file SDK + VR-4 union round-trip. Next: NF-8 Rust ship artifact |
| **M6** — TypeScript backend + runtime (TR-TypeScript, VR-1.5) | discriminated-union lowering, `?:`/`\| null` tri-state, `Promise`-based fetch runtime, TS 7 (Go-native) `tsc` gate | 🔄 ~15% — **model layer** (D-157): one `.ts` module per IR module, structs → `interface` with the tri-state as `?:`/`\| null` (no wrapper, TR-TS.2), `oneOf` → discriminated unions + open-union catch-all, open enums → `(string & {})`-widened literal unions (TR-TS.1), cross-module `import type` + namespaced barrel, provenance; **`generate --target typescript`** wired + the **VR-1.5 gate** (`tsc --noEmit` under `strict`, the TS 7 native compiler) green on the full spec, wired as `verify --target typescript` + CI + a backend compile test. Next: `Promise`/`fetch` managers + runtime (TR-TS.3/.5) |

## v1 (Go SDK) — requirement coverage

Every R§7 v1 acceptance criterion is met. Detail in
[`DECISIONS.md`](./DECISIONS.md) (D-101…D-119).

```text
Ingestion + IR (FR-1/2)     ██████████  100%   loud-fail load, typed IR, lowering
Semantics (FR-3)            ██████████  100%   one pass; refs bound, types well-formed
Manifest + contract (FR-4/5)██████████  100%   Go manifest frozen; contract + drift gate
Feature synthesis (FR-7)    ██████████  100%   managers, 4 auth flows, pagination, uploads, tests, docs
Determinism/traceability    ██████████  100%   byte-identical output; spec fingerprint + engine version (NF-7)
CLI (FR-8)                  ██████████  100%   check / generate / verify / conform / diff
Spec-diff (FR-9)            ██████████  100%   IR-level breaking-change report → SDK bump
Go backend + runtime (TR-Go)██████████  100%   compile-clean output; hand-written runtime
Verification (VR-1…VR-7)    ██████████  100%   compile loop, node fixtures, conformance, round-trip, determinism, live smoke
Ship artifact (NF-8)        ██████████  100%   tagged Go-module source; version/tag from FR-9 diff
```

### Verification harnesses (VR)

| Signal | What it proves | Status |
|---|---|---|
| VR-1.1 | full spec generates → `go build`/`vet`/gofmt clean (primary CI signal) | ✅ |
| VR-2 | per-node lowering fixtures (IR fragment → expected Go) | ✅ 17 cases |
| VR-3 | R§1 capability conformance checklist (managers/ops/paged/auth) | ✅ 9/9 |
| VR-4 | round-trip suite (tri-state + union dispatch) + generated tests | ✅ |
| VR-5 | determinism double-generate diff | ✅ |
| VR-6 | skip/fallback summary vs allowlist | ✅ |
| VR-7 | **live smoke vs a real Box account** (auth + paginate + upload/download) | ✅ green 2026-07-13 |

## What's next

**M4 — Apex (v2) is ~95% and effectively complete:** full backend + runtime,
VR-1.3 on-platform + VR-1.4 live smoke green, VR-3 conformance parity (D-141),
and the NF-8 unlocked-2GP ship artifact built end-to-end (D-142). The
`apex-package.yml` dispatch mints a **promotable** version on-platform —
`04tNS000000UGfFYAW` (v0.1.0.2), all namespace tests green, on-platform code
coverage **99%** (`HasPassedCodeCoverageCheck: true`). Getting there cleared four
blockers the strict packaging path surfaced that the lenient deploy never did:
the RSS source-format suffix (D-144), the suppress-nulls test premise (D-145),
and the 56%→99% coverage gap via a generated wire-hook test suite (D-146). The
version now clears the 75% gate to promote from beta to `released`.

**M5 — Rust (v3) is underway.** Slice 1 landed the model layer (D-147): a new
`gantry-backend-rust` crate emitting a self-contained SDK crate — one Rust
module per IR module (names collide across API versions, so they can't share a
namespace), serde structs with `snake_case` fields + wire renames, the
`Option<Option<T>>` tri-state, open enums as `String` newtypes and closed enums
as real `enum`s, aliases, and `buildinfo` provenance. Slice 2 added **typed
discriminated unions** (TR-Rust.1, D-148): hand-written `Serialize`/
`Deserialize` that dispatch on the discriminator (serde's own tagging would
double-emit the tag the variant already carries), with open unions retaining an
unrecognized tag in `Unknown(serde_json::Value)` and closed unions rejecting it;
structural unions stay a `Value` newtype. Slice 3 added the **async
managers/client** (TR-Rust.3, D-149): one `<Name>Manager` per `x-box-tag`, one
`async fn` per operation that builds the URL from structured path segments,
applies params, encodes the body, `fetch`es through a shared session, and
decodes by response shape — reaching the network *only* through the runtime
contract (FR-5.2). The contract crate gained a Rust stub renderer
(`rust_stubs.rs`) so the whole SDK compiles against the declared surface without
the real runtime yet (FR-5.3); `generate --target rust` is wired. It's
rustfmt-clean by construction and passes `cargo check` + `clippy -D warnings` on
the full 185-file SDK (VR-1.2) plus a generated-union round-trip (VR-4), enforced
by backend tests that run the real toolchain. Slice 4 added the **hand-written
`reqwest`/`tokio` runtime** (TR-Rust.5, D-150): a standalone crate
(`runtimes/rust/gantryruntime`, its own workspace so the async stack stays out
of the engine — mirroring the Go runtime's separate `go.mod`) implementing the
contract for real — an async `fetch` with jittered backoff, a single 401
refresh, and Retry-After; the `with_*` builders and `response_*` accessors; and
three auth flows (developer-token, CCG, OAuth refresh, exchanged tokens cached
to expiry). It's verified two ways, like Go: fmt/clippy/tested standalone in CI,
and — the real check — the generated SDK is compiled against it (a backend test
swaps the stub `runtime.rs` for a re-export of the crate and `cargo check`s the
whole SDK + a smoke example), proving no drift between stub and reality
(FR-5.2). Slice 5 (TR-Rust.5, D-151) completed the runtime's auth surface with
**JWT server auth** — the app's `box_config.json` RSA key parsed up front
(encrypted PKCS#8 included) and a hand-signed, single-use RS256 bearer assertion
per token exchange — and added the **live smoke (VR-7)**: an `#[ignore]`d
integration test that drives the stable runtime contract against a real Box
account (one call per auth flow + paginate/upload/download/delete), wired into
the manual `livesmoke.yml` alongside Go. The runtime is now feature-complete for
all four Box auth flows. A CLI-verification slice (D-152) then brought Rust to
parity with Go/Apex on the tooling: **`verify --target rust`** runs the VR-1.2
toolchain gate (rustfmt + `cargo check` + clippy) on the generated SDK and is
wired into CI as a first-class signal, and **`conform --target rust`** (a new
`rust_shape`) measures the R§1 checklist — managers/operations/serialization/
traceability pass today; the shortfalls are an honest progress report (no
faked exclusions) that joins the release gate once they land. Then a small slice typed
`Date`/`DateTime` fields via `chrono` (D-153), and **pagination** as async
paginators threading the marker/offset cursor (D-154) — closing the pagination +
operations capabilities. A **reference-docs** slice (D-155) then ports the Go
backend's `docs.rs`: a `docs/` tree generated from the same IR as the code —
per-manager pages (parameter tables, chrono-typed returns, `_paginate` notes),
an index, and the auth/pagination/errors guides with real Rust call sites —
flipping manager-docs (85/85), docs-guides (4/4), and the auth-flow surface
(4/4). Finally a **round-trip-tests** slice (D-156) generates inline
`#[cfg(test)]` modules that compile *and pass* under `cargo test` — the tri-state
and typed `chrono` date/time, plus one per-union known/unknown
discriminator-dispatch test from the IR — and teaches the VR-1.2 gate to run
`clippy --all-targets` + `cargo test`. With the last capability green,
**`conform --target rust` reads 9/9 and now gates CI** alongside Go and Apex:
full Rust capability parity with the Go reference. What remains for v3 is the
NF-8 ship artifact (a publishable crate + release wiring), as Go and Apex have.
**Then M6 — TypeScript (v4)**
(D-143): discriminated-union lowering, `?:`/`| null`
tri-state, a `Promise`-based `fetch` runtime, and the TypeScript 7 Go-native
`tsc --noEmit` gate (VR-1.5). The three-target IR (FR-2) was built to absorb new
targets through manifest + lowering + printer alone — v4 is the first test of
that beyond the original three.

**Calendar (assessment §8 + D-143):** ~11–15.5 months to four shipped SDKs
(v1 ~6–8 done, v2 ~2.5–3.5, v3 ~1–2, v4 TypeScript ~1–2).
