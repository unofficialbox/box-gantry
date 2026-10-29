// Box webhook signature verification (Box's v2 scheme). Each
// `BOX-SIGNATURE-*` header of a delivery is the base64 HMAC-SHA256, under that
// signature key, of the body followed by the `BOX-DELIVERY-TIMESTAMP` header.
// Built on WebCrypto (`crypto.subtle`), so it runs in Node and browsers alike
// and needs no `node:crypto`; `subtle.verify` compares in constant time.

/** How old a delivery may be, in seconds, before it's rejected as a possible
 * replay: Box's ten minutes. */
export const DEFAULT_WEBHOOK_MAX_AGE_SECONDS = 600;

/** The keys and limits a delivery is checked against. */
export interface WebhookKeys {
  /** The app's primary signature key, from the Developer Console. */
  primaryKey?: string;
  /** The app's secondary signature key. A delivery is genuine when the
   * signature of any configured key matches, so a key can be rotated while the
   * other keeps verifying. */
  secondaryKey?: string;
  /** How old a delivery may be (default `DEFAULT_WEBHOOK_MAX_AGE_SECONDS`). */
  maxAgeSeconds?: number;
  /** The clock (default: now). */
  now?: Date;
}

/**
 * Whether `body` and `headers` are a genuine, recent delivery from Box. Pass
 * the body exactly as received; header names match in any case. Resolves
 * `false` for an unknown signature version or algorithm, a delivery older than
 * the maximum age or timestamped in the future, and a signature no configured
 * key produces.
 */
export async function verifyWebhook(
  body: Uint8Array | string,
  headers: Headers | Record<string, string | undefined>,
  keys: WebhookKeys,
): Promise<boolean> {
  const header = (name: string): string | undefined => {
    if (headers instanceof Headers) {
      return headers.get(name) ?? undefined;
    }
    for (const [key, value] of Object.entries(headers)) {
      if (key.toLowerCase() === name) {
        return value;
      }
    }
    return undefined;
  };
  if (header('box-signature-version') !== '1' || header('box-signature-algorithm') !== 'HmacSHA256') {
    return false;
  }
  const timestamp = header('box-delivery-timestamp');
  const sentMs = timestamp === undefined ? NaN : Date.parse(timestamp);
  if (Number.isNaN(sentMs)) {
    return false;
  }
  // Whole seconds, as Box's own SDKs compare them.
  const sent = Math.floor(sentMs / 1000);
  const at = Math.floor((keys.now ?? new Date()).getTime() / 1000);
  if (sent > at || at - sent > (keys.maxAgeSeconds ?? DEFAULT_WEBHOOK_MAX_AGE_SECONDS)) {
    return false;
  }

  const encoder = new TextEncoder();
  const raw = typeof body === 'string' ? encoder.encode(body) : Uint8Array.from(body);
  const bodies = [raw];
  const escaped = escapeNonAscii(raw);
  if (escaped !== undefined) {
    bodies.push(encoder.encode(escaped));
  }
  const stamp = encoder.encode(timestamp);
  const signed: [string | undefined, string][] = [
    [keys.primaryKey, 'box-signature-primary'],
    [keys.secondaryKey, 'box-signature-secondary'],
  ];
  for (const [key, name] of signed) {
    const received = decodeBase64(header(name));
    if (key === undefined || key === '' || received === undefined) {
      continue;
    }
    const hmac = await crypto.subtle.importKey(
      'raw',
      encoder.encode(key),
      { name: 'HMAC', hash: 'SHA-256' },
      false,
      ['verify'],
    );
    for (const candidate of bodies) {
      const message = new Uint8Array(candidate.length + stamp.length);
      message.set(candidate);
      message.set(stamp, candidate.length);
      if (await crypto.subtle.verify('HMAC', hmac, received, message)) {
        return true;
      }
    }
  }
  return false;
}

/** Standard base64 to bytes; `undefined` when absent, empty, or malformed. */
function decodeBase64(value: string | undefined): Uint8Array<ArrayBuffer> | undefined {
  if (value === undefined || value === '') {
    return undefined;
  }
  try {
    return Uint8Array.from(atob(value), (c) => c.charCodeAt(0));
  } catch {
    return undefined;
  }
}

/** `body` with every character from U+007F up written as a JSON `\uXXXX`
 * escape (UTF-16, lowercase hex) — the form Box signs. A body re-encoded as
 * raw UTF-8 on its way to the caller verifies through it. `undefined` when
 * `body` is already all ASCII, or is not UTF-8. */
function escapeNonAscii(body: Uint8Array): string | undefined {
  let text: string;
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(body);
  } catch {
    return undefined;
  }
  let out = '';
  let escaped = false;
  for (let i = 0; i < text.length; i++) {
    const unit = text.charCodeAt(i);
    if (unit < 0x7f) {
      out += text[i];
    } else {
      escaped = true;
      out += `\\u${unit.toString(16).padStart(4, '0')}`;
    }
  }
  return escaped ? out : undefined;
}
