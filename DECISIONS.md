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
