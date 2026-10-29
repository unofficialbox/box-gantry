// Local runtime unit tests: no Box credentials required, unlike livesmoke.test.ts.
// Run with `node --test runtime.test.ts` (type-checked by the same `tsc` gate).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';

import { BoxApiError } from './src/errors.js';
import {
  Client,
  Stream,
  developerToken,
  verifyWebhook,
  withHeader,
  withMultipartBody,
} from './src/runtime.js';

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

test('a 401 with no retries left surfaces as a BoxApiError', async () => {
  await withServer(
    (req, res) => {
      res.statusCode = 401;
      res.end();
    },
    async (url) => {
      const client = new Client(developerToken('t'), { maxRetries: 0 });
      await assert.rejects(client.fetch(client.newRequest('GET', url)), (err: unknown) => {
        assert.ok(err instanceof BoxApiError);
        assert.equal(err.status, 401);
        return true;
      });
    },
  );
});

// A non-2xx response is a thrown error carrying Box's error fields, never a
// resolved response whose error body a caller would decode as a result.
test('a non-2xx response throws a BoxApiError with the Box error fields', async () => {
  await withServer(
    (req, res) => {
      res.statusCode = 404;
      res.setHeader('Content-Type', 'application/json');
      res.end(
        '{"type":"error","status":404,"code":"not_found","message":"Not Found","request_id":"abc123"}',
      );
    },
    async (url) => {
      const client = new Client(developerToken('t'));
      await assert.rejects(client.fetch(client.newRequest('GET', url)), (err: unknown) => {
        assert.ok(err instanceof BoxApiError);
        assert.equal(err.status, 404);
        assert.equal(err.code, 'not_found');
        assert.equal(err.boxMessage, 'Not Found');
        assert.equal(err.requestId, 'abc123');
        assert.equal(err.message, 'box: status 404 not_found: Not Found (request_id abc123)');
        return true;
      });
    },
  );
});

test('a non-JSON error body is kept raw on the BoxApiError', async () => {
  await withServer(
    (req, res) => {
      res.statusCode = 502;
      res.end('upstream down');
    },
    async (url) => {
      const client = new Client(developerToken('t'), { maxRetries: 0 });
      await assert.rejects(client.fetch(client.newRequest('GET', url)), (err: unknown) => {
        assert.ok(err instanceof BoxApiError);
        assert.equal(err.status, 502);
        assert.equal(err.code, undefined);
        assert.equal(new TextDecoder().decode(err.body), 'upstream down');
        return true;
      });
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

// The vectors from Box's own SDK test suite (testWebhookValidation): keys,
// bodies, and the signatures Box computed for them, all delivered at
// 2020-01-01T00:00:00-07:00.
const WEBHOOK_PRIMARY_KEY = 'SamplePrimaryKey';
const WEBHOOK_SECONDARY_KEY = 'SampleSecondaryKey';
const WEBHOOK_TIMESTAMP = '2020-01-01T00:00:00-07:00';
const WEBHOOK_BODY =
  '{"type":"webhook_event","webhook":{"id":"1234567890"},"trigger":"FILE.UPLOADED","source":{"id":"1234567890","type":"file","name":"Test.txt"}}';
const WEBHOOK_PRIMARY = '6TfeAW3A1PASkgboxxA5yqHNKOwFyMWuEXny/FPD5hI=';
const WEBHOOK_FILE_PREFIX =
  '{"webhook":{"id":"1234567890"},"trigger":"FILE.UPLOADED","source":{"id":"1234567890","type":"file","name":"';

/** Box's delivery headers, in mixed case on purpose. */
function webhookHeaders(primary: string): Record<string, string> {
  return {
    'box-delivery-id': 'f96bb54b-ee16-4fc5-aa65-8c2d9e5b546f',
    'BOX-DELIVERY-TIMESTAMP': WEBHOOK_TIMESTAMP,
    'box-signature-algorithm': 'HmacSHA256',
    'box-signature-primary': primary,
    'Box-Signature-Secondary': 'v+1CD1Jdo3muIcbpv5lxxgPglOqMfsNHPV899xWYydo=',
    'box-signature-version': '1',
  };
}

const deliveredAt = new Date('2020-01-01T07:00:00Z');
const bothKeys = { primaryKey: WEBHOOK_PRIMARY_KEY, secondaryKey: WEBHOOK_SECONDARY_KEY, now: deliveredAt };

test('verifyWebhook accepts the signatures Box computed', async () => {
  assert.equal(await verifyWebhook(WEBHOOK_BODY, webhookHeaders(WEBHOOK_PRIMARY), bothKeys), true);
  assert.equal(
    await verifyWebhook(
      new TextEncoder().encode(WEBHOOK_BODY),
      new Headers(webhookHeaders(WEBHOOK_PRIMARY)),
      bothKeys,
    ),
    true,
    'bytes and a Headers object work too',
  );
  assert.equal(
    await verifyWebhook(WEBHOOK_BODY, webhookHeaders('not-the-primary'), {
      secondaryKey: WEBHOOK_SECONDARY_KEY,
      now: deliveredAt,
    }),
    true,
    'the secondary key alone verifies, as during a primary rotation',
  );
  // Box signs non-ASCII characters as \uXXXX escapes; a body that arrives as
  // raw UTF-8 verifies through that form. Escapes already in the JSON
  // (\r, \/, \\) are signed as they are.
  const cases: [string, string][] = [
    ['スクリーンショット 2020-08-05.txt', 'LV2uCu+5NJtIHrCXDYgZ0v/PP5THGRuegw3RtdnEyuE='],
    ['😀 2020-08-05.txt', 'xF/SDZosX4le+v4A0Qn59sZhuD1RqY5KRUKzVMSbh0E='],
    ['test \\r', 'SVkbKgy3dEEf2PbbzpNu2lDZS7zZ/aboU7HOZgBGrJk='],
    ['\\/', 't41PWT5ZB6OcysnD6SDy9Ud+p9hdXxIdXqcdweyZv/Q='],
    ['\\\\', 'ERpMZwUQsGDTfj82ehdX6VvDZfvOhK5ULNfVmwVAGe0='],
  ];
  for (const [name, signature] of cases) {
    const body = `${WEBHOOK_FILE_PREFIX}${name}"}}`;
    const keys = { primaryKey: WEBHOOK_PRIMARY_KEY, now: deliveredAt };
    assert.equal(await verifyWebhook(body, webhookHeaders(signature), keys), true, name);
  }
  const atTheLimit = { ...bothKeys, now: new Date('2020-01-01T07:10:00Z') };
  assert.equal(
    await verifyWebhook(WEBHOOK_BODY, webhookHeaders(WEBHOOK_PRIMARY), atTheLimit),
    true,
    'exactly the maximum age old is still in time',
  );
});

test('verifyWebhook rejects what Box did not send', async () => {
  const headers = webhookHeaders(WEBHOOK_PRIMARY);
  const without = (...names: string[]): Record<string, string> =>
    Object.fromEntries(Object.entries(headers).filter(([k]) => !names.includes(k.toLowerCase())));
  const withValue = (name: string, value: string) => ({ ...without(name), [name]: value });
  const tampered = WEBHOOK_BODY.replace('Test.txt', 'Test.exe');
  const cases: [string, string, Record<string, string>, Parameters<typeof verifyWebhook>[2]][] = [
    ['wrong keys', WEBHOOK_BODY, headers, { primaryKey: 'IncorrectKey', secondaryKey: 'IncorrectKey', now: deliveredAt }],
    ['no keys', WEBHOOK_BODY, headers, { now: deliveredAt }],
    ['secondary key for primary', WEBHOOK_BODY, headers, { primaryKey: WEBHOOK_SECONDARY_KEY, now: deliveredAt }],
    ['tampered body', tampered, headers, bothKeys],
    ['stale', WEBHOOK_BODY, headers, { ...bothKeys, now: new Date('2020-01-01T07:10:01Z') }],
    ['future', WEBHOOK_BODY, headers, { ...bothKeys, now: new Date('2020-01-01T06:59:59Z') }],
    ['short max age', WEBHOOK_BODY, headers, { ...bothKeys, maxAgeSeconds: 60, now: new Date('2020-01-01T07:01:01Z') }],
    ['no timestamp', WEBHOOK_BODY, without('box-delivery-timestamp'), bothKeys],
    ['bad timestamp', WEBHOOK_BODY, withValue('box-delivery-timestamp', 'yesterday'), bothKeys],
    ['no signatures', WEBHOOK_BODY, without('box-signature-primary', 'box-signature-secondary'), bothKeys],
    ['no version', WEBHOOK_BODY, without('box-signature-version'), bothKeys],
    ['version 2', WEBHOOK_BODY, withValue('box-signature-version', '2'), bothKeys],
    ['HmacSHA1', WEBHOOK_BODY, withValue('box-signature-algorithm', 'HmacSHA1'), bothKeys],
    ['not base64', WEBHOOK_BODY, withValue('box-signature-primary', '%%%'), { primaryKey: WEBHOOK_PRIMARY_KEY, now: deliveredAt }],
  ];
  for (const [name, body, h, keys] of cases) {
    assert.equal(await verifyWebhook(body, h, keys), false, name);
  }
});
