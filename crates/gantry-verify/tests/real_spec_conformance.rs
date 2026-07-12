//! VR-3 over the real vendored specs: the generated Go SDK must satisfy
//! the full R§1 capability contract — every manager, operation, and
//! paginated surface expressed, plus the serialization package, generated
//! tests, auth flows, and docs.

use std::path::PathBuf;

use gantry_backend_go::GeneratedFile;
use gantry_spec::SpecSet;
use gantry_verify::{GeneratedView, conformance};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

fn generate() -> (gantry_ir::Program, Vec<GeneratedFile>) {
    let lowering = gantry_spec::lower(
        &SpecSet::load(&[
            fixture("openapi.json"),
            fixture("openapi-v2025.0.json"),
            fixture("openapi-v2026.0.json"),
        ])
        .unwrap(),
    )
    .unwrap();
    let program = Box::leak(Box::new(lowering.program));
    let analysis = gantry_sema::analyze(program).unwrap();
    let files = gantry_backend_go::generate(&analysis).unwrap();
    (program.clone(), files)
}

#[test]
fn the_generated_go_sdk_is_conformant() {
    let (program, files) = generate();
    let analysis = gantry_sema::analyze(&program).unwrap();
    let views: Vec<GeneratedView> = files
        .iter()
        .map(|f| GeneratedView {
            path: &f.path,
            content: &f.content,
        })
        .collect();

    let report = conformance("go", &analysis, &views);
    assert!(
        report.passed(),
        "the generated Go SDK is not R§1-conformant:\n{}",
        report.report()
    );

    // Spot-check the headline capabilities carry real, non-trivial counts
    // (a checklist that passes on an empty program proves nothing).
    let by = |cap: &str| {
        report
            .checks
            .iter()
            .find(|c| c.capability == cap)
            .unwrap_or_else(|| panic!("no {cap} check"))
    };
    assert_eq!(by("managers").expected, 85);
    assert_eq!(by("operations").expected, 336);
    assert_eq!(by("operations").actual, 336, "one method per operation");
    assert_eq!(by("pagination").expected, 64);
    assert_eq!(
        by("pagination").actual,
        64,
        "one iterator per paged surface"
    );
    assert_eq!(by("auth-flows").actual, 4);
}
