//! The TypeScript backend (FR-6, TR-TypeScript, D-143).
//!
//! Lowers the verified IR to a self-contained TypeScript package. The type
//! system expresses the IR's shapes almost directly, so the model layer is a
//! near-structural map: structs → `interface`s, the tri-state → `?:`/`| null`
//! (no wrapper type, TR-TS.2), `oneOf` → discriminated unions and open enums →
//! string-literal unions widened with `(string & {})` (TR-TS.1).
//!
//! The verification gate is the TypeScript 7 native compiler: `tsc --noEmit`
//! under `strict` (VR-1.5), the TS analogue of `go build`/`cargo check`.
//!
//! This slice emits the model layer, the `Promise`-based managers/client (with
//! async paginators), the reference docs (per-manager pages + the
//! auth/pagination/errors guides), the generated behavioral tests (tri-state +
//! per-union round-trip, run under `node --test`), and the NF-8 ship scaffold —
//! a publishable **dual ESM/CJS** `package.json` with an `exports` map, the
//! ESM/CJS build configs, and the dual-package post-build step, so the release
//! pipeline (which vendors the real runtime, as Go does) can build a package
//! that `npm publish --dry-run`s clean with shipped `.d.ts`.

mod docs;
mod managers;
mod models;
mod tests;

pub use docs::generate_docs;
pub use managers::generate_managers;
pub use models::generate_models;
pub use tests::generate_tests;

/// One generated file, path relative to the SDK package root.
#[derive(Debug)]
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
}

/// Provenance stamped into the generated SDK for traceability (NF-7): the
/// engine version that produced it and the fingerprint of the input specs.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    pub engine: String,
    pub spec_fingerprint: String,
}

impl BuildInfo {
    pub fn new(spec_fingerprint: impl Into<String>) -> Self {
        Self {
            engine: env!("CARGO_PKG_VERSION").to_string(),
            spec_fingerprint: spec_fingerprint.into(),
        }
    }
}

/// Generate the SDK package tree for a verified program, stamped with the build
/// provenance (NF-7). The output is a self-contained TypeScript package that
/// `tsc --noEmit` type-checks clean under `strict` (VR-1.5).
///
/// Takes the manifest so synthesis keys off capability axes, never the language
/// name (FR-4.2); this slice reads only `manifest.key`.
pub fn generate(
    analysis: &gantry_sema::Analysis<'_>,
    manifest: &gantry_manifest::CapabilityManifest,
    build: &BuildInfo,
) -> Vec<GeneratedFile> {
    let mut files = vec![
        GeneratedFile {
            path: "package.json".to_string(),
            content: package_json(),
        },
        GeneratedFile {
            path: "tsconfig.json".to_string(),
            content: TSCONFIG.to_string(),
        },
        // The NF-8 ship-artifact build: dual ESM/CJS emit + `.d.ts` + the
        // dual-package markers + a package README.
        GeneratedFile {
            path: "tsconfig.build.json".to_string(),
            content: TSCONFIG_BUILD.to_string(),
        },
        GeneratedFile {
            path: "tsconfig.cjs.json".to_string(),
            content: TSCONFIG_CJS.to_string(),
        },
        GeneratedFile {
            path: "scripts/postbuild.mjs".to_string(),
            content: POSTBUILD_MJS.to_string(),
        },
        GeneratedFile {
            path: "README.md".to_string(),
            content: readme(),
        },
        // The license itself, not just `package.json`'s declaration of it —
        // npm expects the file to ship inside the tarball.
        GeneratedFile {
            path: "LICENSE".to_string(),
            content: gantry_manifest::LICENSE.to_string(),
        },
        // The shared community-design banner the README renders at its top (NF-8).
        GeneratedFile {
            path: "assets/banner.svg".to_string(),
            content: gantry_manifest::banner_svg("TypeScript"),
        },
        GeneratedFile {
            path: "src/buildinfo.ts".to_string(),
            content: buildinfo_ts(manifest, build),
        },
        // The hand-written `fetch` runtime, vendored into the shipped SDK
        // (TR-TS.5, D-192). `gantry-contract` renders a compile-only stub of the
        // same surface for generation-time verification (FR-5.3); shipping that
        // stub would type-check and then throw on every call, so the real
        // implementation is embedded here at build time. `jwt.ts` is included
        // because `package.json` already declares the `./jwt` subpath export.
        GeneratedFile {
            path: "src/runtime.ts".to_string(),
            content: vendored(
                "runtimes/typescript/gantryruntime/src/runtime.ts",
                include_str!("../../../runtimes/typescript/gantryruntime/src/runtime.ts"),
            ),
        },
        GeneratedFile {
            path: "src/auth.ts".to_string(),
            content: vendored(
                "runtimes/typescript/gantryruntime/src/auth.ts",
                include_str!("../../../runtimes/typescript/gantryruntime/src/auth.ts"),
            ),
        },
        GeneratedFile {
            path: "src/errors.ts".to_string(),
            content: vendored(
                "runtimes/typescript/gantryruntime/src/errors.ts",
                include_str!("../../../runtimes/typescript/gantryruntime/src/errors.ts"),
            ),
        },
        GeneratedFile {
            path: "src/tokens.ts".to_string(),
            content: vendored(
                "runtimes/typescript/gantryruntime/src/tokens.ts",
                include_str!("../../../runtimes/typescript/gantryruntime/src/tokens.ts"),
            ),
        },
        GeneratedFile {
            path: "src/jwt.ts".to_string(),
            content: vendored(
                "runtimes/typescript/gantryruntime/src/jwt.ts",
                include_str!("../../../runtimes/typescript/gantryruntime/src/jwt.ts"),
            ),
        },
        GeneratedFile {
            path: "src/node-crypto.d.ts".to_string(),
            content: vendored(
                "runtimes/typescript/gantryruntime/src/node-crypto.d.ts",
                include_str!("../../../runtimes/typescript/gantryruntime/src/node-crypto.d.ts"),
            ),
        },
        GeneratedFile {
            path: "src/index.ts".to_string(),
            content: index_ts(emits_chunked_upload(analysis)),
        },
    ];
    files.extend(generate_models(analysis, build));
    files.extend(generate_managers(analysis, build));
    files.extend(generate_docs(analysis));
    files.extend(generate_tests(analysis));
    // The chunked-upload orchestrator (D-183): a fixed hand-written helper over
    // the generated `ChunkedUploadsManager` — create a session, upload the parts
    // with bounded concurrency, commit — for a new file or a new version. It
    // names concrete `models.schemas.*` types and manager methods, so it is
    // emitted **only** when the spec carries the whole surface (VR-6: never emit
    // code that wouldn't type-check); `tsc --strict` is the ultimate backstop.
    if emits_chunked_upload(analysis) {
        files.push(GeneratedFile {
            path: "src/chunked_upload.ts".to_string(),
            content: CHUNKED_UPLOAD.to_string(),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// The package barrel (`src/index.ts`). The chunked-upload orchestrator is
/// re-exported only when it is emitted (VR-6), so the barrel never names a
/// module that isn't there.
fn index_ts(chunked: bool) -> String {
    let mut out = String::from(
        "// Code generated by box-gantry. DO NOT EDIT.\n\n\
         export { Client } from './client.js';\n\
         export * as models from './models/index.js';\n\
         export * as managers from './managers/index.js';\n\
         export * as runtime from './runtime.js';\n\
         export * as auth from './auth.js';\n\
         export * as buildinfo from './buildinfo.js';\n",
    );
    if chunked {
        out.push_str("export { ChunkedUpload } from './chunked_upload.js';\n");
    }
    out
}

/// Whether the program carries the whole chunked-upload surface the
/// `src/chunked_upload.ts` orchestrator references (D-183) — both the concrete
/// `models.schemas.*` types **and** the `chunkedUploads` manager methods it
/// calls, so emitting it against a spec that lacks any of them would not
/// type-check. Emit only when all are present (VR-6), which also keeps the
/// synthetic-spec gates free of the dependency. The exact method signatures
/// can't be checked here, so `tsc --strict` is the compile-time backstop.
fn emits_chunked_upload(analysis: &gantry_sema::Analysis<'_>) -> bool {
    use std::collections::HashSet;
    let program = analysis.program;
    // The concrete `schemas.*` types the orchestrator names, as `module.Name`.
    let modules = models::module_names(program);
    let mut fqns: HashSet<String> = HashSet::new();
    for decl in &program.decls {
        if let Some(module) = modules.get(&decl.module) {
            fqns.insert(format!(
                "{module}.{}",
                models::type_name(decl.name.as_str())
            ));
        }
    }
    const REQUIRED_TYPES: [&str; 7] = [
        "schemas.UploadSession",
        "schemas.UploadPart",
        "schemas.UploadedPart",
        "schemas.Files",
        "schemas.CreateFileUploadSessionRequest",
        "schemas.CreateFileVersionUploadSessionRequest",
        "schemas.CommitFileUploadSessionRequest",
    ];
    if !REQUIRED_TYPES.iter().all(|r| fqns.contains(*r)) {
        return false;
    }
    // The four `chunkedUploads` methods the orchestrator calls, named the way the
    // manager printer names them (`camel(method_base)`).
    let mut methods: HashSet<String> = HashSet::new();
    for indices in analysis.managers.values() {
        for base in managers::method_bases(program, indices) {
            methods.insert(managers::camel(&base));
        }
    }
    const REQUIRED_METHODS: [&str; 4] = [
        "createFileUploadSession",
        "createFileVersionUploadSession",
        "updateFileUploadSession",
        "commitFileUploadSession",
    ];
    REQUIRED_METHODS.iter().all(|m| methods.contains(*m))
}

/// The chunked-upload orchestrator source (D-183), a fixed hand-written helper
/// over the generated `ChunkedUploadsManager`. Uses only platform globals — Web
/// Crypto (`crypto.subtle`) for the Box part/whole-file SHA-1 digests and a
/// bounded worker pool for concurrency — so it adds no dependency.
const CHUNKED_UPLOAD: &str = r#"// Code generated by box-gantry. DO NOT EDIT.

import type { Client } from './client.js';
import type * as models from './models/index.js';

/**
 * ChunkedUpload orchestrates a Box chunked (multipart) upload over a Client: it
 * creates an upload session, uploads the content's parts with bounded
 * concurrency, and commits — for a new file (upload) or a new version of an
 * existing file (uploadVersion). Box requires chunked upload for files at or
 * above its minimum session size; smaller files use the single-shot endpoints.
 */
export class ChunkedUpload {
  /** Bounds parts in flight, capping peak memory. */
  private readonly maxConcurrent = 4;

  constructor(private readonly client: Client) {}

  /** Upload content as a new file named fileName into folderId. */
  async upload(content: Uint8Array, fileName: string, folderId: string): Promise<models.schemas.Files> {
    const session = await this.client.chunkedUploads.createFileUploadSession({
      folder_id: folderId,
      file_size: content.length,
      file_name: fileName,
    });
    return this.finish(session, content);
  }

  /** Upload content as a new version of the existing file fileId. */
  async uploadVersion(content: Uint8Array, fileName: string, fileId: string): Promise<models.schemas.Files> {
    const session = await this.client.chunkedUploads.createFileVersionUploadSession(fileId, {
      file_size: content.length,
      file_name: fileName,
    });
    return this.finish(session, content);
  }

  private async finish(session: models.schemas.UploadSession, content: Uint8Array): Promise<models.schemas.Files> {
    if (session.id === undefined) {
      throw new Error('gantry: upload session returned no id');
    }
    if (session.part_size === undefined || session.part_size <= 0) {
      throw new Error('gantry: upload session returned a non-positive part_size');
    }
    const id = session.id;
    const partSize = session.part_size;
    const total = content.length;

    const offsets: number[] = [];
    for (let off = 0; off < total; off += partSize) {
      offsets.push(off);
    }
    const parts: models.schemas.UploadPart[] = new Array(offsets.length);

    // A bounded worker pool: each of maxConcurrent workers claims the next
    // offset (the `next++` read is atomic between awaits on a single thread).
    // Once any part fails, `failed` stops the others from starting new parts,
    // so a failure early in a large upload doesn't push every remaining part.
    let next = 0;
    let failed = false;
    const worker = async (): Promise<void> => {
      for (;;) {
        if (failed) {
          return;
        }
        const i = next++;
        if (i >= offsets.length) {
          return;
        }
        const start = offsets[i];
        const end = Math.min(start + partSize, total);
        try {
          parts[i] = await this.uploadPart(id, content, start, end, total);
        } catch (err) {
          failed = true;
          throw err;
        }
      }
    };
    const workerCount = Math.min(this.maxConcurrent, offsets.length);
    await Promise.all(Array.from({ length: workerCount }, () => worker()));

    const digest = 'sha=' + (await sha1Base64(content));
    return this.client.chunkedUploads.commitFileUploadSession(id, digest, { parts });
  }

  private async uploadPart(id: string, content: Uint8Array, start: number, end: number, total: number): Promise<models.schemas.UploadPart> {
    const slice = content.subarray(start, end);
    const digest = 'sha=' + (await sha1Base64(slice));
    const contentRange = `bytes ${start}-${end - 1}/${total}`;
    // The `as BlobPart` cast bridges the `Uint8Array<ArrayBufferLike>` vs
    // `BufferSource` (ArrayBuffer-only) generic gap in the DOM lib, exactly as
    // the runtime casts its `fetch` body; a plain `Uint8Array` is a valid part.
    const uploaded = await this.client.chunkedUploads.updateFileUploadSession(id, digest, contentRange, new Blob([slice as BlobPart]));
    if (uploaded.part === undefined) {
      throw new Error('gantry: upload part returned no part');
    }
    return uploaded.part;
  }
}

/** Base64 of the SHA-1 of data — the Box `Digest: sha=…` header value. Uses Web
 *  Crypto (browser and Node 18+), so it adds no dependency. */
async function sha1Base64(data: Uint8Array): Promise<string> {
  // `as BufferSource` bridges the `Uint8Array<ArrayBufferLike>` vs ArrayBuffer-only
  // generic gap in the DOM lib (as the runtime does for its `fetch` body).
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-1', data as BufferSource));
  let binary = '';
  for (const byte of digest) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}
"#;

/// Prefix a vendored runtime file with the do-not-edit header (FR-6.3).
///
/// Vendored files are copies: an edit made in the SDK repository is lost at the
/// next regeneration, so the header names the upstream to change instead.
fn vendored(origin: &str, source: &str) -> String {
    format!("// Code generated by box-gantry (vendored from {origin}). DO NOT EDIT.\n\n{source}")
}

/// The generated package manifest (NF-8). A publishable, **dual ESM/CJS**
/// package: `exports` route `import`/`require`/`types` into the built `dist/`
/// tree (ESM + CJS + `.d.ts`), and only `dist/` (with the docs) ships — the
/// `src/` stub runtime is a build input, not shipped. The JWT flow is a
/// separate `./jwt` subpath (Node-only, mirroring the runtime's own export).
/// The scoped name `@unofficialbox/box-open-sdk` marks these as community SDKs,
/// distinct from Box's official ones; `version` is set by the release pipeline
/// from the FR-9 spec-diff (as Go's tag is).
///
/// `typescript` is a declared devDependency because `scripts.build` invokes
/// `tsc`: a clone must be buildable with `npm install && npm run build` alone.
/// The compile gates put `tsc` on `PATH` themselves, so nothing else here would
/// notice its absence — hence the explicit assertion in the ship test.
fn package_json() -> String {
    // `@VERSION@` sentinel rather than `format!`: the manifest is dense with
    // literal `{ }` (the `exports` map, `engines`, `scripts`), all of which
    // would need doubling under `format!`. One `replace` keeps the JSON readable
    // and leaves no hardcoded version behind.
    //
    // `repository.url` is box-gantry, not box-open-ts-sdk — deliberately. npm
    // trusted publishing signs a provenance statement naming the repo that ran
    // the release workflow (box-gantry) and then rejects the publish unless
    // `repository.url` matches it (E422). It is also the honest pointer: this
    // package is generated, so a fix belongs in the engine, never in the SDK repo
    // (an edit there is lost at the next regeneration; see `vendored`). `homepage`
    // stays on box-open-ts-sdk, where a TypeScript consumer reads and installs.
    const TEMPLATE: &str = "{\n\
     \x20 \"name\": \"@unofficialbox/box-open-sdk\",\n\
     \x20 \"version\": \"@VERSION@\",\n\
     \x20 \"description\": \"Box API client for TypeScript (open source, community, punk rock) — typed models, async managers, and a fetch runtime with retry, backoff, and token refresh.\",\n\
     \x20 \"keywords\": [\"box\", \"box-api\", \"sdk\", \"api-client\", \"typescript\", \"unofficial\"],\n\
     \x20 \"license\": \"MIT\",\n\
     \x20 \"repository\": { \"type\": \"git\", \"url\": \"git+https://github.com/unofficialbox/box-gantry.git\" },\n\
     \x20 \"homepage\": \"https://github.com/unofficialbox/box-open-ts-sdk\",\n\
     \x20 \"type\": \"module\",\n\
     \x20 \"sideEffects\": false,\n\
     \x20 \"engines\": { \"node\": \">=20\" },\n\
     \x20 \"main\": \"./dist/cjs/index.js\",\n\
     \x20 \"module\": \"./dist/esm/index.js\",\n\
     \x20 \"types\": \"./dist/types/index.d.ts\",\n\
     \x20 \"exports\": {\n\
     \x20   \".\": {\n\
     \x20     \"types\": \"./dist/types/index.d.ts\",\n\
     \x20     \"import\": \"./dist/esm/index.js\",\n\
     \x20     \"require\": \"./dist/cjs/index.js\"\n\
     \x20   },\n\
     \x20   \"./jwt\": {\n\
     \x20     \"types\": \"./dist/types/jwt.d.ts\",\n\
     \x20     \"import\": \"./dist/esm/jwt.js\",\n\
     \x20     \"require\": \"./dist/cjs/jwt.js\"\n\
     \x20   }\n\
     \x20 },\n\
     \x20 \"files\": [\"dist\", \"docs\", \"README.md\"],\n\
     \x20 \"publishConfig\": { \"access\": \"public\" },\n\
     \x20 \"devDependencies\": { \"typescript\": \"^7.0.2\" },\n\
     \x20 \"scripts\": {\n\
     \x20   \"build\": \"tsc -p tsconfig.build.json && tsc -p tsconfig.cjs.json && node scripts/postbuild.mjs\"\n\
     \x20 }\n\
     }\n";
    TEMPLATE.replace("@VERSION@", gantry_manifest::SDK_VERSION)
}

/// The ESM + declarations build config (NF-8). Emits ES modules into
/// `dist/esm` and `.d.ts` into `dist/types`. `rootDir` is explicit (TS 7
/// requires it alongside `declarationDir`).
const TSCONFIG_BUILD: &str = "{\n\
     \x20 \"compilerOptions\": {\n\
     \x20   \"strict\": true,\n\
     \x20   \"target\": \"ES2022\",\n\
     \x20   \"module\": \"NodeNext\",\n\
     \x20   \"moduleResolution\": \"NodeNext\",\n\
     \x20   \"lib\": [\"ES2022\", \"DOM\"],\n\
     \x20   \"rootDir\": \"./src\",\n\
     \x20   \"outDir\": \"./dist/esm\",\n\
     \x20   \"declaration\": true,\n\
     \x20   \"declarationDir\": \"./dist/types\",\n\
     \x20   \"skipLibCheck\": true,\n\
     \x20   \"forceConsistentCasingInFileNames\": true\n\
     \x20 },\n\
     \x20 \"include\": [\"src/**/*.ts\"],\n\
     \x20 \"exclude\": [\"src/**/*.test.ts\"]\n\
     }\n";

/// The CommonJS build config (NF-8). Emits CJS into `dist/cjs`; TS 7 accepts
/// `module: CommonJS` with the default resolution (it removed `node10`).
const TSCONFIG_CJS: &str = "{\n\
     \x20 \"compilerOptions\": {\n\
     \x20   \"strict\": true,\n\
     \x20   \"target\": \"ES2022\",\n\
     \x20   \"module\": \"CommonJS\",\n\
     \x20   \"lib\": [\"ES2022\", \"DOM\"],\n\
     \x20   \"rootDir\": \"./src\",\n\
     \x20   \"outDir\": \"./dist/cjs\",\n\
     \x20   \"declaration\": false,\n\
     \x20   \"skipLibCheck\": true,\n\
     \x20   \"forceConsistentCasingInFileNames\": true\n\
     \x20 },\n\
     \x20 \"include\": [\"src/**/*.ts\"],\n\
     \x20 \"exclude\": [\"src/**/*.test.ts\"]\n\
     }\n";

/// Post-build step (NF-8): stamp the dual-package `type` markers so Node reads
/// each `dist/` tree in the right module system regardless of the root
/// package's `type`.
const POSTBUILD_MJS: &str = "// Code generated by box-gantry. DO NOT EDIT.\n\
     \n\
     // Write the dual-package `type` markers so Node reads `dist/esm` as ES\n\
     // modules and `dist/cjs` as CommonJS regardless of the root package type.\n\
     import { writeFileSync } from 'node:fs';\n\
     \n\
     writeFileSync('dist/esm/package.json', '{\\n  \"type\": \"module\"\\n}\\n');\n\
     writeFileSync('dist/cjs/package.json', '{\\n  \"type\": \"commonjs\"\\n}\\n');\n";

/// A short package README (NF-8: `files` ships it). Points at the generated
/// reference docs.
fn readme() -> String {
    r##"<!-- Generated by box-gantry. DO NOT EDIT — regenerate from the specs instead. -->
![Box Open SDK for TypeScript](assets/banner.svg)

# @unofficialbox/box-open-sdk

[![release](https://img.shields.io/github/v/release/unofficialbox/box-open-ts-sdk?sort=semver)](https://github.com/unofficialbox/box-open-ts-sdk/releases/latest)
[![npm](https://img.shields.io/npm/v/@unofficialbox/box-open-sdk.svg)](https://www.npmjs.com/package/@unofficialbox/box-open-sdk)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

An **open source, community-built** Box API client for TypeScript — fully typed models
for the whole Box surface, one manager per API area behind a single `Client`,
and a `fetch`-based runtime with retry, backoff, and token refresh. Ships as a
**dual ESM/CJS** package with bundled `.d.ts` declarations; no runtime
dependencies.

> **Not affiliated with, authorized, or endorsed by Box, Inc.** "Box" is a
> trademark of Box, Inc. This is an independent, generated client.

## Install

```sh
npm install @unofficialbox/box-open-sdk
```

## Quickstart

Authenticate, look up the current user, create a folder, upload a file, extract
its fields with Box AI, tag it with metadata, and query for it — end to end:

```ts
import { Client, auth } from '@unofficialbox/box-open-sdk';

// Client Credentials Grant (server-to-server); developer token, OAuth, and JWT
// also live in the `auth` namespace.
const client = new Client(auth.clientCredentials({
  clientId: 'CLIENT_ID',
  clientSecret: 'CLIENT_SECRET',
  enterpriseId: 'ENTERPRISE_ID',
}));

// The current user.
const me = await client.users.getMe();
console.log(`authenticated as ${me.id}`);

// Create a folder at the account root ("0").
const folder = await client.folders.create({
  name: 'Invoices',
  parent: { id: '0' },
});

// Upload a file into it.
const uploaded = await client.uploads.uploadFile({
  attributes: { name: 'invoice.pdf', parent: { id: folder.id } },
  file: new Blob(['<file bytes>']),
});
const fileId = uploaded.entries![0].id;

// Extract fields from the file with Box AI.
const answer = await client.ai.extract({
  prompt: 'Extract the invoice number and total amount.',
  items: [{ id: fileId, type: 'file' }],
});
console.log(answer);

// Attach that metadata to the file (an enterprise template).
await client.fileMetadata.createFileMetadata(fileId, 'enterprise', 'invoiceData', {
  invoiceNumber: 'INV-0042',
  total: 1250,
});

// Query for files carrying that metadata.
const results = await client.search.queryByMetadata({
  from: 'enterprise_0.invoiceData',
  ancestor_folder_id: folder.id,
});
console.log(results);
```

## Authentication

The runtime implements Box's four auth flows — **developer token**, **client
credentials (CCG)**, **OAuth 2.0** (with a pluggable refresh-token store), and
**JWT** (server auth, exposed as the Node-only `./jwt` subpath). See
[`docs/auth.md`](./docs/auth.md).

## Documentation

The [`docs/`](./docs/README.md) tree carries the per-manager reference and the
authentication, pagination, and errors guides.

## License

MIT. Generated by [box-gantry](https://github.com/unofficialbox/box-gantry).
"##
    .to_string()
}

/// The compiler configuration: `strict` + `noEmit` (VR-1.5), NodeNext ESM
/// resolution (so imports carry the `.js` extension), `skipLibCheck` to keep
/// the gate scoped to the generated code.
const TSCONFIG: &str = "{\n\
     \x20 \"compilerOptions\": {\n\
     \x20   \"strict\": true,\n\
     \x20   \"noEmit\": true,\n\
     \x20   \"target\": \"ES2022\",\n\
     \x20   \"module\": \"NodeNext\",\n\
     \x20   \"moduleResolution\": \"NodeNext\",\n\
     \x20   \"declaration\": true,\n\
     \x20   \"skipLibCheck\": true,\n\
     \x20   \"forceConsistentCasingInFileNames\": true\n\
     \x20 },\n\
     \x20 \"include\": [\"src/**/*.ts\"]\n\
     }\n";

/// The `buildinfo` module: exported constants naming the engine, the spec
/// fingerprint, and the target key the SDK was generated from (NF-7).
fn buildinfo_ts(manifest: &gantry_manifest::CapabilityManifest, build: &BuildInfo) -> String {
    format!(
        "// Code generated by box-gantry {engine} (spec {fingerprint}). DO NOT EDIT.\n\
         \n\
         /** The box-gantry engine version that generated this SDK. */\n\
         export const ENGINE = {engine:?};\n\
         /** Fingerprint of the input spec set. */\n\
         export const SPEC_FINGERPRINT = {fingerprint:?};\n\
         /** The target language key (FR-4). */\n\
         export const TARGET = {target:?};\n",
        engine = build.engine,
        fingerprint = build.spec_fingerprint,
        target = manifest.key,
    )
}
