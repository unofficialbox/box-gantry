// VR-7 live smoke: exercise the hand-written `fetch` runtime against a real Box
// account — one authenticated call per configured auth flow, plus paginate +
// upload + download + delete, with the etag flow (304 / 412) on the uploaded
// file. It drives only the stable runtime contract
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
  type Response as BoxResponse,
  responseBytes,
  statusCode,
  Stream,
  BoxApiError,
  eventStream,
  parseJsonExact,
  representationContent,
  streamPosition,
  isPreconditionFailed,
  withFormBody,
  withHeader,
  withJsonBody,
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
  // user, proving the flow yields a usable token. Every flow runs before any
  // failure is raised, so one bad credential (an expired developer token)
  // doesn't hide the others' results.
  let primary: Client | undefined;
  const failed: string[] = [];
  for (const [name, auth] of sources) {
    const client = new Client(auth);
    try {
      const me = await getJson(client, `${client.baseUrl('api')}/users/me`);
      assert.ok(me.id, '/users/me returned no id');
      console.error(`${name} auth: authenticated as user ${me.id}`);
      primary ??= client;
    } catch (err) {
      console.error(`${name} auth: FAILED: ${err instanceof Error ? err.message : err}`);
      failed.push(name);
    }
  }

  if (primary === undefined) {
    throw new Error(`no auth flow yielded a usable token: ${failed.join(', ')}`);
  }
  await smokePaginate(primary);
  await smokeUploadDownloadDelete(primary);
  await smokeAsUser(primary);
  assert.deepEqual(failed, [], `auth flow(s) failed: ${failed.join(', ')}`);
});

/// Act as a managed user through `withAsUserHeader`: `/users/me` must answer
/// as that user. It needs an admin token from an app allowed to "make API
/// calls using the as-user header" and a managed user to act as; without them
/// Box answers 403 or lists no one, and the check is skipped with a notice
/// rather than failed (the account's setup, not the runtime, is missing).
async function smokeAsUser(client: Client): Promise<void> {
  const me = await getJson(client, `${client.baseUrl('api')}/users/me`);
  const listing = withQuery(
    withQuery(client.newRequest('GET', `${client.baseUrl('api')}/users`), 'user_type', 'managed'),
    'limit',
    '100',
  );
  const users = await asUserCall('listing managed users', () => client.fetch(listing));
  if (users === undefined) {
    return;
  }
  const entries = JSON.parse(new TextDecoder().decode(responseBytes(users))).entries ?? [];
  const target = entries.find(
    (user: { id: string; status: string }) => user.status === 'active' && user.id !== me.id,
  )?.id;
  if (typeof target !== 'string') {
    console.error('as-user: SKIPPED, no other active managed user to act as');
    return;
  }

  const asUser = client.withAsUserHeader(target);
  const acted = await asUserCall(`acting as user ${target}`, () =>
    asUser.fetch(asUser.newRequest('GET', `${asUser.baseUrl('api')}/users/me`)),
  );
  if (acted === undefined) {
    return;
  }
  const actedId = JSON.parse(new TextDecoder().decode(responseBytes(acted))).id;
  assert.equal(actedId, target, `as-user: /users/me answered user ${actedId}`);
  console.error(`as-user: acted as user ${target}`);
}

/// Run an as-user call; Box's 403 (the app or token may not act as users) is
/// logged as a skip and yields `undefined`, any other failure throws.
async function asUserCall(
  what: string,
  call: () => Promise<BoxResponse>,
): Promise<BoxResponse | undefined> {
  try {
    return await call();
  } catch (err) {
    if (err instanceof BoxApiError && err.status === 403) {
      console.error(
        `as-user: SKIPPED, ${what} got ${err.message} (enable "Make API calls using the as-user header" and use an admin token)`,
      );
      return undefined;
    }
    throw err;
  }
}

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
  const events = await smokeEvents(client, fileId).then(
    () => undefined,
    (err: unknown) => err,
  );
  const etags = await smokeEtags(client, fileId).then(
    () => undefined,
    (err: unknown) => err,
  );
  const representation = await smokeRepresentation(client, fileId, 'box-gantry live smoke').then(
    (length) => ({ length }),
    (err: unknown) => ({ err }),
  );
  const downscope = await smokeDownscope(client, fileId).then(
    (refusal) => ({ refusal }),
    (err: unknown) => ({ err }),
  );

  // Delete unconditionally now that we hold the id.
  const deleted = await client.fetch(
    client.newRequest('DELETE', `${client.baseUrl('api')}/files/${fileId}`),
  );

  const dlCode = statusCode(download);
  assert.ok(dlCode >= 200 && dlCode < 300, `download: unexpected status ${dlCode}`);
  assert.deepEqual(responseBytes(download), content, 'download: content mismatch');
  console.error('download: content round-tripped');

  if (etags !== undefined) {
    throw etags;
  }
  console.error('etag: 304 for the current etag, 412 for a stale If-Match, the current one writes');
  if ('err' in representation) {
    throw representation.err;
  }
  if (representation.length !== undefined) {
    console.error(`representation: Box's extracted text holds the uploaded text (${representation.length} bytes)`);
  }
  if (events !== undefined) {
    throw events;
  }
  console.error(`events: the long poll reported ITEM_RENAME for file ${fileId}`);
  if ('err' in downscope) {
    throw downscope.err;
  }
  console.error(`downscope: the narrow token renamed the file; its delete was refused (${downscope.refusal})`);

  assert.equal(statusCode(deleted), 204, 'delete: expected 204');
  console.error(`delete: cleaned up file ${fileId}`);
}

/// Fetch the uploaded file's extracted text through `representationContent` —
/// the request the generated `getRepresentationContent` builds — and find the
/// uploaded text in it within two minutes. Resolves to its length, or to
/// undefined when Box says it has no such representation for the file (none
/// offered, or it could not generate one: logged as a skip).
async function smokeRepresentation(client: Client, fileId: string, want: string): Promise<number | undefined> {
  let request = withQuery(client.newRequest('GET', `${client.baseUrl('api')}/files/${fileId}`), 'fields', 'representations');
  request = withHeader(request, 'x-rep-hints', '[extracted_text]');
  let text: string;
  try {
    const stream = await representationContent(client, request, 'representations', '', { maxWaitMs: 120_000 });
    text = new TextDecoder().decode(stream.data);
  } catch (err) {
    const message = String(err);
    if (message.includes('no representation matches') || message.includes('could not generate')) {
      console.error(`representation: SKIPPED, Box has no extracted text for file ${fileId} (${message})`);
      return undefined;
    }
    throw err;
  }
  assert.ok(text.includes(want), `representation: extracted text ${JSON.stringify(text)}, want it to contain ${JSON.stringify(want)}`);
  return text.length;
}

/// Drive Box's etag flow on the uploaded file, as the generated `…IfModified`
/// variants and the errors guide's re-read-and-retry loop expect it:
/// `If-None-Match` with the current etag is a 304 and a stale one reads the
/// file; `If-Match` with a stale etag is a 412 and the current one writes.
async function smokeEtags(client: Client, fileId: string): Promise<void> {
  const url = `${client.baseUrl('api')}/files/${fileId}`;
  const etag = (await getJson(client, url)).etag;
  assert.ok(typeof etag === 'string' && etag !== '', 'etag: the file read yields no etag');
  // Box's etags are version counters, so this one is well-formed but stale.
  const stale = `9${etag}`;

  const read = (value: string) =>
    client.fetch(withHeader(client.newRequest('GET', url), 'If-None-Match', value));
  await assert.rejects(
    read(etag),
    (err) => err instanceof BoxApiError && err.status === 304,
    'etag: If-None-Match with the current etag should be a 304',
  );
  await read(stale);

  const description = new TextEncoder().encode(
    JSON.stringify({ description: 'box-gantry etag smoke' }),
  );
  const write = (value: string) =>
    client.fetch(
      withHeader(withJsonBody(client.newRequest('PUT', url), description), 'If-Match', value),
    );
  await assert.rejects(
    write(stale),
    (err) => isPreconditionFailed(err),
    'etag: If-Match with a stale etag should be a 412',
  );
  await write(etag);
}

/// Exchange the session's token for one limited to renaming and previewing the
/// uploaded file — the form the generated `downscopeToken` posts — then prove
/// the narrow token renames the file and Box refuses its delete. Resolves to
/// the refusal; the caller still deletes the file with the original token.
async function smokeDownscope(client: Client, fileId: string): Promise<string> {
  const url = `${client.baseUrl('api')}/files/${fileId}`;
  const form = Object.entries({
    grant_type: 'urn:ietf:params:oauth:grant-type:token-exchange',
    subject_token: await client.accessToken(),
    subject_token_type: 'urn:ietf:params:oauth:token-type:access_token',
    scope: 'item_rename item_preview',
    resource: url,
  })
    .map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(value)}`)
    .join('&');
  const exchange = withFormBody(
    client.newRequest('POST', `${client.baseUrl('api_root')}/oauth2/token`),
    new TextEncoder().encode(form),
  );
  const token = JSON.parse(new TextDecoder().decode(await fetchOk(client, exchange))).access_token;
  assert.ok(typeof token === 'string' && token !== '', 'downscope: the exchange returned no access token');
  const narrow = new Client(developerToken(token));

  const name = new TextEncoder().encode(JSON.stringify({ name: `box-gantry-smoke-downscoped-${Date.now()}.txt` }));
  await fetchOk(narrow, withJsonBody(narrow.newRequest('PUT', url), name));
  let refusal = '';
  await assert.rejects(
    narrow.fetch(narrow.newRequest('DELETE', url)),
    (err) => {
      refusal = err instanceof Error ? err.message : String(err);
      return err instanceof BoxApiError && (err.status === 401 || err.status === 403);
    },
    'downscope: deleting with the narrow token should be refused (401/403)',
  );
  return refusal;
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

/// Follow Box's long-poll event stream from "now" while the uploaded file is
/// renamed, and see that rename (`ITEM_RENAME`) within a minute: the long poll
/// reports the change, and the page after it carries it.
async function smokeEvents(client: Client, fileId: string): Promise<void> {
  const api = client.baseUrl('api');
  const controller = new AbortController();
  const deadline = setTimeout(() => controller.abort(new Error('no ITEM_RENAME within a minute')), 60_000);
  // Rename the file once the stream is waiting, so the long poll reports it.
  const renamed = new Promise<void>((resolve, reject) => {
    setTimeout(() => {
      const name = new TextEncoder().encode(JSON.stringify({ name: `box-gantry-smoke-renamed-${Date.now()}.txt` }));
      client
        .fetch(withJsonBody(client.newRequest('PUT', `${api}/files/${fileId}`), name))
        .then(() => resolve(), reject);
    }, 3_000);
  });
  // Awaited below; handled here so a failure isn't an unhandled rejection meanwhile.
  renamed.catch(() => undefined);
  type RawEvent = { event_id?: string; event_type?: string; source?: { id?: string } };
  const source = {
    server: async () => {
      const body = await fetchOk(client, client.newRequest('OPTIONS', `${api}/events`));
      const listed = JSON.parse(new TextDecoder().decode(body)) as {
        entries?: Array<{ url?: string; retry_timeout?: number }>;
      };
      const server = listed.entries?.[0];
      assert.ok(server?.url !== undefined, 'events: no real-time server');
      return { url: server.url, retryTimeoutSeconds: server.retry_timeout };
    },
    page: async (position: string) => {
      const req = withQuery(client.newRequest('GET', `${api}/events`), 'stream_position', position);
      const page = parseJsonExact(new TextDecoder().decode(await fetchOk(client, req))) as {
        entries?: RawEvent[];
        next_stream_position?: unknown;
      };
      return { events: page.entries ?? [], next: streamPosition(page.next_stream_position) };
    },
    id: (event: RawEvent) => event.event_id,
  };
  try {
    for await (const event of eventStream(client, 'now', source, controller.signal)) {
      if (event.event_type === 'ITEM_RENAME' && event.source?.id === fileId) {
        break;
      }
    }
  } finally {
    clearTimeout(deadline);
    await renamed;
  }
}
