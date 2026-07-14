# Box Apex runtime

The hand-written runtime for the generated Salesforce Apex SDK (TR-Apex.6) —
the behavior behind the generated `BoxClient` contract. Apex has no package
imports (every class deploys together in one flat namespace), so these classes
are **embedded into every generated project** under
`force-app/main/default/classes/` rather than referenced as a dependency. This
directory is the source of truth; `crates/gantry-backend-apex/src/runtime.rs`
embeds it at build time.

## Classes

| Class | Role |
|---|---|
| `BoxClient` (generated) | the contract the managers call: `BoxResponse send(BoxRequest)` |
| `BoxRequest` / `BoxResponse` (generated) | structural request/response data |
| `BoxHttpClient` | `implements BoxClient` — real HTTP callout, base-URL resolution, retries, non-2xx → exception |
| `BoxTokenProvider` | auth-token contract (`getAccessToken()` + `invalidate()`) |
| `BoxDeveloperTokenProvider` | simplest flow: a fixed developer token |
| `BoxCachingTokenProvider` | abstract base: token cache, refresh-before-expiry, error normalization |
| `BoxCcgTokenProvider` | Client Credentials Grant: mint + cache a server-to-server token (no crypto) |
| `BoxJwtTokenProvider` | JWT server auth: RS256-signed assertion via an org-stored key (`Crypto.signWithCertificate`) |
| `BoxChunkedUpload` | orchestrates Box's create-session → PUT parts → commit protocol (SHA-1 digests, byte ranges) |
| `BoxApiException` | the error type carrying HTTP status + response body |

## Usage

```apex
// Quick testing — a fixed developer token (~60 min lifetime):
BoxTokenProvider auth = new BoxDeveloperTokenProvider('DEVELOPER_TOKEN');

// Production — Client Credentials Grant (server-to-server, auto-refreshing):
BoxTokenProvider auth = new BoxCcgTokenProvider(
    'CLIENT_ID', 'CLIENT_SECRET', 'enterprise', 'ENTERPRISE_ID');

Box client = new Box(new BoxHttpClient(auth));
FileFull f = client.files.getById(fileId, null, null, null, null);
```

Store the client secret in a **Named Credential** or protected Custom Metadata,
never in source. `BoxCcgTokenProvider` caches the access token and refreshes it a
minute before expiry; on a `401` the HTTP client calls `invalidate()` so a
prematurely-revoked token is re-minted on the next attempt.

For the **JWT** flow, import the app's RSA private key into Salesforce
**Certificate and Key Management** and register the public key with Box (Box
returns a public-key id). The private key never touches Apex — signing goes
through the stored certificate by name:

```apex
BoxTokenProvider auth = new BoxJwtTokenProvider(
    'CLIENT_ID', 'CLIENT_SECRET', 'enterprise', 'ENTERPRISE_ID',
    'PUBLIC_KEY_ID',   // the JWT `kid` Box assigned the registered key
    'BoxAppKey');      // the Cert & Key Management unique name of the RSA key
```

### Chunked upload

> ⚠️ **Not usable for a real Box chunked upload on Apex today.** `BoxChunkedUpload`
> is a **correct reference implementation** of Box's three-step protocol (create
> session → PUT each part with its byte range + SHA-1 digest → commit with the
> whole-file digest), verified against mocked callouts — but two independent
> platform limits make an actual upload impossible in a single transaction:
>
> - **Heap vs. Box's threshold (the airtight blocker).** Box only offers chunked
>   upload for files ≥ 20 MB, but Apex heap is 6 MB sync / 12 MB async and the
>   content is an in-memory `Blob`. No file is simultaneously ≥ 20 MB (so Box
>   accepts it) and ≤ ~12 MB (so it fits heap) — there is no size at which a real
>   upload succeeds, independent of the slicing limit below.
> - **No `Blob` slice.** Apex can't slice a `Blob` at arbitrary byte offsets; the
>   base64-substring workaround only lands on a real boundary at a 3-byte-aligned
>   `part_size`. Box sets `part_size` from the session (not documented to be any
>   particular value), but the sizes it issues in practice — e.g. 8 MB — are
>   powers of two, which aren't divisible by 3, so the multi-part path rejects them.
>
> A production path needs an out-of-transaction, `Queueable`-chained design (one
> part per transaction) **and** a byte-accurate slicing mechanism — a tracked
> follow-up. The helper still fails loudly (a `BoxApiException`, never a raw
> `LimitException`) at each limit, and `withMaxContentBytes(...)` tunes the
> in-heap ceiling for the mocked/reference scenario.

```apex
Box client = new Box(new BoxHttpClient(auth));
Files result = new BoxChunkedUpload(client).upload(content, 'big.bin', folderId);
FileFull uploaded = result.entries[0];
// or a new version of an existing file:
// new BoxChunkedUpload(client).uploadVersion(content, 'big.bin', fileId);
```

## Governor limits

Apex has no `sleep`, so the retry policy is **immediate** (no backoff) and
bounded by the per-transaction callout limit (100). Retries cover `429` and
`5xx`; a single `401` re-attempt lets a caching token provider refresh.
Callers needing true backoff should drive retries across transactions (e.g. a
`Queueable`). Both server-to-server auth flows are available now — Client
Credentials Grant (`BoxCcgTokenProvider`) and JWT (`BoxJwtTokenProvider`, with
`Crypto`-based assertion signing).

> Verified by the scratch-org compile loop (VR-1.3): these classes deploy and
> compile as part of every generated project.
