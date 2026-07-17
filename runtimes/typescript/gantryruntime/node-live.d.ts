// A minimal ambient declaration for the slice of `node:test` / `node:assert` /
// `node:fs` / `node:process` the VR-7 live smoke uses — kept local so the
// runtime's `tsc` gate needs no `@types/node` (whose global
// `fetch`/`Request`/`Response` would clash with the DOM lib the runtime relies
// on). Node supplies the behavior at run time (the smoke runs under
// `node --test`, not the gate). The same trick `jwt.ts` uses for `node:crypto`.

declare module 'node:test' {
  export function test(name: string, fn: () => void | Promise<void>): void;
}
declare module 'node:assert/strict' {
  interface StrictAssert {
    ok(value: unknown, message?: string): void;
    equal(actual: unknown, expected: unknown, message?: string): void;
    deepEqual(actual: unknown, expected: unknown, message?: string): void;
  }
  const assert: StrictAssert;
  export default assert;
}
declare module 'node:fs' {
  export function readFileSync(path: string, encoding: 'utf8'): string;
}
declare module 'node:process' {
  export const env: Record<string, string | undefined>;
}
