# Vendored Box OpenAPI specs

Generation inputs are vendored — no network during generation (NF-6).
Refreshing these files is a deliberate, reviewed change: re-run the
checksums below, update them, and expect the counts asserted in
`crates/gantry-spec/tests/real_specs.rs` to need review.

| File | API version | Source (retrieved 2026-10-16) |
|---|---|---|
| `openapi.json` | 2024.0 (base) | <https://raw.githubusercontent.com/box/box-openapi/main/openapi/openapi.json> |
| `openapi-v2025.0.json` | 2025.0 | <https://raw.githubusercontent.com/box/box-openapi/main/openapi/openapi-v2025.0.json> |
| `openapi-v2026.0.json` | 2026.0 | <https://raw.githubusercontent.com/box/box-openapi/main/openapi/openapi-v2026.0.json> |

`name-overrides.json` pins synthesized type names (see `gantry names` and
`crates/gantry-spec/src/overrides.rs`). It sits beside the base spec, so
every `gantry` command and test that loads these specs applies it. Its main
job is stability across refreshes: an inline shape shared by several schemas
(D-127) is named after whichever lowers first, so when Box adds an identical
shape that sorts earlier, an existing public type would be renamed. A
`locations` pin keeps the old name. When `gantry diff` reports a refresh
renaming a type whose wire shape didn't change, add a pin here rather than
ship the rename.

Checksums (`sha256sum`):

```
see checksums.txt
```
