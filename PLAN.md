# 🗺️ box-gantry — Execution Plan

The build plan for **box-gantry**: a Rust engine that consumes the Box
OpenAPI specification and generates Box SDKs — **Go** (v1), **Salesforce
Apex** (v2), **Rust** (v3).

- Normative spec: [`NEW_ENGINE_REQUIREMENTS.md`](./NEW_ENGINE_REQUIREMENTS.md) (R§n)
- Rationale: [`REWRITE_ASSESSMENT.md`](./REWRITE_ASSESSMENT.md) (§n)
- This file is the PLAN quarter of the ISSUES/DECISIONS/PLAN/SCOPE docs
  regime (NF-5), maintained as milestones move.

## 🧭 Ground rules (day one, non-negotiable)

1. **Typed IR, no string-encoded semantics** (FR-2). Closed Rust enums;
   adding a node kind is a compile error in every lowering that misses it.
2. **One semantic pass before any backend runs** (FR-3). Backends receive
   verified programs only.
3. **Capability manifest per language** (FR-4). No language-name
   comparisons anywhere in feature synthesis.
4. **Machine-checked runtime contract** (FR-5). Generation fails on
   signature drift.
5. **Compile-the-output is the primary CI signal** for every backend from
   its first week (VR-1.x). Snapshot/fixture tests are change-detection
   aids, not the oracle.
6. **Deterministic, loud, and traceable** (FR-6.2, NF-1, NF-7): identical
   inputs → byte-identical output; nothing skipped silently; every artifact
   embeds spec hash + engine version.
7. **box-codegen is consult-only** (D-102). Its designs, lessons, and test
   semantics inform the work; its code is never ported verbatim and its
   output is never an acceptance oracle. Correctness = the R§1 capability
   contract + each language's toolchain + round-trip and live verification.
8. **Docs regime from day one**: [`ISSUES.md`](./ISSUES.md),
   [`DECISIONS.md`](./DECISIONS.md), this file, and [`SCOPE.md`](./SCOPE.md)
   stay current; every irreversible choice gets a decision record.

## 🏛️ Workspace layout

A single Cargo workspace; crate boundaries mirror the architecture
(assessment §3) so requirement areas map one-to-one onto crates:

| Crate | Responsibility | Requirements |
|---|---|---|
| `gantry-spec` | OpenAPI ingestion, multi-doc merge, naming rules, Box quirks | FR-1 |
| `gantry-ir` | Typed IR node set | FR-2 |
| `gantry-sema` | Semantic analysis, type environment, diagnostics | FR-3 |
| `gantry-manifest` | Per-language capability manifests | FR-4 |
| `gantry-contract` | Runtime-contract definitions + drift checking; per-target stubs | FR-5 |
| `gantry-synth` | Feature synthesis: managers, auth, pagination, uploads, tests, docs | FR-7 |
| `gantry-backend-go` / `-apex` / `-rust` | Lowering + printers | FR-6, TR-* |
| `gantry-cli` | Thin driver: `generate` / `check` / `verify` | FR-8 |
| `gantry-verify` | Conformance checklist, spec-diff, determinism check, harness glue | VR-*, FR-9 |
| `runtimes/{go,apex,rust}` | Hand-written runtimes shipped with each generated SDK | TR-Go.7, TR-Apex.6, TR-Rust.5 |

## 🪜 Milestones

Calendar figures assume agent-driven delivery (~938 agent-hours total per
the requirements roll-up) and are re-baselined at every milestone exit —
the ~3× throughput multiplier is a single data point from the box-codegen
Go target (assessment §8).

### M0 — Bootstrap (week 1) — ✅ complete 2026-07-11

Scaffold the workspace; pin the toolchain (`rust-toolchain.toml`, locked
deps — NF-6); CI running `cargo check` + `clippy` + `rustfmt` + tests;
vendor the Box OpenAPI specs (base + `2025.0` + `2026.0`) as fixtures;
docs regime seeded.
**Exit:** CI green on the empty workspace; specs vendored; ISSUES /
DECISIONS / SCOPE live.

### M1 — Ingestion + IR (FR-1, FR-2) — ~weeks 2–8 — 🔄 in progress

Done so far: loud-fail ingestion slice (FR-1.4 first, per the ground
rules) — multi-document version-aware loading, `operationId`/`x-box-tag`
invariants (D-104), JSON-path error reporting, `gantry check` over the
full real spec set green in CI; IR core drafted (identifier hygiene,
optionality constructor, rich unions, module concept, decl arena);
capability-manifest axes drafted for all three targets. Typed schema
model + lowering into the IR (D-105): the full real spec set lowers to
967 declarations with pinned stats — `allOf` wrapper/composition split,
`type`-const discriminator inference, open enums, synthesized inline
decls, versioned modules (`schemas::v2025_0`).

Multi-document, version-aware ingestion with naming rules inside the
ingestion layer (FR-1.2 — the `PostFolders` lesson); Box quirks represented
structurally; the closed IR node set designed against the three-target axes
(modules, optionality, unions, error model — assessment §4).
**Exit:** the full real spec ingests with zero errors and loud failures on
mutation (FR-1.4); IR dumps are deterministic; the three-target axes are
documented in `DECISIONS.md`.

### M2 — Semantics, manifest, runtime contract (FR-3, FR-4, FR-5) — ~weeks 8–14

One semantic pass to a complete type environment; requires/provides
validation if passes multiply; draft manifests for all three languages
(Apex axes included now, not in v2); contract format + drift check; Go
runtime stubs.
**Exit:** `gantry check` on the full spec runs in seconds (NF-2); every ref
bound, every expression typed; backends can be started against a verified
program.

### M3 — Go backend, runtime, verification (FR-6…FR-9, TR-Go, VR) — ✅ complete 2026-07-13

Lowering + printer; feature synthesis re-expressed against the IR (auth
flows, `iter.Seq2` pagination, chunked upload, `oneOf` variant structs,
generated tests and docs); hand-written Go runtime against the contract
(TR-Go.7); the G-1-style generate→`go build`→`go vet` loop in CI from this
milestone's **first week**; per-node fixtures; conformance checklist;
round-trip, determinism, live-smoke harness; spec-diff (FR-9); CLI.
**Exit = v1 acceptance criteria** (R§7), including generated tests + docs
and the tagged Go module ship artifact.

### M3.5 — Apex spike (timeboxed ~2 weeks, inside M1–M3)

Throwaway lowering of two or three representative managers (one paginated,
one `oneOf`-heavy, one upload) to Apex against the draft IR. Output is
discarded; the deliverable is the list of IR changes it forces, recorded as
decisions. This is the early-warning system for the assessment §8 primary
risk.

### 🚢 v1 ship — Go SDK

### M4 — Apex backend + runtime + scratch-org harness (TR-Apex, VR-1.3) — 🔄 in progress

Flat-namespace layout and name mangling; manifest-driven no-generics
lowering; governor-limit-aware pagination/upload/retry shapes;
`JSON.deserializeUntyped` dispatch; generated test classes clearing the 75%
gate; Apex runtime (`Http`, Crypto-based JWT, token storage); scratch-org
CI built in the **first week** of this milestone (tiered: syntax per
commit, full `sf project deploy validate` per merge).
**Exit = v2 acceptance criteria** (R§7), including the recorded packaging
decision.

### M5 — Rust backend + runtime (TR-Rust, VR-1.2) — ~1–2 months

Serde-tagged enums with unknown-discriminator retention; `Result`/`Option`
lowering; async-first runtime on `reqwest`+`tokio`; builders; `cargo
check`+`clippy`+`rustfmt` gates; `cargo publish --dry-run`.
**Exit = v3 acceptance criteria** (R§7).

**Calendar to four shipped SDKs: ~11–15.5 months** (v1 ~6–8, v2 ~2.5–3.5,
v3 ~1–2, v4 ~1–2), against ~3,430 human-equivalent hours (~21 engineer-months)
of effort. (TypeScript v4 added in D-143; the original three-SDK path was
~10–13.5 months / ~3,110 hrs.)

## 🔬 Verification cadence

| Signal | When | Gate |
|---|---|---|
| `cargo check`/`clippy`/tests (engine) | every commit | merge-blocking |
| Generate full spec → target toolchain compile (VR-1.x) | every commit once a backend exists | merge-blocking |
| Determinism double-generate diff (VR-5) | every commit | merge-blocking |
| Skip/fallback summary vs allowlist (VR-6) | every commit | merge-blocking |
| Conformance checklist report (VR-3) | every CI run | report + release-blocking |
| Round-trip suite (VR-4) | every commit | merge-blocking |
| Apex scratch-org validate (VR-1.3) | per merge (M4+) | merge-blocking |
| Apex live smoke vs real Box (VR-1.4) | per release + on demand | release-blocking |
| Live smoke vs Box dev account (VR-7, Go) | per release + on demand | release-blocking |
| Spec-diff report (FR-9) | per spec update | informs SDK version bump |

## ⚠️ Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| Apex constraints leak into the IR | Target-shaped debt engine-wide (assessment §8) | IR models rich concepts; backends lower; spike output reviewed for IR pressure, not accommodated silently |
| Apex-readiness of the IR discovered late | IR rework during v2 | M3.5 spike during engine development |
| Scratch-org CI operability (Dev Hub auth, org limits, flakiness) | v2 schedule slip | Harness in M4 week one; tiered checking; cache orgs where possible |
| 3× agent throughput extrapolated from one target | Schedule slip everywhere | Re-baseline hours at each milestone exit; roll-up updated in the requirements doc |
| Box spec drift/quality during the build | Rework, churning fixtures | Specs vendored and pinned per milestone; FR-9 diff report on every spec bump |
| Real-spec edge cases exceed the IR design (`oneOf`, extensible enums) | IR churn late | Ingest the full real spec from M1 — never develop against toy fixtures alone |

## 🤝 Working agreements

- Every irreversible choice → `DECISIONS.md` entry (numbering starts at
  D-101; D-001…D-013 refer to box-codegen's records).
- Every engine defect found by verification → `ISSUES.md` entry (BG-n),
  with the fix class noted so it can become a lint or a type.
- Weekly: regenerate the full spec, review the conformance-checklist delta
  and the skip summary.
- No silent caps, ever: if generation bounds anything (top-N, fallback,
  skip), it appears in the run summary and CI asserts the allowlist (VR-6).

## ⏭️ Immediate next steps

1. ~~M0 scaffolding: workspace, pinned toolchain, CI, license/readme.~~ ✅
2. ~~Vendor the Box OpenAPI specs (base + 2025.0 + 2026.0) under
   `fixtures/specs/`.~~ ✅
3. ~~Begin `gantry-spec` ingestion against the vendored base spec,
   loud-fail first (FR-1.4 before FR-1.1 breadth).~~ ✅ (first slice)
4. ~~Typed schema model in `gantry-spec`: `$ref` graph, `oneOf`/`allOf`,
   open enums — lowered into the IR `Program`.~~ ✅ (D-105; schemas done —
   parameters and request/response bodies still open)
5. ~~Operations into the IR: parameters, request/response bodies, binary
   responses, base-URL mapping (FR-1.3 remainder).~~ ✅ (D-106; all 336
   real operations lower with classified shapes)
6. Remaining naming rules in the ingestion layer (FR-1.2): idiomatic
   operation/manager/schema naming and casing policy (`#variation` and
   version-suffix translation are done); tri-state null-vs-absent
   optionality decision (D-105 consequence).
7. ~~Semantic pass (FR-3) over the now-complete Program: every ref bound,
   every type well-formed.~~ ✅ (`gantry-sema`: one pass, collects all
   findings, engine-bug vs spec-error classes drive FR-8.3 exit codes;
   manager index as the queryable analysis product — 85 managers on the
   real spec set)
8. ~~Freeze the IR node set as a decision~~ ✅ (D-107) ~~; then the M3.5
   Apex spike~~ ✅ (D-108: 85/85 managers lower, zero IR changes forced;
   name-length + tri-state findings feed the naming layer)
9. ~~M2 remainder: manifest freeze + runtime contract format (FR-4,
   FR-5).~~ ✅ (D-109: Go manifest frozen; contract v1 as data; Go stubs
   rendered from the contract and compile-gated with go build/vet +
   gofmt in tests and CI)
10. ~~Decide the null-vs-absent tri-state.~~ ✅ (D-110:
    `Optional<Nullable<T>>`; synthesized-name shortening included)
11. ~~M3 begins: Go backend — model lowering + printer (TR-Go.1/2/5),
    compile-the-output loop from week one (VR-1.1).~~ ✅ first slice:
    all 1,332 declarations generate as Go (structs + tags, open enums,
    union variant structs with generated marshal/unmarshal, aliases);
    `gantry generate`/`verify` live (FR-8.1); `go build`+`vet`+gofmt
    clean on the full real spec, in tests and CI. The loop caught four
    real defects on first contact (`User--Mini` names, `$`-prefixed
    metadata keys, `:append` custom methods, union-field padding).
12. ~~M3: managers + client (FR-7.1), URL/query building against the
    runtime contract.~~ ✅ All 85 managers + the client entry point
    generate and compile against the contract stubs (FR-5.2/5.3 closed
    loop): structured-path URL building, typed query/header conversion
    (incl. JSON-in-query for `mdfilters`-class params), JSON / stream /
    multipart / form bodies, response decoding per shape. `gantry
    verify` covers the whole 92-file SDK tree in CI.
13. ~~M3: pagination (`iter.Seq2`, TR-Go.4).~~ ✅ (D-111: gantry-synth
    detects marker/offset pagination language-agnostically; Go lowers 64
    iterators, compile-gated. The synth layer is now established for
    Apex/Rust to reuse.)
14. ~~M3: the serialization package (BG-1 tri-state wrapper, `Date`).~~
    ✅ (D-112: generic `Nullable[T]` + `Date`; 412 tri-state + 3 Date
    sites round-trip; BG-1 resolved.)
15. ~~Generated reference docs (FR-7.7).~~ ✅ per-manager Markdown (85
    pages) + index + auth/pagination/errors guides, from the same IR so
    they can't drift; every manager's page verified present and linked.
16. ~~Client/session threading + `With*` decorators (G-3, FR-7.1) + the
    hand-written Go runtime (TR-Go.7).~~ ✅ (D-113: session-receiver
    contract axis; managers hold a shared session; retrying runtime with
    auth/backoff/Retry-After; generated SDK compiles against the real
    runtime + a smoke main — FR-5.2 conformance by construction.)
17. ~~Generated round-trip tests (FR-7.8, VR-4).~~ ✅ per-module union
    tests (known/unknown discriminator dispatch on real generated types)
    + serialization tri-state/Date tests; `go test` now runs in the
    compile gate and passes.
18. ~~Remaining auth flows (CCG/JWT/OAuth).~~ ✅ (D-114: all three
    land in the hand-written runtime as `TokenSource`s — CCG, OAuth
    authorization-code with refresh-token rotation, and stdlib-only
    RS256 JWT with encrypted-PEM key support. A runtime `auth_test.go`
    exercises every flow against an `httptest` token endpoint; CI now
    runs `go test ./...` on the runtime; the generated auth guide
    documents all four flows.)
19. ~~FR-9 spec-diff.~~ ✅ (D-115: `gantry-verify::diff` diffs two
    verified IR `Program`s, classifying every difference as breaking
    (removals, type changes, new required params → major) or compatible
    (additions, deprecation → minor); cross-program type identity is by
    structural signature. `gantry diff --from … --to …` prints the report
    and exits 4 on a breaking diff so CI can gate a major bump; an
    integration test proves the `2025.0` overlay is additive and its
    removal breaking.)
20. ~~VR-3 conformance checklist.~~ ✅ (D-116: `gantry-verify::conformance`
    derives the expected R§1 surface from the verified program (managers,
    operations, paginated surfaces) and measures the generated output,
    capability by capability — 85 managers, 336 operations, 64 paginators,
    4 auth flows, plus serialization/tests/docs. `gantry conform --target
    go` exits 4 on any shortfall; CI runs it every build. Target-neutral
    (reads a `GeneratedView`), so Apex/Rust reuse it.)
21. ~~Build provenance (NF-7) + Go-module ship artifact (NF-8).~~ ✅
    (D-117: `SpecSet::fingerprint()` — a dependency-free FNV-1a hash of the
    input specs — plus the engine version are threaded via `BuildInfo`
    into every model header and a generated `buildinfo` package
    (`EngineVersion`/`SpecFingerprint`); VR-3 gains a `traceability`
    capability. The generated tree is the NF-8 artifact: a self-contained
    Go module, tag/version set by the release pipeline from the FR-9
    diff.)
22. ~~VR-2 per-node lowering fixtures.~~ ✅ (D-118: `node_fixtures.rs` —
    17 cases, one IR node kind (and quirk) at a time, asserting the exact
    Go each rule produces (tri-state shapes, Date/DateTime/Binary/JSON
    scalars, containers, open enums, union dispatch + unknown retention,
    aliases). Semantic assertions, alignment-insensitive; they pinpoint
    which rule regressed where VR-1.1 only says the spec stopped building.)
23. ~~VR-7 live smoke.~~ ✅ (D-119: `livesmoke_test.go` in the committed
    runtime, build-tagged `//go:build live` so the per-commit gate never
    runs it; drives the stable runtime contract for one call per auth flow
    (Developer/CCG/OAuth/JWT) + paginate + upload/download/delete;
    credential-gated (`t.Skip` when unset); a manual `workflow_dispatch`
    CI job runs it from repo secrets on demand. **Green 2026-07-13** — the
    CCG `workflow_dispatch` run passed against the live Box account
    (`PASS: TestLiveSmoke (3.33s)`): auth → paginate 20 items → upload →
    download byte-compare → delete.)

## ✅ v1 — Go SDK: SHIPPED (2026-07-13)

**Every R§7 v1 acceptance criterion is met.** Full spec (base + 2025.0 +
2026.0) generates and builds/vets/gofmt clean (VR-1.1); per-node fixtures
(VR-2); conformance checklist covering the full R§1 contract (VR-3);
round-trip + generated per-manager tests that compile and pass (VR-4);
determinism (VR-5) + skip allowlist (VR-6); FR-9 spec-diff across the
versioned specs; reference docs for every manager + auth/pagination/errors
guides; the four auth flows; provenance + tagged Go-module artifact
(NF-7/8); and **VR-7 live smoke green against a real account**.

### 🔄 M4 — Apex backend (v2), in progress

The engine's three-target IR was designed for this; the M3.5 spike (D-108)
already proved it (85/85 managers lower, zero IR changes forced).

24. ~~M4 opens: the Apex model layer.~~ ✅ (D-120: `gantry-backend-apex`
    lowers every schema to a top-level `.cls` — flat-namespace mangling to
    the 40-char limit, tri-state erasure, open-enum-as-String-class,
    `JSON.deserializeUntyped` union dispatch. The full real spec →
    **1,330 classes**, deterministic, all names unique and ≤ 40 chars, 23
    dispatches; 10 structural + determinism tests (VR-2-equivalent).)
25. ~~SFDX packaging + the scratch-org compile loop (VR-1.3).~~ ✅ (D-121:
    `generate()` emits a deployable SFDX project — `sfdx-project.json` +
    per-class `.cls` + `-meta.xml` under `force-app/main/default/classes/`;
    `gantry generate --target apex` wired; a manual `apex-scratch.yml`
    dry-run-deploys to a fresh scratch org, gated on the `SFDX_AUTH_URL`
    Dev Hub secret like the VR-7 live smoke. Real spec → 2,661 files.)
    **VR-1.3 GREEN 2026-07-13** — the Salesforce platform compiler
    validated all **1,419** generated classes (`sf project deploy start
    --dry-run` → "Dry-run complete", run `29221112203`). The compile loop
    surfaced real generator bugs no unit test could (Apex's letter-terminated
    identifiers, case-insensitive names, reserved words `sort`/`float`/`by`/
    `commit`, a reserved `Group` class) — D-124; each fixed with a regression
    test.)
26. ~~Native JSON round-trip for models.~~ ✅ (D-122: enum-typed fields →
    `String` (the enum class is a constants namespace), union-typed →
    `Object` (caller dispatches via `<Union>.parse`), struct/scalar/List/
    Map already native — so a struct round-trips through plain
    `JSON.deserialize`. Real spec: `FileMini.type` now `String`, no stray
    `value` fields, 1,330 classes / 23 dispatches unchanged.)
27. ~~Managers + client + runtime contract stubs.~~ ✅ (D-123: one
    `Box<Manager>` class per tag with a method per operation (request built
    structurally, response deserialized per D-122), the `Box` client
    wiring them, and `BoxRequest`/`BoxResponse`/`BoxClient` stubs — the Go
    `gantryruntime` pattern. Real spec: 85 managers + client + 3 stubs,
    336 methods, all names globally unique ≤ 40 chars; `generate --target
    apex` → 2,839 files.)
28. ~~Synthesized names use immediate context, not the full ancestry.~~ ✅
    (D-125: inline anonymous schemas were named by concatenating the whole
    ancestor path — a 109-char box-node-sdk-style monster; now each is named
    from its parent's leaf + its own leaf, so depth adds one segment per
    level. Longest Go type 109 → 83, 60+‑char bucket 56 → 35, Apex opaque
    hash names 190 → 124. Shared-engine change, all backends; counts and
    tests unchanged. A `lowering` regression test pins it.)
29. ~~Method-name shortening (keep the entity, Box-SDK-flavoured).~~ ✅
    (D-126: strip the opaque id handles + manager-tag echo, map the HTTP verb
    (`post`→`create`…), trailing id→`ById`, interior ids drop —
    `get_files_id`→`GetById`, `post_files_id_copy`→`CreateCopy`. The type
    seed stays qualified so inline `…Body`/`…Response` names don't collide;
    the Go `…Options` structs are manager-qualified. Longest Go method
    75→46, one collision fallback in 336 methods, Apex hash names 124→91.)
30. ~~Structural dedupe of identical inline shapes.~~ ✅ (D-127: `synthesize`
    dedupes on the `DeclKind`'s structure — repeated `{id}`/`{id,type}` inline
    refs collapse to one shared type. Real spec 1332→900 decls (924→492
    synthesized), Go types 1550→1118, Apex classes 1419→987. Deterministic; a
    `lowering` regression test pins it.)
31. ~~Apex project structure + AI/human-readable code + per-endpoint docs.~~
    ✅ (D-129: full SFDX scaffolding (config/, manifest/, .forceignore,
    README); ApexDoc on every class/method/model; one Markdown page per
    endpoint under `docs/` with an imports/setup block, SDK types used,
    params, and a copy-pasteable example. `generate --target apex` → 2,401
    files (422 docs); deploy surface unchanged. 3 regression tests.)
32. ~~The hand-written Apex runtime (BoxClient implementation).~~ ✅
    (D-130: `runtimes/apex/**` — `BoxHttpClient implements BoxClient` (callout,
    base-URL resolution, immediate governor-limit-aware retries, non-2xx →
    exception), `BoxTokenProvider` + `BoxDeveloperTokenProvider`,
    `BoxApiException` — embedded into every generated project. The SDK is now
    callable. 991 classes; regression tests + VR-1.3.)
33. ~~Governor-limit-aware pagination (documented, no extra classes).~~ ✅
    (D-131: the base method's envelope already carries `entries` + the cursor,
    so no `…Page` sugar — pagination is the regular method plus a documented
    explicit cursor loop (one page per callout). Reuses `gantry-synth`'s
    `detect_pagination` to flag the docs (Paged column + `## Pagination`
    section). Still 991 classes; regression tests + determinism.)
34. ~~Field ↔ wire serialization remap.~~ ✅ (D-132: Apex `JSON.deserialize`
    matches on the field name with no wire-name mapping, so a renamed field
    (`limit`→`limit_r`, `$parent`→`parent`, `Box__…` runs) silently dropped
    values. Affected structs (119/991) now carry `normalizeKeys`/`denormalizeKeys`
    that remap keys on the untyped tree — recursing only into affected children,
    never touching free-form maps — around native (de)serialization; the managers
    route both ways. Regression tests + determinism.)
35. ~~Generated Apex test suite for the 75% coverage deploy gate.~~ ✅
    (D-133: `mandated_test_coverage` drives an `@isTest` suite — a mock
    `BoxClient`, one `Box<Manager>Test` per manager driving every operation
    through it, and a `BoxUnionsTest` for the union `parse` dispatch. Exercises
    managers + the D-132 remap + union dispatch, from the same IR. 1,078 classes;
    regression tests + determinism. On-platform ≥75% confirmed by VR-1.3;
    also normalizes the union-variant follow-up from D-132.)
36. ~~Client Credentials Grant token provider.~~ ✅ (D-134: `BoxCcgTokenProvider`
    mints + caches a server-to-server token (no crypto), refreshing a minute
    before expiry; `BoxTokenProvider` gains `invalidate()` so the client's 401
    re-attempt actually re-mints. `HttpCalloutMock` test covers
    mint/cache/invalidate/error. 1,080 classes. On-platform via VR-1.3.)
37. ~~JWT (server auth) token provider.~~ ✅ (D-135: `BoxJwtTokenProvider` builds
    + RS256-signs a JWT assertion through a Salesforce-stored key
    (`Crypto.signWithCertificate` — key never in Apex/source) and exchanges the
    `jwt-bearer` grant, caching like CCG. `@TestVisible` signing seam makes the
    exchange `HttpCalloutMock`-testable. 1,083 classes. On-platform via VR-1.3.)
38. ~~Chunked upload (reference implementation).~~ ✅ (D-136: `BoxChunkedUpload`
    orchestrates create-session → PUT parts (SHA-1 digest + `Content-Range`) →
    commit. Two Apex limits exclude *real* Box uploads (≥ 20 MB files exceed the
    6/12 MB heap; power-of-two part sizes aren't 3-aligned for Apex's base64
    slice), so it's a correct, mock-verified reference — failing loudly at each
    limit, aborting the session on failure — pending a `Queueable`-chained path.
    `HttpCalloutMock` test across all three endpoints. 1,085 classes.)
39. ~~`BoxHttpClient` `HttpCalloutMock` coverage.~~ ✅ (D-137: `BoxHttpClientTest`
    drives the client's own callout paths through an `HttpCalloutMock` — URL/query
    building, JSON-body serialization + `overrideBaseUrl`, transient 5xx/429 retry,
    the single 401 refresh (asserted via a counting token provider, since Apex's
    `HttpRequest` has no `getHeader()`), the bounded-retry exhaustion throw, and
    non-2xx → `BoxApiException` with the raw body. Closes the last untested runtime
    class. 1,086 classes; on-platform via VR-1.3.)
40. ~~The tri-state serialization gap (absent-vs-null).~~ ✅ (D-138: Box clears a
    field with an explicit JSON `null` vs. leaves it unchanged when absent, but the
    runtime suppressed all nulls. Moved the null policy from `BoxHttpClient` into
    the managers (`BoxRequest.suppressNulls`, default true); a body reaching a
    write-affected struct is reduced to its set keys + remapped + handed over with
    suppression off. A body-reachable struct with a `Nullable` field gains
    `fieldsToNull` (Apex field names) whose `denormalizeKeys` injects explicit
    nulls. Split "affected" into read (normalizeKeys/responses) vs write
    (denormalizeKeys/requests). 22 structs affected; no new classes, still 1,086.)
41. ~~Apex live smoke (VR-1.4) + shipped Remote Site Settings.~~ ✅ (D-139: the
    generator emits a `RemoteSiteSetting` per Box host so the deployed SDK can
    actually make callouts; a manual workflow deploys the SDK to a scratch org and
    runs anonymous Apex that mints a CCG token and calls `GET /users/me` +
    `GET /folders/0/items` against live Box. The Apex analogue of Go's VR-7. Still
    1,086 classes + 4 Remote Site Settings.)
42. ~~`Object`-field deserialization (unions / free-form JSON).~~ ✅ (D-140:
    VR-1.4 caught that Apex's typed `JSON.deserialize` throws on a populated
    `Object` field — so any response reaching a union/`JsonValue` leaf (e.g. a
    folder listing's `entries`) failed at runtime. An object-bearing struct now
    gets a `deserialize(Object)` builder that detaches the `Object` fields,
    deserializes the typed shell, and reattaches them from the raw tree; managers
    + union parse route through it. 105 structs; still 1,086 classes.)
43. ~~Target-neutral conformance (VR-3 for Apex) + the two platform exclusions.~~
    ✅ (D-141: `TargetShape` makes the VR-3 checklist measure any backend by
    swapping recognizers; `conform --target apex` runs in CI and reports 9
    capabilities, 2 excluded, 0 failing — managers 85, operations 336,
    pagination 64, tests 86, traceability + guides. Fixed two real gaps found by
    making the check honest — spec-fingerprint traceability (`BoxBuildInfo` +
    `BuildInfo` threaded through `generate`) and the three cross-cutting docs
    guides — and documented the two genuine platform exclusions: erased
    serialization (D-138) and interactive OAuth. 1,087 classes.)
44. ~~Apex ship artifact: unlocked 2GP package (NF-8).~~ ✅ (D-142: ship the
    Apex SDK as an **unlocked** 2GP package under the registered `unbox`
    namespace ("Unbox Salesforce SDK") — unlocked, not managed, so the
    spec-regenerated surface can add/remove/rename members without a managed
    package's global-member lock. The generated `sfdx-project.json` now carries
    the namespace + package definition (`0.1.0.NEXT`, major.minor from the FR-9
    diff); the README documents the `sf package create` / `sf package version
    create` / `sf package install` flow. Configure + record slice. The namespace
    is now linked to the configured Dev Hub and `SFDX_AUTH_URL` authenticates to
    it, so the CI package-build job is unblocked (no new secret) and is the next
    follow-up. 1,087 classes; deterministic.)
45. ~~Wire the Apex package-build CI job (NF-8).~~ ✅ (D-142: `apex-package.yml`,
    a manual (`workflow_dispatch`) job — auth the Dev Hub via `SFDX_AUTH_URL`,
    resolve the package `0Ho…` id (creating it on first run), inject it as the
    alias the regenerated project omits, run `sf package version create`
    (compiles + runs every generated test in the `unbox` namespace), and surface
    the installable `04t…` id in the job summary. First dispatch validates it
    on-platform, like VR-1.3/1.4.)
46. ~~Add TypeScript as the fourth SDK target (v4 / M6).~~ ✅ (D-143: a roadmap
    addition, positioned **after Rust**. TypeScript 7's Go-native compiler makes
    `tsc --noEmit` a fast per-commit gate (VR-1.5), and the type system is the
    strongest IR fit yet — `oneOf` → discriminated unions, tri-state →
    `?:`/`| null` with no wrapper, no platform limits. Recorded across
    `NEW_ENGINE_REQUIREMENTS.md` (TR-TypeScript + v4 release + roll-up),
    `PROGRESS.md`, and this plan; +300 (90) hrs → total 3,430 (1,034), overall
    ~84% → ~78% by effort from the added scope. No engine code yet — M6 work.)
47. **Next**: M5 — Rust backend + runtime (v3), then **M6 — TypeScript** (v4).
    Apex (M4, v2) is conformance-proven (D-141) with its ship artifact defined
    and built in CI (D-142) — v2's "shipped" bar is met bar the first on-platform
    package-build dispatch.
