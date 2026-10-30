// The runtime error type, in its own module so both the core runtime and the
// auth flows can throw it without an import cycle.

/** Options for a `BoxApiError`. */
export interface BoxApiErrorOptions {
  /** The HTTP status, when the error carries a response. */
  status?: number;
  /** Box's error `code` (e.g. `not_found`), from a Box error body. */
  code?: string;
  /** Box's error `message`, from a Box error body. */
  boxMessage?: string;
  /** Box's `request_id`, from a Box error body — quote it to Box support. */
  requestId?: string;
  /** The raw response body of a non-2xx response. */
  body?: Uint8Array;
  /** The underlying failure (a transport error, a decode error), preserved for
   * diagnostics. */
  cause?: unknown;
}

/**
 * A runtime error: a failed request, auth acquisition, or body decode. The
 * generated SDK's exceptions model (TR-TS.3) surfaces every failure as a thrown
 * `BoxApiError` — the TypeScript analogue of Go's `error` return and Rust's
 * `Result::Err`.
 */
export class BoxApiError extends Error {
  /** The HTTP status, when the error carries a response. */
  readonly status?: number;
  /** Box's error `code` (e.g. `not_found`), when the body was a Box error. */
  readonly code?: string;
  /** Box's error `message`, when the body was a Box error. */
  readonly boxMessage?: string;
  /** Box's `request_id`, when the body was a Box error. */
  readonly requestId?: string;
  /** The raw response body of a non-2xx response. */
  readonly body?: Uint8Array;

  constructor(message: string, options: BoxApiErrorOptions = {}) {
    super(message, options.cause !== undefined ? { cause: options.cause } : undefined);
    this.name = 'BoxApiError';
    this.status = options.status;
    this.code = options.code;
    this.boxMessage = options.boxMessage;
    this.requestId = options.requestId;
    this.body = options.body;
  }
}

/** Whether `err` is Box's 412 Precondition Failed: an `If-Match` write whose
 * etag no longer matches, because the item changed since it was read. Re-read
 * the item and retry with its new etag. */
export function isPreconditionFailed(err: unknown): err is BoxApiError {
  return err instanceof BoxApiError && err.status === 412;
}

/** The error for a non-2xx response: `Client.fetch` throws it once the retry
 * budget and the single 401 refresh are spent, so a caller never mistakes an
 * error body for a result. Box's error fields are read from the body when it
 * is a Box error object; the raw body is kept either way. */
export function apiErrorFromResponse(status: number, body: Uint8Array): BoxApiError {
  let payload: unknown;
  try {
    payload = JSON.parse(new TextDecoder().decode(body));
  } catch {
    payload = undefined;
  }
  const field = (key: string): string | undefined => {
    if (typeof payload !== 'object' || payload === null) {
      return undefined;
    }
    const value = (payload as Record<string, unknown>)[key];
    return typeof value === 'string' ? value : undefined;
  };
  // A Box error object, or from the token endpoint an OAuth 2.0 error.
  const code = field('code') ?? field('error');
  const boxMessage = field('message') ?? field('error_description');
  const requestId = field('request_id');
  let message = `box: status ${status}`;
  if (code !== undefined) {
    message += ` ${code}`;
  }
  if (boxMessage !== undefined) {
    message += `: ${boxMessage}`;
  }
  if (requestId !== undefined) {
    message += ` (request_id ${requestId})`;
  }
  return new BoxApiError(message, { status, code, boxMessage, requestId, body });
}
