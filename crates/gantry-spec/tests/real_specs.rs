//! The vendored real Box specs must always ingest cleanly — the earliest
//! form of the "generate the real spec" primary CI signal (VR-1.1 lineage).

use std::path::PathBuf;

use gantry_spec::SpecSet;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

#[test]
fn the_full_real_spec_set_ingests() {
    let set = SpecSet::load(&[
        fixture("openapi.json"),
        fixture("openapi-v2025.0.json"),
        fixture("openapi-v2026.0.json"),
    ])
    .expect("the vendored Box specs must ingest without errors");

    let versions: Vec<&str> = set
        .documents
        .iter()
        .map(|d| d.api_version.as_str())
        .collect();
    assert_eq!(versions, ["2024.0", "2025.0", "2026.0"]);

    let base = &set.documents[0];
    // Counts as of the vendored snapshot (see fixtures/specs/README.md).
    // If a spec refresh changes them, updating these numbers is the
    // deliberate, reviewed act the determinism rules want (FR-6.2, VR-6).
    assert_eq!(base.operations.len(), 296);
    assert_eq!(base.schemas.len(), 305);
    assert_eq!(base.managers().len(), 73);

    assert_eq!(set.documents[1].operations.len(), 37);
    assert_eq!(set.documents[2].operations.len(), 3);

    // Every operation everywhere is grouped and identified.
    for doc in &set.documents {
        for op in &doc.operations {
            assert!(!op.id.is_empty() && !op.manager.is_empty());
        }
    }

    // The whole set lowers into typed IR with zero errors; the counts are
    // pinned so growth and free-form holes only change deliberately (NF-1,
    // VR-6 lineage).
    let lowering = gantry_spec::lower(&set).expect("the vendored Box specs must lower");
    // After structural dedupe (D-127) identical inline shapes collapse
    // (492 synthesized), then the version merge (D-190) collapses 21 same-named
    // cross-version schemas (16 structs + 5 enums) into one superset each:
    // 900 → 879 decls.
    assert_eq!(lowering.program.decls.len(), 879);
    let stats = &lowering.stats;
    assert_eq!(
        (stats.structs, stats.unions, stats.discriminated_unions),
        (592, 42, 23)
    );
    assert_eq!((stats.enums, stats.aliases), (243, 2));
    assert_eq!(stats.synthesized, 492);
    assert_eq!(stats.json_value_sites, 26);

    // Operations: every one lowered, with classified success shapes.
    assert_eq!(lowering.program.operations.len(), 336);
    assert_eq!(stats.operations, 336);
    assert_eq!(
        (
            stats.empty_responses,
            stats.binary_responses,
            stats.text_responses,
            stats.redirect_responses
        ),
        (56, 4, 1, 0)
    );
    // Nineteen `#variation` ids became structured variations (D-104).
    let variations = lowering
        .program
        .operations
        .iter()
        .filter(|op| op.variation.is_some())
        .count();
    assert_eq!(variations, 19);
    // The base-URL quirk (G-2): non-default servers map to the closed set.
    let non_default = lowering
        .program
        .operations
        .iter()
        .filter(|op| op.base_url != gantry_ir::BaseUrl::Api)
        .count();
    assert_eq!(non_default, 14);

    // The semantic pass verifies the whole real program (FR-3): every
    // reference bound, every type well-formed, identities unique.
    let analysis = gantry_sema::analyze(&lowering.program)
        .expect("the real program must pass semantic analysis");
    assert_eq!(analysis.managers.len(), 85);
    let indexed: usize = analysis.managers.values().map(Vec::len).sum();
    assert_eq!(indexed, 336);
}

#[test]
fn the_spec_fingerprint_is_deterministic_and_order_sensitive() {
    let load = |files: &[PathBuf]| SpecSet::load(files).unwrap().fingerprint();
    let base_then_2025 = [fixture("openapi.json"), fixture("openapi-v2025.0.json")];
    let reversed = [fixture("openapi-v2025.0.json"), fixture("openapi.json")];

    // Deterministic: same inputs in the same order → same fingerprint (NF-7).
    assert_eq!(load(&base_then_2025), load(&base_then_2025));
    // Sixteen lowercase hex digits.
    let fp = load(&base_then_2025);
    assert_eq!(fp.len(), 16);
    assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));

    // Load order changes the fingerprint (the set is ordered — base first).
    assert_ne!(load(&base_then_2025), load(&reversed));
    // Adding a document changes it.
    assert_ne!(load(&base_then_2025), load(&[fixture("openapi.json")]));
}
