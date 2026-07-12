# 🐛 box-gantry — Issue Log

Engineering issues discovered while building the engine, numbered **BG-n**.
(G-1…G-18, cited in the requirements and assessment, are box-codegen's
issue log — consultable prior art in that repository.)

Each entry records: symptom → root cause → fix → **fix class** (so
recurring classes can be promoted into a type, a lint, or a CI gate — the
G-1 lesson: the compile-the-output loop found 11 bugs the unit suite never
could).

Template:

```
## BG-n — <one-line title>
**Found:** <date> · <where: milestone / CI signal>
**Symptom:**
**Root cause:**
**Fix:**
**Fix class:** <e.g. unhandled-IR-shape / printer-formatting / spec-quirk / harness>
**Promoted to:** <type change / lint / CI gate / n/a>
```

---

## BG-1 — Go models collapse `Optional<Nullable<T>>` to `*T,omitempty` — RESOLVED

**Found:** 2026-07-11 · M3 model slice (D-110 follow-through)
**Symptom:** A Go caller cannot express "set this field to explicit
`null`" (Box's clear-on-update semantics) for fields that are both
optional and nullable: nil pointer + `omitempty` omits the key instead
of sending `null`.
**Root cause:** `encoding/json` has no native tri-state; representing it
needs a serialization-package wrapper type (D-004-class design), which
the model slice deliberately defers.
**Fix:** Resolved (D-112) — the generated `serialization` package ships a
generic `Nullable[T]` (`Valid` + `Value`, custom Marshal/Unmarshal).
`Optional<Nullable<T>>` now lowers to `*serialization.Nullable[T]` with
`,omitempty`: nil = absent, `Null[T]()` = explicit null, `Value(v)` =
value. 412 tri-state field sites now round-trip all three states.
**Fix class:** deferred-design, now delivered.
**Promoted to:** the D-110 tri-state IR distinction + the `serialization`
runtime package.
**Promoted to:** PLAN.md M3 work item.
