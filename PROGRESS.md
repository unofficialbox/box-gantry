# 📈 box-gantry — Progress

Effort-weighted progress toward **four shipped SDKs** (Go, Apex, Rust,
TypeScript) from one Rust engine. Weights are the human-equivalent hour
estimates from [`NEW_ENGINE_REQUIREMENTS.md`](./NEW_ENGINE_REQUIREMENTS.md)
(agent-hours in parentheses); status is tracked in [`PLAN.md`](./PLAN.md). Last
updated **2026-07-15**.

## Overall

```text
████████░░  ~78%
```

**~78% complete by effort** — **v1 (Go SDK) is shipped**; v2 (Apex) is
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
| v2 — Apex ☁️ | 640 | 194 | ~94% |
| v3 — Rust 🦀 | 380 | 114 | 0% |
| v4 — TypeScript 🟦 | 320 | 96 | 0% |
| **Total** | **3,430** | **1,034** | **~78%** |

## By SDK target

```text
v1 — Go 🐹    ██████████  100%   SHIPPED 2026-07-13
v2 — Apex ☁️  █████████░   ~94%   near-shipped (M4) — models, SFDX loop, JSON, managers, VR-1.3 green; naming overhaul (immediate-context, short methods, dedupe → 991 classes); SFDX scaffolding + ApexDoc + per-endpoint docs (D-129); hand-written runtime — callable SDK (D-130); governor-limit-aware pagination — documented, no extra classes (D-131); field↔wire serialization remap + union variants (D-132); generated @isTest suite for the 75% gate (D-133); CCG + JWT server-to-server auth (D-134/D-135); chunked-upload orchestrator → 1,085 classes (D-136); HttpCalloutMock coverage for the HTTP client → 1,086 classes (D-137); explicit-null (absent-vs-null) serialization via `fieldsToNull` (D-138); live smoke vs real Box (VR-1.4) + shipped Remote Site Settings (D-139); Object-field deserialization for unions/free-form JSON (D-140); target-neutral VR-3 conformance green in CI + spec-fingerprint traceability + cross-cutting docs guides → 1,087 classes (D-141); NF-8 ship artifact — unlocked 2GP package under the `unbox` namespace, defined + packaging flow documented, and the CI package-build job wired (`apex-package.yml`: create/resolve → `sf package version create` → `04t` id) (D-142); RSS source-format suffix + suppress-nulls test corrected under the strict packaging path (D-144/D-145); **first version minted on-platform — `04tNS000000UGaPYAW` v0.1.0.1, all namespace tests green**. Remaining: raise on-platform code coverage from 56% to the 75% needed to promote the version from beta to `released`.
v3 — Rust 🦀  ░░░░░░░░░░    0%   after (M5)
v4 — TypeScript 🟦  ░░░░░░░░░░    0%   after (M6) — Go-native TS 7 compiler as the `tsc --noEmit` gate; discriminated unions + `?:`/`| null` tri-state map straight onto the IR (D-143)
```

## By milestone

| Milestone | Scope | Status |
|---|---|---|
| **M0** — Bootstrap | workspace, pinned toolchain, CI, vendored specs, docs regime | ✅ 2026-07-11 |
| **M1** — Ingestion + IR (FR-1, FR-2) | multi-doc load, naming rules, typed IR, lowering | ✅ |
| **M2** — Semantics, manifest, contract (FR-3–FR-5) | one semantic pass, per-language manifests, runtime contract + drift check | ✅ |
| **M3** — Go backend, runtime, verification (FR-6–FR-9, TR-Go, VR) | lowering + printer, feature synthesis, Go runtime, full VR suite, CLI | ✅ 2026-07-13 |
| **M3.5** — Apex spike | throwaway lowering to de-risk the IR for Apex (D-108) | ✅ |
| **M4** — Apex backend + scratch-org harness (TR-Apex, VR-1.3) | flat-namespace lowering, no-generics, governor limits, Apex runtime, 75% test gate | 🔄 ~94% — models + SFDX loop + native JSON + managers/client (D-120–123); **VR-1.3 green** on-platform (D-124); immediate-context naming + dedupe (D-125–127); runtime + docs (D-129/130); pagination (D-131); serialization remap + unions (D-132); @isTest suite (D-133); CCG/JWT auth (D-134/135); chunked upload (D-136); mock coverage (D-137); explicit-null (D-138); **VR-1.4 live smoke** + Remote Site Settings (D-139); Object-field deserialization (D-140); **VR-3 conformance parity** + traceability + guides → 1,087 classes (D-141); **NF-8** unlocked-2GP ship artifact defined + CI build job (D-142); **first version minted on-platform** (`04t…` v0.1.0.1, tests green; D-144/D-145). Remaining: 56%→75% coverage to promote beyond beta |
| **M5** — Rust backend + runtime (TR-Rust, VR-1.2) | serde-tagged enums, `Result`/`Option`, async reqwest/tokio runtime | ⬜ 0% |
| **M6** — TypeScript backend + runtime (TR-TypeScript, VR-1.5) | discriminated-union lowering, `?:`/`\| null` tri-state, `Promise`-based fetch runtime, TS 7 (Go-native) `tsc` gate | ⬜ 0% (D-143) |

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

**M4 — Apex (v2) is ~94% and essentially complete:** full backend + runtime,
VR-1.3 on-platform + VR-1.4 live smoke green, VR-3 conformance parity (D-141),
and the NF-8 unlocked-2GP ship artifact defined + the CI build job wired (D-142).
The first `apex-package.yml` dispatch has now **minted the initial installable
version on-platform** — `04tNS000000UGaPYAW` (v0.1.0.1), all namespace tests
green — closing NF-8 end-to-end (D-144/D-145 cleared the source-format and
on-platform-test blockers the strict packaging path surfaced). One follow-up
remains before an AppExchange/production *release*: on-platform code coverage came
back **56%**, below the **75%** Salesforce requires to promote a version from beta
to `released`; the beta `04t` installs today, promotion needs more generated-test
coverage.

**M5 — Rust (v3)** is next: serde-tagged enums, `Result`/`Option`, an async
`reqwest`/`tokio` runtime, and the `cargo check`/`clippy` gate (VR-1.2). **Then
M6 — TypeScript (v4)** (D-143): discriminated-union lowering, `?:`/`| null`
tri-state, a `Promise`-based `fetch` runtime, and the TypeScript 7 Go-native
`tsc --noEmit` gate (VR-1.5). The three-target IR (FR-2) was built to absorb new
targets through manifest + lowering + printer alone — v4 is the first test of
that beyond the original three.

**Calendar (assessment §8 + D-143):** ~11–15.5 months to four shipped SDKs
(v1 ~6–8 done, v2 ~2.5–3.5, v3 ~1–2, v4 TypeScript ~1–2).
