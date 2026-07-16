// The Box auth flows, each an `Auth` passed to the runtime `Client`.
//
// `developerToken` is a fixed token; `clientCredentials` (CCG) and the OAuth 2.0
// authorization-code flow exchange credentials at Box's token endpoint and cache
// the resulting access token until shortly before it expires. JWT server auth
// (which needs an RSA signing key) is a follow-up slice — it is the only flow
// that reaches beyond the platform `fetch`.

import { BoxApiError } from './errors.js';

/** Box's OAuth 2.0 token endpoint, shared by every exchange flow. */
const DEFAULT_TOKEN_URL = 'https://api.box.com/oauth2/token';

/** Where a user is sent to grant an OAuth 2.0 app access. */
const AUTHORIZE_URL = 'https://account.box.com/api/oauth2/authorize';

/** Refresh a cached token this long before expiry, so in-flight requests never
 * race an expiry. */
const REFRESH_MARGIN_MS = 60_000;

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
interface Token {
  accessToken: string;
  refreshToken: string;
  ttlMs: number;
}

/**
 * The simplest flow: a fixed access token from the Box developer console. The
 * other flows implement the same `Auth` interface and can be passed to the
 * client in its place. A fixed token cannot be refreshed, so `forceRefresh`
 * returns it unchanged (a subsequent 401 then surfaces to the caller).
 */
export function developerToken(token: string): Auth {
  return {
    accessToken: async () => token,
    forceRefresh: async () => token,
  };
}

/**
 * Caches an access token and refreshes it — via the flow-specific `refresh` —
 * when it is missing or within the refresh margin of expiry. Concurrent callers
 * during a refresh share one in-flight exchange (single-flight), so a token is
 * never fetched twice at once, and the exchange is not tied to any one caller.
 */
class CachedToken implements Auth {
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
async function postTokenForm(
  tokenUrl: string,
  form: Record<string, string>,
  signal?: AbortSignal,
): Promise<Token> {
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
      signal,
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

/**
 * The Client Credentials Grant: server-to-server auth with no signing key. Set
 * exactly one subject — `enterpriseId` for the service account, or `userId` to
 * act as a managed user.
 */
export interface CcgConfig {
  clientId: string;
  clientSecret: string;
  enterpriseId?: string;
  userId?: string;
  /** Optional; defaults to Box's token endpoint. */
  tokenUrl?: string;
}

/** Build a CCG `Auth`. Requires a subject: `enterpriseId` or `userId`. */
export function clientCredentials(config: CcgConfig): Auth {
  if (!config.userId && !config.enterpriseId) {
    throw new BoxApiError('clientCredentials requires a subject: enterpriseId or userId');
  }
  const tokenUrl = config.tokenUrl ?? DEFAULT_TOKEN_URL;
  // A `userId` acts as that managed user; otherwise the enterprise service
  // account. The guard above rules out an empty subject.
  const subject = config.userId
    ? { type: 'user', id: config.userId }
    : { type: 'enterprise', id: config.enterpriseId ?? '' };
  return new CachedToken(() =>
    postTokenForm(tokenUrl, {
      grant_type: 'client_credentials',
      client_id: config.clientId,
      client_secret: config.clientSecret,
      box_subject_type: subject.type,
      box_subject_id: subject.id,
    }),
  );
}

/**
 * The OAuth 2.0 authorization-code flow (a user grants a Box app access). Use
 * `authorizeUrl` to build the redirect, `exchangeCode` to turn the returned code
 * into an `Auth`, or `oauth` to resume from a stored refresh token.
 */
export interface OAuthConfig {
  clientId: string;
  clientSecret: string;
  /** Optional; defaults to Box's token endpoint. */
  tokenUrl?: string;
  /**
   * Called with each rotated refresh token (Box invalidates the previous one on
   * every exchange). Persist it so a later `oauth` resume does not start from a
   * stale, already-invalidated token.
   */
  onRefresh?: (refreshToken: string) => void;
}

/**
 * Build the URL to redirect a user to so they can grant access. `state` is
 * echoed back to the redirect URI for CSRF protection.
 */
export function authorizeUrl(config: OAuthConfig, redirectUri: string, state: string): string {
  const query = new URLSearchParams({
    response_type: 'code',
    client_id: config.clientId,
    redirect_uri: redirectUri,
    state,
  });
  return `${AUTHORIZE_URL}?${query.toString()}`;
}

/**
 * The refresh exchange for the authorization-code flow: it swaps the current
 * refresh token for a fresh access token and rotates the refresh token Box
 * returns (Box invalidates the old one on each exchange), reporting each rotated
 * token through `config.onRefresh` so a caller can persist it.
 */
function refreshTokenExchange(
  config: OAuthConfig,
  tokenUrl: string,
  initialRefresh: string,
): () => Promise<Token> {
  let refreshToken = initialRefresh;
  return async () => {
    const token = await postTokenForm(tokenUrl, {
      grant_type: 'refresh_token',
      refresh_token: refreshToken,
      client_id: config.clientId,
      client_secret: config.clientSecret,
    });
    if (token.refreshToken) {
      refreshToken = token.refreshToken;
      config.onRefresh?.(token.refreshToken);
    }
    return token;
  };
}

/**
 * Resume the authorization-code flow from a previously stored refresh token,
 * exchanging it for access tokens as needed.
 */
export function oauth(config: OAuthConfig, refreshToken: string): Auth {
  const tokenUrl = config.tokenUrl ?? DEFAULT_TOKEN_URL;
  return new CachedToken(refreshTokenExchange(config, tokenUrl, refreshToken));
}

/**
 * Exchange an authorization code for an `Auth` that refreshes itself thereafter.
 * The initial refresh token is reported through `config.onRefresh` so it can be
 * persisted immediately.
 */
export async function exchangeCode(
  config: OAuthConfig,
  code: string,
  redirectUri: string,
  signal?: AbortSignal,
): Promise<Auth> {
  const tokenUrl = config.tokenUrl ?? DEFAULT_TOKEN_URL;
  const token = await postTokenForm(
    tokenUrl,
    {
      grant_type: 'authorization_code',
      code,
      client_id: config.clientId,
      client_secret: config.clientSecret,
      redirect_uri: redirectUri,
    },
    signal,
  );
  if (!token.refreshToken) {
    throw new BoxApiError('authorization-code exchange returned no refresh_token');
  }
  config.onRefresh?.(token.refreshToken);
  // Seed the cache with the access token we just obtained, and refresh via the
  // rotating refresh token thereafter.
  return new CachedToken(refreshTokenExchange(config, tokenUrl, token.refreshToken), token);
}
