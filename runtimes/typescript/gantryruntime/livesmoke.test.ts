// VR-7 live smoke: exercise the hand-written `fetch` runtime against a real Box
// account — one authenticated call per configured auth flow, plus paginate +
// upload + download + delete. It drives only the stable runtime contract
// (`new Client` / `newRequest` / `fetch` / the `with*` builders / response
// accessors), so it is independent of any generated method names — it verifies
// the runtime, which is the part the `tsc` gate cannot exercise.
//
// It is type-checked by the runtime's `tsc` gate (so it can't rot) but only
// *runs* on demand, under `node --test`, with credentials in the environment:
//
//   node --test livesmoke.test.ts
//
// With no credentials set it returns early (a clean no-op), like the Go/Rust
// smokes — so the manual `livesmoke.yml` dry-run still passes.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { env } from 'node:process';

import {
  type Auth,
  Client,
  clientCredentials,
  developerToken,
  oauth,
  Request as BoxRequest,
  responseBytes,
  statusCode,
  Stream,
  withMultipartBody,
  withQuery,
} from './src/runtime.js';
import { jwtAuth } from './src/jwt.js';

/// The process environment, with an optional `BOX_ENV_FILE` filling gaps (real
/// env vars win) — dependency-free, matching the runtime's zero-dep posture.
function loadEnv(): Record<string, string> {
  const map: Record<string, string> = {};
  for (const [key, value] of Object.entries(env)) {
    if (typeof value === 'string') {
      map[key] = value;
    }
  }
  const file = map.BOX_ENV_FILE;
  if (file) {
    for (const raw of readFileSync(file, 'utf8').split('\n')) {
      const line = raw.trim();
      if (line === '' || line.startsWith('#')) {
        continue;
      }
      const eq = line.indexOf('=');
      if (eq < 0) {
        continue;
      }
      const key = line.slice(0, eq).trim();
      const value = line.slice(eq + 1).trim().replace(/^['"]|['"]$/g, '');
      if (!(key in map)) {
        map[key] = value;
      }
    }
  }
  return map;
}

function nonempty(e: Record<string, string>, key: string): string | undefined {
  const value = e[key];
  return value !== undefined && value.length > 0 ? value : undefined;
}

/// Every auth flow the environment configures, by name. A flow is built only
/// when its variables are present, so a token-only setup still runs the smoke.
async function authSources(e: Record<string, string>): Promise<Array<[string, Auth]>> {
  const sources: Array<[string, Auth]> = [];

  const token = nonempty(e, 'BOX_DEVELOPER_TOKEN');
  if (token !== undefined) {
    sources.push(['developer', developerToken(token)]);
  }
  const clientId = nonempty(e, 'BOX_CLIENT_ID');
  const clientSecret = nonempty(e, 'BOX_CLIENT_SECRET');
  if (clientId !== undefined && clientSecret !== undefined) {
    const enterpriseId = nonempty(e, 'BOX_ENTERPRISE_ID');
    if (enterpriseId !== undefined) {
      sources.push(['ccg', clientCredentials({ clientId, clientSecret, enterpriseId })]);
    }
    const refresh = nonempty(e, 'BOX_OAUTH_REFRESH_TOKEN');
    if (refresh !== undefined) {
      sources.push(['oauth', oauth({ clientId, clientSecret }, refresh)]);
    }
  }
  const jwtConfig = nonempty(e, 'BOX_JWT_CONFIG');
  if (jwtConfig !== undefined) {
    sources.push(['jwt', await jwtSource(jwtConfig)]);
  }
  return sources;
}

/// Build a JWT flow from a Box `box_config.json` file.
async function jwtSource(path: string): Promise<Auth> {
  // The config is external JSON; `any` is the honest shape here.
  const cfg = JSON.parse(readFileSync(path, 'utf8'));
  const app = cfg.boxAppSettings;
  const appAuth = app.appAuth;
  return jwtAuth({
    clientId: app.clientID,
    clientSecret: app.clientSecret,
    publicKeyId: appAuth.publicKeyID,
    privateKeyPem: appAuth.privateKey,
    passphrase: appAuth.passphrase ?? undefined,
    enterpriseId: cfg.enterpriseID,
  });
}

test('VR-7 live smoke: per-flow auth + paginate + upload/download/delete', async () => {
  const e = loadEnv();
  const sources = await authSources(e);
  if (sources.length === 0) {
    console.error('VR-7: no Box credentials in the environment; skipping live smoke');
    return;
  }

  // One authenticated call per auth flow: GET /users/me must return the current
  // user, proving the flow yields a usable token.
  let primary: Client | undefined;
  for (const [name, auth] of sources) {
    const client = new Client(auth);
    const me = await getJson(client, `${client.baseUrl('api')}/users/me`);
    assert.ok(me.id, `${name}: /users/me returned no id`);
    console.error(`${name} auth: authenticated as user ${me.id}`);
    primary ??= client;
  }

  const client = primary as Client;
  await smokePaginate(client);
  await smokeUploadDownloadDelete(client);
});

/// Walk the root folder's items, following the marker cursor across pages just
/// like the generated paginators do.
async function smokePaginate(client: Client): Promise<void> {
  let seen = 0;
  let marker = '';
  for (let page = 0; page < 100; page++) {
    let req = withQuery(
      client.newRequest('GET', `${client.baseUrl('api')}/folders/0/items`),
      'limit',
      '100',
    );
    if (marker !== '') {
      req = withQuery(req, 'marker', marker);
    }
    const body = await fetchOk(client, req);
    const decoded = JSON.parse(new TextDecoder().decode(body));
    seen += Array.isArray(decoded.entries) ? decoded.entries.length : 0;
    const next = decoded.next_marker;
    if (typeof next === 'string' && next !== '') {
      marker = next;
    } else {
      break;
    }
  }
  console.error(`paginate: walked the root folder, ${seen} item(s)`);
}

/// Upload a small file to the root folder, download it back byte-for-byte, then
/// delete it — cleaning up even if the download fails.
async function smokeUploadDownloadDelete(client: Client): Promise<void> {
  const content = new TextEncoder().encode('box-gantry live smoke');
  // Unique per run: Box rejects a duplicate name in the same folder (409).
  const name = `box-gantry-smoke-${Date.now()}.txt`;
  const attributes = new TextEncoder().encode(
    JSON.stringify({ name, parent: { id: '0' } }),
  );

  const upReq = withMultipartBody(
    client.newRequest('POST', `${client.baseUrl('upload')}/files/content`),
    'attributes',
    attributes,
    'file',
    new Stream(content),
  );
  const upBody = await fetchOk(client, upReq);
  const uploaded = JSON.parse(new TextDecoder().decode(upBody));
  const fileId = uploaded.entries?.[0]?.id;
  assert.ok(typeof fileId === 'string' && fileId !== '', 'upload: no file id');
  console.error(`upload: created file ${fileId}`);

  // Download, capturing the result without asserting yet — the file must be
  // deleted even if the download fails, so a smoke run never leaks an artifact.
  const download = await client.fetch(
    client.newRequest('GET', `${client.baseUrl('api')}/files/${fileId}/content`),
  );

  // Delete unconditionally now that we hold the id.
  const deleted = await client.fetch(
    client.newRequest('DELETE', `${client.baseUrl('api')}/files/${fileId}`),
  );

  const dlCode = statusCode(download);
  assert.ok(dlCode >= 200 && dlCode < 300, `download: unexpected status ${dlCode}`);
  assert.deepEqual(responseBytes(download), content, 'download: content mismatch');
  console.error('download: content round-tripped');

  assert.equal(statusCode(deleted), 204, 'delete: expected 204');
  console.error(`delete: cleaned up file ${fileId}`);
}

/// GET a URL and decode a JSON object, failing on non-2xx.
async function getJson(client: Client, url: string): Promise<any> {
  const body = await fetchOk(client, client.newRequest('GET', url));
  return JSON.parse(new TextDecoder().decode(body));
}

/// Run a request and return the body bytes, failing on transport error or non-2xx.
async function fetchOk(client: Client, request: BoxRequest): Promise<Uint8Array> {
  const response = await client.fetch(request);
  const code = statusCode(response);
  const body = responseBytes(response);
  assert.ok(
    code >= 200 && code < 300,
    `unexpected status ${code}: ${new TextDecoder().decode(body)}`,
  );
  return body;
}
