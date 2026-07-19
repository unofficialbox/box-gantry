# Java SDK runtime (`gantryruntime`)

The hand-written runtime the generated Box Java SDK ships against (TR-Java.5).
It implements the machine-readable runtime contract (`gantry-contract` V1) — the
same signatures the generated compile-time stub declares
(`crates/gantry-contract/src/java_stubs.rs`) — so the generated code compiles
against it unchanged (FR-5.2). It is **pure JDK**: `java.net.http.HttpClient` for
transport, no third-party dependency, so it needs no build tool — the swap gate
and its tests compile the source directly with `javac`.

The whole surface lives in one file,
`gantryruntime/src/main/java/dev/unofficialbox/runtime/Runtime.java`, so the swap is a
single-file overwrite (the Java analogue of the Rust runtime's one-line
re-export) — the envelope types (`Request`/`Response`/`Stream`/`Auth`/
`BoxApiException`) and the `Session` are nested classes, and the free contract
functions are its `static` methods. Unlike the async Rust/TS runtimes, this one
is **blocking** and **exception-based** (the Java manifest's Sync + Exceptions
axes): `fetch` returns a `Response` directly and a failure throws the unchecked
`BoxApiException`, and there is no context parameter (cancellation is the
caller's business over virtual threads).

- **`Session.fetch`** — the retrying network entry point. The retry policy is
  idempotency-aware: a 429 (rate-limited, never processed) retries for any
  method, while a transport error or 5xx retries only for idempotent methods
  (GET/HEAD/PUT/DELETE/OPTIONS/TRACE), so a write is never silently replayed.
  Retries back off exponentially (500ms · 2ⁿ, capped at 30s) with full jitter; a
  server `Retry-After` raises the floor and a 300s ceiling caps it. A 401
  triggers one single-flight force-refresh past the token cache. The request
  body is fully buffered so a retry can replay it.
- **The `with_*` builders / `response_*` accessors** — fluent request mutation
  (`withHeader` replaces case-insensitively, `withQuery` appends, the body
  builders set the content type; `withMultipartBody` assembles a Box-style
  multipart body with a sanitized filename) and buffered response reads
  (`responseHeader` is case-insensitive and returns `""` when absent).
- **Auth — all four Box flows.** `Runtime.developerToken` (a fixed token),
  `Runtime.clientCredentials` (CCG server auth), `Runtime.oauth` /
  `oauthWithStore` (authorization-code, resumed from a stored refresh token —
  Box rotates the refresh token on every exchange, so each new one is persisted
  through a `RefreshTokenStore` and the current one is saved *before* it is
  spent), and `Runtime.jwt` (server auth: the app's `box_config.json` RSA key is
  parsed up front — encrypted or plain PKCS#8, all `java.security` built-ins, no
  third-party crypto — and each refresh RS256-signs a fresh, single-use JWT
  bearer assertion). Tokens are cached until shortly before expiry behind a
  single-flight `ReentrantLock`; a 401 triggers one force-refresh.

## Verification

Gated inside `cargo test --workspace` (the JDK is installed in CI), three ways:

- **Standalone** — `Runtime.java` compiles warning-clean under the same
  `javac --release 26 -Xlint:all -Werror` bar as the generated SDK
  (`crates/gantry-backend-java/tests/runtime.rs`). It prefers **HTTP/3** (JEP
  517, a Java 26 API), so 26 is the compile/runtime floor.
- **Behavioral** — an in-process `com.sun.net.httpserver.HttpServer` drives the
  real code end to end: a 429 is retried and then succeeds, a CCG token is
  threaded as `Authorization: Bearer …`, an OAuth exchange rotates and persists
  the refresh token, and a JWT assertion is RS256-signed and **verified against
  the generated key** by the server before it issues a token (same file).
- **Contract conformance (FR-5.2)** — the generated SDK is compiled against this
  runtime: the swap test overwrites the generated stub with this file and
  `javac`s the whole SDK plus a smoke driver that builds the `Client` from
  `Runtime.developerToken(...)`
  (`crates/gantry-backend-java/tests/compile_output.rs`).
- **Live smoke (VR-7)** — a driver does one authenticated `GET /users/me` per
  auth flow whose credentials are in the environment (`BOX_DEVELOPER_TOKEN`;
  `BOX_CLIENT_ID`+`BOX_CLIENT_SECRET`+`BOX_ENTERPRISE_ID`; `…`+
  `BOX_OAUTH_REFRESH_TOKEN`; `BOX_JWT_CONFIG` → a `box_config.json`). It compiles
  under the gate (so it can't rot) and runs only when credentialed — a clean
  no-op otherwise (same file).
