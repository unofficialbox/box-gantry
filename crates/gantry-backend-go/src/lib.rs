//! The Go backend: lowering + printer (FR-6, TR-Go).
//!
//! Generates the SDK tree: `schemas/` model packages (D-110 tri-state
//! mapping, open enums, union variant structs with the only generated
//! serializers), `managers/` with one method per operation calling only
//! through the runtime contract (FR-5.2), the `client/` entry point
//! (FR-7.1), and the contract's compilable runtime stubs so the whole
//! output compile-verifies without a real runtime (FR-5.3). Output is
//! deterministic (FR-6.2) and gofmt-clean by construction (G-17),
//! verified by the real toolchain (VR-1.1 — the primary CI signal).

mod managers;
mod models;

pub use managers::{BackendError, generate_managers};
pub use models::{GeneratedFile, generate_models};

/// Generate the complete SDK tree for a verified program.
pub fn generate(analysis: &gantry_sema::Analysis<'_>) -> Result<Vec<GeneratedFile>, BackendError> {
    let paged = gantry_synth::detect_pagination(analysis);
    let mut files = generate_models(analysis);
    files.extend(generate_managers(analysis, &paged)?);
    // The runtime stubs are rendered from the contract data (FR-5.2):
    // generated managers compile against exactly the declared surface.
    files.push(GeneratedFile {
        path: "gantryruntime/runtime.go".to_string(),
        content: gantry_contract::go_stubs(&gantry_contract::V1, &gantry_manifest::go()),
    });
    Ok(files)
}
