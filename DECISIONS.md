# 📔 box-gantry — Decision Records

Numbering starts at **D-101**. D-001…D-013, cited throughout
[`NEW_ENGINE_REQUIREMENTS.md`](./NEW_ENGINE_REQUIREMENTS.md), refer to
box-codegen's decision records (consultable prior art in that repository).

Template: Context → Decision → Consequences. Status is one of
proposed / accepted / superseded-by-D-nnn.

---

## D-101 — Implement the engine in Rust

**Status:** accepted · 2026-07-10

**Context:** The generator's daily activity is pattern-matching over IR
nodes across three backend lowerings. The dominant failure mode of the
predecessor engine was the silent miss (assessment §2, §6).

**Decision:** The engine is written in Rust. The IR is a closed set of
enums; lowerings use exhaustive `match`, so an unhandled node shape is a
compile error, not a runtime pass-through.

**Consequences:** Single-binary distribution; `serde` for spec parsing;
`miette`-style diagnostics for spec-located errors; the engine's own
serde-tagged enums double as the reference design for the Rust SDK's
`oneOf` modeling. TypeScript's salvage/speed advantage is forgone
deliberately (assessment §6, runner-up analysis).

## D-102 — box-codegen is consultable prior art, not a normative baseline

**Status:** accepted · 2026-07-10

**Context:** The project has zero existing users and zero obligations to
the six legacy SDK targets. Earlier drafts treated box-codegen as a frozen
reference whose artifacts (Go fixtures, output baseline) ported verbatim
and served as acceptance oracles.

**Decision:** box-codegen may be consulted — its designs, lessons (G-n),
decisions (D-00n), test semantics, and spec-quirk knowledge inform this
project — but nothing is ported verbatim, and **no acceptance criterion
references box-codegen's code or output**. Correctness is defined by the
capability contract (requirements R§1), each target language's toolchain,
and the round-trip / conformance / live-smoke suites (VR-3, VR-4, VR-7).

**Consequences:** TR-Go.6's oracle is the VR-3 conformance checklist (the
old Go output is at most an informal comparison); VR-2 fixtures are
authored fresh against the new IR; completion credit in the requirements
roll-up reflects consultable designs (🔶), never portable code.

## D-103 — Target order: Go → Apex → Rust, with Apex designed-for from day one

**Status:** accepted · 2026-07-10

**Context:** The three targets are near-opposite extremes; Apex is the
stress test (no modules, no generics, governor limits, mandatory test
coverage — assessment §4).

**Decision:** Ship Go first (proven design, cheapest verification loop),
Apex second, Rust third — but bake Apex's constraints into the IR and
manifests during engine development, and validate them early with a
timeboxed throwaway Apex spike (PLAN.md M3.5) rather than waiting for v2.

**Consequences:** The IR models rich concepts (modules, generics-shaped
containers) that Apex lowers away; manifest axes for generics, callout
limits, and coverage mandates exist from M2; the spike's IR pressure is
reviewed explicitly so Apex limitations never silently reshape the IR
(assessment §8).

## D-104 — `x-box-tag` is the manager grouping key; `tags` is display-only

**Status:** accepted · 2026-07-11

**Context:** Manager grouping (FR-7.1) needs a machine-readable key on
every operation. The vendored real specs show `tags` is a display concern:
at least one operation (`get_enterprise_configurations_id_v2025.0`) has no
`tags` at all but does carry `x-box-tag`. Every operation in all three
documents (base 2024.0, 2025.0, 2026.0) carries a non-empty `x-box-tag`,
and `operationId` is unique per document.

**Decision:** Ingestion groups operations by `x-box-tag` and treats a
missing/empty `x-box-tag` or `operationId` as a loud ingestion error
(FR-1.4) — an operation that cannot be grouped or named must fail the run,
never be skipped (NF-1). `tags` is not consumed.

**Consequences:** `gantry-spec` validates both invariants on every load;
the real-spec integration test pins the current counts (296/37/3
operations, 73 managers in base). `operationId`s carrying the
`#variation` suffix (19 in the base spec) are a spec-authoring convention
that the naming layer (FR-1.2) must translate into structured variation
data — the `#` never survives into an IR identifier ([`Identifier`]
rejects it).

## D-105 — Schema-lowering conventions for the Box specs

**Status:** accepted · 2026-07-11

**Context:** The Box OpenAPI documents rely on conventions the OpenAPI
spec doesn't formalize. Surveyed on the vendored set: `allOf` is used
both as composition (2–3 structural parts) and as a reference wrapper
(one structural `$ref` + annotation-only parts carrying
description/example/`nullable` — 8 sites put `nullable` in the
annotation part); no union declares an OpenAPI `discriminator`, but most
variants carry a single-valued `type` property; enums have no
extensibility markers; 60 properties are inline anonymous objects.

**Decision:** The lowering (in `gantry-spec::lower`) fixes these
conventions structurally, once:
- `allOf` parts split into structural vs annotation-only. One structural
  part + nothing own = reference passthrough (annotation `nullable` still
  read); several structural parts = flattened composition, later parts
  overriding; zero structural parts = explicit `JsonValue` hole.
- Unions are discriminated on `"type"` iff every variant carries a
  distinct single-valued `type` constant (walked through `allOf`
  chains); otherwise structural, with no half-inferred values.
- String enums are **open** (unknown values round-trip); `null` entries
  encode nullability, not a value; all-numeric enums lower to their base
  numeric type.
- Inline anonymous shapes get synthesized decls named
  `Owner` + PascalCase(property), disambiguated with numeric suffixes,
  deterministically.
- Anything unclassifiable is a loud error with file + schema location;
  `JsonValue` sites are counted and pinned in tests, never silent.

**Consequences:** The full real spec set lowers to 967 declarations
(550 structs, 43 unions / 23 discriminated, 372 enums, 559 synthesized,
20 JsonValue sites), pinned in `tests/real_specs.rs`. The null-vs-absent
tri-state currently collapses into `Optional` — revisit before Go
serializer work (PLAN.md).

## D-106 — Operation-lowering conventions for the Box specs

**Status:** accepted · 2026-07-11

**Context:** Lowering the 336 real operations surfaced more unformalized
conventions, each caught by a loud ingestion error rather than observed
in broken output: versioned documents suffix `operationId`s with
`_v2025.0`; path keys carry `#variation` fragments; one path segment
mixes literal and parameter text (`thumbnail.{extension}`); operations
override the base URL with six distinct `servers` values; success
responses vary from body-less 204s to binary downloads with 202/302
siblings.

**Decision:**
- **Names**: the `_v{version}` suffix is stripped when it matches the
  document's declared version (a mismatched marker fails); `#variation`
  splits into a structured `variation` field; `#` fragments on path keys
  are authoring plumbing, excluded from the request path.
- **Paths**: templates parse into structured segments
  (literal / parameter / composite); every placeholder must be backed by
  a declared, required path parameter.
- **Base URLs** (the G-2 quirk): spec `servers` URLs map to a closed
  six-value `BaseUrl` enum (Api, ApiRoot, Upload, UploadSession,
  OAuthAuthorize, Download); an unknown URL fails ingestion.
- **Bodies**: exactly one media type per request body, from the closed
  set {json, json-patch, url-encoded, multipart, octet-stream}.
- **Responses**: ascending status order, the first content-bearing
  2xx/3xx decides the shape (Json/Binary/Text); all media of that
  response must classify identically; a content-free 302 makes the
  operation a Redirect; otherwise the success is body-less.

**Consequences:** All 336 operations lower (275 JSON, 56 body-less,
4 binary, 1 text, 0 redirect-only; 19 variations; 14 non-default base
URLs), pinned in `tests/real_specs.rs`. Program totals grow to 1,332
declarations / 26 JsonValue sites with parameter and body synthesis.

## D-107 — The IR node set is frozen as the v1 baseline

**Status:** accepted · 2026-07-11

**Context:** The IR (FR-2) now expresses the complete real Box surface:
1,332 declarations and 336 operations lower from the three vendored
documents, the semantic pass verifies the whole program, and the M3.5
Apex spike lowers all 85 managers through exhaustive matches with no
escape hatches.

**Decision:** The node set as of this record — `Type` (11 kinds incl.
`Optional`, `JsonValue`), `DeclKind` (Struct/Union/Enum/Alias),
`Operation` with `Param`/`RequestBody`/`ResponseShape`/`PathSegment`
(incl. `Composite`)/`BaseUrl`/`HttpMethod`, and the closed enums behind
them — is the v1 baseline. Adding or changing a node kind now requires a
decision record, and the compiler enumerates every lowering the change
breaks (FR-2.1, NF-4).

**Consequences:** Backends and feature synthesis can build against a
stable surface. The known open item is deliberate: the null-vs-absent
tri-state (D-105, reconfirmed by D-108) may add one axis to optionality
before Go serializer work — that change will get its own record.

## D-108 — M3.5 Apex spike findings

**Status:** accepted · 2026-07-11

**Context:** PLAN.md M3.5: a timeboxed, throwaway Apex lowering
(`spikes/apex-spike`) run against the full real spec set to surface any
IR changes the extreme target forces, months before the real Apex
backend (assessment §8 primary risk). The spike consumes only manifest
axes (flat namespace + 40-char identifier limit, no user generics,
buffered streaming) — never the language name.

**Findings (spike run: 85/85 managers lower; 4,678 identifiers minted;
719 KB of throwaway source; deterministic across runs):**
1. **The IR held — zero node changes forced.** Every node kind lowers
   through exhaustive matches; the assessment §8 risk did not
   materialize at this depth.
2. **Name-length pressure is real: 337 identifiers exceed 40 chars**
   (synthesized request-body/response names). The real backend needs the
   deterministic abbreviation scheme the spike prototyped (prefix +
   FNV-hash suffix), and the FR-1.2 naming layer should shorten
   synthesized names before mangling ever triggers.
3. **Optionality erases in Apex** (every reference is nullable) — the
   D-105 null-vs-absent tri-state matters on the Apex side too; resolve
   before serializer work.
4. **The identifier/wire-name split (FR-2.2) paid off**: Apex reserved
   words (`limit`, `group`, `date`…) appear as Box wire names and mangle
   safely without touching serialization.
5. **49 discriminated unions** get `JSON.deserializeUntyped` dispatch
   (TR-Apex.4 shape confirmed); structural unions erase to `Object`, a
   manifest-accepted loss.
6. **66 paged operations** get per-type page classes — TR-Apex.2's
   no-user-generics lowering works without a shared `Page<T>`.

**Consequences:** The spike stays in the workspace so its exhaustive
matches keep proving IR-totality for Apex in CI; it is retired when the
real backend lands (M4). Its output is never shipped.

## D-109 — Go manifest frozen; runtime contract v1 drafted, stubs rendered from data

**Status:** accepted · 2026-07-11

**Context:** M2 requires the capability manifests (FR-4) and the
machine-readable runtime contract (FR-5). The Apex spike (D-108) already
consumed the Apex manifest's axes; the Go backend (M3) is next and needs
a stable manifest plus the runtime surface generated code will call.

**Decision:**
- The **Go manifest is frozen**: hierarchical modules, full generics,
  `(T, error)`, sync with `context.Context` threading, streaming
  supported, no platform limits or coverage mandates. Apex and Rust
  manifests remain drafts until their backends consume them (M4/M5).
- The **runtime contract v1** (`gantry-contract::V1`) declares the
  hand-written surface as data — name, params, return, fallibility,
  context threading, and a behavior clause per function: `fetch` (the
  retrying network layer), `access_token`, request builders
  (headers/query/json/stream/multipart bodies), and response accessors
  (bytes/stream/header/status). Draft until the Go backend consumes it
  (M3); frozen then.
- **Stubs are rendered from the contract data itself** (FR-5.2 by
  construction — stub and declaration cannot drift), keyed off manifest
  axes only (FR-4.2). Every stub panics loudly when called (NF-1):
  a stub silently returning zero values would hide missing wiring.
- The FR-5.3 gate is real from day one: tests compile the rendered Go
  package with `go build` + `go vet` and assert gofmt-cleanliness
  (G-17), locally when a Go toolchain exists (skipping loudly otherwise)
  and always in CI (Go 1.23 pinned).

**Consequences:** The Go backend can generate calls against a checked
surface; the real `box-go-sdk`-style runtime implements the same
contract (TR-Go.7). Serialization stays out of the contract by design:
models serialize via struct tags (TR-Go.2), and union/enum helpers are
generated, not hand-written.
