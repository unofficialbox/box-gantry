// JWT server auth — the fourth Box auth flow (TR-TS.5 parity with Go/Rust).
//
// Unlike the other three flows, this one needs an RSA signing key, so it uses
// `node:crypto` and runs only on Node-like runtimes. It is a leaf module — not
// re-exported by `runtime.ts` — and imports `node:crypto` lazily, so importing
// the core runtime or the `fetch`-only flows never pulls in a Node dependency.
// Reach it via the package's `./jwt` export.

import { BoxApiError } from './errors.js';
import { type Auth, CachedToken, DEFAULT_TOKEN_URL, postTokenForm } from './tokens.js';

// The `node:crypto` surface this flow uses is declared ambiently in
// `node-crypto.d.ts` (kept local so the gate needs no `@types/node`).
type Crypto = typeof import('node:crypto');
type PrivateKey = import('node:crypto').KeyObject;

/**
 * JWT server auth with a signing key (Box's `box_config.json` fields).
 * `privateKeyPem` is the RSA private key, optionally encrypted with
 * `passphrase`. Set exactly one subject — `enterpriseId` or `userId`.
 */
export interface JwtConfig {
  clientId: string;
  clientSecret: string;
  publicKeyId: string;
  privateKeyPem: string;
  passphrase?: string;
  enterpriseId?: string;
  userId?: string;
  /** Optional; defaults to Box's token endpoint. */
  tokenUrl?: string;
}

/**
 * Build a JWT-assertion `Auth`, parsing (and if needed decrypting) the RSA
 * private key up front so a bad key fails loudly here rather than on the first
 * request. Returns a promise because it imports `node:crypto` on demand.
 */
export async function jwtAuth(config: JwtConfig): Promise<Auth> {
  if (!config.userId && !config.enterpriseId) {
    throw new BoxApiError('jwtAuth requires a subject: enterpriseId or userId');
  }
  const crypto: Crypto = await import('node:crypto');
  let privateKey: PrivateKey;
  try {
    privateKey = crypto.createPrivateKey(
      config.passphrase !== undefined
        ? { key: config.privateKeyPem, passphrase: config.passphrase }
        : { key: config.privateKeyPem },
    );
  } catch (err) {
    throw new BoxApiError('failed to parse the JWT private key', { cause: err });
  }
  // The assertion is RS256-signed, so a non-RSA key would fail confusingly at
  // sign time — reject it up front (parity with the Go/Rust runtimes).
  if (privateKey.asymmetricKeyType !== 'rsa') {
    throw new BoxApiError(
      `JWT auth requires an RSA private key, got ${privateKey.asymmetricKeyType ?? 'an unrecognized key type'}`,
    );
  }
  const tokenUrl = config.tokenUrl ?? DEFAULT_TOKEN_URL;
  const subject = config.userId
    ? { type: 'user', id: config.userId }
    : { type: 'enterprise', id: config.enterpriseId ?? '' };
  return new CachedToken(async () => {
    const assertion = signAssertion(crypto, config, privateKey, subject, tokenUrl);
    return postTokenForm(tokenUrl, {
      grant_type: 'urn:ietf:params:oauth:grant-type:jwt-bearer',
      assertion,
      client_id: config.clientId,
      client_secret: config.clientSecret,
    });
  });
}

/**
 * Build and RS256-sign the JWT bearer assertion Box's jwt-bearer grant expects.
 * The claim set is single-use: a random `jti` and a short (45s) expiry.
 */
function signAssertion(
  crypto: Crypto,
  config: JwtConfig,
  privateKey: PrivateKey,
  subject: { type: string; id: string },
  audience: string,
): string {
  const header = { alg: 'RS256', typ: 'JWT', kid: config.publicKeyId };
  const claims = {
    iss: config.clientId,
    sub: subject.id,
    box_sub_type: subject.type,
    aud: audience,
    jti: hex(crypto.randomBytes(16)),
    exp: Math.floor(Date.now() / 1000) + 45,
  };
  const signingInput = `${base64urlJson(header)}.${base64urlJson(claims)}`;
  const signature = crypto.sign('RSA-SHA256', new TextEncoder().encode(signingInput), privateKey);
  return `${signingInput}.${base64url(signature)}`;
}

/** Base64url-encode a JSON value's UTF-8 bytes (no padding). */
function base64urlJson(value: unknown): string {
  return base64url(new TextEncoder().encode(JSON.stringify(value)));
}

/** Base64url-encode bytes (RFC 7515 §2: base64 with `+/` → `-_`, no padding). */
function base64url(bytes: Uint8Array): string {
  let binary = '';
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/** Lowercase hex of bytes, for the assertion `jti`. */
function hex(bytes: Uint8Array): string {
  let out = '';
  for (const byte of bytes) {
    out += byte.toString(16).padStart(2, '0');
  }
  return out;
}
