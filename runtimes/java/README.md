# Java SDK runtime (`gantryruntime`)

The hand-written runtime the generated Box Java SDK ships against (TR-Java.5).
It implements the machine-readable runtime contract (`gantry-contract` V1) — the
same signatures the generated compile-time stub declares
(`crates/gantry-contract/src/java_stubs.rs`) — so the generated code compiles
against it unchanged (FR-5.2). It is **pure JDK**: `java.net.http.HttpClient` for
transport, no third-party dependency, so it needs no build tool — the swap gate
and its tests compile the source directly with `javac`.

The whole surface lives in one file,
`gantryruntime/src/main/java/com/box/sdk/runtime/Runtime.java`, so the swap is a
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
- **Auth** — `Runtime.developerToken` (a fixed token) and
  `Runtime.clientCredentials` (CCG server auth), the token cached until shortly
  before expiry behind a single-flight `ReentrantLock`. The `Auth` interface
  (`accessToken` / `forceRefresh`) and the cache are already shaped to accept
  the OAuth-refresh and JWT flows in the next slice.

> **Scope.** This runtime currently ships the **developer-token and CCG** auth
> flows. **OAuth-refresh** (with a durable rotated-refresh-token store) and
> **JWT** server auth (RS256-signed assertions from a `box_config.json` key, all
> `java.security` built-ins), plus the **VR-7 live smoke** against a real Box
> account, are the next runtime slice — mirroring how the Rust runtime split its
> core (D-150) from JWT + smoke (D-151).

## Verification

Gated inside `cargo test --workspace` (the JDK is installed in CI), three ways:

- **Standalone** — `Runtime.java` compiles warning-clean under the same
  `javac --release 21 -Xlint:all -Werror` bar as the generated SDK
  (`crates/gantry-backend-java/tests/runtime.rs`).
- **Behavioral** — an in-process `com.sun.net.httpserver.HttpServer` drives the
  real code: a 429 is retried and then succeeds, and a CCG token is fetched from
  a token endpoint and threaded as `Authorization: Bearer …` on the next call
  (same file).
- **Contract conformance (FR-5.2)** — the generated SDK is compiled against this
  runtime: the swap test overwrites the generated stub with this file and
  `javac`s the whole SDK plus a smoke driver that builds the `Client` from
  `Runtime.developerToken(...)`
  (`crates/gantry-backend-java/tests/compile_output.rs`).
