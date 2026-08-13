// The `fetch`-only Box auth flows, each an `Auth` passed to the runtime
// `Client`: `developerToken` (a fixed token), `clientCredentials` (CCG), and the
// OAuth 2.0 authorization-code flow. They exchange credentials at Box's token
// endpoint and cache the access token (via the shared machinery in `tokens.ts`).
// JWT server auth lives in `jwt.ts` — the only flow that reaches beyond `fetch`.

import { BoxApiError } from './errors.js';
import { type Auth, CachedToken, DEFAULT_TOKEN_URL, type Token, postTokenForm } from './tokens.js';

export type { Auth } from './tokens.js';

/** Where a user is sent to grant an OAuth 2.0 app access. */
const AUTHORIZE_URL = 'https://account.box.com/api/oauth2/authorize';

/**
 * The simplest flow: a fixed access token from the Box developer console. A
 * fixed token cannot be refreshed, so `forceRefresh` returns it unchanged (a
 * subsequent 401 then surfaces to the caller).
 */
export function developerToken(token: string): Auth {
  return {
    accessToken: async () => token,
    forceRefresh: async () => token,
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
  /** Override the platform `fetch` used for the token exchange — e.g. to match
   * the runtime `Client`'s own transport override. Defaults to the global
   * `fetch`. */
  fetch?: typeof fetch;
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
    postTokenForm(
      tokenUrl,
      {
        grant_type: 'client_credentials',
        client_id: config.clientId,
        client_secret: config.clientSecret,
        box_subject_type: subject.type,
        box_subject_id: subject.id,
      },
      undefined,
      config.fetch,
    ),
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
  /** Override the platform `fetch` used for token exchanges — e.g. to match
   * the runtime `Client`'s own transport override. Defaults to the global
   * `fetch`. */
  fetch?: typeof fetch;
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
    const token = await postTokenForm(
      tokenUrl,
      {
        grant_type: 'refresh_token',
        refresh_token: refreshToken,
        client_id: config.clientId,
        client_secret: config.clientSecret,
      },
      undefined,
      config.fetch,
    );
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
    config.fetch,
  );
  if (!token.refreshToken) {
    throw new BoxApiError('authorization-code exchange returned no refresh_token');
  }
  config.onRefresh?.(token.refreshToken);
  // Seed the cache with the access token we just obtained, and refresh via the
  // rotating refresh token thereafter.
  return new CachedToken(refreshTokenExchange(config, tokenUrl, token.refreshToken), token);
}
