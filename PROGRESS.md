# 📈 box-gantry — Progress

Effort-weighted progress toward **three shipped SDKs** (Go, Apex, Rust) from
one Rust engine. Weights are the human-equivalent hour estimates from
[`NEW_ENGINE_REQUIREMENTS.md`](./NEW_ENGINE_REQUIREMENTS.md) (agent-hours in
parentheses); status is tracked in [`PLAN.md`](./PLAN.md). Last updated
**2026-07-14**.

## Overall

```text
████████░░  84%
```

**~84% complete by effort** — **v1 (Go SDK) is shipped**; v2 (Apex) is
underway (models, native-JSON, managers/client, serialization remap, server
auth, generated tests, and **VR-1.3 green** — the generated classes compile on
the Salesforce platform); v3 (Rust) remains. The large shared cost
— the whole engine core (ingestion, typed IR, semantics, manifest, runtime
contract) and the verification harnesses — landed with v1 and is **reused
unchanged** by v2/v3, so the remaining ~17% is mostly target-specific
backends + runtimes, not new engine.

| | Human-equiv hrs | Agent-hrs | Complete |
|---|---:|---:|---:|
| **v1 — Go** 🐹 | 2,090 | 630 | **100%** |
| v2 — Apex ☁️ | 640 | 194 | ~74% |
| v3 — Rust 🦀 | 380 | 114 | 0% |
| **Total** | **3,110** | **938** | **~84%** |

## By SDK target

```text
v1 — Go 🐹    ██████████  100%   SHIPPED 2026-07-13
v2 — Apex ☁️  ███████░░░   ~74%   in progress (M4) — models, SFDX loop, JSON, managers, VR-1.3 green; naming overhaul (immediate-context, short methods, dedupe → 991 classes); SFDX scaffolding + ApexDoc + per-endpoint docs (D-129); hand-written runtime — callable SDK (D-130); governor-limit-aware pagination — documented, no extra classes (D-131); field↔wire serialization remap + union variants (D-132); generated @isTest suite for the 75% gate (D-133); CCG + JWT server-to-server auth (D-134/D-135); chunked-upload orchestrator → 1,085 classes (D-136); HttpCalloutMock coverage for the HTTP client → 1,086 classes (D-137)
v3 — Rust 🦀  ░░░░░░░░░░    0%   after (M5)
```

## By milestone

| Milestone | Scope | Status |
|---|---|---|
| **M0** — Bootstrap | workspace, pinned toolchain, CI, vendored specs, docs regime | ✅ 2026-07-11 |
| **M1** — Ingestion + IR (FR-1, FR-2) | multi-doc load, naming rules, typed IR, lowering | ✅ |
| **M2** — Semantics, manifest, contract (FR-3–FR-5) | one semantic pass, per-language manifests, runtime contract + drift check | ✅ |
| **M3** — Go backend, runtime, verification (FR-6–FR-9, TR-Go, VR) | lowering + printer, feature synthesis, Go runtime, full VR suite, CLI | ✅ 2026-07-13 |
| **M3.5** — Apex spike | throwaway lowering to de-risk the IR for Apex (D-108) | ✅ |
| **M4** — Apex backend + scratch-org harness (TR-Apex, VR-1.3) | flat-namespace lowering, no-generics, governor limits, Apex runtime, 75% test gate | 🔄 models + SFDX loop + native JSON + managers/client (D-120–123); **VR-1.3 green** — 1,419 classes compile on the platform (D-124); immediate-context naming (D-125) |
| **M5** — Rust backend + runtime (TR-Rust, VR-1.2) | serde-tagged enums, `Result`/`Option`, async reqwest/tokio runtime | ⬜ 0% |

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

**M4 — Apex backend (v2).** The three-target IR was designed for this and the
M3.5 spike already proved it (85/85 managers lower, zero IR changes forced).
First week targets the operational risk — the **scratch-org CI harness** —
then manifest-driven no-generics lowering, governor-limit-aware
pagination/upload/retry, `JSON.deserializeUntyped` dispatch, the Apex runtime
(`Http` + Crypto-JWT), and generated test classes clearing the 75% coverage
gate. Then **M5 — Rust (v3)**.

**Calendar (assessment §8):** ~10–13.5 months to three shipped SDKs
(v1 ~6–8 done, v2 ~2.5–3.5, v3 ~1–2).
