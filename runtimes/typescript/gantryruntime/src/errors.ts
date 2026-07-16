// The runtime error type, in its own module so both the core runtime and the
// auth flows can throw it without an import cycle.

/**
 * A runtime error: a failed request, auth acquisition, or body decode. The
 * generated SDK's exceptions model (TR-TS.3) surfaces every failure as a thrown
 * `BoxApiError` — the TypeScript analogue of Go's `error` return and Rust's
 * `Result::Err`.
 */
export class BoxApiError extends Error {
  /** The HTTP status, when the error carries a response. */
  readonly status?: number;

  constructor(message: string, status?: number) {
    super(message);
    this.name = 'BoxApiError';
    this.status = status;
  }
}
