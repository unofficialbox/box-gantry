# 🏛️ box-gantry — Architecture

box-gantry is a Rust engine that reads the **Box OpenAPI specification** and
generates **Box SDKs** — Go (v1, shipped), Salesforce Apex (v2), and Rust
(v3). This document maps the codebase: the pipeline, each component, and the
directory layout. For *why* things are the way they are, see
[`DECISIONS.md`](./DECISIONS.md); for *what's next*, [`PLAN.md`](./PLAN.md).

## The pipeline

One spec set flows through a fixed sequence of stages. Each stage has a
dedicated crate and hands the next a *more verified* value — a backend never
sees anything but a checked program.

```
 spec files (.json)
      │
      ▼
┌─────────────┐   RawDocument → typed schema model → lowering
│ gantry-spec │   (ingestion + naming rules + Box quirks, FR-1)
└─────────────┘
      │  ir::Program  (the typed IR — closed node set, FR-2)
      ▼
┌─────────────┐   one semantic pass: refs bound, types well-formed,
│ gantry-sema │   identities unique → Analysis (manager index) (FR-3)
└─────────────┘
      │  Analysis<'_>   (only verified programs reach backends, FR-3.2)
      ├───────────────► gantry-synth   language-agnostic feature detection
      │                 (pagination …) → PagedOperation (FR-7)
      ▼
┌──────────────────┐   lowering + printer, guided by:
│ gantry-backend-* │     • gantry-manifest  (per-language capabilities, FR-4)
│   (go / …)       │     • gantry-contract  (runtime contract + stubs, FR-5)
└──────────────────┘
      │  Vec<GeneratedFile>   (a complete, gofmt-clean SDK tree, FR-6)
      ▼
┌───────────────┐   generate → compile with the real toolchain (VR-1.1),
│  gantry-verify│   conformance checklist (VR-3), spec-diff (FR-9)
│  + gantry-cli │   check / generate / verify / conform / diff (FR-8)
└───────────────┘
      │
      ▼
 generated SDK  +  runtimes/<lang>  (hand-written, shipped alongside)
```

Two invariants hold across the whole pipeline (the ground rules in
[`PLAN.md`](./PLAN.md)): **no semantics in strings** — optionality,
references, and operation kinds are structured data, never parsed out of a
name — and **loud, never silent** — an unclassifiable shape is an error, not
a pass-through.

## Components (crates)

Each crate maps one-to-one onto a requirement area, so the boundary between
"spec knowledge", "IR", "semantics", and "target lowering" is enforced by the
compiler, not convention.

| Crate | Responsibility | Requirements |
|---|---|---|
| [`gantry-ir`](crates/gantry-ir) | The typed intermediate representation | FR-2 |
| [`gantry-spec`](crates/gantry-spec) | OpenAPI ingestion, naming rules, Box quirks, lowering to IR | FR-1 |
| [`gantry-sema`](crates/gantry-sema) | Semantic analysis, diagnostics, the manager index | FR-3 |
| [`gantry-manifest`](crates/gantry-manifest) | Per-language capability manifests | FR-4 |
| [`gantry-contract`](crates/gantry-contract) | Runtime-contract definition + drift check; per-target stubs | FR-5 |
| [`gantry-synth`](crates/gantry-synth) | Language-agnostic feature synthesis (pagination, …) | FR-7 |
| [`gantry-backend-go`](crates/gantry-backend-go) | Go lowering + printer | FR-6, TR-Go |
| [`gantry-verify`](crates/gantry-verify) | Conformance checklist, spec-diff, verification glue | VR-*, FR-9 |
| [`gantry-cli`](crates/gantry-cli) | The one CLI: `check` / `generate` / `verify` / `conform` / `diff` | FR-8 |

### `gantry-ir` — the typed IR (FR-2)

The vocabulary every other crate speaks. A **closed** node set: adding a
variant is a compile error in every lowering that fails to handle it.

- `Type` — the closed type set: scalars (`Bool`/`Int64`/`Float64`/`String`/
  `Date`/`DateTime`/`Binary`), the optionality constructors `Optional` and
  `Nullable` (the D-110 tri-state), `List`/`Map`, `Decl` (a *resolved*
  reference — an unresolved ref cannot be represented), and `JsonValue` (an
  explicit, deliberate schema-less hole).
- `Decl`/`DeclKind` — `Struct`, `Union` (rich: discriminator + per-variant
  values + open/closed extensibility), `Enum` (open string enums), `Alias`.
- `Operation` — method, base-URL class, structured path segments, params,
  request body, classified response shape, manager, variation, api-version.
- `Identifier` — rejects the characters the old engine smuggled semantics
  through (`.`, `!`, `#`, `/`, whitespace). `naming` (in `naming.rs`) holds
  the `pascal`/`camel`/`constant` casing helpers.
- `Program` — the decl arena + operations; the only thing a backend receives.

### `gantry-spec` — ingestion + lowering (FR-1)

**The only place spec-shaping knowledge lives.** Turns raw OpenAPI JSON into
an `ir::Program`.

- `raw.rs` — serde model of the OpenAPI document as authored.
- `ingest.rs` — `SpecSet::load`: multi-document, version-aware loading;
  `operationId`/`x-box-tag` invariants; loud failure on any error (FR-1.4);
  `SpecSet::fingerprint()` (the NF-7 input hash).
- `lower.rs` — the big one: `clean_name` normalization, `allOf`
  composition-vs-wrapper split, `type`-const discriminator inference, open
  enums, synthesized inline decls, the tri-state `nullable()` helper,
  operation lowering, base-URL mapping.
- `error.rs` — `IngestError` with JSON-path locations (NF-3).

### `gantry-sema` — semantics (FR-3)

One pass over the `Program` producing an `Analysis` or **every** finding at
once. Binds references, checks type well-formedness (canonical
`Optional<Nullable<T>>` nesting only), and enforces unique identities.
`SemaError::is_engine_bug()` splits engine bugs from spec errors to drive the
CLI's exit codes. The queryable product is `Analysis.managers` — the
`x-box-tag` → operations index feature synthesis and backends walk.

### `gantry-manifest` — capability manifests (FR-4)

Per-language capability axes (generics? nullable value types? namespaces?) so
feature synthesis and backends branch on **capabilities, not language names**.
The Go manifest is frozen (D-109); Apex/Rust drafts exist.

### `gantry-contract` — the runtime contract (FR-5)

The machine-checked boundary between generated code and the hand-written
runtime. `V1` declares the contract as data (functions + a `Receiver`
`Session | Free` axis, D-113); `go_stubs.rs` renders compilable stubs from it,
so generated managers compile against exactly the declared surface — and the
real runtime, satisfying the same signatures, drops in unchanged.

### `gantry-synth` — feature synthesis (FR-7)

Language-agnostic detection of features from the IR, so every backend reuses
one definition. `pagination.rs` detects marker/offset paging *structurally*
(the query param **and** the response envelope must both be present) →
`PagedOperation`. This is the seam Apex/Rust reuse.

### `gantry-backend-go` — the Go backend (FR-6, TR-Go)

Lowering + printer: `Analysis` → a complete Go SDK tree, gofmt-clean by
construction.

- `models.rs` — schema packages: structs (+ D-110 tri-state field types),
  open enums, union variant structs with the only generated serializers, the
  `go.mod`, and the per-file provenance header.
- `managers.rs` — one method per operation calling only through the contract;
  the `client/` entry point; `iter.Seq2` paginators.
- `docs.rs` — per-manager Markdown + the auth/pagination/errors guides.
- `tests.rs` — generated round-trip tests (VR-4).
- `lib.rs` — `generate(analysis, &BuildInfo)` orchestrates the above and emits
  the serialization package, runtime stub, and `buildinfo` package.

### `gantry-verify` — verification + evolution (VR-*, FR-9)

The harness glue, depending only on the IR crates so it stays target-neutral.

- `conformance.rs` — the VR-3 checklist: derives *expected* surface from the
  program, measures *actual* from the generated files, capability by
  capability.
- `diff.rs` — the FR-9 spec-diff: classifies IR differences as breaking or
  compatible and recommends the SDK version bump.

### `gantry-cli` — the driver (FR-8)

A thin argument parser (`clap`) over the library — **no** spec shaping or
naming rules live here. Subcommands: `check`, `generate`, `verify`,
`conform`, `diff`. Distinct exit codes (`SPEC_ERROR=3`,
`VERIFICATION_FAILURE=4`, `ENGINE_BUG=5`) so CI and callers can tell input
problems from engine problems.

## Directory breakdown

```
box-gantry/
├── crates/                    # the Rust engine (Cargo workspace)
│   ├── gantry-ir/             #   typed IR (FR-2)
│   ├── gantry-spec/           #   ingestion + lowering (FR-1)
│   ├── gantry-sema/           #   semantic analysis (FR-3)
│   ├── gantry-manifest/       #   capability manifests (FR-4)
│   ├── gantry-contract/       #   runtime contract + stubs (FR-5)
│   ├── gantry-synth/          #   feature synthesis (FR-7)
│   ├── gantry-backend-go/     #   Go lowering + printer (FR-6, TR-Go)
│   ├── gantry-verify/         #   conformance, spec-diff (VR-*, FR-9)
│   └── gantry-cli/            #   the `gantry` binary (FR-8)
│
├── runtimes/                  # hand-written runtimes shipped WITH each SDK
│   └── go/gantryruntime/      #   Go runtime (TR-Go.7): Client + retrying
│                              #   Fetch, the four auth flows, builders,
│                              #   response accessors; auth_test.go +
│                              #   livesmoke_test.go (VR-7, //go:build live)
│
├── spikes/                    # throwaway, decision-producing experiments
│   └── apex-spike/            #   M3.5 IR-readiness spike for Apex (D-108)
│
├── fixtures/specs/            # vendored, pinned real Box specs (never toy)
│   ├── openapi.json           #   base (2024.0) + …-v2025.0 + …-v2026.0
│   └── checksums.txt          #   pinned so a spec refresh is deliberate
│
├── .github/workflows/         # ci.yml (per-commit gate) + livesmoke.yml
│                              #   (VR-7, manual workflow_dispatch)
│
├── Cargo.toml / Cargo.lock    # workspace + locked deps (NF-6)
├── rust-toolchain.toml        # pinned toolchain (NF-6)
│
└── docs regime (NF-5):        # kept current as the work moves
    ├── NEW_ENGINE_REQUIREMENTS.md  # normative spec (R§n, FR/VR/NF/TR)
    ├── REWRITE_ASSESSMENT.md       # rationale + estimates (§n)
    ├── PLAN.md                     # milestones + execution log
    ├── DECISIONS.md                # irreversible choices (D-101…)
    ├── ISSUES.md                   # engine defects found by verification (BG-n)
    ├── SCOPE.md                    # target/version scope
    ├── PROGRESS.md                 # effort-weighted % breakdown
    └── ARCHITECTURE.md             # this file
```

## Generated SDK layout (the output)

What `gantry generate --target go` produces — a self-contained, tagged-ready
Go module:

```
<out>/
├── go.mod                     # module + go directive (NF-8 ship artifact)
├── schemas/                   # model packages (structs, enums, unions);
│   └── v2025r0/ …             #   versioned overlays get their own subpackage
├── managers/                  # one file per x-box-tag manager + helpers
├── client/                    # NewClient() entry point wiring the session
├── serialization/             # tri-state Nullable[T] + Date (+ generated tests)
├── gantryruntime/             # compilable contract STUB (swapped for the real
│                              #   runtimes/go at ship time)
├── buildinfo/                 # EngineVersion + SpecFingerprint (NF-7)
└── docs/                      # README index, per-manager pages, guides
```

The generated `gantryruntime` stub and the hand-written `runtimes/go`
implement the *same* contract signatures — which is why the generated SDK
compiles against either, and why `crates/gantry-backend-go/tests` can prove
conformance by compiling the output against the real runtime.
