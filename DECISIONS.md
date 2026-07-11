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
