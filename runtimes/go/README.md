# Go SDK runtime (`gantryruntime`)

The hand-written runtime the generated Box Go SDK ships against (TR-Go.7).
It implements the machine-readable runtime contract (`gantry-contract`
V1) — the same signatures the generated compile-time stubs declare — so
the generated code compiles against it unchanged.

- `gantryruntime/runtime.go` — session (`Client`), retrying `Fetch`,
  request builders, response accessors, `With*` options.
- `gantryruntime/auth.go` — `TokenSource` implementations (`DeveloperToken`
  today; CCG / JWT / OAuth 2.0 land with auth synthesis).

Verified two ways: built/vetted standalone in CI, and — the real check —
the generated SDK is compiled against these files in
`crates/gantry-backend-go/tests` (contract conformance, FR-5.2).
