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
        GeneratedFile {
            path: "src/lib.rs".to_string(),
            content: lib_rs(manifest, build),
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
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
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
     description = \"Generated Box API SDK for Rust (community, unofficial).\"\n\
     license = \"MIT\"\n\
     repository = \"https://github.com/unofficialbox/box-open-rust-sdk\"\n\
     readme = \"README.md\"\n\
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
    "<!-- Generated by box-gantry. DO NOT EDIT. -->\n\
     # Box SDK\n\
     \n\
     A generated Rust SDK for the Box API.\n\
     \n\
     ```rust,ignore\n\
     let client = box_open_sdk::client::Client::new(box_open_sdk::runtime::Auth::developer_token(\"DEVELOPER_TOKEN\"));\n\
     ```\n\
     \n\
     See the `docs/` tree for the manager reference and the authentication,\n\
     pagination, and errors guides.\n"
        .to_string()
}

/// The crate root: module tree + the provenance `buildinfo` constants (NF-7),
/// which let the shipped SDK report its own version and inputs.
fn lib_rs(manifest: &gantry_manifest::CapabilityManifest, build: &BuildInfo) -> String {
    format!(
        "// Code generated by box-gantry {engine} (spec {fingerprint}). DO NOT EDIT.\n\
         #![allow(clippy::large_enum_variant)]\n\
         \n\
         pub mod client;\n\
         pub(crate) mod internal;\n\
         pub mod managers;\n\
         pub mod models;\n\
         pub mod runtime;\n\
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
