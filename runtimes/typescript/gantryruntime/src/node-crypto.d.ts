// A minimal ambient declaration for the slice of `node:crypto` that `jwt.ts`
// uses. Kept local (in a non-module `.d.ts`) so the runtime's `tsc` gate needs
// no `@types/node`, whose global `fetch`/`Request`/`Response` would clash with
// the DOM lib the core runtime relies on. The real module supplies the behavior
// at run time; a live smoke test (a later slice) exercises it end to end.

declare module 'node:crypto' {
  export interface KeyObject {
    readonly type: string;
    /** The asymmetric key algorithm (`'rsa'`, `'ec'`, …); absent for secret keys. */
    readonly asymmetricKeyType?: string;
  }
  export function createPrivateKey(input: { key: string; passphrase?: string }): KeyObject;
  export function sign(algorithm: string, data: Uint8Array, key: KeyObject): Uint8Array;
  export function randomBytes(size: number): Uint8Array;
}
