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

## D-110 — Null-vs-absent is structural: `Optional<Nullable<T>>`

**Status:** accepted · 2026-07-11

**Context:** D-105 collapsed `nullable` and not-required into one
`Optional` wrapper and flagged the tri-state for revisit before Go
serializer work; the Apex spike (D-108) confirmed the pressure from the
second target. Box APIs use an explicit JSON `null` to clear fields on
update — absent and null are different requests. This is the first IR
change under the D-107 freeze process.

**Decision:**
- `Type::Nullable(T)` joins the IR: *the wire value may be an explicit
  `null`*. `Type::Optional(T)` now means exactly *the key may be absent*.
- Canonical nesting is `Optional<Nullable<T>>`; sema rejects
  `Nullable<Nullable>`, `Nullable<Optional>`, and `Optional<Optional>`
  as engine bugs (new `BadNullability` finding, exit-code class 5).
- Lowering derives both axes structurally: `nullable` (direct or via
  annotation parts) → `Nullable`; not-required → `Optional`; an enum
  value list containing `null` also marks its property `Nullable`.
- Alongside (D-108 finding 2): synthesized request-body names shorten
  from `{Op}RequestBody…` to `{Op}Body…`, cutting over-limit Apex
  identifiers from 337 to 295.

**Consequences:** Go can lower `Optional` to omit-when-nil and
`Nullable` to serialize-explicit-null (the D-004-class distinction),
Rust to `Option`/double-`Option` or a tri-state enum, while Apex erases
both at the type level and handles the difference in serializers. The
compiler enumerated every lowering the new kind touched (sema, spike) —
the D-107 process working as intended. Program decl counts are
unchanged; the real spec set verifies with the new wrappers in place.

## D-111 — Pagination detection is language-agnostic synthesis; Go lowers to iter.Seq2

**Status:** accepted · 2026-07-11

**Context:** FR-7.3/D-013 require idiomatic paged surfaces per target.
Surveyed on the real spec: 64 operations paginate — 54 marker (`marker`
query param + `entries`/`next_marker` response), 10 offset (`offset`
param + `entries`/`offset`). No OpenAPI extension marks pagination; it
is a structural property of the (verified) IR.

**Decision:**
- Detection lives in the new `gantry-synth` crate (FR-7 feature
  synthesis), keyed off IR shape only, never a language name (FR-4.2):
  a JSON response struct with an `entries` list, plus an *optional*
  cursor query param whose matching response field is present. An
  operation with the param but no envelope (or vice versa) is
  conservatively **not** paginated — it keeps its plain method.
- Go lowers each `PagedOperation` to a `{Method}Paginate` returning
  `iter.Seq2[*Element, error]` (Go ≥ 1.23, TR-Go.4): it wraps the plain
  method, threads the cursor through a *copy* of the caller's options,
  and yields elements one page at a time.
- Cursor types are resolved, not assumed: the request param and response
  field may disagree (DevicePinners has a `string` marker param but an
  `int64` `next_marker`), so the backend converts explicitly
  (`strconv.FormatInt`) rather than emit a type-mismatched assignment.

**Consequences:** 64 iterators generate and compile (54 marker, 10
offset), pinned in the Go compile test. The synth layer is now
established for Apex (`Queueable` continuations) and Rust (`Stream`) to
consume the same detection. The compile loop caught the
string-vs-int64 cursor mismatch on first contact — the G-1 effect again.

## D-112 — The Go serialization package: generic Nullable[T] and Date

**Status:** accepted · 2026-07-12

**Context:** The D-110 tri-state needs a Go representation `encoding/json`
lacks natively (BG-1), and Box's RFC 3339 full-date (`2006-01-02`) is not
`time.Time`'s default format. TR-Go.2 forbids per-model serializers, so
these live in one hand-authored, generated static package.

**Decision:** `serialization/serialization.go` ships:
- `Nullable[T]` (Go generics, manifest axis `generics: Full`): a
  `{Valid bool; Value T}` with custom Marshal/Unmarshal — `null` when
  `!Valid`. Field mapping: `Optional<Nullable<T>>` →
  `*serialization.Nullable[T],omitempty` (nil absent, `Null[T]()`
  explicit null, `Value(v)` value); bare `Nullable<T>` → the wrapper by
  value; nested in containers, the wrapper is kept so per-element null
  round-trips.
- `Date` wrapping `time.Time` with `2006-01-02` Marshal/Unmarshal and a
  `String()` for query rendering.

**Consequences:** 412 tri-state field sites + 3 Date sites serialize
correctly; BG-1 resolved on the **write** side (absent / explicit-null /
value). On read, `encoding/json` collapses a JSON `null` to a nil
pointer without calling `UnmarshalJSON`, so null and absent both surface
as nil — an accepted limitation, since Box's clear-on-update semantics
are a write concern. Generated `go test` round-trip tests (VR-4) pin
this behavior. The pagination iterators learned to read the
cursor *through* its wrapper (`page.X.Value` guarded by `.Valid`), since
`next_marker` fields became `Nullable`. The whole SDK still compiles
clean (VR-1.1). Apex/Rust will map the same IR distinction to their
serializers (Rust: `Option<Option<T>>` or a tri-state enum; Apex:
explicit null handling in `JSON.serialize`).

## D-113 — Runtime session threading + the hand-written Go runtime

**Status:** accepted · 2026-07-12

**Context:** The generated managers compiled against panicking stubs but
had nowhere to hold per-client state (auth, base URLs, HTTP client, retry
policy), so the SDK could not actually run. TR-Go.7 requires a
hand-written runtime implementing the FR-5 contract.

**Decision:**
- The contract gains a **receiver axis** (`Session` | `Free`): stateful
  functions (`new_request`, `base_url`, `fetch`, `access_token`) are
  methods on the runtime `Client`; pure builders/accessors stay free
  functions. Stubs render the split from the contract data (FR-5.2), so
  the real runtime and the stubs share one shape.
- Generated managers hold an unexported `session *gantryruntime.Client`
  and are built by a generated `New<M>Manager(session)`. The client's
  `NewClient(ts, opts...)` constructs one shared session (from a
  `TokenSource` + `With*` options, G-3) and wires every manager to it —
  so config applies everywhere.
- The hand-written runtime lives at `runtimes/go/gantryruntime/`
  (TR-Go.7): retrying `Fetch` (full-jitter backoff, single 401 refresh,
  `Retry-After` on 429/503), request builders (header/query/json/form/
  stream/multipart), buffered response accessors, `With*` options, and a
  `DeveloperToken` `TokenSource` (one of the four flows).

**Consequences:** A new test swaps the stubs for the real runtime, adds a
smoke `main` (`NewClient(DeveloperToken(...))` reaching a manager method),
and `go build`s — proving the runtime satisfies the contract and the
public API composes end to end (FR-5.2 conformance by construction). CI
builds/vets/gofmt-checks the runtime standalone. The remaining three auth
flows (CCG, JWT, OAuth) implement the same `TokenSource` and land with
auth synthesis.

## D-114 — The four Box auth flows in the hand-written Go runtime

**Status:** accepted · 2026-07-12

**Context:** D-113 shipped the runtime with only `DeveloperToken`. A
usable SDK needs the three credentialed flows Box supports: Client
Credentials Grant (CCG), JWT server auth, and OAuth 2.0 authorization
code. Each is identical across every generated Box SDK (same endpoints,
same grants), so — like the serialization package (TR-Go.2) and the rest
of the runtime (TR-Go.7) — they are **hand-written into the runtime**, not
synthesized per-spec. There is nothing in the OpenAPI document that varies
them, so putting them behind the code generator would add moving parts
without adding fidelity.

**Decision:**
- All flows implement the existing `TokenSource` interface, so they drop
  into `client.NewClient(ts, opts...)` with no generation change.
- A shared `cachedToken` caches the access token behind a mutex and
  refreshes it within `refreshMargin` (60s) of expiry via a flow-specific
  closure; a shared `postTokenForm` posts the grant to Box's token
  endpoint and surfaces non-2xx bodies as errors. `TokenURL` and
  `HTTPClient` are overridable per config (custom deployments, tests).
- **CCG** (`ClientCredentials(CCGConfig)`): `client_credentials` grant
  with `box_subject_type`/`box_subject_id` — enterprise by default, user
  when `UserID` is set.
- **OAuth** (`OAuth`/`OAuthConfig.ExchangeCode`/`.AuthorizeURL`): the
  authorization-code exchange plus refresh, **rotating** the refresh token
  Box returns on each exchange (Box invalidates the previous one).
- **JWT** (`JWTAuth(JWTConfig)`): builds and RS256-signs the bearer
  assertion with stdlib crypto only. The RSA key is parsed (and, for the
  legacy passphrase-encrypted PEM that Box's `box_config.json` ships,
  decrypted) **at construction**, so a bad key fails loudly before the
  first request rather than deep in a call.

**Consequences:** Stdlib-only — no new dependencies (NF-6); JWT rides
`crypto/rsa` + `crypto/x509` (the deprecated `DecryptPEMBlock` is exactly
Box's key format and passes `go vet`). A runtime `auth_test.go` exercises
every flow against an `httptest` token endpoint — CCG subject selection
and caching, expiry-driven refresh, OAuth refresh-token rotation and code
exchange, and a JWT assertion the paired public key verifies — and CI now
runs `go test ./...` on the runtime as a gate. The generated auth guide
(FR-7.7) documents all four with copy-paste constructors. Apex/Rust will
re-express the same flows in their runtimes (TR-Apex.6/TR-Rust.5); JWT in
Apex uses `Crypto.sign` per the M4 plan.

## D-115 — FR-9 spec-diff runs on the IR, classified breaking vs compatible

**Status:** accepted · 2026-07-12

**Context:** FR-9 requires a spec-diff/breaking-change report on every spec
bump to inform the SDK version. The question was *what* to diff: the raw
OpenAPI documents, or the lowered IR the SDK is actually generated from.

**Decision:** Diff the **verified IR `Program`s** (`gantry-verify::diff`),
not the raw specs. The report then describes exactly the surface the
generated SDK exposes — a field the naming/`allOf` layer normalizes away
is not a diff; a removed operation, a changed response type, or a newly
required parameter is. Each difference is classified:
- **Breaking** (→ major bump): any removal (operation, schema, field, enum
  value, union variant), any type change (param/field/request/response/
  alias/variant), a decl-kind change, or a **new required** parameter.
- **Compatible** (→ minor bump): additions (operation, schema, optional
  field, enum value, union variant) and deprecation flips (advisory).
- No differences → no bump.

Cross-program type identity is by **structural signature**: a `Decl(id)`
renders to its qualified name (`module::Name@version`), never its arena id,
so the two programs' independent `DeclId` spaces compare correctly. Output
is deterministic (sorted by category, key, kind).

**Consequences:** `gantry diff --from <specs> --to <specs>` prints the
report and **exits 4 on a breaking diff** (the `VERIFICATION_FAILURE`
class), so CI can gate a major bump. Unit tests pin every classification
rule; an integration test over the real vendored specs proves adding the
`2025.0` overlay is purely additive (257 compatible changes, minor) and
removing it is breaking (257 removals, major). The diff is language-neutral
(it reads the IR), so the same report serves every target SDK's versioning.
The remaining VR items (VR-2 fixtures, VR-3 conformance checklist, VR-7
live smoke) build on this seam in `gantry-verify`.

## D-116 — VR-3 conformance checklist: contract-derived, target-neutral

**Status:** accepted · 2026-07-12

**Context:** VR-3 requires the R§1 capability contract as a
machine-checkable, per-target checklist (operation/manager counts, auth
flows, paged surfaces), reported every CI run and release-blocking. The
question was how to express "expected" without hand-maintaining golden
numbers that rot as the spec moves.

**Decision:** Derive **expected** from the verified program and **actual**
from the generated output — never from a hard-coded table (`gantry-verify`
`conformance`). Each capability is one `Check { expected, actual, status }`:
- **managers** = `analysis.managers.len()` → generated `managers/*.go`;
- **manager-docs** = managers → `docs/managers/*.md`;
- **operations** = `program.operations.len()` → generated methods (counted
  as `(ctx context.Context` signatures minus the `Paginate` ones);
- **pagination** = `detect_pagination().len()` → `Paginate` iterators;
- **serialization / round-trip-tests / auth-flows / docs-guides** =
  presence/enumeration of the tri-state package, VR-4 tests, the four auth
  flows surfaced in the generated guide, and the cross-cutting guides.

A capability passes iff `actual >= expected` (extra helpers are fine; a
shortfall is not). Because "expected" tracks the spec, the checklist can
never silently pass on a partial SDK: drop a manager's methods or a
paginator and the count falls short.

The checklist reads a lightweight `GeneratedView { path, content }`, not
the Go backend's file type, so it depends only on the IR crates and will
measure the Apex and Rust outputs unchanged (TR-Apex/TR-Rust conformance
parity is then the same report with a different target string).

**Consequences:** `gantry conform --target go <specs>` prints the checklist
and **exits 4 when any capability falls short** (the `VERIFICATION_FAILURE`
class); CI runs it every build (release-blocking, per the verification
cadence). On the real spec it reports 85 managers, 336 operations, 64
paginated surfaces, 4 auth flows — all green. Unit tests pin the pass/fail
logic (a dropped method and a missing auth flow both fail the gate); an
integration test asserts the real generated SDK is fully conformant with
non-trivial counts.

## D-117 — Build provenance (NF-7) and the Go-module ship artifact (NF-8)

**Status:** accepted · 2026-07-12

**Context:** NF-7 requires every generated SDK to embed a spec hash + engine
version so a release is traceable to its inputs; NF-8 requires each release
to define and produce its ship artifact — for v1, a tagged Go module — with
the packaging decision recorded.

**Decision:**
- **Spec fingerprint (NF-7):** `SpecSet::fingerprint()` is an **FNV-1a**
  hash of the raw document bytes, folded in load order, rendered as 16
  lowercase hex digits. Dependency-free (no `sha2` — consistent with the
  stdlib-only runtime and NF-6's locked-deps discipline) and deterministic
  (FR-6.2); it is an input *fingerprint* for traceability, not a security
  hash, so collision resistance against an adversary is not required. It is
  order-sensitive (the set is ordered, base first) and moves on any input
  change.
- **Provenance carried two ways:** a `BuildInfo { engine, spec_fingerprint }`
  is threaded into generation. Every model file header gains
  `(spec <fingerprint>)`, and a dedicated **`buildinfo` package** exports
  `EngineVersion` and `SpecFingerprint` constants so the shipped SDK can
  report its own provenance programmatically. The VR-3 checklist gains a
  `traceability` capability gating the package's presence.
- **Ship artifact (NF-8):** the generated tree **is** the artifact — a
  self-contained Go module (`go.mod` with `module boxgantry.invalid/boxsdk`,
  `go 1.23`) that builds/vets/gofmt/tests clean (VR-1.1). The `.invalid`
  module path is the in-repo placeholder; the **real import path and the
  `vMAJOR.MINOR.PATCH` tag are set by the release pipeline**, with the
  version bump chosen by the FR-9 spec-diff (D-115): breaking → major,
  additive → minor. Go modules ship as source (no build step), so there is
  no unlocked-package-vs-source question as there will be for Apex — the
  packaging choice here is simply *tagged module source*, recorded so v2/v3
  can point back to it.

**Consequences:** `gantry generate/verify/conform` all stamp the output;
the fingerprint is computed once from the `SpecSet` and reused. On the real
spec the fingerprint is `ee7d55aedefe2fa0` and the engine version `0.1.0`,
embedded in the headers and the `buildinfo` package (both compile-gated in
CI). Unit tests pin the fingerprint's determinism, hex shape, and
order-sensitivity; the conformance checklist now reports 9 capabilities.
Apex/Rust will carry the same `BuildInfo` into their own provenance
surfaces (a `buildinfo` class / a `build_info` module).

## D-118 — VR-2 per-node lowering fixtures: semantic, not byte-exact

**Status:** accepted · 2026-07-12

**Context:** VR-2 requires per-node lowering fixtures (IR fragment →
expected source) per backend, the box-codegen 54-case Go suite informing
the initial set, authored fresh against the new IR. The question was what
to assert: byte-exact golden output, or the semantics.

**Decision:** Assert the **semantics per node**, not byte-exact output.
`node_fixtures.rs` builds a minimal IR program for one node kind (a struct
field of a given type, an enum, a discriminated/structural union, an alias,
an empty struct), renders the `schemas` module, and asserts the specific Go
the rule must produce: the `*T`/`serialization.Nullable[T]`/`*serialization
.Nullable[T]` tri-state shapes (D-110), `Date`→`serialization.Date`,
`DateTime`→`time.Time`, `Binary`→`io.Reader`+`json:"-"`, `JsonValue`→`any`,
slices/maps, per-element `[]serialization.Nullable[T]`, open-enum string
type + prefixed constants, union variant structs with `Marshal`/`Unmarshal`
dispatch and unknown-tag retention, and aliases.

Byte-exact/gofmt-cleanliness/determinism are already the job of
`compile_output.rs` (VR-1.1, VR-5); duplicating them as goldens would make
the fixtures brittle to formatting. So assertions are **column-alignment
insensitive** (a whitespace-squeezing matcher for field blocks) and target
the type expression, tag, and method — the parts that encode meaning.

**Consequences:** 17 focused cases cover every IR node kind and the Box
quirks the tri-state/union/enum rules encode. They pinpoint *which rule*
regressed when one changes — where VR-1.1 only says "the spec stopped
building." The harness renders through the real `generate_models` +
`BuildInfo`, so fixtures track the true printer. Apex/Rust get their own
`node_fixtures` against the same IR fragments, which is how conformance
parity is demonstrated node by node.

## D-119 — VR-7 live smoke: build-tagged, credential-gated, runtime-level

**Status:** accepted · 2026-07-12

**Context:** VR-7 requires a live smoke — one call per auth flow plus
upload/download/paginate — green against a real Box dev account, per
release and on demand. It needs credentials and touches a live account, so
it cannot be a per-commit gate.

**Decision:**
- The smoke lives in the **committed runtime** (`gantryruntime/
  livesmoke_test.go`), not the generated output, and drives **only the
  stable runtime contract** (`New` / `NewRequest` / `Fetch` / the `With*`
  builders / response accessors) — never generated method names. So it
  verifies the hand-written runtime (auth token exchange, retry, multipart,
  streaming), which is exactly the part the compile gate (VR-1.1) cannot
  exercise, and it does not churn when the spec/methods change.
- It is **build-tagged `//go:build live`**, so the standard CI gate
  (`go build`/`vet`/`test` without `-tags live`) never compiles or runs it —
  a true no-op in the per-commit pipeline. gofmt still checks it (syntactic).
- Credentials come from the **environment**; each flow runs only when its
  variables are present, and the test `t.Skip()`s when none are — a
  credential-free run passes cleanly.
- In CI it runs only via a manual **`workflow_dispatch`** workflow that
  reads the credentials from **repo secrets**, never the repository. This
  is how the release pipeline "produces" the VR-7 result on demand.

**Consequences:** The smoke covers all four flows (Developer Token / CCG /
OAuth / JWT via `box_config.json`), then paginates the root folder
(following the marker cursor like the generated iterators), and
uploads → downloads (byte-compares) → deletes a scratch file. It compiles
under `-tags live` and is excluded otherwise (both verified). Apex/Rust get
the same shape (a tagged / ignored live test driving their runtimes) for
their VR-7.

**Green — 2026-07-13.** The maintainer added the CCG secrets and the
`workflow_dispatch` run passed against the live Box account
(run 29216656902): CCG authenticated as the enterprise service account
(`GET /users/me`), paginated the root folder (20 items via the marker
cursor), and uploaded → downloaded (byte-compared) → deleted a scratch
file — `PASS: TestLiveSmoke (3.33s)`. This exercised the hand-written
runtime's token exchange, retry layer, multipart, streaming, and
pagination end to end against the real API — the one thing the compile
gate can't prove. The `secrets`-in-`if:` fix was validated in the same
run (the JWT step correctly skipped). **This closes the last open v1
acceptance item: v1 (the Go SDK) is complete.**

## D-120 — M4 opens: the Apex model layer (flat namespace, no generics)

**Status:** accepted · 2026-07-13

**Context:** M4 (the Apex backend, v2) begins. Apex is the assessment's
primary risk — one flat namespace, no user generics, exceptions, buffered
bodies, a 40-char identifier limit, a 75% coverage gate. The M3.5 spike
(D-108) already proved the IR is total for Apex (85/85 managers lowered,
zero IR changes forced); this is the real, non-throwaway backend, opening
with the model layer, mirroring how the Go backend opened (models first,
then managers/client/runtime).

**Decision** (`gantry-backend-apex`, consuming only the `apex()` manifest
axes — never the language name, FR-4.2):
- **One top-level `.cls` per schema** (`Struct`/`Union`/`Enum`). Apex has
  no packages, so the IR module tree collapses into a **globally-unique,
  deterministically-mangled** class name (TR-Apex.1): module prefix + decl
  name, abbreviated to the manifest's 40-char limit as `prefix_<7-hex FNV>`
  with a numeric-suffix fallback on collision. Names are computed once in
  program order so a reference always renders to the same name as its class.
- **Tri-state erases at the type level.** Every Apex reference is nullable,
  so `Optional<T>`, `Nullable<T>`, and `Optional<Nullable<T>>` (D-110) all
  render as the bare Apex type; absent-vs-null becomes the serializer's job
  (a later slice). Built-in `List`/`Map` are used (the no-generics axis
  forbids *user-defined* generics, not platform collections).
- **Open enum → a `String`-valued class** with `static final String`
  constants (PascalCase, deduped), so unknown values round-trip — a real
  Apex `enum` cannot (D-105/G-11).
- **Discriminated union → a generated `JSON.deserializeUntyped` dispatch**
  on the tag, unknown tags returning the raw map (open unions, TR-Apex.4).
  **Structural union → `Object value`** (the manifest-accepted loss).
- **Alias → nothing**; references resolve through it (Apex has no aliases).
- Scalars: `Boolean`/`Long`/`Double`/`String`/`Date`/`Datetime`; `Binary` →
  `Blob` (buffered platform); `JsonValue` → `Object`. Reserved words gain a
  trailing `_` (`safe_word`); the wire name is untouched (FR-2 split).

**Consequences:** the full real spec lowers to **1,330 Apex classes**
(1,332 decls − 2 aliases), deterministically, every name ≤ 40 chars and
globally unique, with 23 `deserializeUntyped` dispatches (matching the IR's
discriminated-union count). No Apex toolchain runs here, so the per-commit
signal is structural + determinism tests (10, mirroring the VR-2 node
fixtures); the scratch-org `sf project deploy validate` loop (VR-1.3) is
the CI/merge gate once a Dev Hub is configured — the next slice, alongside
managers/client/serializer/runtime. Serialization field-name↔wire-name
mapping is deferred to the serializer slice (structs currently carry a
`// wire:` comment); the union path already uses `deserializeUntyped`.

## D-121 — Apex ships as an SFDX project; the scratch-org loop is the gate

**Status:** accepted · 2026-07-13

**Context:** M3's engine came alive because the generated Go compiled every
commit (VR-1.1). Apex needs the same loop, but its "compiler" is a
Salesforce org. To run one, the generated tree must be a **deployable SFDX
project**, and CI must be able to `sf project deploy validate` it against a
scratch org.

**Decision:**
- The Apex backend's top-level `generate()` emits a **deployable SFDX
  source-format project**: an `sfdx-project.json` (package dir `force-app`,
  a pinned `sourceApiVersion`), every class under
  `force-app/main/default/classes/<Name>.cls`, and a `<Name>.cls-meta.xml`
  sidecar per class. Output is path-sorted for determinism (FR-6.2).
- `gantry generate --target apex` is wired: both backends emit
  `(path, content)`, so the CLI dispatches on the target and writes a
  common shape. (`verify`/`conform` stay Go-only — their oracle is the Go
  toolchain; Apex's is the scratch org.)
- **VR-1.3 harness** (`.github/workflows/apex-scratch.yml`): a manual
  `workflow_dispatch` job that generates the SDK, creates a fresh scratch
  org from the Dev Hub, and **check-only-deploys** it (the platform
  compiles every class; nothing persists), then deletes the org. Like the
  VR-7 live smoke, the Dev Hub auth is a **repo secret** (`SFDX_AUTH_URL`),
  never the repository; absent the secret the job skips, so a dry run
  passes. This attacks the assessment §8 operational risk (Ded Hub auth,
  org limits, flakiness — Dev Hub auth) early, per the M4 week-one mandate.

**Consequences:** on the real spec, `generate --target apex` writes **2,661
files** (1 project + 1,330 classes + 1,330 meta sidecars). A packaging test
asserts the project JSON is valid and every class has exactly one meta
sidecar under the source-format path; determinism holds. The scratch-org
compile loop turns **green the moment a Dev Hub secret is added** (as VR-7
did for the live account) — until then structural + determinism tests are
the per-commit signal. Serialization field↔wire mapping, managers, the
client, and the Apex runtime are the next slices, now verifiable against a
real org.

## D-122 — Apex models round-trip natively; enums are Strings, unions are Object

**Status:** accepted · 2026-07-13

**Context:** The model slice (D-120) made open enums a class with a `value`
field and discriminated unions a dispatch-only class. But on the wire an
enum is a plain JSON string and a union is a JSON object, so
`JSON.deserialize(json, StructClass.class)` — the natural Apex path — would
mis-map both: it would try to build an enum object from a string and an
empty union instance from the variant's fields. Serialization has to be
right before managers can return usable models.

**Decision:** Represent model fields with the **native JSON shape** so
`JSON.serialize`/`JSON.deserialize` round-trip a struct directly:
- **Enum-typed field → `String`.** The `<Enum>` class becomes a pure
  constants namespace (the `value` field is dropped); unknown values
  survive for free because any string is valid (open enums, G-11).
- **Union-typed field → `Object`.** The field holds the raw untyped map;
  the caller dispatches with the still-generated `<Union>.parse(...)`
  (TR-Apex.4). Typing it as the dispatch-only class would make
  `JSON.deserialize` mis-map the wire object.
- **Struct-typed field → the struct class** (JSON.deserialize recurses).
  Scalars/`List`/`Map` are already native. This composes: `List<Enum>` →
  `List<String>`, `List<Union>` → `List<Object>`.

**Consequences:** a struct whose fields are scalars, nested structs,
enums-as-String, or unions-as-Object round-trips through plain
`JSON.deserialize` with no generated per-class serializer — the simplest
thing that can work on the platform. On the real spec, `FileMini.type` is
now `String` and no class carries a stray `value` field; the class count
(1,330) and union dispatch count (23) are unchanged. **Still open** (the
next serialization step, to be nailed once the scratch-org loop is live to
verify it): fields whose Apex identifier differs from the wire key —
reserved words (`limit` → `limit_`) and `$`-prefixed metadata keys — need a
name remap that `JSON.deserialize` can't do by itself, and the D-110 null-
vs-absent tri-state (explicit-null clear-on-update) needs explicit handling
rather than omit-on-null. Both are deferred, not silently lost.

## D-123 — Apex managers + client call a runtime contract stub

**Status:** accepted · 2026-07-13

**Context:** With models in place, the SDK needs the callable surface: one
class per manager, a method per operation, and an entry point. The Go
backend proved the pattern — managers call a machine-checked runtime
contract and compile against compilable stubs, and a hand-written runtime
drops in behind the same signatures (D-113). Apex takes the same shape.

**Decision** (`gantry-backend-apex::generate_managers`):
- **One `Box<Manager>` class per `x-box-tag`**, holding a `BoxClient` and a
  method per operation. Method bodies build a `BoxRequest` structurally from
  the IR — `method`, base-URL class key, the path expression from the
  structured segments (FR-2.2), null-guarded `query`/`headers` puts by wire
  name, and the body — call `client.send(request)`, and deserialize the
  response into the model type. `void` when there is no body; a non-2xx is
  an exception the runtime raises (manifest `ErrorModel::Exceptions`).
- **Response deserialization** follows D-122: a struct/`List`/`Map` return
  goes through `(T) JSON.deserialize(body, T.class)`; a union (`Object`) via
  `JSON.deserializeUntyped`; an enum (`String`) or text is the raw body;
  binary is the `Blob`.
- **The `Box` client** exposes one field per manager, each constructed from
  the shared `BoxClient`.
- **The runtime contract is emitted as stubs** (`BoxRequest`, `BoxResponse`,
  and the `BoxClient` interface) — the Apex analogue of the Go
  `gantryruntime` stubs — so the generated managers compile standalone; the
  hand-written Apex runtime (auth + callout + retry + governor limits) is a
  later slice implementing `BoxClient`.
- **Flat-namespace uniqueness:** manager/client/stub names are minted into a
  set **seeded with every model class name** plus the four reserved runtime
  names, and mangled to the 40-char limit (TR-Apex.1) — so nothing in the
  one namespace collides. Method names are unique within their class.

**Consequences:** on the real spec the backend adds **85 manager classes +
the `Box` client + 3 stubs = 89 classes**, with exactly **336 methods**
(one per operation), all names globally unique and ≤ 40 chars.
`gantry generate --target apex` now writes **2,839 files** (1 project +
1,419 classes + 1,419 metas). Four structural + determinism tests plus the
packaging test's new global-uniqueness assertion pin it. Pagination
(per-type page classes, no generics), chunked upload, and the D-122
serialization remainders ride on this surface in later slices; the Apex
runtime implements `BoxClient`.

## D-124 — Apex identifier shaping: the platform compiler is the oracle (VR-1.3)

**Context.** The first scratch-org dry-run deploy (VR-1.3) of the 1,419
generated classes surfaced three whole classes of invalid identifier that
the structural tests could not have caught, because only the Salesforce
compiler encodes Apex's identifier rules:

1. The reserved-word escape appended a **trailing `_`** (`limit_`, `end_`,
   `value_`) — but Apex forbids an identifier that *ends* in `_`.
2. **Enum constants** (`ASC`, `Date`, `Group`, `Trigger`, `by`) were emitted
   raw, never checked against the reserved list.
3. Wire names with **runs of `__`** (`Box__Security__Classification__Key`)
   and **leading digits** became field identifiers verbatim — both illegal.

**Decision.** `safe_word` is now a full **Apex-identifier sanitizer**, not a
reserved-word-only helper: fold every non-alphanumeric to `_`, collapse
runs, drop leading/trailing `_`, prefix `x` when a letter doesn't lead, then
suffix reserved words with **`_r`** (a letter-terminated escape, so it can't
reintroduce a trailing `_`). It is applied at **every** identifier site —
struct fields, params, manager fields, method names, and enum constants.
Added `by` and `commit` to the reserved list. The wire name (JSON key) is
untouched — it rides the `// wire:` comment today and the serializer later
(D-122), so shaping the identifier never changes the contract.

**Consequences.** A local scan of the regenerated 1,419 classes shows zero
trailing-underscore/`__` identifiers and zero intra-class duplicate fields
(the collapse introduced no collisions on the real spec). Three regression
tests pin the escapes (reserved fields, reserved enum constants, `__`/
leading-digit shaping). This is the VR-1.3 compile-the-output loop doing its
job: the platform is the oracle for exactly the rules no unit test encodes.

## D-125 — Synthesized names use immediate context, not the full ancestry

**Context.** 924 of 1332 IR declarations are synthesized from inline
anonymous schemas. The synthesizer threaded an `owner` string that grew by
one segment at every nesting level, seeded from the (already long) Box
`operationId` — so a 4-deep inline field produced a 109-char type name
(`PutMetadataTemplates…SchemaUpdateBodyDataStaticConfigClassification`), the
exact box-node-sdk failure mode. Apex only hid it by hashing the overflow to
opaque names like `V20250EnterpriseConfigurationCon_0de4df4`.

**Decision.** `lower_type`/`lower_struct`/`lower_union` now thread two things
instead of one accumulating `owner`: `name` (the exact name for a
synthesized type at this position = its parent's leaf + this leaf) and `leaf`
(just this leaf, passed to *children*). Depth therefore adds one segment per
level rather than concatenating the whole path: the deepest enum above is now
`StaticConfigClassification` (26 chars), and `FileFull.type` is `FileFullType`
(12). A named schema seeds its children from its own normalized name.

**Consequences.** On the real spec the longest Go type drops **109 → 83**,
the 60+‑char bucket **56 → 35**, and `<= 25` chars grows **845 → 997**; Apex
opaque hash-mangled class names drop **190 → 124**. Every remaining long name
is now either operation-seeded (the long `operationId` prefix — addressed by
the method-name-shortening slice) or an inherently long Box source name
(named schema + long field). A `lowering` regression test pins the
immediate-context rule and asserts the full-ancestry name is never emitted.
Structural dedupe of identical inline shapes (collapsing repeated `{id}`
references to one shared type) is the follow-on to this slice. Counts and all
100 existing tests are unchanged — naming only.

## D-126 — Method names are shortened; the type seed stays qualified

**Context.** After D-125 killed the *deep* long names, the remaining ones were
all operation-seeded: Box `operationId`s are long (`put_metadata_templates_
enterprise_security_classification_6VMVochwUWo_schema_update` → a 75-char
method name and 83-char inline types). The user approved shortening methods
"but keep the entity" (Box-SDK-flavoured, not terse).

**Decision.** A Box `operationId` is reduced to tokens with two kinds of noise
removed: **opaque id handles** leaked from example URLs (a token mixing an
uppercase letter with a digit, `6VMVochwUWo`) and the **manager-tag echo** (the
call is already `client.<manager>.<method>`). From those tokens:

- The **method name** maps the HTTP verb to a semantic one (`get`→`get`,
  `post`→`create`, `put`/`patch`→`update`, `delete`→`delete`), turns a
  trailing path id into `ById`, and drops interior ids (parent-path context):
  `get_files_id`→`GetById`, `post_folders`→`Create`, `get_folders_id_items`→
  `GetItems`, `post_files_id_copy`→`CreateCopy`. No dictionary distinguishes a
  verb-action from a noun-subresource, so the mapping is uniform.
- The **type seed** (for `…Body`/`…Response`/param inline types) keeps the
  fuller token list, *not* the pretty method name — many operations share a
  pretty name (`GetById`), so a `GetById`-seeded `…Body` would collide, while
  the token-list seed stays operation-unique.

Method names are unique **per (manager, variation)** — they are receiver-scoped
in Go/Apex. A one-vs-two-`{id}` collision (`get_metadata_taxonomies_id` and
`…_id_id` both want `GetById`) falls back to keeping all ids (`GetByIdById`),
then a numeric suffix. Sema still rejects a true duplicate loudly. The Go
backend's package-level `…Options` structs are now manager-qualified, since a
per-manager-unique method name no longer guarantees a package-unique helper.

**Consequences.** Longest Go method **75 → 46**, methods over 40 chars down to
8, exactly **one** numeric-suffix fallback in all 336 methods
(`CreateFilesContent2`). Longest Go inline type **83 → 77** (the residue is now
named-schema + long-field, the dedupe target), the 60+‑char type bucket
**35 → 15**, and Apex opaque hash-mangled class names **124 → 91**. A `lowering`
regression test pins the verb mapping, `ById`, and the collision fallback; all
102 tests, fmt, clippy green. Structural dedupe of identical inline shapes is
the remaining naming slice.

## D-127 — Structural dedupe of identical inline shapes

**Context.** Inline anonymous schemas are 924 of 1332 IR decls, and many are
identical: Box request bodies repeat `{id}` and `{id, type}` reference objects
in dozens of places, each previously minting its own type. That is the bulk of
the type-count and a source of near-duplicate long names.

**Decision.** `synthesize` now dedupes on structure: the `DeclKind`'s Debug
form is a faithful structural key (kind + every field's wire name and type +
enum values + union variants), and the inner `DeclId`s it references are
already canonical because children are synthesized first. An inline shape
identical to one already synthesized **reuses that decl** instead of minting a
copy; the decl *name* is not part of the key, so differently-named copies of
one shape collapse. Dedupe is per document/module (versioned specs keep their
own namespace, G-9) and deterministic (spec order; first occurrence wins the
name). Only *synthesized* decls dedupe — named schemas are never merged.

The kind breakdown in `LoweringStats` is now computed from the final decls
rather than counted during lowering, since a build-time counter would include
the copies dedupe discards.

**Consequences.** The real spec lowers to **900 decls** (was 1332) — **492
synthesized** (was 924), 608 structs / 248 enums / 42 unions / 2 aliases.
Go types **1550 → 1118**; Apex classes **1419 → 987** (and files 2839 → 1975),
opaque hash-mangled Apex names **91 → 69**. Output stays byte-identical across
runs. A `lowering` regression test pins that two identical inline shapes share
one decl while a different shape stays distinct; all 102 tests, fmt, clippy
green. (The longest *type* names — named-schema + long Box field — are not
duplicates, so dedupe leaves them; shortening those would need abbreviation,
not collapse.)

## D-128 — Curated action verbs for custom-method endpoints

**Context.** D-126's uniform verb map rendered Box custom-action endpoints as
`Create<Action>` (`post_files_id_copy`→`CreateCopy`), because no mechanical
rule distinguishes a verb-action from a noun-subresource. The user asked for
the Box-SDK reading (`CopyById`).

**Decision.** A small **curated** list of action verbs — grounded in the real
spec, not guessed — is recognised as the *trailing* operationId token:
`append, apply, ask, authorize, cancel, commit, convert, copy, extract,
resend, revoke, start, trim`. When one trails, it leads the method name and
the HTTP verb drops. `short_op_tokens` now also splits on `:` (Box's
custom-method separator, `levels:append`) so the action becomes its own token.

**Consequences.** `post_files_id_copy`→`CopyById` (across files/folders/hubs/
file_requests), `post_ai_ask`→`Ask`, `post_ai_extract`→`Extract`,
`post_sign_requests_id_cancel`→`CancelById`, `post_metadata_cascade_policies_
id_apply`→`ApplyById`, `post_metadata_taxonomies_…_levels:append`→
`AppendLevels`, `get_authorize`→`Authorize`. Class/method counts unchanged
(987 classes, 336 methods); a `lowering` regression test pins the action-lead
and the `:` split; 103 tests, fmt, clippy, determinism green.

## D-129 — Apex project structure, ApexDoc, and per-endpoint reference docs

**Context.** Three quality requirements before the Apex runtime work: a proper
SFDX project layout, generated code readable by both humans and AI coding
assistants, and Markdown documentation for every endpoint.

**Decision.**

- **SFDX scaffolding.** `generate()` now emits the full project a developer
  expects: `sfdx-project.json` (with `namespace`/`sfdcLoginUrl`),
  `config/project-scratch-def.json` (the same def the VR-1.3 loop deploys —
  now shipped, and the workflow uses it instead of an inline heredoc),
  `.forceignore` (keeps `docs/`+`README.md` out of deploys),
  `manifest/package.xml` (wildcard ApexClass deploy), and a project `README.md`.
- **ApexDoc.** Every generated class and method carries `/** … */` doc:
  managers describe their tag + `client.<field>` access; methods document the
  `HTTP path`, each `@param` (with in-location + optional), `@param body`,
  `@return`, and `@deprecated`; models describe the schema; the `Box` client
  and each manager field are documented. Structural (the IR carries no
  per-operation prose), so it never invents descriptions.
- **Per-endpoint docs.** One Markdown page per endpoint under
  `docs/<manager>/<method>.md`, plus a per-manager index and a top index. Each
  page opens with an **Imports & setup** section — Apex has no `import`
  statement, so it documents the namespace-global model and the one-time
  `Box` client bootstrap — lists the **SDK types used**, tables the parameters,
  states the request body / return type, and closes with a **complete,
  copy-pasteable example** calling the real method (required params as named
  variables, optionals as `null`). Tuned for AI-assistant consumption.

The method signature, its ApexDoc, and its doc page all derive from one
`OpSignature` built in `managers.rs`, so they can never drift; the manager
class names the docs reference come from a shared `manager_infos` minter.

**Consequences.** `generate --target apex` now writes **2,401 files**: 5
scaffolding + 987 classes + 987 metas + **422 docs** (336 endpoint pages + 85
manager indexes + 1 top index). Docs/scaffolding live outside the package
directory, so the deployable surface (and VR-1.3) is unchanged. Deterministic;
new regression tests pin the scaffolding, the doc-per-endpoint count, and a
sample page's setup/types/example. 105 tests, fmt, clippy green.

## D-130 — The hand-written Apex runtime (BoxClient implementation)

**Context.** The generated managers call `client.send(request)` against the
`BoxClient` *interface* stub; nothing ran. The Go analogue is the hand-written
`gantryruntime` package the generated code imports. Apex has no package
imports — every class deploys together in one flat namespace — so the runtime
must ship *inside* the generated project.

**Decision.** A hand-written runtime lives in
`runtimes/apex/main/default/classes/*.cls` (the source of truth) and is
**embedded** into every generated project under `classes/` via `include_str!`
(`crates/gantry-backend-apex/src/runtime.rs`), behind the generated
`BoxClient`/`BoxRequest`/`BoxResponse` contract:

- **`BoxHttpClient implements BoxClient`** — resolves the D-106 base-URL class,
  attaches the bearer token + the request's headers/query/body, sends the
  Salesforce HTTP callout, and maps a non-2xx response to a `BoxApiException`.
- **`BoxTokenProvider`** (auth contract) + **`BoxDeveloperTokenProvider`** (the
  simplest flow, a fixed token). CCG/JWT providers are later slices.
- **`BoxApiException`** — carries HTTP status + response body (the manifest
  error model, D-107).

**Governor limits shape the retry policy (a genuine Apex constraint):** Apex
has no `sleep`, so retries are **immediate** rather than backed-off, bounded by
the per-transaction callout limit; they cover 429/5xx, with a single 401
re-attempt for a caching token provider. The runtime's class names are
reserved in the flat-namespace minter so no generated class collides.

**Consequences.** `generate --target apex` now ships **991 classes** (was 987:
+4 runtime), all deployable. The generated SDK is now actually callable:
`new Box(new BoxHttpClient(new BoxDeveloperTokenProvider(token))).files.getById(...)`.
Verified by the scratch-org compile loop (VR-1.3). New regression tests assert
the runtime classes are present; 105 tests, fmt, clippy, determinism green.

## D-131 — Governor-limit-aware Apex pagination (documented, no extra classes)

**Context.** Box paginates by `marker` (opaque cursor + `next_marker`) or
`offset` (numeric), detected structurally by the shared synth pass
(`detect_pagination`, FR-7.3) — the same source the Go backend uses. Go lowers
each to a lazy `iter.Seq2` that auto-fetches every page. Apex can't: there are
no lazy iterators, and the per-transaction callout governor limit (100) forbids
auto-fetching an unbounded number of pages.

**Decision.** No sugar. The base method already returns the paginated
envelope — the response struct carries both this page's `entries` and the
cursor for the next page (`next_marker`, or the running `offset`). That
envelope *is* the page, so an extra `…Page` helper and a per-operation page
class would only re-wrap high-fidelity data in a lossy `List<Object>`. We keep
the surface lean: pagination is supported by the regular method + envelope, and
the explicit cursor loop is **documented** in each paged endpoint's `docs/`
page (the `## Pagination` section) rather than generated as code.

```apex
// marker style — the envelope carries entries + next_marker
Items page = client.folders.getItems(folderId, null /* marker */, limit);
while (String.isNotBlank(page.next_marker)) {
    // handle page.entries
    page = client.folders.getItems(folderId, page.next_marker, limit);
}
```

The caller loops **explicitly**: call, check the cursor, feed it back — one
page per callout, staying within governor limits. Offset style advances a
client-side `Long offset` by `page.entries.size()` until a page is empty.

**Consequences.** `generate --target apex` still ships **991 classes** — no
class-count churn for pagination. Reuses `gantry-synth`'s `detect_pagination`
(the 64 paged ops) purely to flag the docs: the manager index gains a **Paged**
column and each paged endpoint page gains a runnable `## Pagination` cursor
loop, rendered from the same `OpSignature` the manager code is (name, param
order, and envelope type can't drift). Deterministic; a `model_shapes`
regression pins the pagination-doc section; 105 tests, fmt, clippy green.

## D-132 — Apex field ↔ wire serialization remap

**Context.** Apex's `JSON.deserialize(body, T.class)` matches JSON keys to
instance-variable names (case-insensitively) with **no** wire-name mapping —
there is no `@JsonProperty` equivalent. But Box wire keys routinely can't *be*
Apex identifiers: reserved words (`limit`, `value`, `from`, `group`, …, ~90 of
them), `$`-prefixed metadata keys (`$parent`, `$id`, `$template`, …), `__` runs
(`Box__Security__Classification__Key`), and digit-leading keys. The name is
shaped to a legal identifier (`limit`→`limit_r`, `$parent`→`parent`,
`Box__…`→`Box_…`), at which point native deserialize **silently drops** the
value on read and emits the wrong key on write. Until now the wire name lived
only in a `// wire:` comment — informational, unused at runtime.

**Decision.** The only thing broken is the **key names**; native
`JSON.deserialize` still converts every value type (dates, blobs, nested
structs, lists) correctly once the keys match. So remap keys on the *untyped*
JSON tree, type-directed, around native (de)serialization — no hand-rolled
per-type value marshaling. A struct is **affected** iff it has a name-mismatched
field or transitively contains an affected struct (fixpoint). Only affected
structs get two generated statics:

```apex
public static Map<String, Object> normalizeKeys(Map<String, Object> raw) { … } // wire → Apex (read)
public static Map<String, Object> denormalizeKeys(Map<String, Object> raw) { … } // Apex → wire (write)
```

Each renames only its own mismatched keys and recurses **only** into affected
children (structs, and lists/maps of them). Free-form `Object`/`Map<String,
Object>` fields — which carry keys like `$id` as *data* — are never touched, so
metadata blobs pass through intact. The managers route an affected response
through `deserializeUntyped → normalizeKeys → serialize → deserialize(T.class)`
and an affected request body through `serialize → deserializeUntyped →
denormalizeKeys`; the 872 clean classes keep the direct native path unchanged.

**Consequences.** Correctness fix, no class-count change (**991 classes**);
**119** carry remap methods. Leans entirely on native type conversion — the
generated code only moves keys. Deterministic; `model_shapes`/`manager_shapes`
regressions pin the affected/clean split, the nested-recursion shape, and the
both-way manager routing; fmt, clippy (`-D warnings`) green.

**Union variants (follow-up, done).** A discriminated union dispatches inside
`parse` via `JSON.deserialize(JSON.serialize(untyped), Variant.class)`, which
would drop an affected variant's renamed keys just like a bare response would.
Each variant whose struct is affected (5 in the real spec — `FileFull`,
`Folder`, `FolderFull`, `SearchResults`, `SearchResultsWithSharedLinks`) now
routes through `Variant.normalizeKeys(untyped)` first; clean variants stay on
the raw map. A `model_shapes` regression pins the mixed union (clean variant
bare, affected variant normalized). Still open: the tri-state absent-vs-null
distinction, tracked separately.

## D-133 — Generated Apex test suite for the 75% coverage deploy gate

**Context.** Salesforce refuses a production deploy unless ≥ 75% of the org's
Apex lines are covered by running tests (`mandated_test_coverage: Some(75)` on
the Apex manifest — a first-class capability axis, not a Go/Rust concept). A
generated SDK that can't clear the gate can't be deployed, so the SDK must ship
its own tests. Apex tests also can't make real callouts, so any code behind the
`BoxClient` contract needs a mock.

**Decision.** Generate an `@isTest` suite (`tests.rs`), emitted only when the
manifest mandates coverage, that **exercises** the generated layer from the same
IR it covers (so it never drifts):

- **`BoxCalloutMock`** — an `@isTest` `BoxClient` returning a caller-tunable
  canned `BoxResponse` (no callout), recording the last request.
- **`Box<Manager>Test`** (one per manager, 85) — a single method that constructs
  the manager with the mock and calls **every** operation inside
  `Test.startTest/stopTest`, shaping the canned body to each return type (`[]`
  for array responses, a `Blob` for binary, `{}` otherwise) and passing
  syntactic placeholders for arguments (scalars as literals, containers empty, a
  model body via its no-arg constructor). This covers request-building,
  deserialization, and — through the response/body types — the D-132 key remap
  (a non-null model body also exercises `denormalizeKeys`).
- **`BoxUnionsTest`** — drives each discriminated union's `parse` dispatch with a
  known tag (selects a variant) and an unknown tag (open-union fallthrough).

**Consequences.** `generate --target apex` now ships **1,078 classes** (was 991:
+85 manager tests, +the mock, +the unions test). Deterministic; a `model_shapes`
regression pins the mock/​manager-test/union-test shapes and the file counts;
fmt, clippy (`-D warnings`) green. Line coverage is a *platform* measurement, so
the actual ≥ 75% is confirmed on-platform by VR-1.3 (scratch-org quota permitting
— the generated tests compile-check there too). The hand-written runtime keeps
its own tests under `runtimes/apex/**`; runtime callout-coverage via
`HttpCalloutMock` is a follow-up. Tri-state absent-vs-null remains the last open
serialization item.

## D-134 — Client Credentials Grant token provider (Apex runtime auth)

**Context.** The runtime shipped only `BoxDeveloperTokenProvider` — a fixed
developer token that expires in ~60 minutes, unusable for a real integration. Of
Box's server-to-server flows, **Client Credentials Grant (CCG)** needs only the
app's client id/secret (no signing key), so it works on any org; **JWT** needs
`Crypto`-signed assertions and is heavier. CCG is the right first production
auth.

**Decision.** Add `BoxCcgTokenProvider` (hand-written runtime, TR-Apex.6). It
POSTs `grant_type=client_credentials` with the client id/secret and the subject
(`enterprise`/`user` + id) to `oauth2/token`, then **caches** the access token
and refreshes it a minute before its `expires_in` so a token never lapses
mid-request. It reads `expires_in` through its string form (JSON numbers decode
as Integer/Long/Decimal), defaulting to Box's usual hour.

The `BoxTokenProvider` interface gains **`invalidate()`** — the missing half of
the 401-refresh the interface already documented. `BoxHttpClient` now calls
`this.tokens.invalidate()` before its single 401 re-attempt, so a token revoked
*ahead* of its expiry (which the expiry cache alone wouldn't catch) is discarded
and re-minted. `BoxDeveloperTokenProvider.invalidate()` is a no-op (a static
token can't refresh).

**Consequences.** The SDK now authenticates server-to-server without a
hardcoded developer token. Runtime grows from 4 to 6 classes
(`BoxCcgTokenProvider` + `BoxCcgTokenProviderTest`), so `generate --target apex`
ships **1,080 classes** (993 base + the 87-class `@isTest` suite). The CCG test
uses `HttpCalloutMock` to cover mint / cache / invalidate / non-2xx — seeding the
runtime-test coverage that D-133 deferred (`BoxHttpClient`'s own `HttpCalloutMock`
coverage is still a follow-up, as is JWT). As hand-written Apex, on-platform
compile is confirmed by VR-1.3 when the scratch-org quota permits;
`model_shapes` pins the packaging, and generation stays deterministic.

## D-135 — JWT (server auth) token provider

**Context.** After CCG (D-134), the other Box server-to-server flow is **JWT**:
prove possession of an RSA private key by signing a short-lived JWT assertion
(RS256) and exchanging it for a token. Some Box apps are provisioned JWT-only, so
the SDK needs it. The hard part is key handling — Apex's `Crypto.sign` can't
decrypt Box's passphrase-protected PEM, and a raw private key must never sit in
Apex or source.

**Decision.** Add `BoxJwtTokenProvider` that signs through **Salesforce
Certificate and Key Management**: the developer imports the RSA key into the org
and registers the public key with Box (which returns a public-key id); the
provider signs with `Crypto.signWithCertificate('RSA-SHA256', …, certDevName)`
and tags the JWT header `kid` with the public-key id. The key stays in the
platform keystore — never in Apex, never in the SDK. It builds
`base64url(header).base64url(claims).signature` (claims: `iss`/`sub`/
`box_sub_type`/`aud`/a random `jti`/`exp` = now + 45s), POSTs the
`jwt-bearer` grant, and reuses the same cache-and-refresh shape as CCG.

Signing is a `@TestVisible protected virtual` seam so the token exchange is
testable: `BoxJwtTokenProviderTest` overrides it with a fixed signature and drives
mint / cache / invalidate / non-2xx through `HttpCalloutMock`. The one line that
needs a real org certificate (`signWithCertificate`) is the only path a test
can't reach.

The two providers' identical cache-and-refresh logic — token cache,
refresh-before-expiry, `invalidate()`, and the token-endpoint exchange — is
factored into an abstract **`BoxCachingTokenProvider`** base; each provider
supplies only its `tokenRequestBody()`. That base also normalizes every exchange
failure (callout, non-2xx, unparseable body, missing/mistyped token) to
`BoxApiException` in one place, so callers (and `BoxHttpClient`'s 401 path) never
see a raw `CalloutException`/`TypeException`. A related fix: `BoxHttpClient` now
tracks the single 401 refresh re-attempt separately from the transient-retry
budget, so the refresh still fires when `maxRetries` is 0.

**Consequences.** The SDK now supports both server-to-server flows. Runtime grows
4 → 9 classes (the caching base, the CCG + JWT providers, and their tests), so
`generate --target apex` ships **1,083 classes** (996 base + the 87-class
`@isTest` suite). `model_shapes` pins the packaging; fmt, clippy (`-D warnings`),
determinism green. On-platform compile is confirmed by VR-1.3 when the
scratch-org quota permits. Remaining runtime follow-up: `BoxHttpClient`'s own
`HttpCalloutMock` coverage. The tri-state absent-vs-null distinction is the last
open serialization item.

## D-136 — Chunked-upload orchestrator (Apex runtime), bounded by platform limits

**Context.** Box's chunked upload is a three-step protocol — create an upload
session, PUT each part with its byte range + a per-part SHA-1 digest, then commit
with the whole-file SHA-1 and the ordered part list. The generated
`BoxChunkedUploads` manager exposes each raw call, but hand-wiring the digests,
`Content-Range` headers, slicing, and commit is exactly the error-prone glue a
caller shouldn't write. Two Apex platform limits, though, bound what's possible:
**heap** (6 MB sync / 12 MB async — the content is an in-memory `Blob`) and the
absence of any native **`Blob` byte-slice**.

**Decision.** Add `BoxChunkedUpload` (hand-written runtime) that orchestrates the
protocol — `upload(content, name, folderId)` for a new file,
`uploadVersion(content, name, fileId)` for a new version. It creates the session,
slices the content into `part_size` parts, PUTs each with `Digest: sha=<base64
sha1>` and `Content-Range`, then commits with the whole-file digest and the part
list, returning the `Files` envelope.

**A reference implementation, not a working upload path (CodeRabbit review).**
The heap limit alone excludes the helper for *real* Box uploads, and the
code/docs now say so plainly. Box only offers chunked upload for files ≥ 20 MB,
but a ≥ 20 MB `Blob` can't fit the 6/12 MB heap, so no size satisfies both Box's
minimum and Apex's ceiling — this is the airtight blocker, independent of
slicing. (The slicing limit is secondary: Box sets `part_size` from the session,
not documented to be any particular value, but the sizes it issues in practice —
e.g. 8 MB — are powers of two, which aren't 3-aligned, so the base64-slice guard
rejects those.) `BoxChunkedUpload` therefore stands as a **correct,
mock-verified reference implementation** of the protocol — failing loudly (a
`BoxApiException`, never a raw `LimitException`) at each limit — while the
production path (an out-of-transaction, `Queueable`-chained uploader plus a
byte-accurate slicing mechanism) is a tracked follow-up. Two review nits are
also folded in: the session is aborted best-effort on a mid-upload failure (no
orphaned session), and the test asserts distinct per-part `Content-Range`,
digest, and byte size plus the ordered commit part list.

**Consequences.** Runtime grows 9 → 11 classes (`BoxChunkedUpload` +
`BoxChunkedUploadTest`), so `generate --target apex` ships **1,085 classes**. The
test uses `HttpCalloutMock` across the three endpoints with a 3-aligned part size
to exercise the multi-part slice/digest/range/commit path, plus the oversize and
non-aligned rejections. `model_shapes` pins the packaging; fmt, clippy
(`-D warnings`), determinism green. As hand-written Apex it's confirmed on-platform
by VR-1.3 when the scratch-org quota permits. The helper references the generated
`BoxChunkedUploads` surface by name (create/update/commit + the session/part
models), so it's coupled to those generated identifiers — acceptable for a Box-
specific runtime that ships embedded with each generation.

## D-137 — HttpCalloutMock coverage for the Apex HTTP client

**Context.** `BoxHttpClient` is the one runtime class that turns a structural
`BoxRequest` into a real Salesforce callout — URL building, the bearer token,
transient retries, the single 401 refresh, the bounded retry budget, and mapping
a non-2xx response to `BoxApiException`. Every other runtime class (the token
providers, the chunked-upload helper) shipped with its own `HttpCalloutMock`
test, but the HTTP client itself did not — and VR-1.3 had just caught two real
compile bugs in its `send()` loop (a `while (true)` that Apex won't treat as
always-returning, and a retry bound that could overrun the 100-callout limit),
so the exact method every generated manager depends on was the least-tested.

**Decision.** Add `BoxHttpClientTest` (hand-written runtime, embedded like the
rest) driving the client's callout paths through an `HttpCalloutMock`: a
2xx success (asserting the built endpoint = base-URL class + path + encoded
query, and the method), a 401 that refreshes once and then succeeds, a
persistent 401 that gives up, a transient 5xx retried to success, exhausted
transient retries surfacing the last status, a non-retriable 4xx carrying the
raw error body, JSON-body serialization with `overrideBaseUrl`, an unknown
base-URL class rejected before any callout, and a null token provider rejected
at construction. Because Apex's `HttpRequest` exposes no `getHeader()`, the
`Authorization`/refresh behaviour is asserted through a counting token provider
(a token read per attempt; `invalidate()` before the 401 re-attempt) rather than
by inspecting the sent header.

**Consequences.** Runtime grows 11 → 12 classes, so `generate --target apex`
ships **1,086 classes**. `model_shapes` pins the new count; fmt, clippy
(`-D warnings`), determinism green. As hand-written Apex it's confirmed
on-platform by VR-1.3. This closes the last untested runtime class — every
runtime `.cls` now has direct callout coverage.

## D-138 — Explicit null (absent-vs-null) is the serializer's concern

**Context.** D-110 split the tri-state structurally: `Optional<T>` = the key may
be **absent**, `Nullable<T>` = the wire value may be an explicit **null**, and it
recorded that "Apex erases both at the type level and handles the difference in
serializers." Box uses an explicit JSON `null` to *clear* a field on update, and
an absent key to leave it unchanged — two different requests. But the Apex
runtime serialized every body with `JSON.serialize(body, true)` (suppress-nulls),
so an explicit `null` was indistinguishable from an unset field: **you could not
clear a Box field.** The type erasure (D-110) means a plain Apex instance field
is `null` whether the caller never touched it or set it to clear — the intent has
to be carried out of band.

**Decision.** Move the null policy from the runtime into the generated managers,
and give the caller a way to express "send explicit null":

- **Runtime.** `BoxRequest` gains `Boolean suppressNulls = true`; `BoxHttpClient`
  serializes `JSON.serialize(request.body, request.suppressNulls)`. The default
  keeps the 105 simple body paths byte-identical.
- **Managers.** A body whose type reaches a **write-affected** struct is reduced
  to a `Map` of only its set keys (`JSON.serialize(body, true)` → untyped), key-
  remapped Apex → wire, then handed to the runtime with `suppressNulls = false` —
  the map now carries exactly the intended keys, including any intentional nulls.
- **Models.** A struct that is (a) reachable from a JSON request body and (b) has
  a `Nullable` field ("null-writable") gains `public Set<String> fieldsToNull`;
  the caller adds the Apex field name of a field to clear. Its `denormalizeKeys`
  injects `"<wire>": null` for each listed `Nullable` field, then drops the
  control key so it never reaches Box. Only `Nullable` fields are injectable — an
  absent-only (`Optional`) field left unset simply stays absent.
- **Read vs write split.** The D-132 "affected" notion splits: **read-affected**
  (name-mismatch or transitively contains one) still drives `normalizeKeys` and
  the response/union-parse path; **write-affected** (read-affected ∪ null-writable
  ∪ their containers) drives `denormalizeKeys` and the request path. The transform
  recurses only into the direction-appropriate set, so responses are untouched by
  the null machinery.

Usage:

```apex
PutIdBody7 body = new PutIdBody7();
body.name = 'New name';              // set → sent
body.fieldsToNull.add('enterprise'); // explicit null → Box clears it
// every other field is unset → absent, unchanged
```

**Consequences.** In the real spec 22 structs are body-reachable with a nullable
field (of 146 null-bearing / 608 structs), so the surface is small and no new
classes are added — `generate --target apex` still ships **1,086 classes**. Class
content changes only; `model_shapes`/`manager_shapes` pin the new shape, plus new
generator tests for the injection and the response-only negative case. fmt, clippy
(`-D warnings`), determinism green; on-platform via VR-1.3. This closes the last
Apex correctness gap before M5 (Rust). A known limitation: because the tri-state
erases (D-110), explicit null is opt-in through `fieldsToNull` (Apex can't infer
"set to null" from a plain field), which is the honest ceiling of the platform.

## D-139 — Apex live smoke (VR-1.4) + shipped Remote Site Settings

**Context.** VR-1.3 proves the generated Apex SDK *compiles and deploys* on the
platform, but not that it *works* — the Go SDK cleared a higher bar (VR-7: a live
smoke against a real Box account). Two things blocked an Apex equivalent:

1. **Callouts were impossible in any org.** Apex blocks a callout to any host
   without a **Remote Site Setting** (or Named Credential); the generated project
   shipped none, so every request would throw "Unauthorized endpoint" — the SDK
   was undeployable-into-usefulness, not just untested.
2. **Real callouts can't run in an Apex test.** Unit tests must mock callouts, so
   the only way to exercise the SDK against live Box is *anonymous Apex*.

**Decision.**
- **Ship Remote Site Settings.** The generator now emits one
  `RemoteSiteSetting` per Box host — `api.box.com`, `upload.box.com`,
  `account.box.com`, `dl.boxcloud.com` (the D-106 base-URL classes collapsed to
  origins) — under `force-app/main/default/remoteSiteSettings/`, added to the
  wildcard `package.xml`. The runtime sets its own `Authorization: Bearer` header
  and builds absolute URLs, so a Remote Site Setting (not a Named Credential) is
  the right fit. This makes the SDK usable out of the box, independent of the
  smoke.
- **VR-1.4 live smoke** (`.github/workflows/apex-livesmoke.yml`, manual): deploy
  the SDK to the configured org (the Dev Hub `SFDX_AUTH_URL` authorizes — no
  scratch org, so the daily signup limit never applies; the classes persist and
  are overwritten each run) with a *real* deploy, then run anonymous Apex
  (`runtimes/apex/smoke/livesmoke.apex`) that mints a token via
  **Client Credentials Grant** and makes two live calls — `GET /users/me`
  (auth + typed deserialization) and `GET /folders/0/items` (pagination + the
  `limit`→`limit_r` wire remap). Credentials come from repo secrets
  (`BOX_CLIENT_ID`/`SECRET`/`ENTERPRISE_ID` + `SFDX_AUTH_URL`); absent any, the
  job skips, so a dry run still passes. Success requires the run to succeed and a
  `SMOKE OK` sentinel in the returned debug log.

**Consequences.** The generated tree gains 4 Remote Site Setting files (no new
classes — still **1,086**); `model_shapes` pins them and the file count. The
smoke is the Apex analogue of Go's VR-7 and is the last major assurance gap
before "shipped". CCG was chosen over the developer-token flow because it mints a
real token (exercising the auth callout) and needs no per-org certificate the way
JWT would. Both VR-1.3 and VR-1.4 target the configured org directly rather than
spinning up a scratch org, so the Dev Hub's daily scratch-org signup limit is
never on the critical path.
## D-140 — `Object`-field deserialization (unions / free-form JSON)

**Context.** VR-1.4 (the live smoke, D-139) immediately earned its keep: CCG auth
and `GET /users/me` worked end-to-end against real Box, but `GET /folders/0/items`
threw `System.JSONException: Apex Type unsupported in JSON: Object`. Apex's typed
`JSON.deserialize(str, T.class)` **cannot populate a field typed `Object`** when
the JSON provides a value for it. Structural unions and `JsonValue` erase to
`Object` (a union field renders as `Object`; the caller dispatches with
`<Union>.parse`), so `Items.entries` is `List<Object>` — and the moment a real
response carries entries, deserialization fails. The mocked `@isTest` suite
returned *empty* lists, so it never populated an `Object` element; only the live
smoke, with real data, exposed it. In the spec **105 of 608 structs** are
object-bearing and **75 of 275 JSON operations** reach one — a widespread runtime
gap, invisible to compile-checks and mocks.

**Decision.** A struct that transitively reaches an `Object` leaf (a union or
`JsonValue`) is **object-bearing** and gets a generated static
`deserialize(Object)` — the read-path analogue of the D-132/D-138 machinery.
It **detaches** the fields that reach an `Object` (so native typed deserialize
never sees them), deserializes the **typed shell** (renamed first via
`normalizeKeys` if the struct is also read-affected), then **reattaches** the
detached fields from the raw untyped tree:

- an `Object` leaf (union / `JsonValue`) → assigned raw (the caller inspects it /
  calls `<Union>.parse`);
- `List<Object>` / `Map<String, Object>` → reattached whole with a cast;
- a nested object-bearing struct → recurses into that struct's `deserialize`;
- `List`/`Map` of object-bearing structs → looped element-by-element.

Managers route a response reaching an object-bearing struct through the builder
(`JSON.deserializeUntyped` → `deserialize`), taking priority over the D-132 remap
path; the union `parse` dispatch routes an object-bearing variant through its
`deserialize` too.

**On-platform corrections (caught by VR-1.3).** The first cut named the reattach
temporaries with a leading double underscore (`__d_entries`, `__lst0`, …), which
Apex forbids — identifiers must begin with a letter and may not contain `__`
(reserved for managed-package namespaces). The dry-run deploy failed with
`Invalid character in identifier`. Fixed to letter-prefixed names (`d_<field>`
for the detached field temp; `dLst`/`dEl`/`dEv`/`dMap`/`dSm`/`dK`/`dMv` for the
loop locals), mirroring the D-132 `w`-prefix convention. Separately, the
collection-rebuild branches assigned a fresh empty `List`/`Map` when the source
was null or absent, so a nullable/optional collection of object-bearing structs
deserialized as empty rather than null; they now guard `src == null` → assign
`null`, and cast directly (a malformed non-container value fails loudly on the
cast instead of silently emptying).

**Consequences.** 105 structs gain a `deserialize` builder (no new classes —
still **1,086**); `model_shapes`/`manager_shapes` gain generator tests for the
detach/reattach, the `List<Object>` reattach, nested recursion, and the manager
routing. fmt, clippy (`-D warnings`), determinism, full workspace green;
on-platform via VR-1.3, and VR-1.4 re-run confirms `getItems` now round-trips a
populated union list against real Box. This is exactly the class of correctness
bug the live smoke exists to catch.

## D-141 — Target-neutral conformance (VR-3 for Apex) + the two platform exclusions

**Context.** The conformance checklist (VR-3) proves the generated SDK expresses
the full R§1 contract — every manager, operation, paged surface, auth flow,
test, doc, and traceability record. It was written for Go and, despite a
target-neutral doc comment, measured the output with **Go-specific markers**
(`managers/*.go`, `(ctx context.Context`, `serialization/serialization.go`,
`buildinfo/buildinfo.go`). CI only ran `conform --target go`, so the Apex SDK —
functionally complete and VR-1.3/VR-1.4 green — had **no machine-checked parity
proof**, which the v2 acceptance criterion requires ("conformance parity with v1
minus manifest-documented platform exclusions, each recorded in `DECISIONS.md`").

**Decision.** Introduce a `TargetShape` in `gantry-verify`: a set of per-capability
recognizers plus a list of documented platform exclusions. `conformance()` still
derives the **expected** surface from the verified program (target-neutral) and
now measures the **actual** surface through the shape, so one contract checks Go,
Apex, and Rust by swapping recognizers, never by forking the checklist. A
capability whose shortfall is fully covered by a documented exclusion is reported
`Excluded` (n/a) and passes the gate; any larger shortfall still `Fail`s. The
Apex shape measures the real `.cls` surface — 85 manager classes (the em-dash
`resource manager —` ApexDoc), 336 operation methods (the HTTP-verb ApexDoc
line), 64 paged surfaces (`## Pagination`), the generated `@isTest` suite, the
three token-provider classes, `BoxBuildInfo`, and the four `docs/` guides.
`conform --target apex` runs in CI alongside Go.

Making the check honest surfaced two real gaps and two genuine platform
exclusions:

- **Traceability (fixed).** Apex stamped the engine version in every header but
  carried **no spec fingerprint** (Go's `buildinfo` does). Threaded a `BuildInfo`
  (engine + `SpecSet::fingerprint`) into `gantry_backend_apex::generate` and emit
  `BoxBuildInfo.cls` — on-platform constants `ENGINE_VERSION`/`SPEC_FINGERPRINT`
  (a pure-constant class, so it never affects the 75% coverage gate) — plus the
  fingerprint in the project `README`.
- **Docs guides (fixed).** Apex emitted only `docs/README.md`; added the three
  cross-cutting topic guides (`docs/auth.md`, `docs/pagination.md`,
  `docs/errors.md`), Apex-flavored (token providers, the D-131 cursor loop, the
  exceptions error model). `.forceignore`d like the rest of `docs/`.
- **Serialization (documented exclusion).** Apex erases the tri-state at the type
  level — every reference is nullable and `Date` is native, so absent-vs-null is
  handled inline by the wire remap (D-138). There is no `Nullable[T]`/`Date`
  package to emit; the capability is genuinely N/A.
- **Interactive OAuth (documented exclusion).** OAuth 2.0 authorization-code needs
  a browser redirect/callback that server-side Apex cannot perform. The three
  server-to-server flows (Developer Token, CCG, JWT) ship as runtime classes; the
  auth guide documents that OAuth is unavailable on-platform.

**Consequences.** `conform --target apex` reports **9 capabilities, 2 excluded, 0
failing — PASS**: managers 85/85, operations 336/336, pagination 64/64,
round-trip-tests 86, traceability 1, docs-guides 4, with serialization and one
auth flow marked n/a against their recorded reasons. Apex gains one class
(`BoxBuildInfo`, **1,087**) and three guide docs. A new
`the_generated_apex_sdk_is_conformant` real-spec test plus five unit tests
(including a documented-exclusion case) cover the harness. fmt, clippy
(`-D warnings`), determinism, and the full workspace are green; VR-3 for Apex now
gates every CI run. This closes the conformance half of the v2 "shipped" bar;
the packaging/ship-artifact (NF-8) remains.

## D-142 — Apex ship artifact: unlocked 2GP package (NF-8)

**Context.** NF-8 requires each release to define its ship artifact and the
pipeline to produce it, with the packaging decision recorded here. Go ships a
tagged Go module; Apex needs its own answer. A namespace org is now registered:
namespace **`unbox`**, package name **"Unbox Salesforce SDK"** (org `00D8Y…`; the
registration login lives in the private ops runbook, not here).

**Decision.** Ship the Apex SDK as an **unlocked second-generation (2GP)
package** under the `unbox` namespace — not a managed package. The SDK is
regenerated wholesale from the Box spec on every version; a **managed** package
permanently locks its released global/public members (you cannot remove or
rename them), which fights a spec-driven regenerator the moment Box removes or
renames a field. An **unlocked** package is namespaced and upgradable in place
but imposes no such lock, so each regenerated version is free to add, remove,
and rename members — the right fit for a generated, versioned artifact that
tracks an upstream spec. (Unlocked also skips the security review and keeps the
source visible, both fine for an SDK.)

The generated SFDX project now carries the packaging identity: `sfdx-project.json`
sets `namespace: "unbox"` and a `packageDirectories` entry naming the package with
`versionNumber: "0.1.0.NEXT"` (the `.NEXT` build segment auto-increments; the
major.minor comes from the FR-9 spec-diff — a breaking change is a major bump).
`packageAliases` is emitted empty for the one-time `sf package create` to fill.
The namespace is a separate prefix (`unbox.ClassName`), so it does **not** eat
into the 40-char Apex identifier budget — no name-mangling change. A plain source
deploy to a non-namespace org (VR-1.3's Dev Hub dry-run) ignores the namespace,
so the compile gate is unaffected; the namespace + members are exercised for real
by `sf package version create`, which compiles the whole SDK in the namespace and
emits the installable `04t` version — the ship artifact. The generated README's
Packaging section documents the one-time `sf package create` and the per-version
`sf package version create`/`sf package install` flow.

**Scope of this slice (configure + record).** Namespace + package wired into the
generated project, decision recorded here, packaging flow documented, and the
sfdx-project shape covered by a `model_shapes` assertion. No new CI job and no
new secrets. The `unbox` namespace is **linked to the configured Dev Hub's
Namespace Registry**, and the existing `SFDX_AUTH_URL` secret authenticates to
that same Dev Hub — so producing a version needs no new secret. (Dev Hub and
namespace-org operational details live in the private ops runbook, not here.)

**The build job.** `apex-package.yml` (manual `workflow_dispatch` — version
builds are slow and count against Dev Hub limits, so not a per-push gate)
generates the SDK, auths the Dev Hub, resolves the package `0Ho…` id (creating
the package on the first run), injects that id as the alias the regenerated
`sfdx-project.json` omits, then runs `sf package version create` — which compiles
and runs every generated test in the `unbox` namespace — and surfaces the
installable `04t…` `SubscriberPackageVersionId` in the job summary. Like VR-1.3
and VR-1.4, the workflow is validated by its first dispatch (no Salesforce
toolchain runs in unit CI).

**Consequences.** The Apex ship artifact is defined, producible on demand, and
built by CI (`apex-package.yml`); `sf package version create` compiles it from the
generated project. No class-count change (still **1,087**); output stays
deterministic. fmt, clippy (`-D warnings`), and the full workspace are green. With
VR-3 (D-141) green and the build job wired, NF-8 for Apex is met — the only
remaining step is the workflow's first manual dispatch (which mints the initial
`04t…` version on-platform), the same first-dispatch validation VR-1.3/1.4 use.

## D-143 — TypeScript as the fourth SDK target (v4)

**Context.** With v1 (Go) shipped, v2 (Apex) essentially complete, and v3 (Rust)
next, a fourth target is added to the roadmap: **TypeScript**, positioned *after*
Rust (v4 / M6). The trigger is TypeScript 7.0 (GA 2026-07-08, codename "Project
Corsa"), which is a **native port of the compiler to Go** — a file-by-file rewrite
of the old JavaScript ("Strada") codebase that preserves type-checking semantics
but runs ~10× faster (Microsoft's VS Code benchmark: 125.7s → 10.6s, ~11.9×) with
6–26% less memory.

**Decision.** Adopt TypeScript as a first-party target (not a plugin — non-goal 3
still stands for *third-party* languages). Two things make it attractive:

- **Best IR fit of any target.** The type system expresses the IR's own shapes
  almost directly — `oneOf` → discriminated unions (`{ type: 'a' } | { type: 'b' }`),
  unknown discriminators → a catch-all member, open enums → string-literal unions,
  and the tri-state maps to the type system with **no wrapper**: absent is
  `field?: T`, explicit null is `T | null`. None of Apex's erasure/dispatch work
  and none of Rust's borrow/lifetime concerns apply. The effort concentrates on the
  runtime and the packaging surface, so TR-TypeScript is estimated a touch below
  Rust: **300 (90) hrs**.
- **A fast verification gate.** TypeScript 7's Go-native `tsc --noEmit` type-checks
  the full generated SDK quickly enough to be a per-commit signal — the TS analogue
  of Go's `go build` (VR-1.5). ("Backed by Go" is about compiler *speed*; the
  generated SDK is ordinary TypeScript.)

The v4 acceptance criteria mirror v1/v3: `tsc --noEmit` clean under `strict`,
Prettier-clean, conformance parity with v1, round-trip (incl. unknown-discriminator
retention), generated tests + docs, VR-7 live smoke, and an NF-8 ship artifact
(`npm publish --dry-run` clean, shipped `.d.ts`, dual ESM/CJS).

**Consequences.** A roadmap/spec addition only — **no engine code yet** (M6 is
after M5/Rust). Recorded in `NEW_ENGINE_REQUIREMENTS.md` (scope line, roll-up row,
a TR-TypeScript section, a v4 release row, VR bumped for VR-1.5), `PROGRESS.md`
(v4 row + milestone M6), `PLAN.md`, `README.md`, and `SCOPE.md`. Total scope grows
**3,110 (938) → 3,430 (1,034)** hrs; because v4 starts at 0%, the overall
effort-weighted figure steps **~84% → ~78%** even though no completed work was
lost. The three-target IR (FR-2) was designed to absorb new targets through
manifest + lowering + printer only (FR-6.1); TypeScript is the first test of that
beyond the original three.

## D-144 — Remote Site Setting source-format suffix (2GP packaging fix)

**Context.** The first `apex-package.yml` dispatch (D-142) got all the way through
auth and package creation — it registered the unlocked package **"Unbox Salesforce
SDK"** (`0Ho…`) on the Dev Hub — then `sf package version create` failed:

> `TypeInferenceError: …/remoteSiteSettings/Box_account_box_com.remoteSiteSetting-meta.xml:
> Could not infer a metadata type — Did you mean ".remoteSite-meta.xml" …?`

The generated Remote Site Settings (D-139) used the file suffix
`.remoteSiteSetting-meta.xml`. That mixes the MDAPI type suffix with the source
`-meta.xml` convention; the SDR registry's canonical **source-format** suffix for
`RemoteSiteSetting` is `remoteSite`, i.e. `<name>.remoteSite-meta.xml`. The 2GP
build does a strict source→MDAPI conversion and rejects the unrecognized suffix.

Notably `sf project deploy start` (VR-1.3/1.4) is *lenient* and tolerated the wrong
suffix, so the bug hid behind two green harnesses — the packaging path is the first
strict validator of the source-format layout, exactly the kind of gap the
first-dispatch validation exists to catch (cf. VR-1.4 → D-140).

**Decision.** Emit `remoteSiteSettings/<host>.remoteSite-meta.xml` (the canonical
SDR suffix), recognized by both `sf project deploy start` and `sf package version
create`. One-line generator change plus the matching `model_shapes` assertion.

**Consequences.** fmt, clippy (`-D warnings`), 132 workspace tests, and determinism
green. The package already exists on the Dev Hub, so the next dispatch skips
creation, injects the alias, and proceeds to the actual version build. No
class-count change.

## D-145 — `JSON.serialize(x, true)` suppresses Apex-object nulls, not Map nulls

**Context.** With the D-144 suffix fixed, the next `apex-package.yml` dispatch got
into `sf package version create`, which — unlike the lenient VR-1.3/1.4 deploy —
runs **every** test in the namespace on-platform. Exactly one failed:

> `Apex Test Failure: unbox.BoxHttpClientTest.suppressNullsOmitsNullKeysByDefault:
> line 257 … Assertion Failed: a null key is omitted: {"drop":null,"keep":"x"}`

The test synthesized a body of `new Map<String, Object>{ 'keep' => 'x', 'drop' => null }`
with the default `suppressNulls = true` and asserted the null key was dropped. But
Apex's `JSON.serialize(obj, suppressApexObjectNulls)` only suppresses null **fields
of Apex objects** — it does *not* drop null **entries of a `Map<String, Object>`**.
So on-platform the null map entry was serialized (`{"drop":null,…}`) and the
assertion failed. This never surfaced off-platform because Rust/CI never runs the
Apex; the packaging path is the first place the runtime executes on a real org.

Crucially, the test misrepresented the real runtime contract. The generated
managers only assign `request.body` directly (leaving `suppressNulls = true`) for a
**typed model object** (`managers.rs` line 363); a raw `Map` only ever reaches the
runtime via the D-138 denormalizeKeys path, which sets `suppressNulls = false`. A
`Map` + `suppressNulls = true` is a combination generated code never produces.

**Decision.** Correct the test to exercise the real default path — a typed body
(`TypedBody { keep; drop; }`) with `drop = null`. `JSON.serialize(typedObj, true)`
drops the null field, so the "an unset field stays absent" assertion holds. The
denormalize path stays covered by `explicitNullsAreSentWhenSuppressionIsOff`
(Map + `suppressNulls = false`). No runtime behavior change — the runtime was
correct; the test's premise was not.

**Consequences.** The sole on-platform test failure is resolved; the next dispatch
proceeds to mint the first installable `04t…` version, closing NF-8 end-to-end.
Documents the Apex platform gotcha so a future Map-bodied runtime path (if one is
ever added) knows `JSON.serialize(map, true)` will not strip its nulls.

**Outcome (2026-07-15).** The re-dispatched `apex-package.yml` built the first
version successfully: `SubscriberPackageVersionId 04tNS000000UGaPYAW`
(`Package2VersionId 05iNS00000019E9YAI`, version `0.1.0.1`, 2,181 metadata files),
with **all namespace tests green** (`"Error": []`) — proving the whole
generate → source-convert → compile → test → package pipeline on-platform. NF-8
(mint an installable ship artifact) is met.

One release-gate follow-up surfaced, not a blocker for beta install: on-platform
**code coverage came back 56%** (`HasPassedCodeCoverageCheck: false`). A version
installs as a beta at any coverage, but Salesforce requires **≥ 75%** to *promote*
it to `released` (the same 75% gate D-133's generated tests target for a production
deploy). The generated `@isTest` suite covers the runtime and exercised managers,
but not enough of the ~1,087-class generated surface to clear 75% org-wide.
Raising generated-test breadth to promote past beta is tracked as the remaining
Apex item in `PROGRESS.md`; it does not affect installability of the beta `04t`.

## D-146 — Generated wire-hook test suite (closing the 75% coverage gap)

**Context.** The first package version minted (D-145) but on-platform code
coverage came back **56%**, below the **75%** Salesforce requires to promote a
version from beta to `released`. The generated `@isTest` suite (D-133) drove
every manager operation and every union `parse`, but through the mock it fed each
request/response an **empty** body (`{}` / `[]`). The bulk of the generated
executable code lives in the per-struct wire statics — `normalizeKeys` /
`denormalizeKeys` (D-132) and `deserialize` (D-140) — whose bodies are a chain of
`if (raw.containsKey('<field>')) { … }` branches plus the explicit-null injection
loop (D-138) and the object-field reattach arms. An empty body takes none of
those branches: the guard and `return` execute, the per-field bodies don't. So
~220 structs' worth of remap/injection/reattach lines stayed unrun, and the org
sat at 56%.

**Decision.** Generate a dedicated **`BoxModelWireTest{n}`** suite that calls each
struct's wire statics directly with **populated** inputs shaped to enter every
branch one level deep:

- `normalizeKeys(null)` + `normalizeKeys(map)` where the map carries the wire key
  of every renamed/recursive field, each value shaped to drive the transform
  (a reaching struct → an empty map, a reaching list/map → one such element).
- `denormalizeKeys(map)` with the Apex-side keys plus, for null-writable structs,
  the `fieldsToNull` control list naming every nullable field — so each
  `if (nfName == '…')` injection branch runs.
- `deserialize(null)` + `deserialize(map)` with the object-bearing fields'
  wire keys, each shaped to drive its reattach arm.

The nested structs' own branches are covered by their own exercises, so each
input need only recurse one level. The exerciser lives on `Wire` (the single
source of truth for which fields each hook branches on), so the tests can never
drift from the hooks. Chunked at ≤ 60 structs per class (4 classes for the full
spec) so no method overruns Apex's compiled-size limit.

**Safety.** The generated calls cannot throw on the shaped inputs:
`normalizeKeys`/`denormalizeKeys` only remap keys on the untyped map (no typed
deserialize), and `deserialize` `remove`s the object-bearing keys *before* its
typed `JSON.deserialize`, leaving an empty shell — so a shaped value never
reaches a typed coercion.

**Consequences.** +4 generated classes (1087 → **1091**); fmt, clippy
(`-D warnings`), 28 `model_shapes` assertions (class + file counts updated), the
full workspace suite, and the double-generate determinism check all green. The
on-platform coverage lift is measured by re-dispatching `apex-package.yml`; the
target is ≥ 75% so the next version is promotable past beta. No runtime or
generated-model change — this adds test classes only.

**Outcome (2026-07-15).** The re-dispatched `apex-package.yml` built version
`0.1.0.2` with on-platform coverage **99%** (`HasPassedCodeCoverageCheck: true`)
— up from 56% — minting a **promotable** `SubscriberPackageVersionId
04tNS000000UGfFYAW` (`Package2VersionId 05iNS00000019FlYAI`). The wire-hook suite
closed the gap in one shot: every generated `@isTest` passed on-platform, and the
version now clears the 75% gate required to promote from beta to `released`. NF-8
is complete end-to-end — a promotable ship artifact, built and coverage-verified
by CI.

## D-147 — Rust backend, slice 1: the model layer (M5, TR-Rust)

**Context.** M5 opens the third target (v3, Rust). The architecture map
confirmed backends are not a trait but a free `generate(...)` returning
`Vec<GeneratedFile>`, dispatched by a string `match` on `--target`; the `rust()`
manifest already exists (`Result`/`Async`/`Hierarchical`). The full backend is
large (models, serde-tagged unions, async managers/client, the `reqwest`/`tokio`
runtime, tests, docs), so it lands in reviewed slices. This is slice 1: the
model layer plus its compile gate (VR-1.2), the foundation everything else
compiles against.

**Decision.** A new `gantry-backend-rust` crate mirrors the Apex signature
(`generate(&Analysis, &CapabilityManifest, &BuildInfo)`), emitting a
self-contained SDK crate: `Cargo.toml` (serde + serde_json), `src/lib.rs`
(module tree + `buildinfo` provenance, NF-7), `src/serde_helpers.rs`, and a
`models` module. Lowerings:

- **Module tree.** One Rust module per IR module. API versions redefine names
  (`ClientError` exists in the base document *and* in `2025.0`, 712/168/20
  decls across the three modules) and the modules share no cross-references, so
  each IR module becomes its own flat-sibling Rust module
  (`[schemas, v2025_0]` → `schemas_v2025_0`) rather than one namespace — bare
  `PascalCase` names stay collision-free within a module and never clash across.
- **Structs** → `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]`.
  Field identifiers are `snake_case` (the IR name may arrive camelCased) with a
  `#[serde(rename = "<wire>")]` whenever the serde default name diverges; Rust
  keywords become raw identifiers (`r#type`), the four that cannot be raw get a
  trailing underscore.
- **Tri-state (D-110)**: `Optional<Nullable<T>>` → `Option<Option<T>>` with
  `#[serde(default, skip_serializing_if = "Option::is_none", deserialize_with =
  "double_option")]`, so absent (`None`), explicit `null` (`Some(None)`), and a
  value (`Some(Some(v))`) stay distinct on the wire. Bare `Optional<T>` →
  `Option<T>` + skip-if-none; bare `Nullable<T>` → `Option<T>` (key always
  present). The `double_option` helper is generated once and imported per module
  on demand.
- **Open enums** → a transparent newtype over `String` with the known values as
  associated constants (case-only duplicates like `ASC`/`asc` get suffixed
  constant names) — unknown values round-trip for free (TR-Rust.1, string case),
  the Rust analogue of Go's `type X string`. **Closed enums** → a real `enum`
  with per-variant `rename`, rejecting unknowns.
- **Unions** lower, *for this slice only*, to a transparent `serde_json::Value`
  newtype: it compiles, round-trips every value, and retains unknown
  discriminators. The typed serde-tagged representation (TR-Rust.1, the headline
  union feature) is the next slice — kept out of this PR because getting the
  tagged-enum + catch-all retention right is its own reviewed unit. Date/time
  are RFC 3339 `String` in the interim, typed alongside serialization.

**rustfmt-clean by construction (TR-Rust.4).** The printer replicates rustfmt's
wrapping rather than post-processing: field-level `#[serde(...)]` attributes
inline up to 84 columns (`max_width` 100 minus the 16 rustfmt reserves at field
indent) and otherwise break one argument per line; over-long field and `const`
lines wrap the same way rustfmt would. Verified against the full real spec.

**Verification (VR-1.2).** `crates/gantry-backend-rust/tests/compile_output.rs`
generates the full base+2025.0+2026.0 spec and runs `cargo fmt --check` +
`cargo check` + `clippy -D warnings` on the output — the real-toolchain gate,
the Rust analogue of Go's VR-1.1. Plus a determinism check and fast structural
unit tests (no toolchain). Match arms in the lowering are enumerated, never
wildcarded, so a new IR type breaks this backend at compile time (NF-1, FR-2.1).

**Consequences.** New crate wired into the workspace; a shared `snake()` helper
added to `gantry_ir::naming`. No CLI/CI wiring yet (the backend test is the gate
for now) — `--target rust`, conformance (`rust_shape`), and the `ci.yml` steps
join when the surface is broad enough to conform. Engine `cargo fmt` + workspace
`clippy -D warnings` + all tests green.

## D-148 — Rust backend, slice 2: typed discriminated unions (TR-Rust.1)

**Context.** Slice 1 (D-147) lowered every union to a transparent
`serde_json::Value` newtype — correct but untyped. TR-Rust.1 wants `oneOf` as
native enums with a serde tagged representation and unknown discriminators
retained via a catch-all. The blocker is that serde's own internally-tagged
representation (`#[serde(tag = "type")]`) is *wrong* here: Box variant structs
carry their own discriminator field (`UserBase` has `type`), so serde would
strip it for the tag on the way in and re-emit it on the way out — a duplicate
`type` key. It also has no catch-all that *retains* an unknown tag's data
(`#[serde(other)]` is unit-only). The Go backend already solved the same
problem with hand-written `MarshalJSON`/`UnmarshalJSON` that delegate to the
variant (whose own field is the tag) and dispatch on a probe.

**Decision.** Mirror the Go dispatch with hand-written `serde` impls. A union
with a discriminator whose every variant pairs a discriminator value with a
decl-backed type lowers to:

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum AiAgentAllowedEntity {
    User(UserBase),
    Group(GroupBase),
    Unknown(serde_json::Value), // open unions only
}
impl serde::Serialize for … {          // delegate to the active variant;
    …match self { Self::User(inner) => inner.serialize(serializer), … }
}
impl<'de> serde::Deserialize<'de> for … {
    let value = serde_json::Value::deserialize(deserializer)?;
    let tag = value.get("type").and_then(Value::as_str).map(str::to_owned);
    match tag.as_deref() { Some("user") => …from_value(value)…, _ => Unknown(value) }
}
```

- **Variant name** = `constant(discriminator_value)` (`ai_agent_ask` →
  `AiAgentAsk`), de-duplicated per enum and seeded so none collides with
  `Unknown`. **Variant type** = the decl.
- **Serialize** delegates to the active variant, so the discriminator travels
  in the variant's own field — nothing is injected or duplicated.
- **Deserialize** peeks at the discriminator, `from_value`s into the matched
  variant, and (open) retains an unrecognized tag verbatim in `Unknown` or
  (closed) errors. The tag is copied to an owned `String` first so `value` can
  move into the matched arm.
- **Structural unions** (no discriminator, or a non-decl variant) keep the
  slice-1 transparent `serde_json::Value` newtype — the Go structural fallback.

The real spec has 23 discriminated unions (all open) and 19 structural ones.

**rustfmt-clean by construction.** The generated `Deserialize` builds the tag
via a method chain that always exceeds rustfmt's `chain_width` (60), so the
printer emits the broken-per-call form directly rather than a single line.

**Verification.** `tests/union_roundtrip.rs` (VR-4) generates a synthetic SDK
with an open and a closed union, compiles it, and runs real round-trips: known
dispatch, unknown-discriminator retention + re-serialization, and closed-union
rejection. Plus fast structural unit tests for all three lowering paths, and
the full-spec VR-1.2 compile gate (all 23 real unions compile clean).

**Consequences.** No new dependency (serde_json already present). Union match
arms remain enumerated, not wildcarded (NF-1). Typed date/time, managers, and
the runtime are the next slices.

## D-149 — Rust backend, slice 3: managers/client + the runtime contract stub (TR-Rust.3)

**Context.** Slices 1–2 (D-147/D-148) built the Rust model layer. This slice
adds the callable surface: one `<Name>Manager` per `x-box-tag`, one `async fn`
per operation, a `Client` entry point, and — the FR-5 seam — a rendered runtime
**stub** so the generated SDK compiles without the real runtime yet.

**Decision.**

- **Contract stub renderer** (`gantry-contract/src/rust_stubs.rs`, mirroring
  `go_stubs.rs`): renders the `V1` contract to a `src/runtime.rs` module, keyed
  off the manifest axes, not a language name (FR-4.2) — it asserts
  `ErrorModel::Result` + `AsyncModel::Async`, so a fallible fn returns
  `Result<T, Error>` and a `takes_context` fn becomes `async fn` with *no*
  context parameter (Rust threads cancellation through the future). Session
  functions are methods on `Client`; free functions are module-level. The
  opaque `Request`/`Response`/`Stream`/`Auth`/`Error`/`Client` types are
  hand-declared in the stub header; every body `unimplemented!()`s loudly
  (NF-1). Contract-crate test compiles + rustfmt-checks the stub (FR-5.3).
- **Managers** (`gantry-backend-rust/src/managers.rs`, porting the Go
  `managers.rs`): each method builds the URL from structured path segments
  (percent-escaped via a generated `internal::path_escape`, never a re-parsed
  template — FR-2.2), applies query/header params (scalars, comma-joined lists
  via `internal::join`, complex values as JSON), encodes the request body per
  media, `fetch`es through the shared session, and decodes the response by
  shape. Optional params travel in a per-operation module-level options struct
  (`#[derive(Default)]`, passed as `Option<…>`). Methods reach the network only
  through `crate::runtime` (FR-5.2). A `Client` holds one manager field per API
  area over a shared `Arc<runtime::Client>`.
- **CLI**: `generate --target rust` wired (`generate_rust`, mirroring
  `generate_apex`); `rust` added to `--target all`.

**rustfmt-clean by construction.** The managers hit several rustfmt wrapping
rules beyond `max_width`: `chain_width` (60) breaks long method chains and
`fn_call_width` breaks nested calls unpredictably from line length alone. Rather
than reproduce each rule, the printer keeps generated expressions short — a
per-file `use crate::runtime::{self, Error}` and `use crate::internal::…` shrink
paths, long query values and every path segment bind to a local first, list
joins route through `internal::join` (a short 2-call chain), and the over-long
`Option<…>` param + fn signatures wrap one item per line. Verified against the
full spec.

**Verification (VR-1.2).** The compile gate now generates the whole SDK (96
files) and runs `cargo fmt --check` + `cargo check` + `clippy -D warnings` on
it. Plus fast manager/stub structural unit tests. Lowering match arms stay
enumerated, never wildcarded (NF-1).

**Deferred.** Pagination `Stream`s (paged ops keep their plain method for now);
request builders (options structs are the interim ergonomics); typed date/time;
the real `reqwest`/`tokio` runtime crate + `verify --target rust` + conformance
`rust_shape` + `ci.yml` steps. Uploads (octet-stream/multipart) send an empty
file part as a documented placeholder — the same posture as the Go backend's
`nil` file — until the runtime slice wires real body streaming.

## D-150 — Rust backend, slice 4: the hand-written `reqwest`/`tokio` runtime (TR-Rust.5)

**Context.** Slice 3 (D-149) added the callable surface (managers/client) and
the FR-5 runtime *stub* the generated SDK compiles against. This slice supplies
the real behavior: the hand-written runtime that satisfies the same contract,
so a generated SDK can actually reach Box. It mirrors the shipped Go runtime
(`runtimes/go/gantryruntime`).

**Decision.**

- **A standalone crate** at `runtimes/rust/gantryruntime` (its own `[workspace]`
  root, outside the engine's `crates/*`), so the heavy async stack
  (`reqwest`/`tokio`/`rustls`) lives with the runtime and never enters the
  engine's dependency tree — exactly how the Go runtime's separate `go.mod`
  keeps Go's HTTP stack out of the engine. `reqwest` uses `rustls-tls`
  (`default-features = false`) to avoid a system-OpenSSL build dependency in CI.
- **The contract surface, implemented** (`src/lib.rs`): `Client::new(auth)` over
  the default Box base URLs (D-106), an async `fetch` with the R§1 network-layer
  policy — jittered exponential backoff capped at 30s (a dependency-free
  nanosecond-seeded xorshift for the jitter, which only needs to decorrelate
  retries, not be cryptographic), a single 401 token refresh, and Retry-After on
  429/503. Request/response envelopes buffer their bodies fully so retries and
  response replay stay safe (the manifest's `Streaming::Supported` axis is met
  by buffering — the same posture as the Go runtime). The `with_*` builders and
  `response_*` accessors match the free-function contract signatures exactly;
  the multipart body is framed by hand (an `attributes` JSON part + a `file`
  part, G-7) to avoid pulling reqwest's multipart/stream features.
- **Auth flows** (`src/auth.rs`): `Auth::developer_token` (fixed),
  `Auth::client_credentials` (CCG), and `Auth::oauth` (authorization-code,
  resumed from a stored refresh token or minted via `OAuthConfig::exchange_code`,
  rotating the refresh token Box returns each exchange). Exchanged tokens cache
  behind a `tokio::sync::Mutex` and refresh within a 60s margin of expiry. The
  token endpoint POST and `x-www-form-urlencoded` encoding are hand-written
  (no `url`/`oauth` crate). Async token acquisition threads through `fetch`.

**Async, concretely.** The contract's `takes_context` functions are `async fn`
returning `Result<T, Error>` (the Rust manifest's async + Result axes);
cancellation rides the future itself, so there is no context parameter. `Error`
is the opaque contract error with `From<serde_json::Error>` (and
`From<reqwest::Error>`) so generated `?` decodes compile.

**JWT deferred.** RSA-assertion server auth (signing keys, encrypted-PEM
handling) is genuinely separate — external crypto crates and Box's legacy
encrypted-key format — and lands in the next slice alongside the Rust live
smoke (VR-7). The three flows here cover fixed-token and both refresh-based
exchanges, which is all the contract-conformance and default flows need.

**Verification.** Two ways, mirroring Go: (1) the runtime crate is
fmt/clippy/tested standalone (a new CI step; 13 unit tests over base-URL config,
request builders, retry math, response accessors, form encoding, and the
developer-token/CCG/OAuth-config surface); (2) the real check — a new backend
test (`the_generated_sdk_compiles_against_the_real_runtime`) swaps the generated
stub `runtime.rs` for a re-export of this crate, adds the path dependency, and
`cargo check --examples` the whole generated SDK plus a smoke example that
constructs the client. That proves the runtime satisfies every contract
signature the managers call (FR-5.2) — no drift between stub and reality.

**Review hardening (Rust leads Go here).** Review surfaced four network-layer
behaviors this port had faithfully mirrored from the shipped Go runtime; per an
explicit call to have Rust lead, all four were fixed here (Go to follow in a
cross-runtime pass): (1) **idempotency-gated retries** — a 429 (rate-limited,
never processed) still retries for every method, but a transport error or 5xx
retries only for idempotent methods (`Method::is_idempotent`), so a POST/upload
that may have committed is never silently replayed; (2) **exponential 429
backoff** — the retry delay is the jittered exponential backoff, with a server
`Retry-After` raised to a floor rather than used as a flat repeat; (3) a
**`MAX_RETRY_DELAY` clamp** (300s) so a hostile/absurd `Retry-After` can't stall
`fetch` past the caller's intent; (4) **single-flight force-refresh on 401** —
`Auth::force_refresh` re-acquires past the freshness cache (a plain re-read
returned the same rejected token), collapsing a burst of concurrent 401s to one
refresh. Plus a **`RefreshTokenStore`** hook (`Auth::oauth_with_store`) that
persists each rotated OAuth refresh token so a restart never reloads a token Box
has already killed: `save` is `async` (a file/DB/secret-manager store does real
I/O without blocking the executor), the rotating call surfaces a `save` failure,
and — since Box has already rotated the token server-side and can't be undone —
the runtime marks the rotation unpersisted and *retries persistence on every
later call until it sticks* rather than treating the in-memory cache as durable
(a failure otherwise masks itself after the first call). Also fixed a defect
unique to the Rust port (Go delegates to `mime/multipart`): the hand-rolled
multipart body now uses a boundary verified absent from both parts and escapes
the filename (strip CR/LF, backslash-escape `\`/`"`), closing a
framing-collision / header-injection gap.

## D-151 — Rust runtime, slice 5: JWT server auth + the live smoke (VR-7)

**Context.** D-150 landed the Rust runtime with three auth flows (developer
token, CCG, OAuth) and deferred JWT — signing-key server auth needs external
crypto and Box's encrypted-key format — plus the Rust live smoke. This slice
adds both, completing the runtime's auth surface and its end-to-end check
against a real Box account (mirroring the Go runtime).

**Decision.**

- **JWT server auth** (`runtimes/rust/gantryruntime/src/jwt.rs` + `Auth::jwt`).
  The RSA private key from the app's `box_config.json` is parsed up front (a bad
  key fails at construction, not on the first request): unencrypted PKCS#8 or
  PKCS#1, or **encrypted PKCS#8** with a passphrase (the `rsa` crate's `pkcs5`
  feature — Box's current `box_config` key format). Each token refresh RS256-signs
  a fresh, single-use JWT bearer assertion by hand (`rsa` + `sha2` + `base64`,
  mirroring the Go runtime's `crypto` use rather than pulling a JWT library that
  can't take an encrypted key) — header `{alg:RS256,typ:JWT,kid}`, claims
  `iss/sub/box_sub_type/aud/jti/exp` with a 45s expiry and a nanosecond+counter
  `jti` — then exchanges it at the `jwt-bearer` grant. `JwtSource` caches the
  resulting token like the CCG flow but re-mints the assertion each refresh.
  Legacy passphrase-encrypted **PKCS#1** keys (DEK-Info) are a documented
  non-target; Box issues PKCS#8-encrypted keys. The `Source::Jwt` variant is
  boxed (the parsed key dwarfs the other variants — `clippy::large_enum_variant`).
- **Live smoke** (`tests/livesmoke.rs`, VR-7). Drives only the stable runtime
  contract (`Client::new`/`new_request`/`fetch`/`with_*`/response accessors), so
  it is independent of generated method names — it verifies the hand-written
  runtime, the part a compile check can't reach: one `GET /users/me` per
  configured flow, then paginate the root folder + upload/download/delete a small
  file. `#[ignore]`d so the standard gate compiles it (no rot) but never runs it;
  with no credentials it returns a clean no-op. Env/`.env` loading is
  dependency-free, same recognized variables as Go.

**Verification.** Unit tests (now 25) add JWT coverage: an assertion is a
well-formed three-part JWT whose header/claims decode as expected and whose
**RS256 signature verifies** against the key's public half, the user subject
overrides enterprise, a bad key errors at construction, and `jti`s are unique.
The encrypted-PKCS#8 path is exercised end to end — an AES-256-CBC/PBES2 key
decrypts with its passphrase and signs a verifiable assertion, a wrong
passphrase errors, and a missing passphrase is rejected. CI gains a Rust step in
`livesmoke.yml` (the manual VR-7 workflow), alongside the Go one; the per-commit
gate still compiles the ignored smoke via the standalone runtime `cargo test` +
`clippy --all-targets`. The generated SDK still compiles against the real runtime
(the FR-5.2 conformance test is unaffected — JWT is additive to `Auth`).

The two smokes share one Box account: the Go step runs first and consumes the
**rotating** `BOX_OAUTH_REFRESH_TOKEN`, so the Rust step nulls it and skips OAuth
(Go covers that shared flow; Rust's OAuth path is unit-tested and reuses the
CCG flow's cached-refresh machinery, which the Rust smoke does exercise). The
Rust smoke uses a per-run unique filename and deletes the uploaded file
unconditionally — even if the download assertion would fail — so a run never
leaves an artifact or 409s a later run on a duplicate name.

**Deferred.** Pagination `Stream`s, typed date/time, `verify --target rust` +
conformance `rust_shape` + generated tests/docs remain the backend-side M5 work;
the runtime itself is now feature-complete for the four Box auth flows.

## D-152 — Rust CLI verification: `verify --target rust` + `conform` `rust_shape` (VR-1.2/VR-3)

**Context.** The Rust backend's VR-1.2 gate ran only as a backend integration
test; the CLI's `verify` was Go-only and `conform` covered Go/Apex. This slice
brings Rust to CLI parity so the same commands that gate Go/Apex also gate Rust,
and wires the Rust compile gate into the primary CI signal.

**Decision.**

- **`verify --target rust`** (`gantry-cli`): generates the SDK to a temp crate
  and runs the Rust toolchain as the oracle — `cargo fmt --check` + `cargo check`
  + `clippy -D warnings` (VR-1.2), exit 4 on any failure. `verify` now dispatches
  the toolchain by target (Go keeps `go build`/`vet`/`gofmt`; the `gofmt -l`
  empty-stdout check stays Go-only since `cargo fmt --check` signals via exit
  code). A shared `generate_pairs(specs, target)` produces the `(path, content)`
  currency both `verify` and `conform` consume, so backend file types stay
  internal. Wired into `ci.yml` right after the Go verify step — the Rust
  compile gate is now a first-class CI signal, not only a backend test.
- **`conform --target rust`** + **`rust_shape`** (`gantry-verify`): the R§1
  conformance checklist now measures the Rust `src/` layout — `<Name>Manager`
  struct definitions (not their `impl`s or options structs), one `pub async fn`
  per operation, the `double_option` tri-state helper, and the `buildinfo`
  provenance in `lib.rs`. Managers (85), operations (336), serialization, and
  traceability match the program-derived expectations and pass today.

**Rust conformance is a progress report, not yet a green gate.** The Rust
backend does not emit generated docs, tests, or pagination surfaces yet (later
M5 slices), so manager-docs, pagination, round-trip-tests, auth-flows, and
docs-guides read **zero** — five capabilities fail. Crucially these are
**pending work, not platform exclusions**: unlike Apex (which genuinely cannot
do interactive OAuth or a tri-state package, D-141), Rust *will* emit these, so
`rust_shape` declares **no exclusions** and `conform --target rust` honestly
reports `FAIL` (4/9). It is therefore added to the CLI (developers can watch the
number climb) but **deliberately not wired into the release-blocking CI gate**
— it joins the gate when the docs/tests/pagination slices land and flip it
green, exactly as `conform --target apex` was gated only once Apex reached
parity (D-141). Faking exclusions to force a green would misreport deferred work
as permanently excused, so it wasn't done.

**Verification.** A `rust_shape` unit test drives a synthetic Rust SDK: the
emitted capabilities are measured and pass (managers count struct defs not
`impl`s; operations count `pub async fn`), and the not-yet-emitted ones read
zero and fail — so the report is honestly partial. `verify --target rust` passes
end to end on the full spec (96 files, rustfmt + check + clippy clean).

## D-153 — Rust models: typed date/time via `chrono` (TR-Rust.2 lineage)

**Context.** The Rust model layer lowered `Date`/`DateTime` IR types to `String`
(an interim from D-147), so callers got no type safety and no parsing. This
slice types them, mirroring the Go backend's `serialization.Date` + `time.Time`.

**Decision.** Map `ir::Type::Date → chrono::NaiveDate` (Box's full-date, e.g.
`2020-01-31`) and `ir::Type::DateTime → chrono::DateTime<chrono::Utc>` (RFC 3339).
Both serde-serialize to *exactly* Box's wire format by default — `NaiveDate` as
`YYYY-MM-DD`, `DateTime<Utc>` as RFC 3339 — so no custom (de)serializer is
needed, and the tri-state `double_option` helper composes over them unchanged.

`chrono` is added to the generated crate's manifest with a **lean feature set**
— `default-features = false, features = ["serde", "alloc"]` — which pulls only
the calendar types and their serde impls, not the `clock`/OS-timezone/wasm
machinery a generated data SDK never needs.

**Verification.** The VR-1.2 compile gate (generate the full 96-file SDK → `cargo
fmt --check` + `cargo check` + `clippy -D warnings`) now exercises 190 chrono-typed
fields and stays clean; the real-runtime conformance test is unaffected (chrono
is additive). A generated-output assertion checks the date fields are
chrono-typed and the manifest declares the dependency. The `conform --target
rust` serialization capability remains satisfied (tri-state present); typed date
is the D-152 refinement noted there.

## D-154 — Rust backend: pagination as async paginators (TR-Rust, FR-7.3)

**Context.** Paged operations shipped only their plain single-page method; the
`conform --target rust` pagination capability read 0/64. This slice generates a
paginator per paged operation, mirroring the Go backend's `iter.Seq2` iterators
but as a dependency-free async iterator (no `futures::Stream` dep).

**Decision.** For each operation `gantry_synth::detect_pagination` finds, emit a
module-level `<Manager><Method>Paginator` struct plus a `<method>_paginate`
constructor on the manager. The struct holds the manager (rebuilt from the
shared session), the operation's required args + body (owned), the options
struct (carrying the cursor), a `std::vec::IntoIter` page buffer, and a `done`
flag. Its `pub async fn next(&mut self) -> Option<Result<Element, Error>>`
yields one element at a time: it drains the buffer, then fetches the next page
through the *plain method* (so URL/param/body logic is never duplicated),
appends its entries, and advances the cursor — marker style threads the response
cursor back into the options (flattening the tri-state `next_marker`, converting
an int cursor to string, stopping on empty/absent), offset style increments by
the page's entry count and stops on an empty page. An error is yielded once,
then iteration ends.

**Envelope shape handling.** The generator peels each envelope field's `Option`
layers (0/1/2, matching the model's `Plain`/`Optional`-or-`Nullable`/tri-state
forms) to reach `entries` (`Vec`) and the cursor value. Unsupported cursor
shapes (a marker param that isn't a string, an offset param that isn't an int, a
marker cursor scalar that isn't string/int) **skip the paginator** — the plain
method still ships (a documented VR-6-style fallback, never wrong code). On the
real Box specs all 64 paged operations synthesize a paginator.

**rustfmt-clean by construction.** The paginator hits rustfmt's chain rules: the
`self.manager.method(..).await` scrutinee breaks past `chain_width` (60), its
arguments past `fn_call_width` (60), and the long `next` signature/return wraps
at `max_width` (100, brace-drops at exactly 100). The printer reproduces each
threshold so the emitted code needs no reformatting — verified by the VR-1.2
gate (`cargo fmt --check` + `cargo check` + `clippy -D warnings`) on the full
96-file SDK.

**Conformance.** `rust_shape` now measures pagination — one `_paginate`
constructor per paged surface — and subtracts the paginators' `next` methods
from the async-fn operation count. `conform --target rust` moves from 5 failing
to **4** (pagination 64/64 and operations 336/336 pass); manager-docs,
docs-guides, auth-flows, and round-trip-tests remain for the generated
tests/docs slice.

## D-155 — Rust backend: reference docs (per-manager pages + guides) (FR-7.7)

**Context.** The generated Rust SDK shipped code but no docs; `conform --target
rust` read 0 on `manager-docs` (0/85), `docs-guides` (0/4), and `auth-flows`
(0/4). This slice ports the Go backend's `docs.rs` — a `docs/` tree generated
from the same IR as the code, so the docs describe the real Rust surface (method
names, chrono-typed fields, `Result` returns, `_paginate` variants) and can't
drift from it.

**What's generated.** `generate_docs` emits `docs/README.md` (an index linking
the guides and tabulating every manager), one `docs/managers/<manager>.md` page
per API tag (each operation's HTTP line, a parameter table, request-body and
return types, and a note pointing paged operations at their `_paginate`
constructor), and the three cross-cutting guides `docs/auth.md`,
`docs/pagination.md`, `docs/errors.md`. Every file carries the do-not-edit
header (FR-6.3) and the output is deterministic (FR-6.2).

**Rust-flavored, not Go-flavored.** The shared `describe` type-renderer mirrors
the `models` mapping exactly — `i64`/`f64`/`String`, `chrono::NaiveDate`,
`chrono::DateTime<chrono::Utc>`, `Vec<T>`, `std::collections::HashMap<String,
T>`, `Option<T>` — and the guides show real Rust call sites
(`Client::new(runtime::Auth::…)`, `while let Some(item) = pages.next().await`,
`Result<T, runtime::Error>`). The auth guide documents all four Box flows
(Developer Token / Client Credentials / JWT / OAuth), which is what the
`auth-flows` capability measures.

**Conformance.** The docs paths and the auth-flow name scan are target-agnostic,
so `rust_manager_docs`/`rust_guides`/`rust_auth` reuse the same recognizer logic
as Go. `conform --target rust` moves from 4 failing to **1** — manager-docs
85/85, docs-guides 4/4, auth-flows 4/4 now pass; only `round-trip-tests` (the
generated-tests slice) remains before `conform --target rust` joins the release
gate. The docs are Markdown (no rustfmt/clippy surface); the VR-1.2 gate still
passes on the code, now regenerated alongside the `docs/` tree.

## D-156 — Rust backend: generated round-trip / behavioral tests + conform gate (FR-7.8, VR-4)

**Context.** The generated Rust SDK shipped models, managers, runtime, docs, and
pagination, but no tests — `conform --target rust` read 0 on `round-trip-tests`,
the last of nine capabilities, keeping the Rust conformance report a non-gating
progress report. This slice ports the Go backend's `tests.rs`, generating tests
that compile *and pass* under `cargo test`, and then promotes `conform --target
rust` to a CI release gate.

**What's generated.** Two inline `#[cfg(test)]` modules (declared in the crate
root, so they see `pub(crate)` items — the Rust analogue of Go's same-package
tests):
- `src/serialization_tests.rs` — fixed behavioral tests for the D-110 tri-state
  (absent omits the key, present-null reads back as `Some(None)` via
  `double_option`, a value round-trips) and the typed `chrono` date/time
  (`NaiveDate` → `2026-07-12`, `DateTime<Utc>` RFC 3339 round-trip).
- `src/roundtrip_tests.rs` — one `#[test]` per discriminated union, generated
  from the IR: known-tag dispatch selects the right variant, the discriminator
  survives a re-serialize, and an unknown tag is retained in `Unknown(_)` for
  open unions / rejected for closed ones (G-10/G-11, VR-4).

**Robust by construction — one shared allocation.** Rather than recomputing
names, the test generator consumes the *same* canonical allocations the models
do: `models::discriminated_union_rows` (extracted so `union_decl` and the tests
share it) returns the deduplicated variant rows — enum name, variant idents (kept
clear of the reserved `Unknown` arm), and values — and is `None` for a union
that lowers to the structural `serde_json::Value` newtype (any tagless / non-decl
variant). Tests are emitted only for enum-lowered unions, so a `U::Unknown` or
`U::Variant(_)` assertion can never reference a newtype that has no such variant.
Module paths come from the collision-suffixed `module_names` map (not a
recomputed name), so they resolve to the real modules. The known-tag test is
emitted only when the variant struct has no required field beyond the
discriminator (minimal `{"disc":"value"}` JSON deserializes), and it asserts the
discriminator *key* survives a re-serialize (`out[disc] == value`), not merely
that the value appears somewhere. JSON literals are built through `{:?}` so
escaping is always correct. On the real specs this yields 27 passing tests (23
enum-lowered unions + 4 serialization).

**The gate learned to run tests.** `verify --target rust` (VR-1.2) previously ran
`fmt --check` + `cargo check` + `clippy`; `#[cfg(test)]` code is compiled by none
of those. It now also runs `clippy --all-targets` (linting the tests) and `cargo
test` (compiling *and running* them), and the `the_real_spec_models_compile`
gate test mirrors that. So a broken generated test fails CI like any other drift.

**Conform is now a gate.** With `round-trip-tests` green, `conform --target
rust` reads **9/9, 0 failing** and joins the CI release gate alongside Go and
Apex — no longer a progress report. The Rust backend reaches full capability
parity with the Go reference (minus no platform exclusions).

## D-157 — TypeScript backend, slice 1: the model layer + VR-1.5 gate (TR-TypeScript, M6)

**Context.** M6 opens the fourth SDK target (v4, D-143). TypeScript is the
strongest structural fit for the IR of any target — the type system expresses
the IR's shapes almost directly — so the model layer is a near-structural map
rather than a bridge over a type-system mismatch. This slice lands the model
layer, the package scaffold, `generate --target typescript`, and the VR-1.5
compile gate.

**Model lowering.** A new `gantry-backend-typescript` crate emits one `.ts`
module per IR module (API versions redefine names and share no references, so
each is its own module — mirroring D-147). Structs → `export interface`; the
**tri-state maps straight onto the type system** (TR-TS.2) — absent → `field?:
T`, explicit null → `T | null`, both peeled from the field's `Optional`/
`Nullable` wrappers, so the absent-vs-null distinction needs no wrapper type.
`oneOf` → a discriminated union of the variant interfaces, each carrying its own
literal discriminator; **open unions add a `{ [key: string]: unknown }`
catch-all** so an unknown tag is retained, closed unions omit it (TR-TS.1, same
lowerability predicate as Go/Rust). **Open enums → string-literal unions widened
with `(string & {})`** — literal autocomplete that still accepts any string;
closed enums are the bare union (TR-TS.1). Aliases → `export type`. Field keys
are the wire name verbatim (serialization is identity), quoted only when not a
valid identifier. Cross-module references resolve through a shared
`DeclId`-indexed name map and emit `import type { … } from './mod.js'` (NodeNext
ESM); a namespaced barrel (`export * as <module>`) keeps version-colliding names
(`schemas.ClientError` vs `schemas_v2025_0.ClientError`) distinct. Declaration
names are kept clear of ambient globals (`Date`, `Error`, …) they'd otherwise
shadow.

**The gate: `tsc --noEmit` under `strict` (VR-1.5).** The TypeScript 7 native
(Go-ported) compiler type-checks the whole generated package as a fast
per-commit signal, the TS analogue of `go build`/`cargo check`. Wired as
`verify --target typescript` (CLI), a backend `compile_output` test (generate →
`tsc --noEmit`, determinism + structural assertions), and a CI step (Node +
TypeScript). The generated `tsconfig.json` pins `strict` + `noEmit` + NodeNext.
On the full real spec the model layer type-checks clean.

**Manifest.** `gantry_manifest::typescript()` — ESM (`Hierarchical`), full
generics, `Exceptions` error model (`Promise` rejection / thrown `BoxApiError`,
TR-TS.3), async, streaming supported. Not yet emitted (later M6 slices): the
`Promise`-based managers/client, the `fetch` runtime, docs, tests, and the
`conform --target typescript` shape.

## D-158 — TypeScript backend, slice 2: the runtime-contract stubs (FR-5.2/5.3, TR-TS.5 lineage)

**Context.** The generated TypeScript managers (a later slice) must type-check
against the runtime surface without the real `fetch` runtime (FR-5.3), exactly
as the Go/Rust managers compile against `go_stubs`/`rust_stubs`. This slice adds
the TypeScript stub renderer and wires it into the generated package, so the
managers slice has a contract to build against.

**The renderer.** `gantry_contract::typescript_stubs` renders the V1 contract to
a `runtime.ts` module from the *same* `ContractFn` data as the other targets, so
the stub and the declared surface cannot drift (FR-5.2). Rendering keys off the
manifest axes, never a language name (FR-4.2): `ErrorModel::Exceptions` → a
`BoxApiError extends Error` subclass and functions that throw (no `Result`-style
return widening); `AsyncModel::Async` → the network entry points
(`takes_context`) return `Promise<T>`, builders/accessors are sync. TypeScript
threads cancellation through `AbortSignal`, so the context carrier is dropped
(as in Rust). Session-receiver functions become methods on `Client`; free
functions are exported module-level functions; canonical `snake_case` names
render `camelCase`. Every stub `throw`s loudly (NF-1). Contract types map
`String→string`, `Bytes→Uint8Array`, `Int64→number`, `Json→unknown`,
`Request`/`Response`/`Stream` → the runtime classes.

**Wired + gated.** `generate()` emits `src/runtime.ts` from the stubs; the
tsconfig picks it up, so the VR-1.5 `tsc --noEmit` gate now type-checks the
runtime surface too. A `typescript_stubs` contract test renders it
deterministically, asserts the async/exceptions shape, and type-checks it with
`tsc` (the FR-5.3 gate, mirroring the Rust/Go stub tests). Next: the
`Promise`-based managers/client that call through this surface (TR-TS.3).

## D-159 — TypeScript backend, slice 3: the `Promise`-based managers/client (TR-TS.3)

**Context.** With the model layer (D-157) and the runtime-contract stubs (D-158)
in place, the SDK needs its call surface: one class per API tag, one async method
per operation, and a `Client` entry point — the TypeScript analogue of the Go/Rust
managers (D-149), calling only through the rendered `runtime.ts` contract so it
type-checks without the real runtime (FR-5.2/5.3).

**The shape.** `managers.rs` emits `src/managers/<tag>.ts` (one `<Pascal>Manager`
class per tag, deduped/collision-safe names shared with the models' module
registry), a `managers/index.ts` barrel, `src/client.ts`, and `src/internal.ts`.
Each operation becomes an `async` method: required params positional, the request
body next, and optional params in a per-operation **options object** (`opts?:
<Class><Method>Options` — the TypeScript idiom for keyword arguments, guarded on
`!== undefined`). Methods build the URL from structured path segments (never a
re-parsed template, FR-2.2), percent-escape path params, apply query/header params,
encode the body per media (`withJsonBody`/`withFormBody`/`withStreamBody`/
`withMultipartBody`), `await this.session.fetch(req)`, and decode by response
shape. Return types are `Promise<T>` throughout — the exceptions model surfaces
failure as a throw, never in the type (mirroring D-158). Uploads keep the same
empty-file placeholder posture as Go/Rust until the runtime slice wires real body
streaming.

**Idioms.** Enums lower to string-literal unions, so a query/path/form value that
is an enum is already a `string` — no newtype unwrap (unlike Rust's `.0`).
Interface/option keys are wire names verbatim (serialization identity, TR-TS.2);
member access quotes non-identifier wire names. Local variable/param names are
`camelCase`, digit- and reserved-word-safe (method names may be reserved words, so
only locals are guarded). Generation-side helpers (`pathEscape`, `join`,
`formEncode`) live in `internal.ts`, imported only where used.

**Gated.** The VR-1.5 `tsc --noEmit` gate now type-checks the full package —
managers + client + internal against the runtime stubs and models — clean under
strict TS7 (97 files via `verify --target typescript`). The `compile_output` test
asserts the managers/client files exist, are `async`, and reach the network only
through `this.session.fetch`. Next: the real `fetch` runtime (TR-TS.5), then
docs/generated tests + `conform --target typescript`.

## D-160 — TypeScript backend, slice 4: the hand-written `fetch` runtime (TR-TS.5)

**Context.** The generated managers/client (D-159) call only through the runtime
contract, type-checked against the rendered `runtime.ts` stubs (D-158). This
slice adds the *real* runtime — the hand-written implementation the stubs stand
in for — so the generated SDK compiles and runs against actual behavior, the
TypeScript analogue of `runtimes/go/gantryruntime` and `runtimes/rust/gantryruntime`.

**The runtime.** `runtimes/typescript/gantryruntime/` is a standalone package
(its own `package.json`/`tsconfig`, detached from the engine workspace like the
Go/Rust runtimes). `src/runtime.ts` implements the V1 contract with matching
names/signatures: `Request`/`Response`/`Stream` envelopes, a `Client` session
(`baseUrl`, `newRequest`, `accessToken`, and a retrying `fetch`), the `with*`
request builders, and the response accessors. `src/errors.ts` holds `BoxApiError
extends Error` (the exceptions model, TR-TS.3, carrying an optional `cause`). It
depends only on the platform `fetch`/`Headers`/`URLSearchParams`, so it runs on
any modern JavaScript runtime — no Node-only APIs.

The retry policy mirrors the Rust runtime exactly: exponential backoff + full
jitter with Retry-After (delay-seconds) as a floor, clamped to a 30s ceiling;
**idempotency-gated** — a 429 (never processed) retries for any method, but a
transport error or 5xx (which may have committed a write) retries only for
idempotent methods (GET/HEAD/PUT/DELETE/…); and a **force-refresh on 401** that
re-acquires past the token cache (via an `Auth.forceRefresh(stale)` — the TS
analogue of Rust's `force_refresh`) so the retry never resends the rejected
token. `maxRetries` is validated as a non-negative integer at construction.

**Auth.** `src/auth.ts` provides three of the four flows, all `fetch`-only:
`developerToken` (fixed token), `clientCredentials` (CCG), and the OAuth 2.0
authorization-code flow (`authorizeUrl` / `exchangeCode` / `oauth` resume). CCG
and OAuth cache the access token behind a single-flight refresh (concurrent
callers share one in-flight exchange, not bound to any single request's
cancellation) and refresh a margin before expiry; OAuth rotates the refresh
token Box returns and reports each rotation through an optional `onRefresh`
persistence hook (so a resume doesn't start from an already-invalidated token).
A token acquisition that fails at the transport level surfaces as a `BoxApiError`
(with the underlying error as `cause`), never a bare network error; CCG requires
a subject (`enterpriseId` or `userId`). **JWT server auth is deferred** to a
follow-up slice — it is the only flow that needs an RSA signing key (`node:crypto`
+ `@types/node`), so keeping it separate preserves this slice's platform-neutral,
dependency-free tsc gate.

**Gated (TR-TS.5).** A backend test (`the_generated_sdk_compiles_against_the_real_runtime`)
generates the SDK, swaps the stub `runtime.ts` for the real runtime's source, adds
a smoke module that constructs the client from an auth flow, and `tsc --noEmit`s
the whole package — proving the runtime satisfies the contract signatures the
managers call, with no drift (FR-5.2), exactly as the Rust backend's
`cargo check --examples` gate does. The runtime package also type-checks standalone
under strict TS7, wired as a CI step alongside the Go/Rust runtime jobs. Next: JWT
server auth, then docs/generated tests + `conform --target typescript`.

## D-161 — TypeScript backend, slice 5: JWT server auth (TR-TS.5, auth parity)

**Context.** The runtime (D-160) shipped three of the four Box auth flows; JWT
server auth was deferred because it is the only flow that needs an RSA signing
key, i.e. reaches beyond the platform `fetch`. This slice completes auth parity
with the Go/Rust runtimes.

**The flow.** `jwtAuth(config)` parses (and if needed decrypts) the RSA private
key up front — so a bad key fails loudly at construction, not on the first
request — then, on each refresh, builds and RS256-signs the single-use Box JWT
bearer assertion (`alg`/`typ`/`kid` header; `iss`/`sub`/`box_sub_type`/`aud`/
random `jti`/45s `exp` claims — identical to Go/Rust) and exchanges it at the
token endpoint through the shared cached-refresh machinery. It returns a promise
because it imports `node:crypto` on demand.

**Keeping the core platform-neutral.** JWT is the one Node-only flow, so it is
isolated: `jwt.ts` is a **leaf** module (not re-exported by `runtime.ts`),
reached via the package's `./jwt` export, and it imports `node:crypto` **lazily**
(`await import`), so importing the core runtime or the three `fetch`-only flows
never pulls in a Node dependency. The shared token machinery (`Auth`,
`CachedToken`, `postTokenForm`) was extracted into `tokens.ts` so `jwt.ts` reuses
it without `auth.ts` leaking internals (DRY).

**Types without `@types/node`.** Adding `@types/node` would drag in Node's global
`fetch`/`Request`/`Response`, which clash with the DOM lib the core runtime
relies on. So the small slice of `node:crypto` used (`createPrivateKey`, `sign`,
`randomBytes`) is declared ambiently in `node-crypto.d.ts`, keeping the runtime's
`tsc` gate dependency-free. The generated-SDK swap gate skips `jwt.ts` (and its
ambient decl), staying platform-neutral; the runtime's own gate type-checks
`jwt.ts`. Because the ambient decl can't verify against the real module, the
signing path was smoke-checked against real `node:crypto` (encrypted-key parse →
valid RS256 signature). A runtime unit test + live smoke are a later testing
slice, alongside docs/generated tests + `conform --target typescript`.

## D-162 — TypeScript backend, slice 6: `conform --target typescript` (VR-3 progress report)

**Context.** With the model layer, managers/client, and runtime (all four auth
flows) landed (D-157–D-161), the TypeScript SDK has a measurable capability
surface. This slice brings TypeScript onto the same R§1 conformance contract
that gates Go/Apex and reports Rust — a `typescript_shape` recognizer set plus
the `conform --target typescript` CLI wiring — so the SDK's coverage is tracked
against the spec, capability by capability, rather than by eyeball.

**One contract, a new recognizer set.** The conformance checklist is
target-neutral: it derives the *expected* surface from the verified program and
measures the *actual* surface through a `TargetShape` of per-capability
recognizers. `typescript_shape()` adds recognizers for the `src/` package the
backend emits today — manager classes (`export class <Name>Manager` under
`src/managers/`, excluding the `Client` entry point and the per-operation
`…Options` interfaces), the two-space-indented `async <method>(` operation
methods, and the `buildinfo.ts` provenance (`ENGINE` + `SPEC_FINGERPRINT`). On
the real vendored spec these read **managers 85/85, operations 336/336,
traceability 1/1** — the same expected counts Go and Apex meet, since the
contract is target-neutral.

**One documented platform exclusion: serialization.** Like Apex (D-138/D-141),
TypeScript erases the tri-state onto the type system — absent → `field?: T`,
null → `T | null` — and types dates as ISO-8601 `string`s (TR-TS.2, D-157), so
there is no `Nullable[T]`/`Date` wrapper package to emit. That shortfall is a
**documented exclusion** (recorded here), so it passes as not-applicable rather
than failing, exactly the "parity minus documented platform exclusions"
allowance.

**A progress report, not yet a gate.** Docs, generated tests, and pagination are
later M6 slices, so `manager-docs`, `docs-guides`, `round-trip-tests`,
`auth-flows` (measured from the not-yet-emitted `docs/auth.md`), and
`pagination` read zero and fail today. Those are pending work, **not**
exclusions — so `conform --target typescript` is an honest progress report
(9 capabilities, 1 excluded, 5 failing, exit 4), mirroring how Rust's conform
was partial at D-152 before its docs/tests/pagination slices flipped it green
and it joined the CI release gate. TypeScript joins the gate the same way once
those slices land. A unit test exercises the shape on a synthetic SDK (managers/
operations/traceability pass, serialization excluded, the rest pending); the
real-spec numbers above are produced by the CLI.

## D-163 — TypeScript backend, slice 7: reference docs (FR-7.7)

**Context.** The next slice after `conform --target typescript` (D-162), which
reported `manager-docs`, `docs-guides`, and `auth-flows` as pending. This slice
emits the `docs/` tree — flipping all three green — porting the Go/Rust
backends' `docs.rs` to the TypeScript surface.

**Generated from the same IR as the code.** A new `docs.rs` emits a `docs/`
tree: an index (`README.md`), one reference page per manager
(`docs/managers/<module>.md`), and the three cross-cutting guides
(`auth.md`, `pagination.md`, `errors.md`). Each manager page is derived from the
IR — the `## <method>` heading is the real camelCased method name, followed by
the `HTTP /path` line, a parameter table (wire name, in, TypeScript type,
required), the request-body media/type, and the typed `**Returns:**` — so the
docs describe exactly what the code generates and can't drift.

**No drift, by construction.** The manager naming (module/class/field) and the
per-operation method-base dedup were extracted from `managers.rs` into shared
`plan_managers`/`method_bases` helpers, now the single source of truth for both
the code printer and the docs. A doc's `## getById` heading is the same string
the emitted `async getById(...)` method carries — verified on the real spec (the
generated method names and the doc headings match exactly). Types go through the
same mapping the models use (`describe` mirrors `ts_type`; declaration names
through `type_name`), and paginated operations (via `detect_pagination`) get a
note pointing at the pagination guide.

**Accurate guides.** The auth guide documents all four Box flows with real
runtime call sites (`developerToken`, `clientCredentials`, `await jwtAuth`,
`oauth`) and names each flow so the `conform` auth recognizer reads 4/4. The
errors guide describes the exceptions model against the real `BoxApiError`
surface (`status`, `cause` — no invented `body` field). The pagination guide
documents the manual marker/offset cursor loop the current managers support
(dedicated paginators are a later slice), using the identity (snake_case) wire
field names TR-TS.2 preserves.

**Result.** `conform --target typescript` now reads **9 capabilities, 1
excluded, 2 failing** — managers, operations, manager-docs (85/85), auth-flows
(4/4), docs-guides (4/4), traceability all pass; serialization stays the
documented exclusion; only `pagination` and `round-trip-tests` remain (the last
two M6 slices). Docs are `.md` under `docs/`, outside the `tsc` `include`
(`src/**/*.ts`), so the VR-1.5 gate is unaffected. A backend test asserts the
docs tree (one page per manager, the guides, all four auth flows, real method
headings + return types); generation stays deterministic (FR-6.2).

## D-164 — Java 26 as the fifth SDK target (v5)

**Context.** With v1 (Go) shipped, v2 (Apex) essentially complete, v3 (Rust) at
full capability parity, and v4 (TypeScript) underway (models, managers/client,
runtime with all four auth flows, conformance, docs), a fifth target is added to
the roadmap: **Java**, positioned *after* TypeScript (v5 / M7).

**Decision.** Adopt **Java 26** as a first-party target (not a plugin — non-goal
3 still stands for *third-party* languages). The version choice is deliberate:
Java 26's finalized records, sealed interfaces + `permits`, record patterns, and
pattern matching for `switch` make the IR's shapes map cleanly, the way Rust's
enums and TypeScript's unions do —

- **Good IR fit.** `oneOf` → a sealed interface over record variants dispatched
  by an exhaustive pattern-matching `switch` (unknown discriminators retained via
  a catch-all record); structs → immutable records; open enums → an `enum` plus
  an unknown-value carrier so round-tripping never drops an unrecognized value.
  None of Apex's erasure/dispatch work applies.
- **A dependency-free runtime.** The hand-written runtime uses the JDK's built-in
  `java.net.http.HttpClient` — no third-party HTTP library — with the same
  retry/backoff + `401`-refresh contract as the Go/Rust/TS runtimes (FR-5).
- **A fast verification gate.** `javac` compile-clean under `-Xlint:all` plus a
  formatter (`google-java-format` / Spotless) is the Java analogue of `go build`
  / `cargo check` / `tsc --noEmit` (**VR-1.6**).

The one place Java is *less* direct than TypeScript or Rust: it has no native
absent-vs-null distinction, so the tri-state needs an explicit wrapper (like
Go's `Nullable[T]`), documented as the platform shape rather than mapped onto
the type system.

**Java 25/26 features we take advantage of.** Targeting Java 26 (with Java 25
LTS as the floor) is what makes the clean shapes above possible, and several
recent additions map directly onto SDK-generation concerns — each is adopted for
a concrete reason, not for novelty:

- **HTTP/3 (QUIC) in `HttpClient`** (Java 26) — the runtime is built on the JDK's
  `java.net.http.HttpClient`; HTTP/3's UDP/QUIC transport lowers request latency
  and is negotiated transparently, so the runtime opts in without a third-party
  HTTP or QUIC library (TR-Java.5).
- **Standard PEM Encodings** (Java 26 preview) — the JWT auth flow parses an RSA
  private key from a PEM `box_config.json`; the built-in PEM API decodes it with
  no BouncyCastle/third-party crypto dependency, keeping the runtime
  dependency-free the way the Go/Rust/TS runtimes are (auth parity).
- **Structured Concurrency** (Java 26 preview) — the chunked-upload orchestrator
  fans parts out as a single unit of work in a `try`-with-resources scope: if one
  part fails, its siblings are cancelled, so a failed upload never leaks threads
  (TR-Java.5).
- **Scoped Values** (Java 25) — request/auth context (the access token, an
  idempotency key) propagates through the call tree as an immutable `ScopedValue`
  instead of a `ThreadLocal`, safe across the virtual threads a blocking-API SDK
  spawns.
- **Module Import Declarations** (Java 25) — generated files collapse framework
  imports to `import module java.base;`, cutting generated-import boilerplate
  (FR-6, determinism preserved).
- **Flexible Constructor Bodies** (Java 25) — model records and the JWT config
  validate their arguments *before* `super()`/`this()`, so a bad key or a
  malformed tri-state wrapper "fails loudly at construction" (the same guarantee
  the Go/Rust/TS runtimes give).
- **Primitive patterns in `switch`** (Java 26 preview) — the sealed-interface
  union dispatch pattern-matches without manual boxing when a discriminator is a
  primitive.
- **Compact Source Files + instance `main`** (Java 25) — the generated VR-7 live-
  smoke / example entry point is a compact `void main()`, not a
  `public class … static void main(String[])` ceremony.
- **Compact Object Headers** (Java 25, standard) — a free 10–20% heap reduction
  for the model-heavy response graphs a Box SDK deserializes; no code change,
  recorded so the ship artifact documents the JVM floor.

The v5 acceptance criteria mirror v1/v3/v4: `javac -Xlint:all` clean +
formatter-clean, conformance parity with v1, round-trip (incl.
unknown-discriminator retention), generated tests + docs, VR-7 live smoke, and
an NF-8 ship artifact (Maven Central publish dry-run clean with shipped sources
+ Javadoc JARs).

**Consequences.** A roadmap/spec addition only — **no engine code yet** (M7 is
after M6/TypeScript). Recorded in `NEW_ENGINE_REQUIREMENTS.md` (scope line,
roll-up row + a TR-Java section, a v5 release row, the release decomposition, VR
bumped for VR-1.6), `PROGRESS.md` (v5 row + milestone M7 + the overall step-down),
`PLAN.md`, `README.md`, and `SCOPE.md`. Total scope grows **3,430 (1,034) →
3,790 (1,142)** hrs; because v5 starts at 0%, the overall effort-weighted figure
steps **~91% → ~83%** even though no completed work was lost — the same dynamic
as when TypeScript was added (D-143). The IR (FR-2) was designed to absorb new
targets through manifest + lowering + printer alone (FR-6.1); Java is the second
such addition beyond the original three.

## D-165 — TypeScript backend, slice 8: pagination (FR-7.3)

**Context.** After reference docs (D-163), `conform --target typescript` had two
capabilities left: `pagination` and `round-trip-tests`. This slice adds the
paged surfaces, flipping `pagination` 0/64 → 64/64.

**An `async *` generator per paged operation.** For every operation
`detect_pagination` finds (a marker/offset query param plus an `entries` +
cursor response envelope), the backend emits — right after the plain method — an
async-generator paginator `async *<method>Paginate(...): AsyncIterableIterator<T>`.
It calls the plain method, `yield`s each entry, and threads the next cursor into
a private copy of the options, so callers just write
`for await (const item of client.files.getFolderItemsPaginate(id))`. This is the
idiomatic TypeScript analogue of Rust's `Paginator::next().await` (D-154) — but
where Rust hand-rolls a buffer + state machine (no generators), TS's async
generators carry the iteration state, so the emitted code is a short loop.

**Marker vs offset, and the conservative fallback.** Marker pagination threads
the response cursor (`next_marker`) back into the string `marker` param, stopping
on an absent/empty cursor; a numeric `next_marker` is stringified. Offset
pagination advances `offset` by the page length, stopping on an empty page. A
cursor shape the backend doesn't synthesize (a non-string marker param, a
non-int offset) skips *only* the paginator — the plain method still ships (VR-6,
never wrong code), the same rule the Rust backend follows. On the real spec all
64 paged operations synthesize.

**Naming shared with the plain method + docs.** The paginator reuses the same
deduped method base (`method_bases`) as the plain method, so `getFolderItems` →
`getFolderItemsPaginate` deterministically. The reference-doc pages (D-163) now
point paged operations at the real `<method>Paginate` method, and the pagination
guide shows the `for await ... of` idiom (with the manual cursor loop as a
fallback).

**Conformance.** The `typescript_shape` pagination recognizer counts the
`async *` generators (one per paged operation); the operations recognizer, which
counts every `  async ` method, subtracts them so plain-operation count stays
336/336 (the same subtract-the-paginators trick the Go/Rust shapes use). With
this, `conform --target typescript` reads **9 capabilities, 1 excluded, 1
failing** — only `round-trip-tests` (the final M6 slice) remains. The generated
paginators type-check clean under `tsc --noEmit` (VR-1.5) both against the stubs
and against the real runtime (the swap test), and a backend test asserts the
paginators are emitted; generation stays deterministic (FR-6.2).

## D-166 — TypeScript backend, slice 9: generated behavioral tests (FR-7.8, VR-4)

**Context.** The last M6 slice. After pagination (D-165), `conform --target
typescript` had one capability left: `round-trip-tests`. This slice generates
the behavioral tests, flipping it green — so `conform --target typescript` reads
9/9 (minus the documented serialization exclusion) and joins the CI release gate
alongside Go, Apex, and Rust. Full capability parity with the Go reference.

**Tests that type-check *and* run.** Rust's round-trip tests compile and pass
under `cargo test`; the TypeScript analogue must do both under its own
toolchain. The backend emits two `.ts` test files that (a) type-check under the
VR-1.5 `tsc --noEmit` gate and (b) *run* under Node's built-in test runner
(`node --test`), which type-strips the `.ts` in place (default since Node 22.18)
— no transpile step, no test framework dependency:

- `serialization.test.ts` — the tri-state (absent / explicit-null / value)
  through the `JSON.stringify`/`JSON.parse` wire codec (serialization is
  identity, TR-TS.2), proving Box's clear-on-update semantics: absent omits the
  key, `null` serializes `null`, a value serializes the value — and absent stays
  distinguishable from null on read (`'field' in obj`). Plus ISO-8601
  date/date-time round-trips (dates are strings, TR-TS.2).
- `unions.test.ts` — one test per discriminated union (generated from the same
  IR via `discriminated_variants`): a known-tag document parses as the union and
  the tag round-trips; for **open** unions, an object with an unrecognized tag is
  assignable via the `{ [key: string]: unknown }` catch-all (a line that
  type-checks *only* because the catch-all exists — the TR-TS.1 unknown-retention
  guarantee); **closed** unions carry no catch-all, so they assert known dispatch
  only. On the real spec, 23 union tests + the serialization baseline.

**No `@types/node`.** The tests import `node:test`/`node:assert`, whose types
would otherwise require `@types/node` — which drags in Node's global
`fetch`/`Request`/`Response`, clashing with the DOM lib the runtime relies on. So
a local ambient `node-test.d.ts` declares just the surface used (the same trick
`jwt.ts` uses for `node:crypto`, D-161), keeping the `tsc` gate dependency-free.
Node supplies the real behavior at run time.

**Verification + CI.** A backend test (`the_generated_tests_pass_under_node`)
runs `node --test` on the generated tests as a first-class gate, next to the
existing `tsc` gates; a real-spec conformance test
(`the_generated_typescript_sdk_is_conformant`) asserts the SDK passes 9/9 with
exactly one exclusion; and CI gains a `conform --target typescript` step
alongside Go/Apex/Rust. The conformance recognizer counts the serialization
baseline (required) plus each per-union test. With this, **M6 (TypeScript) is
capability-complete** — every R§1 capability the target can express is emitted,
verified, and gated. Remaining for a shipped v4: the NF-8 npm ship artifact and
VR-7 live smoke.

## D-167 — TypeScript backend, slice 10: NF-8 npm ship artifact (dual ESM/CJS)

**Context.** The remaining TR-TypeScript work toward a shipped v4 is the NF-8
ship artifact: `npm publish --dry-run` clean, shipped `.d.ts`, and **dual
ESM/CJS** entry points (with an import smoke through both). This slice adds the
publishable package scaffold and a gate that assembles + builds + validates the
artifact end to end.

**Self-contained, vendor-the-runtime (the Go model).** The repo had two
precedents: Go ships a self-contained tree that vendors its runtime source, and
Rust depends on the runtime as a separate crate (unfinished). TypeScript follows
**Go** — the ship artifact is self-contained. The generated `src/runtime.ts` is
a compile stub (FR-5.3); the release pipeline vendors the real
`runtimes/typescript/gantryruntime` source into the tree before building (the
same swap the drift test does, D-160), so the built package embeds a working
runtime with no external dependency. `files` ships only the built `dist/` (plus
the docs + README), never the `src/` stub.

**The dual build.** The backend now emits a publishable `package.json` — an
`exports` map routing `types`/`import`/`require` into `dist/types`/`dist/esm`/
`dist/cjs`, plus `main`/`module`/`types`, `sideEffects: false`, `engines`, and a
`build` script — alongside two build configs and a post-build step:

- `tsconfig.build.json` emits ES modules to `dist/esm` and `.d.ts` to
  `dist/types` (TS 7 needs an explicit `rootDir` alongside `declarationDir`).
- `tsconfig.cjs.json` emits CommonJS to `dist/cjs`. TS 7 removed the `node10`
  module resolution, so the CJS config uses `module: CommonJS` with the default
  resolution (the one combination that emits `require`/`exports` cleanly).
- `scripts/postbuild.mjs` stamps the dual-package `type` markers
  (`dist/esm/package.json` → `module`, `dist/cjs/package.json` → `commonjs`) so
  Node reads each tree in the right module system regardless of the root
  package's `type`.

The JWT flow ships as a separate `./jwt` subpath (its own `exports` entry),
mirroring the runtime's own `./jwt` export and keeping the Node-only
`node:crypto` leaf out of the main entry.

**The gate.** A backend test (`the_generated_sdk_packs_and_loads_dual_format`)
assembles the artifact the way the release pipeline does — vendors the real
runtime (JWT leaf included), strips the non-shipped behavioral tests, runs the
`build` script, asserts `npm publish --dry-run` is clean and ships the
dual-format entry points + `.d.ts` (and *not* the `src/` stub), then packs the
tarball, installs it into a fresh consumer, and **loads it through both its
`import` and `require` entry points** — constructing a `Client` from the real
runtime (85 managers wired) and touching the `./jwt` subpath under each module
system. It runs in `cargo test --workspace` (CI), skipping cleanly when
tsc/npm/node are unavailable; no separate packaging workflow is needed (unlike
Apex's on-platform 2GP job, npm's dry-run needs no external system or secret).
`name`/`version` are placeholders; the release pipeline sets the real scope and
the `vMAJOR.MINOR.PATCH` from the FR-9 spec-diff, as Go's module tag is set.

**Result.** NF-8 for TypeScript is met — the artifact builds, publishes clean
(dry-run), and loads dual-format. The only remaining item for a fully shipped v4
is **VR-7 live smoke** against a real Box account (as Go/Rust have).

## D-168 — TypeScript runtime, slice 11: VR-7 live smoke

**Context.** The last item for a shippable v4. Go and Rust each have a VR-7 live
smoke that drives the hand-written runtime against a real Box account (one call
per auth flow + paginate + upload/download/delete), `#[ignore]`d so the
per-commit gate never runs it yet compiled so it can't rot, and wired into the
manual `livesmoke.yml`. This adds the TypeScript equivalent.

**A contract-level smoke, mirroring Go/Rust.** `livesmoke.test.ts` drives only
the stable runtime contract (`new Client` / `newRequest` / `fetch` / the `with*`
builders / response accessors + the four auth flows), so it is independent of
any generated method names — it verifies the hand-written `fetch` runtime, the
part the `tsc` gate can't exercise. It builds an `Auth` for each flow the
environment configures (developer token, CCG, OAuth, JWT-from-`box_config.json`),
authenticates each with `GET /users/me`, then paginates the root folder and
round-trips an upload/download/delete. With no credentials set it returns early
(a clean no-op), like the Go/Rust smokes, so the manual dry-run still passes.

**Build-then-run, not strip-and-run.** The other runtimes' smokes compile under
their normal test toolchain (`go test` / `cargo test`) and are gated off by
`#[ignore]`. TypeScript's per-commit gate is `tsc --noEmit` (type-check only), so
the smoke is **type-checked by the runtime's gate** (added to its `tsconfig.json`
`include`, with a local ambient `node-live.d.ts` for the `node:test`/`assert`/
`fs`/`process` surface — the same no-`@types/node` trick `jwt.ts` uses) but only
*runs* under `node --test`, on demand. Node's default strip-only TS mode can't
execute the runtime, though: its NodeNext `.js` import specifiers point at `.ts`
sources (Node won't remap them) and its envelope classes use parameter
properties (non-erasable). So the live-smoke step **builds the runtime + smoke to
JS first** (`tsconfig.livesmoke.json` → `dist-livesmoke/`, gitignored) and runs
the built `dist-livesmoke/livesmoke.test.js` — dependency-free (just the pinned
`tsc` + `node`), no `tsx`/`ts-node`. The smoke lives at the runtime **root** (not
`src/`), so it is never vendored into the shipped SDK by the NF-8 packaging.

**CI.** `livesmoke.yml` gains Node + TypeScript setup and a step that builds and
runs the TS smoke, nulling `BOX_OAUTH_REFRESH_TOKEN` (the Go step already
consumed the rotating token; TS covers developer/CCG/JWT live, its OAuth path
sharing the CCG cached-refresh machinery the smoke exercises). It reads the same
`BOX_*` secrets the Go/Rust steps do.

**Result.** All TR-TypeScript acceptance criteria are now implemented and gated:
`tsc` clean, conformance 9/9, round-trip + generated tests, docs, the NF-8 dual
ESM/CJS ship artifact, and the VR-7 live-smoke harness. v4 is feature-complete —
the remaining step is an actual credentialed live-smoke run + the release tag
(a pipeline/manual action, as with Go's tag), not engineering.

## D-169 — Rust backend, slice: NF-8 ship artifact (publishable crate)

**Context.** Go ships a `go.mod` module, TypeScript ships a dual ESM/CJS npm
package (D-167). Rust's NF-8 obligation is a **self-contained, publishable
crate** — `cargo publish`-clean. Until now the Rust backend emitted a compile
stub for the runtime (`src/runtime.rs`, from `gantry_contract`) so the generated
SDK type-checks against the declared surface without the real runtime (FR-5.3),
but a crate carrying only a stub is not shippable. This slice adds the ship
scaffold and gates the assembled crate through `cargo publish --dry-run`.

**Vendor the runtime, don't depend on it.** The obvious assembly — a
`gantryruntime = { path = "...", version = "..." }` dependency — fails
`cargo publish --dry-run`: the publish path resolves deps against the crates.io
index, and `gantryruntime` is unpublished, so it errors `no matching package
named gantryruntime found`. A bare `{ path }` (no `version`) can't be published
either. Rather than couple the ship gate to a publish-order dance (publish the
runtime first, then the SDK), the release pipeline **vendors the runtime into
the crate** as a `runtime` module, replacing the stub — the same
self-contained shape Go and TypeScript ship. The crate then has no
unpublishable dependency and `cargo publish --dry-run` is clean.

**The assembly (in the gate + the release pipeline).** Starting from the
generated crate: delete `src/runtime.rs`, create `src/runtime/`, and copy the
hand-written runtime's files in — `lib.rs` → `runtime/mod.rs` verbatim (its
`crate::` references are doc-only), `auth.rs`/`jwt.rs` → `runtime/{auth,jwt}.rs`
with `crate::` rewritten to `super::` (they are now children of the `runtime`
module, not a crate root). Each file's trailing `#[cfg(test)]` module is
dropped — the shipped SDK carries no runtime unit tests (its own generated
round-trip tests stay). Finally the runtime's dependencies (`reqwest`, `tokio`,
…) are appended to the SDK manifest, skipping any already present (`serde_json`),
yielding the full stack in one crate.

**Manifest metadata + README.** `cargo publish` requires `description`,
`license`, and either `repository` or `homepage`; the crate now carries them
(license `MIT`, a placeholder `repository`) plus a `readme = "README.md"` and a
short generated `README.md` pointing at the `docs/` tree. `name`/`version`/
`repository` stay placeholders (`box-sdk` / `0.1.0` / an `example.invalid` URL);
the release pipeline sets the real name, the `vMAJOR.MINOR.PATCH` from the FR-9
spec-diff, and the real repository URL, as Go's module tag is set. The
placeholder deliberately uses the reserved `.invalid` TLD (RFC 2606) so an
un-substituted value can never resolve to a real site — it fails safe rather
than pointing a consumer somewhere misleading.

**The gate.** `the_generated_sdk_packages_for_publish` generates the crate,
performs the vendoring assembly into a temp dir, runs
`cargo publish --dry-run --allow-dirty`, and asserts its log contains
`Uploading box-sdk`: cargo prints that only after packaging **and** the verify
build both succeed, so reaching the (intentionally aborted) upload is the real
acceptance signal — a bad manifest, a verify-build error, or an unpublishable
dep aborts earlier. Two environment details make the gate deterministic. It
forces `CARGO_TERM_COLOR=never` on the inner build (CI sets it to `always`,
whose ANSI escapes would split the `Uploading box-sdk` line the assertion
matches on). And because the crate is assembled *outside* the repo, the
workspace `rust-toolchain.toml` pin doesn't reach it, so the gate forces the
pinned channel via `RUSTUP_TOOLCHAIN` (read from `rust-toolchain.toml`) rather
than using the host's default cargo (NF-6 reproducibility). It runs in
`cargo test --workspace` (CI), skipping cleanly when the cargo toolchain is
absent (like the swap gate). No external system or secret is needed — unlike
Apex's on-platform 2GP job — so no separate packaging workflow. The
deterministic-output test additionally asserts the manifest metadata and the
shipped `README.md`.

**Result.** NF-8 for Rust is met: the assembled crate packages and publishes
clean (dry-run) as a self-contained artifact. With models, unions, managers,
pagination, docs, generated tests, conformance (D-152/D-156), the hand-written
runtime + VR-7 smoke (D-150/D-151), and now the ship scaffold all in place, v3
Rust is feature-complete — the remaining step is an actual credentialed
live-smoke run + the release tag (a pipeline/manual action, as with Go), not
engineering.

## D-170 — Java backend, slice 1: the model layer (M7, TR-Java)

**Context.** Java 26 is the fifth target (D-164), positioned after TypeScript.
This is its first engine slice. It mirrors how Rust started (D-147): the model
layer alone — structs, enums, unions, aliases — javac-clean by construction,
with typed unions and the rest of the SDK following in later slices.

**A package tree, not a flat namespace.** Java has real packages, so the IR
module tree lowers directly (like Go/Rust/TS, `ModuleSystem::Hierarchical` — no
Apex-style flattening): one package `com.box.sdk.model.<module>` per IR module,
one `.java` file per declaration (Java's one-public-type-per-file rule). API
versions redefine names (`ClientError` in the base and in `2025.0`), and those
modules share no references, so each is its own package — the D-147 lineage.
Sub-package names are `_`-joined sanitized segments, deduped deterministically
(FR-6.2), since flattening a path with `_` isn't injective.

**Records, and the tri-state as an explicit wrapper.** Structs lower to
immutable `record`s (finalized in Java 16 — the clean fit D-164 calls out, no
getter/setter/builder ceremony). Java has no native absent-vs-null distinction,
so the tri-state (D-110) is the documented platform shape rather than a
type-system mapping (as with Go's `Nullable[T]`): a generated `Tristate<T>`
(emitted into `com.box.sdk.core`) carries absent/null/value; a plain optional is
`java.util.Optional<T>`; a nullable-but-present field is a bare nullable
reference. Open enums (D-012) → a `record` over the raw `String` with the known
values as constants, so an unknown value round-trips; closed enums → a real
`enum` carrying each value's wire spelling for the (later) serialization slice.

**Aliases resolve through; unions are structural for now.** Java has no type
alias, so an alias emits no file and references resolve to the target type.
Unions lower to a structural `record(Object value)` fallback in this slice; the
typed sealed-interface-over-records form — Java's natural `oneOf` shape (a
`sealed interface … permits` over record variants dispatched by pattern-matching
`switch`) — is the next slice, exactly as Rust split models (D-147) from typed
unions (D-148).

**Serialization is a separate slice.** Unlike serde (Rust) or a native JSON
runtime (Go/TS), Java's standard library ships no JSON, so the codec can't come
free with the model derive — it is its own slice. This slice emits pure, typed
model data (records/enums), and free-form JSON (`Type::JsonValue`) is a parsed
`Object` graph for now. Scalars box uniformly (`Boolean`/`Long`/`Double`) so
container elements and nullable fields need no primitive/reference juggling.

**Imports vs FQNs.** Each file imports the library types it uses (java.util /
java.time / `com.box.sdk.core.Tristate`); cross-package *model* references are
inlined as fully-qualified names, so two modules' like-named types can both be
referenced with no import collision. Generated type names are guarded against the
`java.lang` auto-imports and the imported library simple-names (`List`,
`Optional`, …) so a schema type never shadows them; record components are guarded
against Java keywords and `Object`'s method names (a component generates an
accessor of that name).

**Verification (VR-1.6).** The gate generates the whole real-spec model tree
(900 files) and compiles it with `javac --release 21 -Xlint:all -Werror` —
`-Werror` makes any lint a hard failure, the Java analogue of `clippy -D
warnings` / strict `tsc`, and `--release 21` pins the language level to the
documented compatibility floor so the gate enforces it regardless of the host
JDK (rather than floating with whatever `javac` is installed). It runs in `cargo
test --workspace`, skipping cleanly when `javac` is absent; CI gains a
`setup-java` step pinned to the ship-target toolchain — **Java 26** (Corretto,
the `*-amzn` distribution) — so the model layer is compiled by the 26 toolchain
yet held to the 21 floor. The model layer uses only stable language features;
later slices' Java-26-only features (HTTP/3, PEM, structured concurrency) will
raise the `--release` level (and add `--enable-preview`) for the files that use
them. `generate --target java` is wired; a `java()` manifest is added
(`Hierarchical`, `Full` generics, `Exceptions`, **`Sync`** — the blocking
`java.net.http` API, concurrency the caller's business like Go/Apex — streaming
`Supported`). Match arms over IR types stay enumerated, never wildcarded (NF-1).

**Result.** v5 Java has a compiling model layer on the real spec. Next slices:
typed sealed-interface unions, then the JSON codec, managers/client, the
`java.net.http` runtime + auth, docs, tests, and the NF-8 Maven artifact.
