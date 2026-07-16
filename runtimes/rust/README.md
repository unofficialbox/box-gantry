# Rust SDK runtime (`gantryruntime`)

The hand-written runtime the generated Box Rust SDK ships against
(TR-Rust.5). It implements the machine-readable runtime contract
(`gantry-contract` V1) — the same signatures the generated compile-time
stubs declare — so the generated code compiles against it unchanged. It is
a standalone crate: the heavy async stack (`reqwest`/`tokio`/`rustls`) lives
here, never in the engine workspace.

- `gantryruntime/src/lib.rs` — session (`Client`), retrying async `fetch`
  (jittered backoff, single 401 refresh, Retry-After on 429/503), request
  builders (`with_*`), response accessors, and the `Request`/`Response`/
  `Stream`/`Error` envelopes. Async threads cancellation through the future,
  so there is no context parameter (the manifest's async axis).
- `gantryruntime/src/auth.rs` — the auth flows: `Auth::developer_token`,
  `Auth::client_credentials` (CCG), and `Auth::oauth` (authorization-code,
  resumed from a stored refresh token or via `OAuthConfig::exchange_code`).
  Exchanged tokens are cached until shortly before expiry.

Verified two ways: built/clippy/tested standalone in CI, and — the real
check — the generated SDK is compiled against this crate in
`crates/gantry-backend-rust/tests` (contract conformance, FR-5.2): the test
swaps the generated stub `runtime.rs` for a re-export of this crate and
`cargo check`s the whole SDK plus a smoke example.

## Not yet wired

- **JWT server auth** (signing-key assertions) — the next runtime slice.
  The three flows here cover fixed-token and both refresh-based exchanges.
- **Live smoke (VR-7)** against a real Box account — lands with JWT.
