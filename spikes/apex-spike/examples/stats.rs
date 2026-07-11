//! Print the spike's aggregate numbers (consumed by D-108's findings).

use std::path::PathBuf;

use gantry_spec::SpecSet;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/specs");
    let lowering = gantry_spec::lower(
        &SpecSet::load(&[
            root.join("openapi.json"),
            root.join("openapi-v2025.0.json"),
            root.join("openapi-v2026.0.json"),
        ])
        .unwrap(),
    )
    .unwrap();
    let analysis = gantry_sema::analyze(&lowering.program).unwrap();
    let manifest = gantry_manifest::apex();

    let mut identifiers = 0;
    let mut abbreviated = 0;
    let mut paged = 0;
    let mut dispatch = 0;
    let mut source_bytes = 0;
    for manager in analysis.managers.keys() {
        let lowered = apex_spike::lower_manager(&analysis, &manifest, manager);
        identifiers += lowered.identifiers.len();
        abbreviated += lowered
            .identifiers
            .iter()
            .filter(|i| i.len() == 40 && i.as_bytes()[32] == b'_')
            .count();
        paged += lowered.paged_operations;
        dispatch += lowered.dispatch_unions;
        source_bytes += lowered.source.len();
    }
    println!(
        "managers: {} | identifiers minted: {identifiers} (abbreviated: {abbreviated}) | \
         paged ops: {paged} | dispatch unions: {dispatch} | source: {source_bytes} bytes",
        analysis.managers.len()
    );
}
