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

*(no entries yet — the engine is at M0)*
