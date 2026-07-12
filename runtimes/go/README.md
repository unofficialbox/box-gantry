# Go SDK runtime (`gantryruntime`)

The hand-written runtime the generated Box Go SDK ships against (TR-Go.7).
It implements the machine-readable runtime contract (`gantry-contract`
V1) — the same signatures the generated compile-time stubs declare — so
the generated code compiles against it unchanged.

- `gantryruntime/runtime.go` — session (`Client`), retrying `Fetch`,
  request builders, response accessors, `With*` options.
- `gantryruntime/auth.go` — the four `TokenSource` flows: `DeveloperToken`,
  `ClientCredentials` (CCG), `JWTAuth`, and `OAuth`.

Verified two ways: built/vetted standalone in CI, and — the real check —
the generated SDK is compiled against these files in
`crates/gantry-backend-go/tests` (contract conformance, FR-5.2).

## Live smoke (VR-7)

`gantryruntime/livesmoke_test.go` exercises the runtime against a **real
Box account**: one authenticated call per configured auth flow, then
paginate + upload + download + delete. It is build-tagged `live`, so the
standard CI gate never compiles or runs it — run it on demand:

```sh
BOX_DEVELOPER_TOKEN=… go test -tags live -run TestLiveSmoke -v ./gantryruntime/...
```

Credentials come from the environment; a flow runs only when its variables
are present, and the whole test `t.Skip()`s when none are set (so a
credential-free run is a clean no-op). Recognized variables:

| Variable | Flow |
|---|---|
| `BOX_DEVELOPER_TOKEN` | Developer Token |
| `BOX_CLIENT_ID` + `BOX_CLIENT_SECRET` + `BOX_ENTERPRISE_ID` | Client Credentials Grant |
| `BOX_CLIENT_ID` + `BOX_CLIENT_SECRET` + `BOX_OAUTH_REFRESH_TOKEN` | OAuth 2.0 |
| `BOX_JWT_CONFIG` (path to `box_config.json`) | JWT |

In CI it runs only via the manual **Live smoke (VR-7)** workflow
(`workflow_dispatch`), which reads these from repo secrets — never from the
repository.
