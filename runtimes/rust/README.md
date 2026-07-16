# Rust SDK runtime (`gantryruntime`)

The hand-written runtime the generated Box Rust SDK ships against
(TR-Rust.5). It implements the machine-readable runtime contract
(`gantry-contract` V1) — the same signatures the generated compile-time
stubs declare — so the generated code compiles against it unchanged. It is
a standalone crate: the heavy async stack (`reqwest`/`tokio`/`rustls`) lives
here, never in the engine workspace.

- `gantryruntime/src/lib.rs` — session (`Client`), retrying async `fetch`,
  request builders (`with_*`), response accessors, and the `Request`/`Response`/
  `Stream`/`Error` envelopes. Async threads cancellation through the future,
  so there is no context parameter (the manifest's async axis). The retry policy
  is idempotency-aware: a 429 (rate-limited, never processed) retries for any
  method, while a transport error or 5xx retries only for idempotent methods, so
  a write is never silently replayed. Retries back off exponentially with full
  jitter; a server `Retry-After` raises the floor and a fixed ceiling caps it. A
  401 triggers one single-flight force-refresh past the token cache.
- `gantryruntime/src/auth.rs` — the auth flows: `Auth::developer_token`,
  `Auth::client_credentials` (CCG), and `Auth::oauth` (authorization-code,
  resumed from a stored refresh token or via `OAuthConfig::exchange_code`).
  Exchanged tokens are cached until shortly before expiry. `Auth::oauth_with_store`
  persists each rotated refresh token through a `RefreshTokenStore` (an `async`
  `save`) so a restart reloads the live token rather than one Box has already
  invalidated; a failed `save` is surfaced and then retried on later calls until
  it succeeds, so a rotation is never silently treated as durable.
- `gantryruntime/src/jwt.rs` — `Auth::jwt` (server auth with a signing key):
  parses the app's `box_config.json` RSA private key up front (unencrypted
  PKCS#8/PKCS#1, or encrypted PKCS#8 with a passphrase — a bad key fails at
  construction), then RS256-signs a fresh, single-use JWT bearer assertion for
  each token exchange.

> These four behaviors (idempotency-gated retries, exponential 429 backoff,
> single-flight 401 refresh, durable refresh-token store) go beyond the shipped
> Go runtime, which the Rust runtime otherwise mirrors — Rust leads here, and Go
> is expected to follow in a later cross-runtime hardening pass.

Verified three ways: built/clippy/tested standalone in CI (unit tests cover the
retry policy, auth caching/rotation, and JWT assertion signing + verification);
the generated SDK is compiled against this crate in
`crates/gantry-backend-rust/tests` (contract conformance, FR-5.2 — the test
swaps the generated stub `runtime.rs` for a re-export of this crate and
`cargo check`s the whole SDK plus a smoke example); and the live smoke below.

## Live smoke (VR-7)

`tests/livesmoke.rs` exercises the runtime against a **real Box account**: one
authenticated call per configured auth flow, then paginate + upload + download +
delete. It is `#[ignore]`d, so the standard CI gate compiles it (it can't rot)
but never runs it — run it on demand:

```sh
BOX_DEVELOPER_TOKEN=… cargo test -p gantryruntime --test livesmoke -- --ignored --nocapture
```

Credentials come from the environment; a flow runs only when its variables are
present, and the test returns early (a clean no-op) when none are set. For local
runs, copy the repo-root `.env.sample` to `.env` (gitignored) and fill in the
flows you want — the test loads it automatically (real env vars still win). The
recognized variables are the same as the Go runtime's (see
[`runtimes/go/README.md`](../go/README.md)); `BOX_JWT_CONFIG` points at a Box
`box_config.json`. In CI it runs only via the manual **Live smoke (VR-7)**
workflow (`livesmoke.yml`), reading credentials from repo secrets.
