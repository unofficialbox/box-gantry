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
| `BoxTokenProvider` | auth-token contract (`getAccessToken()`) |
| `BoxDeveloperTokenProvider` | simplest flow: a fixed developer token |
| `BoxApiException` | the error type carrying HTTP status + response body |

## Usage

```apex
BoxTokenProvider auth = new BoxDeveloperTokenProvider('DEVELOPER_TOKEN');
Box client = new Box(new BoxHttpClient(auth));
FileFull f = client.files.getById(fileId, null, null, null, null);
```

## Governor limits

Apex has no `sleep`, so the retry policy is **immediate** (no backoff) and
bounded by the per-transaction callout limit (100). Retries cover `429` and
`5xx`; a single `401` re-attempt lets a caching token provider refresh.
Callers needing true backoff should drive retries across transactions (e.g. a
`Queueable`). Client-Credentials and JWT token providers (with `Crypto`-based
signing and token caching) land in later runtime slices.

> Verified by the scratch-org compile loop (VR-1.3): these classes deploy and
> compile as part of every generated project.
