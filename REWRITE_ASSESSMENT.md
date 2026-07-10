# Net-New Codegen for {Go, Apex, Rust}: Plan and Recommendation

An assessment of building a from-scratch codegen engine, written after adding
the Go target to the existing box-codegen engine (see `GO_TARGET_HANDOFF.md`,
`ISSUES.md` G-1…G-18).

**Operating assumptions:** zero existing users, zero obligation to the six
existing SDK targets. Only net-new SDKs matter: **Go** first, **Salesforce
Apex** second, **Rust** third. "Parity" means *functional* parity with the
Box SDK capability contract (managers, four auth flows, retrying network
layer, uploads incl. chunked, downloads, pagination, versioned APIs, `oneOf`
handling, generated tests and docs) — not output parity with any existing
SDK.

**TL;DR — recommendation: build the net-new engine.** ~12.5–18 focused
engineer-months to three shipped SDKs. The engine cost amortizes across
three committed backends; no legacy parity oracle is needed ("correct" is
the capability contract plus each language's compiler); and the existing
engine's C#-shaped assumptions actively fight Apex and Rust (§4). Write the
engine in **Rust** (§6). Ship the engine + Go first, Apex second as the
designed-for stress test, Rust third.

---

## 1. What we're replacing, by the numbers

Context on the existing engine — what informs the new design and what gets
salvaged, not an obligation to preserve.

| Measure | Value |
|---|---|
| Engine source (`src/`) | ~58,000 lines TS |
| Driver/parser layer (`scripts/src/`) | ~7,900 lines TS |
| Language targets | 6 (TypeScript, Python 3, C#, Java, Swift, Go) |
| Per-node transpilers | ~22–23 per language (~134 total) |
| Transformer passes | 29, order-sensitive |
| Largest single transformer | `attachManualCode.tsx` — 5,085 lines of embedded JSX manual code |
| Hardcoded `=== 'CSharp'` language gates | 31 sites across 7 files (plus set-membership gates like `addPagination`'s `{CSharp, Go}`) |
| Test files | 257 (mostly snapshot/unit; only Go gets compile-the-output verification) |

The architecture: OpenAPI spec → JSX node tree (the IR) → 29 transformer
passes → per-language transpilers → hand-written per-language ASTs with
`print()` → source files. The core idea (language-agnostic tree →
per-language printers) is sound and carries into the new design; the
execution accumulated C#-shaped debt the new engine must not repeat.

## 2. Lessons from the existing engine (what the new design does instead)

Collected from the Go-target work; each row is a real cost paid, and a
design rule for the new engine.

| Area | box-codegen today | The new engine |
|---|---|---|
| **IR** | JSX node tree, stringly-typed: `<Call name="if">`, refs by name like `"zipDownloadSession.downloadUrl!"` — semantics (`!` force-unwrap, `.` chains, package paths) embedded in name strings that every consumer re-parses, each slightly differently | A typed IR where optionality, types, chains, and operations are structured data, not string conventions |
| **Type information** | Resolved lazily and partially by 4+ transformers (`resolveRefs`, `resolveTypes`, `inferTypes`, `applyNullSafe`), each stamping different `context.*` fields that may or may not be set depending on which passes ran | One type-checking pass producing a complete, queryable type environment before any backend runs |
| **Transformer pipeline** | 29 order-sensitive passes; the correct order differs per driver and per feature; wrong order fails silently | Explicit pass dependencies (or one normalization pass); one canonical pipeline |
| **Language gating** | `langName === 'CSharp'` scattered across 31 sites in 7 files; widening one gate for Go (`addPagination`, G-8) meant finding and editing a hardcoded set | A per-language capability manifest in one file |
| **Manual/runtime code** | Semantics split between generated DSL and per-language hand-written files, with signature drift between them | A declared runtime interface (name, arity, throws, ctx) each backend implements against, checked at generation time |
| **Naming** | `operationNameMapping` computed in the CLI driver, not the parser — a new driver silently generates wrong names (confirmed in practice: `genGo.tsx` skipped it entirely until G-18, emitting `PostFolders` instead of `CreateFolder`) | Naming rules in the parser/spec layer, impossible to skip |
| **Generator tests** | Mostly snapshot/unit; nothing compiles generated output in CI for most languages | Compile-the-output as the primary test — the G-1 loop found **11 bugs the 1,200-test suite never could** |

Additional failure modes the new engine designs out: silent no-ops from
mis-ordered passes or unregistered transpilers; implicit context contracts
(whether `context.ref` / `props.type` is populated depends on which passes
ran — nothing declares it); string-embedded semantics that cost a
newcomer — human or AI agent — roughly a third of their time in behavior
archaeology rather than implementation.

## 3. Target architecture

1. **Spec front-end** — OpenAPI (multi-version) → typed IR. Naming,
   versioning, and operation-shaping rules live *here*, not in drivers.
2. **Typed IR** — a closed set of node types with structured fields (no
   semantics in strings). Optionality is a type constructor. References are
   resolved links, not names to look up later.
3. **Semantic analysis** — one pass producing a complete type environment:
   every expression typed, every ref bound, every error surfaced with a
   spec-level location. Backends receive a *verified* program.
4. **Capability manifest** — one declarative file per language, with axes
   for module system, generics, error model, async model,
   callout/transaction limits, and test-coverage mandates. Feature synthesis
   (pagination, tests, docs) keys off the manifest, never off a language
   name.
5. **Runtime contract** — a machine-readable interface for the hand-written
   runtime each SDK ships (`sdToJson(data) throws`,
   `fetch(ctx, opts) → (resp, err)`…). Generation fails loudly if a
   backend's runtime doesn't satisfy it.
6. **Backends** — per-language lowering + printer. This layer mirrors
   today's `*ToGo.ts` / Go AST design, which worked well.
7. **Verification harness** — the primary CI signal: generate the real
   spec, compile the output with each language's real toolchain, run
   conformance fixtures. Snapshot tests demoted to change-detection aids.

## 4. Why {Go, Apex, Rust} shapes the design

The three targets are, usefully, near-opposite extremes — designing the IR
against {Go, Apex, Rust} produces a genuinely language-agnostic core in a
way that designing against one language and stretching never does. Go was a
*sympathetic* target for the old engine (packages, familiar types) and still
surfaced 11 engine bugs plus a heavy archaeology tax; Apex and Rust are much
worse fits for a C#-shaped pipeline, which is a core reason to go net-new
rather than retrofit.

**Salesforce Apex** (the stress test — schedule it second *deliberately*,
but design for it from day one):

- **No module/package system.** One flat namespace per org, ~40-char
  identifier limits, inner classes as the only grouping tool. A
  module→directory→package pipeline is meaningless; Apex needs a
  name-mangling + outer-class layout strategy instead. This is a
  *structural* backend difference, not a printer detail.
- **No user-defined generics.** `Page<T>`-style shared types are
  unexpressible; pagination must lower to per-type code or `Object`-based
  erasure. The capability manifest must carry "generics: none" as a
  first-class axis.
- **Governor limits are API-design inputs**: max ~100 HTTP callouts and
  bounded heap/CPU per transaction. Pagination iterators, chunked uploads,
  and retries must expose transaction-boundary-aware shapes (e.g.
  `Queueable` continuation patterns) — a synthesis decision, not a lowering
  detail.
- **No polymorphic JSON deserialization.** `JSON.deserialize` cannot
  dispatch on a discriminator; discriminated-union handling becomes
  *mandatory* generated parse code via `JSON.deserializeUntyped` maps.
- **Mandatory 75% test coverage to deploy.** Generated tests are a
  ship-blocking requirement of the Apex backend itself, not a later phase.
- **Verification harness is the hard part**: full type-checking requires a
  Salesforce scratch org (`sf project deploy validate` — compile + test in
  one step); fast local iteration only gets syntax-level checking
  (apex-parser / Code Analyzer). Budget for this explicitly.

**Rust** (the best-fit target — natural third):

- `oneOf` unions are **native tagged enums** + `#[serde(tag = "type")]` —
  the discriminated-union problem largely dissolves. The IR must represent
  unions richly enough that Rust lowers them to enums while Go lowers to
  variant structs and Apex to generated parse dispatch.
- Error model is `Result<T, E>` — isomorphic to Go's `(T, error)`; one IR
  error-model abstraction covers both, with Apex exceptions as the third
  point.
- Optionals are `Option<T>` (vs. Go pointers vs. Apex nullable references) —
  a clean three-point axis for the IR's optionality representation.
- Practical choices to lock early: async-first (`reqwest` + `tokio`) vs.
  sync façade; owned types everywhere (avoid lifetime parameters in
  generated code); builder pattern for optional-heavy request structs;
  `cargo check` + `clippy` as the verification loop (trivially cheap).

**Go** (first to ship): the error model, pointer optionals, package layout,
and D-012-style variant structs are already designed, implemented, and
compile-verified in the existing engine — the Go backend is substantially a
port, and its verification loop (generate real spec → `go build` →
`go vet`) already exists.

## 5. Estimate

| Workstream | Estimate (focused engineer-months) | Notes |
|---|---|---|
| Typed IR + semantic analysis | 2–3 | Designed against the {Go, Apex, Rust} axes above |
| Spec front-end | 1.5–2 | Port the Box-quirk knowledge from `scripts/src` (~7.9k lines), not the code |
| Feature synthesis + domain-logic re-expression | 2–3 | Auth flows, pagination, chunked upload, client wiring — re-expressed once against the IR (today this lives in the shared manual-code tree and is consumed for free; it must be re-expressed under any rewrite) |
| Go backend | 1–1.5 | Port the existing Go AST/printers/transpiler logic and the G-1 loop wholesale |
| Apex backend + scratch-org verification harness | 2.5–3.5 | The hard one (§4); includes the mandatory generated-test surface |
| Rust backend | 1.5–2 | Best language fit; serde does heavy lifting |
| Runtime contracts + 3 hand-written runtimes | 2–3 | Go's exists (`box-go-sdk`); Apex `Http`/auth runtime and Rust `reqwest` runtime are new |
| **Total (three shipped SDKs)** | **~12.5–18 engineer-months** | |

For comparison, retrofitting Apex + Rust into box-codegen is *not* obviously
cheaper: Go — the sympathetic case — took weeks of transpilers plus the
archaeology tax; Apex's missing module system and generics would force
invasive changes to shared pipeline code that six frozen targets still
depend on. Estimate ~4–7 months of backend work *plus* ongoing legacy drag,
to end up standing on the same C#-shaped foundation this plan has no
obligation to keep.

## 6. Implementation language: Rust

**Decision: write the new engine in Rust.** The deciding criterion is
compile-time exhaustiveness, because the generator's daily activity is
pattern-matching over IR nodes.

The core loop of a multi-backend engine is: define an IR node set, then
write N lowerings over it. The failure mode that dominated the old engine —
and that §2 exists to design out — is the *silent* miss: a transpiler that
doesn't handle a node shape, a default case that quietly passes through, a
string convention one consumer parses differently. Rust's sum types +
exhaustive `match` turn that entire bug class into compile errors: add a
node to the IR enum and the compiler lists every backend lowering that now
fails to build. For a tool whose whole job is "three backends must each
handle every IR shape," that is the property the verification harness would
otherwise be catching after the fact.

Supporting arguments:

- **The domain is Rust's home turf.** Typed IR, semantic analysis, printers —
  compiler-shaped code, with the ecosystem to match (`serde` for spec
  parsing; `miette`-style error reporting for §3's spec-level locations).
  The borrow checker barely bites here: tree processing over owned data,
  no lifetimes in the public API.
- **Dogfooding compounds.** Rust is target #3 anyway, so the fluency must
  be acquired regardless — and the engine's own serde-tagged enums *are*
  the reference design for how the Rust SDK models `oneOf`.
- **Distribution is trivial**: a single static binary; no toolchain for CI
  or contributors to stand up.
- **AI-agent ergonomics** (relevant to how this project is built): the
  compiler converts the archaeology problem ("what does this pass assume?")
  into type signatures, and agents iterate well against `cargo check`
  feedback.

**Runner-up — TypeScript**, which wins on exactly one axis: salvage and
speed. The Go AST/printer layer and the 54-test suite port nearly verbatim,
the OpenAPI-parsing ecosystem is strongest there, and iteration is fastest.
Strict TS with discriminated unions + `never`-exhaustiveness gets ~80% of
Rust's checking — but as opt-in discipline rather than the default, with
types erased at runtime (schema validation at every boundary), and the old
engine is a case study in how TS lets stringly-typed conventions creep in.
Choose TS only if calendar time to the first shipped Go SDK is the
overriding constraint. Note the salvage argument is weaker than it looks:
the plan already ports the *knowledge*, not the code, and the test suite's
expected-output strings carry into any language's fixtures verbatim.

**Explicitly not Go**, despite it being target #1: no sum types and no
exhaustive switch means compiler-shaped code devolves into type-switches
with silent default arms — the precise failure mode this rewrite is meant
to eliminate. Go is a great language for the *SDK*; it is a poor language
for the *generator*. (OCaml/F# are technically ideal for the domain but
lose on ecosystem and contributor/agent familiarity.)

## 7. Execution plan

The engine is a **net-new project in its own repository** — clean history,
no inherited build tooling, config, or conventions from box-codegen. This
repo is not migrated or refactored; it is frozen and consulted (step 5).

1. **Engine + Go backend first** (~5–7 months to first shipped SDK). Go
   re-ships from the new engine; the current box-codegen Go output serves as
   an informal functional baseline (diff the API surfaces, not the bytes).
   Salvage aggressively: the Go AST/printer layer, the 54-test Go regression
   suite, the G-1 verification loop, the spec-quirk knowledge in
   `scripts/src`, and the docs regime (ISSUES/DECISIONS/PLAN/SCOPE) carry
   over nearly intact.
2. **Apex second** (~2.5–3.5 months). Its constraints were already baked
   into the IR in step 1; what's built here is the backend, the Apex
   runtime, and the scratch-org CI harness.
3. **Rust third** (~1.5–2 months). By this point the engine has survived its
   extreme target; Rust is largely a lowering exercise.
4. **Non-negotiables from day one** (the lessons of §2): typed IR with no
   string-encoded semantics; one semantic pass before any backend runs;
   per-language capability manifest; machine-checked runtime contract;
   compile-the-output as the primary CI signal for every backend from its
   first week.
5. **Freeze box-codegen** as reference material. It stops being a
   maintenance obligation and becomes documentation: the encoded Box domain
   knowledge (manual code, test semantics, spec quirks) is consulted, never
   executed.

## 8. The one risk to watch

The IR's three-point design space ({Go, Apex, Rust}) is healthy, but Apex is
so constrained that there will be pressure to let its limitations leak
upward into the IR (e.g. flattening the module concept everywhere because
Apex lacks one). Hold the line: the IR models the *rich* concept; each
backend lowers it. The moment Apex's flat namespace shapes what the Rust
backend can express, the new engine has begun accumulating exactly the kind
of target-shaped debt this plan was meant to escape.

**What to keep no matter what:** the deterministic generate→compile loop,
the per-language hand-written AST/printer design, and the docs regime
(ISSUES/DECISIONS/PLAN/SCOPE). Those are the parts of the current system
that demonstrably worked; the new architecture treats them as load-bearing.
