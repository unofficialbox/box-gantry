// The shared token machinery behind every exchange flow: the `Auth` contract,
// the token-endpoint POST, and the single-flight access-token cache. Split out
// so both `auth.ts` (developer token, CCG, OAuth) and `jwt.ts` (JWT server auth)
// reuse it without either importing the other, and without leaking these
// internals into the package's public surface.

import { BoxApiError } from './errors.js';

/** Box's OAuth 2.0 token endpoint, shared by every exchange flow. */
export const DEFAULT_TOKEN_URL = 'https://api.box.com/oauth2/token';

/** Refresh a cached token this long before expiry, so in-flight requests never
 * race an expiry. */
const REFRESH_MARGIN_MS = 60_000;

/** A hard deadline on a token exchange, so a stalled endpoint can't leave a
 * single-flight refresh (and every caller awaiting it) hung forever. Matches
 * the Go runtime's 30s token HTTP client. */
const TOKEN_TIMEOUT_MS = 30_000;

/**
 * Yields an access token for the configured auth flow. A token acquisition is
 * not bound to any single request's cancellation — a shared refresh serves every
 * concurrent caller — mirroring the Rust runtime, which drops the whole future
 * rather than threading a per-request signal into token acquisition.
 */
export interface Auth {
  /** A valid access token, refreshing through the cache as needed. */
  accessToken(): Promise<string>;
  /**
   * Re-acquire a token after `stale` was rejected (a 401), bypassing the cache.
   * If another caller already refreshed past `stale`, the newer token is
   * returned without a network round-trip.
   */
  forceRefresh(stale: string): Promise<string>;
}

/** A normalized token exchange result. */
export interface Token {
  accessToken: string;
  refreshToken: string;
  ttlMs: number;
}

/**
 * Caches an access token and refreshes it — via the flow-specific `refresh` —
 * when it is missing or within the refresh margin of expiry. Concurrent callers
 * during a refresh share one in-flight exchange (single-flight), so a token is
 * never fetched twice at once, and the exchange is not tied to any one caller.
 */
export class CachedToken implements Auth {
  private token: string;
  private expiry: number;
  private inflight?: Promise<string>;

  constructor(
    private readonly refresh: () => Promise<Token>,
    seed?: Token,
  ) {
    this.token = seed?.accessToken ?? '';
    this.expiry = seed ? Date.now() + seed.ttlMs : 0;
  }

  async accessToken(): Promise<string> {
    if (this.token && this.expiry - Date.now() > REFRESH_MARGIN_MS) {
      return this.token;
    }
    return this.refreshOnce();
  }

  async forceRefresh(stale: string): Promise<string> {
    // Another caller may already have refreshed past the rejected token.
    if (this.token && this.token !== stale) {
      return this.token;
    }
    return this.refreshOnce();
  }

  /** Run the flow's refresh, sharing one in-flight exchange across callers. */
  private refreshOnce(): Promise<string> {
    if (!this.inflight) {
      this.inflight = this.refresh()
        .then((token) => {
          this.token = token.accessToken;
          this.expiry = Date.now() + token.ttlMs;
          this.inflight = undefined;
          return token.accessToken;
        })
        .catch((err: unknown) => {
          this.inflight = undefined;
          throw err;
        });
    }
    return this.inflight;
  }
}

/**
 * POST a form-encoded grant to the token endpoint and normalize the response.
 * A transport failure and a non-2xx body both surface as a `BoxApiError`, so
 * token acquisition never escapes the runtime's error contract. The optional
 * `signal` cancels an explicit caller-initiated exchange (`exchangeCode`); the
 * cached flows do not thread one, so a shared refresh is never bound to a single
 * request's cancellation.
 */
export async function postTokenForm(
  tokenUrl: string,
  form: Record<string, string>,
  signal?: AbortSignal,
): Promise<Token> {
  // An internal deadline independent of the caller's signal, combined with it
  // when present. It also covers the body read, since the signal aborts an
  // in-progress `fetch` (including body streaming). `AbortSignal.timeout` owns
  // its own timer, so there is nothing to clear.
  const deadline = AbortSignal.timeout(TOKEN_TIMEOUT_MS);
  const combined = signal ? AbortSignal.any([signal, deadline]) : deadline;
  let response: Awaited<ReturnType<typeof fetch>>;
  let text: string;
  try {
    response = await fetch(tokenUrl, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/x-www-form-urlencoded',
        Accept: 'application/json',
      },
      body: new URLSearchParams(form).toString(),
      signal: combined,
    });
    text = await response.text();
  } catch (err) {
    throw new BoxApiError('token endpoint request failed', { cause: err });
  }
  if (!response.ok) {
    throw new BoxApiError(`token endpoint returned ${response.status}: ${text.trim()}`, {
      status: response.status,
    });
  }
  let parsed: { access_token?: string; refresh_token?: string; expires_in?: number };
  try {
    parsed = JSON.parse(text) as typeof parsed;
  } catch {
    throw new BoxApiError('token endpoint returned invalid JSON');
  }
  if (!parsed.access_token) {
    throw new BoxApiError('token endpoint returned no access_token');
  }
  return {
    accessToken: parsed.access_token,
    refreshToken: parsed.refresh_token ?? '',
    ttlMs: (parsed.expires_in ?? 0) * 1000,
  };
}
