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
            content: "// Code generated by box-gantry. DO NOT EDIT.\n\n\
                      export { Client } from './client.js';\n\
                      export * as models from './models/index.js';\n\
                      export * as managers from './managers/index.js';\n\
                      export * as runtime from './runtime.js';\n\
                      export * as buildinfo from './buildinfo.js';\n"
                .to_string(),
        },
    ];
    files.extend(generate_models(analysis, build));
    files.extend(generate_managers(analysis, build));
    files.extend(generate_docs(analysis));
    files.extend(generate_tests(analysis));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

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
    "{\n\
     \x20 \"name\": \"@unofficialbox/box-open-sdk\",\n\
     \x20 \"version\": \"0.1.0\",\n\
     \x20 \"description\": \"Generated TypeScript SDK for the Box API (community, unofficial).\",\n\
     \x20 \"license\": \"MIT\",\n\
     \x20 \"repository\": { \"type\": \"git\", \"url\": \"git+https://github.com/unofficialbox/box-open-ts-sdk.git\" },\n\
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
     }\n"
    .to_string()
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
    "<!-- Generated by box-gantry. DO NOT EDIT. -->\n\
     # Box SDK\n\
     \n\
     A generated TypeScript SDK for the Box API — a dual ESM/CJS package with\n\
     shipped type declarations.\n\
     \n\
     ```ts\n\
     import { Client, runtime } from '@unofficialbox/box-open-sdk';\n\
     const client = new Client(runtime.developerToken('DEVELOPER_TOKEN'));\n\
     ```\n\
     \n\
     See [`docs/`](./docs/README.md) for the manager reference and the\n\
     authentication, pagination, and errors guides.\n"
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
