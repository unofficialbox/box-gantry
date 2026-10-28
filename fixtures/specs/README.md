# Vendored Box OpenAPI specs

Generation inputs are vendored — no network during generation (NF-6).
Refreshing these files is a deliberate, reviewed change: re-run the
checksums below, update them, and expect the counts asserted in
`crates/gantry-spec/tests/real_specs.rs` to need review.

| File | API version | Source (retrieved 2026-10-28) |
|---|---|---|
| `openapi.json` | 2024.0 (base) | <https://raw.githubusercontent.com/box/box-openapi/main/openapi/openapi.json> |
| `openapi-v2025.0.json` | 2025.0 | <https://raw.githubusercontent.com/box/box-openapi/main/openapi/openapi-v2025.0.json> |
| `openapi-v2026.0.json` | 2026.0 | <https://raw.githubusercontent.com/box/box-openapi/main/openapi/openapi-v2026.0.json> |

`names.lock` freezes synthesized type names. Shared inline shapes (D-127) are
named after whichever location lowers first, so when Box adds an identical
shape that sorts earlier, an existing public type would silently be renamed.
Every `gantry` command and test that loads these specs reads the lock beside
the base spec and keeps locked names. After refreshing the specs, run

    cargo run -p gantry-cli -- lock fixtures/specs/openapi.json fixtures/specs/openapi-v2025.0.json fixtures/specs/openapi-v2026.0.json

and review the diff: new types are added, and a group whose first location
moved keeps its name under the new key. The full-set test fails on a stale
lock.

Checksums (`sha256sum`):

```
see checksums.txt
```
