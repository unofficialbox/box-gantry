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

`BoxChunkedUpload` runs Box's three-step protocol (create session → PUT each part
with its byte range + SHA-1 digest → commit with the whole-file digest):

```apex
Box client = new Box(new BoxHttpClient(auth));
Files result = new BoxChunkedUpload(client).upload(content, 'big.bin', folderId);
FileFull uploaded = result.entries[0];
// or a new version of an existing file:
// new BoxChunkedUpload(client).uploadVersion(content, 'big.bin', fileId);
```

**Apex limits bound this** (the helper fails loudly rather than obscurely):

- The content is an in-memory `Blob`, and Apex heap is 6 MB sync / **12 MB
  async** — so run uploads from a `Queueable`/`Batchable`. Content beyond a
  configurable ceiling (default ~4 MB, `withMaxContentBytes(...)`) is rejected.
  Files large enough to need Box's server part sizes can't be uploaded in a
  single transaction — a platform limit.
- Apex has no native `Blob` byte-slice; the base64 workaround only lands on real
  byte boundaries when the session's `part_size` is a multiple of 3. A
  multi-part upload with a non-aligned part size is rejected with a clear error.

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
