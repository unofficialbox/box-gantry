// The hand-written runtime the generated Box TypeScript SDK ships against
// (TR-TS.5). It implements the machine-readable runtime contract
// (gantry-contract V1): generated code calls only these declarations, and this
// module supplies the behavior — a retrying network layer (backoff + jitter,
// 401 refresh, Retry-After), auth token threading, request builders, and
// response accessors.
//
// It is the real implementation the compilable stubs stand in for during
// generation-time verification (FR-5.3). Because it exports the same names with
// the same signatures, the generated SDK type-checks against it unchanged. The
// module depends only on the platform `fetch`/`URLSearchParams`/`Headers`, so
// it runs on any modern JavaScript runtime.

import type { Auth } from './auth.js';
import { BoxApiError } from './errors.js';

export { BoxApiError } from './errors.js';
export * from './auth.js';

/** The default base URLs for the D-106 URL classes. */
const DEFAULT_BASE_URLS: Record<string, string> = {
  api: 'https://api.box.com/2.0',
  api_root: 'https://api.box.com',
  upload: 'https://upload.box.com/api/2.0',
  upload_session: 'https://upload.box.com/api/2.0',
  oauth_authorize: 'https://account.box.com/api/oauth2',
  download: 'https://api.box.com/2.0',
};

/** The runtime-owned HTTP request envelope, assembled by the `with*` builders
 * before `fetch` executes it. */
export class Request {
  readonly headers = new Headers();
  readonly query = new URLSearchParams();
  /** The buffered request body, if any (buffered so retries can replay it). */
  body?: Uint8Array;

  constructor(
    readonly method: string,
    readonly url: string,
  ) {}
}

/** The runtime-owned HTTP response envelope. The body is read fully so it can be
 * replayed as bytes or a stream, and so retries stay safe. */
export class Response {
  constructor(
    readonly status: number,
    readonly headers: Headers,
    readonly bodyBytes: Uint8Array,
  ) {}
}

/** A response/request body as bytes. The platform buffers it (the manifest's
 * `Streaming` axis allows buffering) so a retried request can replay it. */
export class Stream {
  constructor(readonly data: Uint8Array) {}

  /** An empty stream (e.g. an absent multipart file part). */
  static empty(): Stream {
    return new Stream(new Uint8Array(0));
  }
}

/** Options for a runtime `Client`. */
export interface ClientOptions {
  /** How many times a retriable failure is retried (default 5). */
  maxRetries?: number;
  /** Override individual base-URL classes (custom deployments). */
  baseUrls?: Record<string, string>;
  /** Override the platform `fetch` (same signature) — e.g. to inject an
   * instrumented or mocked transport. Defaults to the global `fetch`. */
  fetch?: typeof fetch;
}

/** The runtime session: it holds the auth flow, base-URL configuration, and
 * retry policy shared by every manager. */
export class Client {
  private readonly auth: Auth;
  private readonly baseUrls: Record<string, string>;
  private readonly maxRetries: number;
  private readonly fetchImpl: typeof fetch;

  constructor(auth: Auth, options: ClientOptions = {}) {
    this.auth = auth;
    const maxRetries = options.maxRetries ?? 5;
    if (!Number.isInteger(maxRetries) || maxRetries < 0) {
      throw new RangeError('maxRetries must be a non-negative integer');
    }
    this.maxRetries = maxRetries;
    this.baseUrls = { ...DEFAULT_BASE_URLS, ...(options.baseUrls ?? {}) };
    this.fetchImpl = options.fetch ?? fetch;
  }

  /** The configured base URL for a D-106 class, without a trailing slash. */
  baseUrl(name: string): string {
    return this.baseUrls[name] ?? '';
  }

  /** A valid access token for the configured auth flow. The `signal` is
   * accepted for contract symmetry; token acquisition is a shared refresh not
   * bound to a single request's cancellation (mirroring the Rust runtime). */
  async accessToken(_signal?: AbortSignal): Promise<string> {
    return this.auth.accessToken();
  }

  /** A request envelope for a method and fully built URL. */
  newRequest(method: string, url: string): Request {
    return new Request(method, url);
  }

  /** Execute the request with retries: exponential backoff + full jitter, a
   * single force-refresh on 401, and Retry-After (as a floor) on 429/5xx.
   *
   * Retry policy follows idempotency: a 429 (rate-limited, never processed)
   * retries for every method; a transport error or a 5xx may have already
   * committed a write, so those retry only for idempotent methods
   * (GET/HEAD/PUT/DELETE/…). The 401 refresh applies to every method. */
  async fetch(request: Request, signal?: AbortSignal): Promise<Response> {
    let token = await this.auth.accessToken();
    const query = request.query.toString();
    const url = query ? `${request.url}?${query}` : request.url;
    const idempotent = isIdempotent(request.method);

    let refreshed = false;
    let lastError: unknown;
    for (let attempt = 0; attempt <= this.maxRetries; attempt++) {
      const headers = new Headers(request.headers);
      headers.set('Authorization', `Bearer ${token}`);

      // `httpResponse` is the platform `Response` (via inference — the name is
      // shadowed by this module's `Response` class, so it is never annotated).
      let httpResponse;
      try {
        httpResponse = await this.fetchImpl(url, {
          method: request.method,
          headers,
          // A `Uint8Array` is a valid `BodyInit` at runtime; the cast bridges
          // the `Uint8Array<ArrayBufferLike>` vs `BufferSource` generic gap in
          // the DOM lib without narrowing the contract's plain-`Uint8Array` body.
          body: request.body as BodyInit | undefined,
          signal,
        });
      } catch (err) {
        // A transport error gives no response, so a non-idempotent write may
        // already have committed — only retry idempotent methods.
        lastError = err;
        if (attempt === this.maxRetries || !idempotent) {
          break;
        }
        await sleep(backoff(attempt), signal);
        continue;
      }

      const bodyBytes = new Uint8Array(await httpResponse.arrayBuffer());
      const response = new Response(httpResponse.status, httpResponse.headers, bodyBytes);

      // A single force-refresh on 401: re-acquire past the token cache so the
      // retry doesn't just resend the same rejected token.
      if (response.status === 401 && !refreshed) {
        refreshed = true;
        token = await this.auth.forceRefresh(token);
        continue;
      }
      if (shouldRetry(response.status, idempotent) && attempt < this.maxRetries) {
        await sleep(retryDelay(response, attempt), signal);
        continue;
      }
      return response;
    }
    throw new BoxApiError(`request failed after ${this.maxRetries + 1} attempts`, {
      cause: lastError,
    });
  }
}

/** The safety ceiling on any single retry sleep, so a hostile server-sent
 * Retry-After can't stall the client indefinitely. */
const MAX_RETRY_DELAY_MS = 30_000;

/** HTTP's idempotent methods: safe to replay after a transport error or 5xx. */
const IDEMPOTENT_METHODS = new Set(['GET', 'HEAD', 'PUT', 'DELETE', 'OPTIONS', 'TRACE']);

function isIdempotent(method: string): boolean {
  return IDEMPOTENT_METHODS.has(method.toUpperCase());
}

/** Whether a response status warrants a retry for a request of this idempotency.
 * A 429 (rate-limited, never processed) retries for any method; a 5xx may have
 * committed a write, so it retries only for idempotent methods. */
function shouldRetry(status: number, idempotent: boolean): boolean {
  return status === 429 || (status >= 500 && idempotent);
}

/** Full-jitter exponential backoff, capped at the max retry delay. */
function backoff(attempt: number): number {
  const base = Math.min(2 ** attempt * 500, MAX_RETRY_DELAY_MS);
  return Math.random() * base;
}

/** The delay before the next retry: exponential backoff, raised to a
 * (delay-seconds) Retry-After as a floor when present, and clamped to
 * `MAX_RETRY_DELAY_MS`. HTTP-date Retry-After values fall back to backoff (the
 * same posture as the Rust runtime). */
function retryDelay(response: Response, attempt: number): number {
  let delay = backoff(attempt);
  const header = response.headers.get('Retry-After');
  if (header && /^\d+$/.test(header.trim())) {
    delay = Math.max(delay, Number(header.trim()) * 1000);
  }
  return Math.min(delay, MAX_RETRY_DELAY_MS);
}

/** Sleep for `ms`, rejecting promptly if the signal aborts. */
function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(abortError(signal));
      return;
    }
    const onAbort = () => {
      clearTimeout(timer);
      reject(abortError(signal));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    signal?.addEventListener('abort', onAbort, { once: true });
  });
}

function abortError(signal?: AbortSignal): BoxApiError {
  const reason = signal?.reason;
  return reason instanceof Error
    ? new BoxApiError(`request aborted: ${reason.message}`)
    : new BoxApiError('request aborted');
}

/** Return the request with the header set. */
export function withHeader(request: Request, name: string, value: string): Request {
  request.headers.set(name, value);
  return request;
}

/** Return the request with the query parameter appended, encoded. */
export function withQuery(request: Request, name: string, value: string): Request {
  request.query.append(name, value);
  return request;
}

/** Return the request with the serialized JSON body and content type set. */
export function withJsonBody(request: Request, body: Uint8Array): Request {
  request.headers.set('Content-Type', 'application/json');
  request.body = body;
  return request;
}

/** Return the request with an application/x-www-form-urlencoded body (the
 * OAuth2 token endpoints). */
export function withFormBody(request: Request, form: Uint8Array): Request {
  request.headers.set('Content-Type', 'application/x-www-form-urlencoded');
  request.body = form;
  return request;
}

/** Return the request with a streaming body (buffered, so retries can replay
 * it — the manifest's `Streaming` axis permits buffering). */
export function withStreamBody(request: Request, body: Stream, contentType: string): Request {
  request.headers.set('Content-Type', contentType);
  request.body = body.data;
  return request;
}

/** Return the request with a Box-style multipart body: an `attributes` JSON
 * part plus a file part (G-7). */
export function withMultipartBody(
  request: Request,
  attributes: Uint8Array,
  fileName: string,
  file: Stream,
): Request {
  const boundary = `----gantryFormBoundary${Math.random().toString(36).slice(2)}`;
  const encoder = new TextEncoder();
  const escaped = fileName.replace(/["\r\n]/g, '_');
  const parts: Uint8Array[] = [
    encoder.encode(
      `--${boundary}\r\nContent-Disposition: form-data; name="attributes"\r\n\r\n`,
    ),
    attributes,
    encoder.encode(
      `\r\n--${boundary}\r\nContent-Disposition: form-data; name="file"; filename="${escaped}"\r\n` +
        'Content-Type: application/octet-stream\r\n\r\n',
    ),
    file.data,
    encoder.encode(`\r\n--${boundary}--\r\n`),
  ];
  request.headers.set('Content-Type', `multipart/form-data; boundary=${boundary}`);
  request.body = concat(parts);
  return request;
}

/** Concatenate byte chunks into one buffer. */
function concat(chunks: Uint8Array[]): Uint8Array {
  const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}

/** Read the whole response body. */
export function responseBytes(response: Response): Uint8Array {
  return response.bodyBytes;
}

/** The response body as a stream, for binary downloads (FR-7.4). */
export function responseStream(response: Response): Stream {
  return new Stream(response.bodyBytes);
}

/** A response header value, empty when absent (redirect Location, Retry-After
 * surfacing). */
export function responseHeader(response: Response, name: string): string {
  return response.headers.get(name) ?? '';
}

/** The response status code. */
export function statusCode(response: Response): number {
  return response.status;
}
