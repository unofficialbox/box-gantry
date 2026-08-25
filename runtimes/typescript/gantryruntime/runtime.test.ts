// Local runtime unit tests: no Box credentials required, unlike livesmoke.test.ts.
// Run with `node --test runtime.test.ts` (type-checked by the same `tsc` gate).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';

import { Client, Stream, developerToken, statusCode, withHeader, withMultipartBody } from './src/runtime.js';

/** Concatenate byte chunks into one buffer (no `Buffer` — see node-live.d.ts). */
function concatBytes(chunks: Uint8Array[]): Uint8Array {
  const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}

/** Run `fn` against a server that echoes 200 and hands the raw request body
 * (decoded as UTF-8 text) back via the returned promise. */
async function captureRequestBody(fn: (url: string) => Promise<void>): Promise<string> {
  const chunks: Uint8Array[] = [];
  await withServer(
    (req, res) => {
      req.on('data', (chunk) => chunks.push(chunk));
      req.on('end', () => res.end());
    },
    fn,
  );
  return new TextDecoder().decode(concatBytes(chunks));
}

/** Start a local HTTP server, run `fn` against its URL, then shut it down. */
async function withServer(
  handler: (req: http.IncomingMessage, res: http.ServerResponse) => void,
  fn: (url: string) => Promise<void>,
): Promise<void> {
  const server = http.createServer(handler);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const address = server.address();
    if (address === null) {
      throw new Error('server did not bind a port');
    }
    await fn(`http://127.0.0.1:${address.port}/`);
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}

test('a client default header is sent, and a per-request header overrides it', async () => {
  const seen: Array<string | undefined> = [];
  await withServer(
    (req, res) => {
      seen.push(req.headers['x-trace-id'] as string | undefined);
      res.end();
    },
    async (url) => {
      const client = new Client(developerToken('t'), { headers: { 'X-Trace-Id': 'default' } });
      await client.fetch(client.newRequest('GET', url));
      // Different casing than the default, to prove the override is
      // case-insensitive, not just a literal-name match.
      const overriding = withHeader(client.newRequest('GET', url), 'x-trace-id', 'override');
      await client.fetch(overriding);
    },
  );
  assert.deepEqual(seen, ['default', 'override']);
});

test('a 401 with no retries left returns the response, not an error', async () => {
  await withServer(
    (req, res) => {
      res.statusCode = 401;
      res.end();
    },
    async (url) => {
      const client = new Client(developerToken('t'), { maxRetries: 0 });
      const response = await client.fetch(client.newRequest('GET', url));
      assert.equal(statusCode(response), 401);
    },
  );
});

test('withMultipartBody sends the bare JSON part and the file bytes', async () => {
  const body = await captureRequestBody(async (url) => {
    const client = new Client(developerToken('t'));
    const req = withMultipartBody(
      client.newRequest('POST', url),
      'attributes',
      new TextEncoder().encode('{"name":"f.txt"}'),
      'file',
      new Stream(new TextEncoder().encode('file bytes')),
    );
    await client.fetch(req);
  });
  assert.ok(body.includes('name="attributes"'));
  // The bare attributes object, not wrapped in another JSON layer.
  assert.ok(body.includes('{"name":"f.txt"}'));
  assert.ok(body.includes('name="file"; filename="file"'));
  assert.ok(body.includes('file bytes'));
});

// The avatar-upload shape has no attributes field at all (G-7); the bug this
// guards against sent a bogus empty attributes part regardless.
test('withMultipartBody omits an absent JSON part', async () => {
  const body = await captureRequestBody(async (url) => {
    const client = new Client(developerToken('t'));
    const req = withMultipartBody(
      client.newRequest('POST', url),
      '',
      new Uint8Array(0),
      'pic',
      new Stream(new TextEncoder().encode('avatar bytes')),
    );
    await client.fetch(req);
  });
  assert.ok(!body.includes('application/json'));
  assert.ok(body.includes('name="pic"; filename="pic"'));
});
