//! Representation detection over Box's real specs.

use std::path::PathBuf;

use gantry_spec::SpecSet;
use gantry_synth::detect_representations;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

/// Box has exactly one representation source: `GET /files/{file_id}`, the
/// only item read taking both `fields` and an `x-rep-hints` header and
/// answering `representations`. A spec change that breaks the shape drops
/// it, and this fails.
#[test]
fn the_file_get_is_the_only_representation_source() {
    let set = SpecSet::load(&[
        fixture("openapi.json"),
        fixture("openapi-v2025.0.json"),
        fixture("openapi-v2026.0.json"),
    ])
    .unwrap();
    let lowering = gantry_spec::lower(&set).unwrap();
    let analysis = gantry_sema::analyze(&lowering.program).unwrap();
    let ops = &lowering.program.operations;
    let found: Vec<String> = detect_representations(&analysis)
        .iter()
        .map(|r| {
            format!(
                "{}.{} ({} / {} -> {})",
                ops[r.operation].manager.as_str(),
                ops[r.operation].name.as_str(),
                r.fields_param.wire_name,
                r.hints_param.wire_name,
                r.field.wire_name
            )
        })
        .collect();
    assert_eq!(
        found,
        ["files.get (fields / x-rep-hints -> representations)"]
    );
}
