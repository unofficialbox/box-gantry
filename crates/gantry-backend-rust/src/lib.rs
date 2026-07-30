//! The Rust backend: lowering + printer (FR-6, TR-Rust).
//!
//! Generates the SDK crate tree's **model layer**: the `models` module mirrors
//! the IR module tree one Rust module per IR module (names collide across API
//! versions, so they may not share a namespace — D-147), lowering structs,
//! string enums, unions, and aliases to `serde` Rust types.
//!
//! - **Optionality** is `Option<T>`; the absent-vs-null tri-state
//!   (`Optional<Nullable<T>>`, D-110) maps to `Option<Option<T>>` with a
//!   `double_option` deserializer so absence and explicit `null` stay distinct.
//! - **Date/time** are typed via `chrono`: `Date` → `NaiveDate` (Box's
//!   full-date), `DateTime` → `DateTime<Utc>` (RFC 3339) — both serde-serialize
//!   to exactly Box's wire format.
//! - **Unions** (TR-Rust.1, D-148): a discriminated union with decl-backed
//!   variants lowers to a typed `enum` with hand-written `Serialize`/
//!   `Deserialize` that dispatch on the tag; open unions retain an unrecognized
//!   tag in an `Unknown(serde_json::Value)` variant (round-trip safe), closed
//!   unions reject it. Structural unions stay a `serde_json::Value` newtype.
//!
//! Output is deterministic (FR-6.2, sorted by path) and rustfmt-clean by
//! construction (TR-Rust.4, G-17), verified by the real toolchain
//! (`cargo fmt --check` + `cargo check` + `clippy -D warnings` — VR-1.2) plus a
//! generated-union round-trip test (VR-4).
//!
//! The backend emits the contract's compile-time runtime *stub* (`runtime.rs`);
//! the real `reqwest`/`tokio` runtime is hand-written in `runtimes/rust`
//! (TR-Rust.5) and satisfies the same contract, which the backend's
//! conformance test proves by compiling the generated SDK against it (FR-5.2).
//!
//! Emits the full SDK: models, async managers/client, the runtime stub,
//! reference docs, generated round-trip / behavioral tests, and the NF-8 ship
//! scaffold (publish-ready `Cargo.toml` metadata + README) — so the release
//! pipeline, after vendoring the real runtime into the crate, builds a
//! `cargo publish --dry-run`-clean, self-contained crate (as Go/TS ship).

mod docs;
mod managers;
mod models;
mod tests;

pub use docs::generate_docs;
pub use managers::generate_managers;
pub use models::generate_models;
pub use tests::generate_tests;

/// One generated file, path relative to the SDK crate root.
#[derive(Debug)]
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
}

/// Provenance stamped into the generated SDK for traceability (NF-7): the
/// engine version that produced it and the fingerprint of the input specs.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    /// The box-gantry engine version (workspace version).
    pub engine: String,
    /// The input spec-set fingerprint (`SpecSet::fingerprint`).
    pub spec_fingerprint: String,
}

impl BuildInfo {
    /// Build info for the current engine over a given spec fingerprint.
    pub fn new(spec_fingerprint: impl Into<String>) -> Self {
        Self {
            engine: env!("CARGO_PKG_VERSION").to_string(),
            spec_fingerprint: spec_fingerprint.into(),
        }
    }
}

/// Generate the SDK crate tree for a verified program, stamped with the build
/// provenance (NF-7). The output is a self-contained, publishable Rust crate
/// (the NF-8 ship artifact once the runtime lands).
///
/// Takes the manifest so synthesis keys off capability axes, never the
/// language name (FR-4.2); this slice reads only `manifest.key`.
pub fn generate(
    analysis: &gantry_sema::Analysis<'_>,
    manifest: &gantry_manifest::CapabilityManifest,
    build: &BuildInfo,
) -> Vec<GeneratedFile> {
    let mut files = vec![
        GeneratedFile {
            path: "Cargo.toml".to_string(),
            content: cargo_toml(),
        },
        GeneratedFile {
            path: "README.md".to_string(),
            content: readme(),
        },
        // The license itself, not just `Cargo.toml`'s declaration of it —
        // crates.io expects the file to ship inside the package.
        GeneratedFile {
            path: "LICENSE".to_string(),
            content: gantry_manifest::LICENSE.to_string(),
        },
        // The shared community-design banner the README renders at its top (NF-8).
        GeneratedFile {
            path: "assets/banner.svg".to_string(),
            content: gantry_manifest::banner_svg("Rust"),
        },
        GeneratedFile {
            path: "src/lib.rs".to_string(),
            content: lib_rs(manifest, build, emits_chunked_upload(analysis)),
        },
        GeneratedFile {
            path: "src/serde_helpers.rs".to_string(),
            content: SERDE_HELPERS.to_string(),
        },
        // The hand-written `reqwest`/`tokio` runtime, vendored into the shipped
        // crate (TR-Rust.5, D-192). `gantry-contract` renders a compile-only
        // stub of the same surface for generation-time verification (FR-5.3);
        // shipping that stub would compile and then panic on every call, so the
        // real implementation is embedded here at build time.
        GeneratedFile {
            path: "src/runtime/mod.rs".to_string(),
            content: vendored(
                "runtimes/rust/gantryruntime/src/lib.rs",
                strip_tests(include_str!(
                    "../../../runtimes/rust/gantryruntime/src/lib.rs"
                )),
            ),
        },
        GeneratedFile {
            path: "src/runtime/auth.rs".to_string(),
            content: vendored(
                "runtimes/rust/gantryruntime/src/auth.rs",
                nest_submodule(include_str!(
                    "../../../runtimes/rust/gantryruntime/src/auth.rs"
                )),
            ),
        },
        GeneratedFile {
            path: "src/runtime/jwt.rs".to_string(),
            content: vendored(
                "runtimes/rust/gantryruntime/src/jwt.rs",
                nest_submodule(include_str!(
                    "../../../runtimes/rust/gantryruntime/src/jwt.rs"
                )),
            ),
        },
    ];
    files.extend(generate_models(analysis, build));
    files.extend(generate_managers(analysis, build));
    files.extend(generate_docs(
        analysis,
        &gantry_synth::detect_pagination(analysis),
    ));
    files.extend(generate_tests(analysis));
    // The chunked-upload orchestrator (D-183): a fixed hand-written helper over
    // the generated `ChunkedUploadsManager` — create a session, upload the parts,
    // commit — for a new file or a new version. It names concrete
    // `schemas::*` types and manager methods, so it is emitted **only** when the
    // spec carries the whole surface (VR-6: never emit code that wouldn't
    // compile); `cargo check` is the ultimate backstop.
    if emits_chunked_upload(analysis) {
        files.push(GeneratedFile {
            path: "src/chunked_upload.rs".to_string(),
            content: CHUNKED_UPLOAD.to_string(),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// Whether the program carries the whole chunked-upload surface the
/// `src/chunked_upload.rs` orchestrator references (D-183) — both the concrete
/// `schemas::*` types **and** the `ChunkedUploadsManager` methods it calls, so
/// emitting it against a spec that lacks any of them would not compile. Emit only
/// when all are present (VR-6), which also keeps the synthetic-spec gates
/// (round-trip, serialization tests) free of the dependency. The exact method
/// signatures can't be checked here, so `cargo check` is the compile-time
/// backstop — a mismatch fails the build, it never ships broken code.
fn emits_chunked_upload(analysis: &gantry_sema::Analysis<'_>) -> bool {
    use std::collections::HashSet;
    let program = analysis.program;
    // The concrete `schemas::*` types the orchestrator names, as `module::Name`.
    let mut fqns: HashSet<String> = HashSet::new();
    for decl in &program.decls {
        let module = models::module_name(&decl.module);
        fqns.insert(format!(
            "{module}::{}",
            models::type_name(decl.name.as_str())
        ));
    }
    const REQUIRED_TYPES: [&str; 7] = [
        "schemas::UploadSession",
        "schemas::UploadPart",
        "schemas::UploadedPart",
        "schemas::Files",
        "schemas::FileUploadSessionCreateRequest",
        "schemas::FileVersionUploadSessionCreateRequest",
        "schemas::FileUploadSessionCommitRequest",
    ];
    if !REQUIRED_TYPES.iter().all(|r| fqns.contains(*r)) {
        return false;
    }
    // The four `ChunkedUploadsManager` methods the orchestrator calls, named the
    // way the manager printer names them (`managers::method_name`).
    let methods: HashSet<String> = program
        .operations
        .iter()
        .map(managers::method_name)
        .collect();
    const REQUIRED_METHODS: [&str; 4] = [
        "create_file_upload_session",
        "create_file_version_upload_session",
        "update_file_upload_session",
        "commit_file_upload_session",
    ];
    REQUIRED_METHODS.iter().all(|m| methods.contains(*m))
}

/// Drop a vendored runtime file's inline `#[cfg(test)]` module.
///
/// The runtime's unit tests exercise it as a standalone crate and pull in
/// dev-dependencies (tokio's multi-thread runtime, test macros) that the shipped
/// SDK does not declare. Truncating at the marker keeps the crate publishable
/// without teaching consumers about the runtime's own test stack.
fn strip_tests(source: &str) -> String {
    match source.find("#[cfg(test)]") {
        Some(pos) => format!("{}\n", source[..pos].trim_end()),
        None => source.to_string(),
    }
}

/// Prefix a vendored runtime file with the do-not-edit header (FR-6.3).
///
/// Vendored files are copies: an edit made in the SDK repository is lost at the
/// next regeneration, so the header names the upstream to change instead.
fn vendored(origin: &str, source: String) -> String {
    format!("// Code generated by box-gantry (vendored from {origin}). DO NOT EDIT.\n\n{source}")
}

/// Vendor a runtime submodule: strip its tests and re-root its paths.
///
/// `auth.rs`/`jwt.rs` are crate-level modules of the standalone runtime, so they
/// reach its root as `crate::` — both in `use` items and in intra-doc links.
/// Nested under the SDK's `runtime` module that root becomes `super::`; without
/// the rewrite every such path would resolve to the *SDK* crate root and fail to
/// compile. `mod.rs` needs no rewrite because it carries no `crate::` paths at
/// all — it *is* the root being referred to.
fn nest_submodule(source: &str) -> String {
    strip_tests(source).replace("crate::", "super::")
}

/// The generated SDK crate manifest (NF-8 publish-ready). Pinned, minimal deps:
/// serde + serde_json, plus `chrono` for the typed `Date`/`DateTime` model
/// fields (lean feature set — `serde` + `alloc`, no clock/OS-timezone
/// machinery). Carries the package metadata `cargo publish` requires; the
/// release pipeline vendors the real runtime (adding its deps) and sets the
/// `version` from the FR-9 spec-diff, as Go's module tag is set. The crate name
/// `box-open-sdk` (org `unofficialbox`) marks these as community SDKs, distinct
/// from Box's official ones.
fn cargo_toml() -> String {
    format!(
        "[package]\n\
     name = \"box-open-sdk\"\n\
     version = \"{version}\"\n\
     edition = \"2021\"\n\
     description = \"Box API client for Rust (open source, community, punk rock) — typed models, async managers, and a reqwest runtime with retry, backoff, and token refresh.\"\n\
     license = \"MIT\"\n\
     repository = \"https://github.com/unofficialbox/box-open-rust-sdk\"\n\
     homepage = \"https://github.com/unofficialbox/box-open-rust-sdk\"\n\
     documentation = \"https://docs.rs/box-open-sdk\"\n\
     readme = \"README.md\"\n\
     keywords = [\"box\", \"box-api\", \"sdk\", \"api-client\", \"unofficial\"]\n\
     categories = [\"api-bindings\", \"web-programming\"]\n\
     \n\
     [dependencies]\n\
     chrono = {{ version = \"0.4\", default-features = false, features = [\"serde\", \"alloc\"] }}\n\
     serde = {{ version = \"1\", features = [\"derive\"] }}\n\
     serde_json = \"1\"\n",
        version = gantry_manifest::SDK_VERSION,
    ) + &runtime_dependencies()
}

/// The vendored runtime's own `[dependencies]`, appended to the SDK manifest.
///
/// The runtime is a standalone crate whose heavy async stack (`reqwest`,
/// `tokio`, `rsa`, …) is deliberately kept out of the engine workspace. Once its
/// source is vendored into the SDK those become the SDK's dependencies, so they
/// are lifted from its manifest rather than duplicated here — one source of
/// truth, and a runtime dependency bump can't silently desync.
///
/// Only `[dependencies]` is taken: `[dev-dependencies]` serve the runtime's own
/// tests (stripped by [`strip_tests`]) and `[lints]` is engine policy. Keys the
/// generated manifest already declares are skipped, so `serde_json` is not
/// emitted twice.
fn runtime_dependencies() -> String {
    const MANIFEST: &str = include_str!("../../../runtimes/rust/gantryruntime/Cargo.toml");
    let Some(body) = MANIFEST.split_once("[dependencies]").map(|(_, rest)| rest) else {
        return String::new();
    };
    // Stop at the next section header; `[dependencies]` is not the last one.
    let body = body.split_once("\n[").map_or(body, |(deps, _)| deps);

    let mut out = String::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = line.split([' ', '=']).next().unwrap_or_default();
        // `chrono`/`serde`/`serde_json` are already declared above.
        if key.is_empty() || CARGO_TOML_KEYS.contains(&key) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Dependency keys the generated manifest declares itself.
const CARGO_TOML_KEYS: &[&str] = &["chrono", "serde", "serde_json"];

/// A short package README (NF-8: `readme` in the manifest ships it). Points at
/// the generated reference docs.
fn readme() -> String {
    // A sentinel + `replace` rather than `format!` so the Quickstart's struct
    // literals need no `{{`/`}}` escaping. The example is verified to compile
    // against the generated crate (a consumer, since the crate's `tokio` carries
    // only the features the runtime needs, not `#[tokio::main]`).
    const TEMPLATE: &str = r##"<!-- Generated by box-gantry. DO NOT EDIT — regenerate from the specs instead. -->
![Box Open SDK for Rust](assets/banner.svg)

# box-open-sdk (Rust)

[![crates.io](https://img.shields.io/crates/v/box-open-sdk.svg)](https://crates.io/crates/box-open-sdk)
[![docs.rs](https://img.shields.io/docsrs/box-open-sdk)](https://docs.rs/box-open-sdk)

An **open source, community-built** Box API client for Rust — typed models for the whole
Box surface, one async manager per API area behind a single `Client`, and a
`reqwest`/`tokio` runtime with retry, exponential backoff, `Retry-After`
handling, and automatic token refresh.

> **Not affiliated with, authorized, or endorsed by Box, Inc.** "Box" is a
> trademark of Box, Inc. This is an independent, generated client.

## Install

```toml
[dependencies]
box-open-sdk = "@MINOR@"
tokio = { version = "1", features = ["full"] }
```

## Quickstart

Authenticate, look up the current user, create a folder, upload a file, extract
its fields with Box AI, tag it with metadata, and query for it — end to end.
Request bodies derive `Default`, so only the fields you set are named:

```rust,ignore
use box_open_sdk::auth::{Auth, CcgConfig};
use box_open_sdk::client::Client;
use box_open_sdk::models::schemas;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Client Credentials Grant (server-to-server); developer token, OAuth, and
    // JWT also live in `box_open_sdk::auth`.
    let client = Client::new(Auth::client_credentials(CcgConfig {
        client_id: "CLIENT_ID".into(),
        client_secret: "CLIENT_SECRET".into(),
        enterprise_id: "ENTERPRISE_ID".into(),
        ..Default::default()
    }));

    // The current user.
    let me = client.users.get_me(None).await?;
    println!("authenticated as {}", me.id);

    // Create a folder at the account root ("0").
    let folder = client
        .folders
        .create(
            schemas::FolderCreateRequest {
                name: "Invoices".into(),
                parent: schemas::AttributesParent { id: "0".into() },
                ..Default::default()
            },
            None,
        )
        .await?;

    // Upload a file into it.
    let uploaded = client
        .uploads
        .upload_file(
            schemas::FileContentCreateRequest {
                attributes: schemas::PostFileContentAttributes {
                    name: "invoice.pdf".into(),
                    parent: schemas::AttributesParent { id: folder.id.clone() },
                    ..Default::default()
                },
                file: b"<file bytes>".to_vec(),
            },
            None,
        )
        .await?;
    let file_id = uploaded.entries.unwrap_or_default().remove(0).id;

    // Extract fields from the file with Box AI.
    let answer = client
        .ai
        .extract(schemas::AiExtract {
            prompt: "Extract the invoice number and total amount.".into(),
            items: vec![schemas::AiItemBase {
                id: file_id.clone(),
                r#type: schemas::AiCitationType(schemas::AiCitationType::FILE.into()),
                ..Default::default()
            }],
            ..Default::default()
        })
        .await?;
    println!("{answer:?}");

    // Attach that metadata to the file (an enterprise template).
    client
        .file_metadata
        .create_file_metadata(
            file_id.clone(),
            schemas::GetFileIdMetadataIdIdScope(schemas::GetFileIdMetadataIdIdScope::ENTERPRISE.into()),
            "invoiceData".into(),
            std::collections::HashMap::from([
                ("invoiceNumber".to_string(), serde_json::json!("INV-0042")),
                ("total".to_string(), serde_json::json!(1250)),
            ]),
        )
        .await?;

    // Query for files carrying that metadata.
    let results = client
        .search
        .query_by_metadata(schemas::MetadataQuery {
            from: "enterprise_0.invoiceData".into(),
            ancestor_folder_id: folder.id.clone(),
            ..Default::default()
        })
        .await?;
    println!("{results:?}");

    Ok(())
}
```

## Authentication

Box's four auth flows all live in `box_open_sdk::auth` — **developer token**,
**client credentials (CCG)**, **OAuth 2.0** (with a pluggable refresh-token
store), and **JWT** (server auth). See [`docs/auth.md`](./docs/auth.md).

## Pagination

List endpoints return an auto-paging stream — advancing the cursor and fetching
the next page is handled for you. See [`docs/pagination.md`](./docs/pagination.md).

## Documentation

API reference on [docs.rs](https://docs.rs/box-open-sdk); the [`docs/`](./docs)
tree carries the per-manager reference — a call snippet for every method — and
the authentication, pagination, and errors guides.

## License

MIT. Generated by [box-gantry](https://github.com/unofficialbox/box-gantry).
"##;
    TEMPLATE.replace("@MINOR@", &minor_version())
}

/// The `MAJOR.MINOR` prefix of [`gantry_manifest::SDK_VERSION`] — the version
/// requirement a README shows (`box-open-sdk = "0.1"`), which lets patch
/// releases satisfy it without a doc churn.
fn minor_version() -> String {
    let v = gantry_manifest::SDK_VERSION;
    match (v.find('.'), v.rfind('.')) {
        (Some(first), Some(last)) if first != last => v[..last].to_string(),
        _ => v.to_string(),
    }
}

/// The crate root: module tree + the provenance `buildinfo` constants (NF-7),
/// which let the shipped SDK report its own version and inputs.
fn lib_rs(
    manifest: &gantry_manifest::CapabilityManifest,
    build: &BuildInfo,
    chunked: bool,
) -> String {
    // Declared only when the orchestrator is emitted (VR-6), so `lib.rs` never
    // names a module that isn't there. It leads the module list because rustfmt's
    // `reorder_modules` sorts the declaration group and `chunked_upload` < `client`
    // — emitting it in sorted position keeps the crate `cargo fmt --check`-clean.
    let chunked_mod = if chunked {
        "/// The chunked-upload orchestrator ([`chunked_upload::ChunkedUpload`]).\n\
         pub mod chunked_upload;\n"
    } else {
        ""
    };
    format!(
        "// Code generated by box-gantry {engine} (spec {fingerprint}). DO NOT EDIT.\n\
         #![allow(clippy::large_enum_variant)]\n\
         \n\
         {chunked_mod}\
         pub mod client;\n\
         pub(crate) mod internal;\n\
         pub mod managers;\n\
         pub mod models;\n\
         pub mod runtime;\n\
         /// The four Box auth flows. Build an [`auth::Auth`] and pass it to\n\
         /// [`client::Client::new`]. Split from the transport runtime so the\n\
         /// auth surface has a descriptive path (D-193).\n\
         pub mod auth {{\n\
         \x20   pub use crate::runtime::{{Auth, CcgConfig, JwtConfig, OAuthConfig, RefreshTokenStore}};\n\
         }}\n\
         mod serde_helpers;\n\
         \n\
         // Generated round-trip / behavioral tests (FR-7.8, VR-4).\n\
         #[cfg(test)]\n\
         mod roundtrip_tests;\n\
         #[cfg(test)]\n\
         mod serialization_tests;\n\
         \n\
         /// Build provenance for this generated SDK (NF-7).\n\
         pub mod buildinfo {{\n\
         \x20   /// The box-gantry engine version that generated this crate.\n\
         \x20   pub const ENGINE: &str = {engine:?};\n\
         \x20   /// Fingerprint of the input spec set.\n\
         \x20   pub const SPEC_FINGERPRINT: &str = {fingerprint:?};\n\
         \x20   /// The target language key (FR-4).\n\
         \x20   pub const TARGET: &str = {target:?};\n\
         }}\n",
        engine = build.engine,
        fingerprint = build.spec_fingerprint,
        target = manifest.key,
    )
}

/// Tri-state deserialize helper (D-110): distinguishes an absent field from an
/// explicit `null`. Paired with `#[serde(default, skip_serializing_if =
/// "Option::is_none")]` on an `Option<Option<T>>` field — absent → `None`,
/// `null` → `Some(None)`, value → `Some(Some(v))`.
const SERDE_HELPERS: &str = "\
// Code generated by box-gantry. DO NOT EDIT.
use serde::{Deserialize, Deserializer};

/// Deserialize a present field (value or `null`) into the inner `Option`,
/// wrapping in `Some` so a present `null` reads as `Some(None)` and an absent
/// field (via `#[serde(default)]`) reads as `None`.
pub(crate) fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}
";

/// The chunked-upload orchestrator source (D-183), a fixed hand-written helper
/// over the generated `ChunkedUploadsManager`. Dependency-light: SHA-1 is
/// hand-rolled (the crate's crypto stack carries no SHA-1) and the existing
/// `base64` dependency encodes the Box `Digest` header. Parts upload in batches
/// of `MAX_CONCURRENT`, driven concurrently on the current task by the embedded
/// `join_ordered` helper — no `futures`/`tokio` `rt` dependency. A failed batch
/// aborts the upload before the next batch starts (its own parts still finish).
const CHUNKED_UPLOAD: &str = r#"// Code generated by box-gantry. DO NOT EDIT.
use crate::client::Client;
use crate::models::schemas;
use crate::runtime::Error;
use base64::Engine as _;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Parts in flight at once (`join_ordered` bounds each batch), capping peak
/// buffer memory while keeping several requests moving together.
const MAX_CONCURRENT: usize = 4;

/// An orchestrator for Box chunked (multipart) uploads over a [`Client`]: it
/// creates an upload session, uploads the content's parts with bounded
/// concurrency, and commits — for a new file ([`ChunkedUpload::upload`]) or a
/// new version of an existing file ([`ChunkedUpload::upload_version`]). Box
/// requires chunked upload for files at or above its minimum session size;
/// smaller files use the single-shot upload endpoints.
pub struct ChunkedUpload<'a> {
    client: &'a Client,
}

impl Client {
    /// A chunked-upload orchestrator borrowing this client.
    pub fn chunked_upload(&self) -> ChunkedUpload<'_> {
        ChunkedUpload { client: self }
    }
}

impl ChunkedUpload<'_> {
    /// Upload `content` as a new file named `file_name` into `folder_id`.
    pub async fn upload(
        &self,
        content: &[u8],
        file_name: &str,
        folder_id: &str,
    ) -> Result<schemas::Files, Error> {
        let session = self
            .client
            .chunked_uploads
            .create_file_upload_session(schemas::FileUploadSessionCreateRequest {
                folder_id: folder_id.to_string(),
                file_size: content.len() as i64,
                file_name: file_name.to_string(),
            })
            .await?;
        self.finish(session, content).await
    }

    /// Upload `content` as a new version of the existing file `file_id`.
    pub async fn upload_version(
        &self,
        content: &[u8],
        file_name: &str,
        file_id: &str,
    ) -> Result<schemas::Files, Error> {
        let session = self
            .client
            .chunked_uploads
            .create_file_version_upload_session(
                file_id.to_string(),
                schemas::FileVersionUploadSessionCreateRequest {
                    file_size: content.len() as i64,
                    file_name: Some(file_name.to_string()),
                },
            )
            .await?;
        self.finish(session, content).await
    }

    async fn finish(
        &self,
        session: schemas::UploadSession,
        content: &[u8],
    ) -> Result<schemas::Files, Error> {
        let id = session
            .id
            .ok_or_else(|| Error::new("gantry: upload session returned no id"))?;
        let part_size = match session.part_size {
            Some(size) if size > 0 => size as usize,
            _ => {
                return Err(Error::new(
                    "gantry: upload session returned a non-positive part_size",
                ));
            }
        };
        let total = content.len();

        // Upload in batches of MAX_CONCURRENT parts at a time, each batch driven
        // concurrently (the requests are in flight together) but committed in
        // order — Box's commit lists parts in offset order.
        let offsets: Vec<usize> = (0..total).step_by(part_size).collect();
        let mut parts = Vec::with_capacity(offsets.len());
        for batch in offsets.chunks(MAX_CONCURRENT) {
            let uploads = batch
                .iter()
                .map(|&start| {
                    let end = (start + part_size).min(total);
                    self.upload_part(&id, &content[start..end], start, end, total)
                })
                .collect();
            for result in join_ordered(uploads).await {
                parts.push(result?);
            }
        }

        let digest = format!(
            "sha={}",
            base64::engine::general_purpose::STANDARD.encode(sha1(content))
        );
        self.client
            .chunked_uploads
            .commit_file_upload_session(
                id,
                digest,
                schemas::FileUploadSessionCommitRequest { parts },
                None,
            )
            .await
    }

    async fn upload_part(
        &self,
        id: &str,
        slice: &[u8],
        start: usize,
        end: usize,
        total: usize,
    ) -> Result<schemas::UploadPart, Error> {
        let digest = format!(
            "sha={}",
            base64::engine::general_purpose::STANDARD.encode(sha1(slice))
        );
        let content_range = format!("bytes {}-{}/{}", start, end - 1, total);
        let uploaded = self
            .client
            .chunked_uploads
            .update_file_upload_session(id.to_string(), digest, content_range, slice.to_vec())
            .await?;
        uploaded
            .part
            .ok_or_else(|| Error::new("gantry: upload part returned no part"))
    }
}

/// Await every future concurrently on the current task, returning results in
/// input order. No `tokio::spawn`, so it needs neither a runtime feature nor a
/// `Send`/`'static` bound — enough for I/O-bound part uploads, whose requests
/// are in flight together and driven by the reactor, without pulling in a
/// `futures`/`tokio` `rt` dependency the SDK avoids.
async fn join_ordered<F>(futures: Vec<F>) -> Vec<F::Output>
where
    F: Future,
    F::Output: Unpin,
{
    JoinOrdered {
        slots: futures
            .into_iter()
            .map(|f| (Some(Box::pin(f)), None))
            .collect(),
    }
    .await
}

/// The [`Future`] backing [`join_ordered`]: it polls each not-yet-ready future
/// on every wake and finishes once all have produced a value.
struct JoinOrdered<F: Future> {
    #[allow(clippy::type_complexity)]
    slots: Vec<(Option<Pin<Box<F>>>, Option<F::Output>)>,
}

impl<F> Future for JoinOrdered<F>
where
    F: Future,
    F::Output: Unpin,
{
    type Output = Vec<F::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let slots = &mut self.get_mut().slots;
        let mut pending = false;
        for (future, output) in slots.iter_mut() {
            if let Some(f) = future {
                match f.as_mut().poll(cx) {
                    Poll::Ready(value) => {
                        *output = Some(value);
                        *future = None;
                    }
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(
                slots
                    .iter_mut()
                    .map(|(_, out)| out.take().unwrap())
                    .collect(),
            )
        }
    }
}

/// SHA-1 (RFC 3174) of `data`. Hand-rolled so the SDK needs no hashing
/// dependency — its crypto stack (`sha2`, `rsa`) carries no SHA-1, and Box's
/// chunked-upload digests are `sha=<base64(sha1(bytes))>`.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);

    // Pad: append 0x80, zero-fill to 56 mod 64, then the 64-bit big-endian length.
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999_u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (chunk, word) in out.chunks_exact_mut(4).zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::sha1;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha1_matches_known_answer_vectors() {
        // RFC 3174 / NIST vectors, plus the 55/56/64-byte cases that exercise the
        // padding boundary (56 mod 64) a hand-rolled implementation is most likely
        // to get wrong, and the classic 1e6-byte vector spanning many blocks.
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        assert_eq!(
            hex(&sha1(&[b'a'; 55])),
            "c1c8bbdc22796e28c0e15163d20899b65621d65a"
        );
        assert_eq!(
            hex(&sha1(&[b'a'; 56])),
            "c2db330f6083854c99d4b5bfb6e8f29f201be699"
        );
        assert_eq!(
            hex(&sha1(&[b'a'; 64])),
            "0098ba824b5c16427bd7a1122a5a442a25ec644d"
        );
        assert_eq!(
            hex(&sha1(&[b'a'; 1_000_000])),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }
}
"#;
