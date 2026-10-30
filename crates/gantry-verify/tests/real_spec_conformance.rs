//! VR-3 over the real vendored specs: the generated Go SDK must satisfy
//! the full R§1 capability contract — every manager, operation, and
//! paginated surface expressed, plus the serialization package, generated
//! tests, auth flows, and docs.

use std::path::PathBuf;

use gantry_spec::SpecSet;
use gantry_verify::docs::unindented_block_bodies;
use gantry_verify::{GeneratedView, apex_shape, conformance, go_shape, typescript_shape};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

fn spec_set() -> SpecSet {
    SpecSet::load(&[
        fixture("openapi.json"),
        fixture("openapi-v2025.0.json"),
        fixture("openapi-v2026.0.json"),
    ])
    .unwrap()
}

/// A generated file set as `(path, content)` pairs — the backend-neutral
/// shape the conformance views borrow from.
fn views_of(files: &[(String, String)]) -> Vec<GeneratedView<'_>> {
    files
        .iter()
        .map(|(path, content)| GeneratedView { path, content })
        .collect()
}

#[test]
fn the_generated_go_sdk_is_conformant() {
    let set = spec_set();
    let build = gantry_backend_go::BuildInfo::new(set.fingerprint());
    let program = Box::leak(Box::new(gantry_spec::lower(&set).unwrap().program));
    let analysis = gantry_sema::analyze(program).unwrap();
    let files: Vec<(String, String)> = gantry_backend_go::generate(&analysis, &build)
        .unwrap()
        .into_iter()
        .map(|f| (f.path, f.content))
        .collect();
    let views = views_of(&files);
    assert_docs_keep_indentation(&files);

    let report = conformance(&go_shape(), &analysis, &views);
    assert!(
        report.passed(),
        "the generated Go SDK is not R§1-conformant:\n{}",
        report.report()
    );
    // Go has no documented platform exclusions — full parity.
    assert_eq!(report.excluded(), 0, "{}", report.report());

    // Spot-check the headline capabilities carry real, non-trivial counts
    // (a checklist that passes on an empty program proves nothing).
    let by = |cap: &str| {
        report
            .checks
            .iter()
            .find(|c| c.capability == cap)
            .unwrap_or_else(|| panic!("no {cap} check"))
    };
    assert_eq!(by("managers").expected, 88);
    assert_eq!(by("operations").expected, 381);
    assert_eq!(by("operations").actual, 381, "one method per operation");
    assert_eq!(by("token-downscoping").expected, 1);
    assert_eq!(
        by("token-downscoping").actual,
        1,
        "the authorization manager's downscope method"
    );
    assert_eq!(by("conditional-reads").expected, 6);
    assert_eq!(
        by("conditional-reads").actual,
        6,
        "one not-modified variant each"
    );
    assert_eq!(by("pagination").expected, 75);
    assert_eq!(
        by("pagination").actual,
        75,
        "one iterator per paged surface"
    );
    assert_eq!(by("event-stream").expected, 1);
    assert_eq!(by("event-stream").actual, 1, "the events manager's Stream");
    assert_eq!(by("auth-flows").actual, 4);
    assert_eq!(by("docs-guides").actual, 5, "index + 4 topic guides");
    assert_eq!(
        by("webhook-signatures").actual,
        1,
        "the runtime verifies webhooks"
    );
}

#[test]
fn the_generated_apex_sdk_is_conformant() {
    let set = spec_set();
    let build = gantry_backend_apex::BuildInfo::new(set.fingerprint());
    let program = Box::leak(Box::new(gantry_spec::lower(&set).unwrap().program));
    let analysis = gantry_sema::analyze(program).unwrap();
    let files: Vec<(String, String)> =
        gantry_backend_apex::generate(&analysis, &gantry_manifest::apex(), &build)
            .into_iter()
            .map(|f| (f.path, f.content))
            .collect();
    let views = views_of(&files);
    assert_docs_keep_indentation(&files);

    let report = conformance(&apex_shape(), &analysis, &views);
    assert!(
        report.passed(),
        "the generated Apex SDK is not R§1-conformant (parity minus documented \
         platform exclusions):\n{}",
        report.report()
    );

    let by = |cap: &str| {
        report
            .checks
            .iter()
            .find(|c| c.capability == cap)
            .unwrap_or_else(|| panic!("no {cap} check"))
    };
    // Same expected surface as Go — the contract is target-neutral.
    assert_eq!(by("managers").expected, 88);
    assert_eq!(by("managers").actual, 88, "one class per manager");
    assert_eq!(by("operations").expected, 381);
    assert_eq!(by("operations").actual, 381, "one method per operation");
    assert_eq!(by("token-downscoping").expected, 1);
    assert_eq!(
        by("token-downscoping").actual,
        1,
        "the authorization manager's downscope method"
    );
    assert_eq!(by("conditional-reads").expected, 6);
    assert_eq!(
        by("conditional-reads").actual,
        6,
        "one not-modified variant each"
    );
    assert_eq!(by("pagination").expected, 75);
    assert_eq!(
        by("pagination").actual,
        75,
        "one paged surface per operation"
    );
    assert_eq!(
        by("traceability").actual,
        1,
        "BoxBuildInfo carries provenance"
    );
    assert_eq!(by("docs-guides").actual, 5, "index + 4 topic guides");
    assert_eq!(
        by("webhook-signatures").actual,
        1,
        "the runtime verifies webhooks"
    );

    // The two documented platform exclusions pass as not-applicable, never
    // as failures: erased serialization + interactive OAuth (D-141).
    use gantry_verify::CheckStatus;
    assert_eq!(by("serialization").status, CheckStatus::Excluded);
    assert_eq!(by("auth-flows").status, CheckStatus::Excluded);
    assert_eq!(by("auth-flows").actual, 3, "Developer Token / CCG / JWT");
    assert_eq!(report.excluded(), 2, "{}", report.report());
    assert_eq!(report.failures(), 0);
}

#[test]
fn the_generated_typescript_sdk_is_conformant() {
    let set = spec_set();
    let build = gantry_backend_typescript::BuildInfo::new(set.fingerprint());
    let program = Box::leak(Box::new(gantry_spec::lower(&set).unwrap().program));
    let analysis = gantry_sema::analyze(program).unwrap();
    let files: Vec<(String, String)> =
        gantry_backend_typescript::generate(&analysis, &gantry_manifest::typescript(), &build)
            .into_iter()
            .map(|f| (f.path, f.content))
            .collect();
    let views = views_of(&files);
    assert_docs_keep_indentation(&files);

    let report = conformance(&typescript_shape(), &analysis, &views);
    assert!(
        report.passed(),
        "the generated TypeScript SDK is not R§1-conformant (parity minus the \
         documented serialization exclusion):\n{}",
        report.report()
    );

    let by = |cap: &str| {
        report
            .checks
            .iter()
            .find(|c| c.capability == cap)
            .unwrap_or_else(|| panic!("no {cap} check"))
    };
    // Same expected surface as Go — the contract is target-neutral.
    assert_eq!(by("managers").expected, 88);
    assert_eq!(by("managers").actual, 88, "one class per manager");
    assert_eq!(by("operations").expected, 381);
    assert_eq!(by("operations").actual, 381, "one method per operation");
    assert_eq!(by("token-downscoping").expected, 1);
    assert_eq!(
        by("token-downscoping").actual,
        1,
        "the authorization manager's downscope method"
    );
    assert_eq!(by("conditional-reads").expected, 6);
    assert_eq!(
        by("conditional-reads").actual,
        6,
        "one not-modified variant each"
    );
    assert_eq!(by("pagination").expected, 75);
    assert_eq!(
        by("pagination").actual,
        75,
        "one paginator per paged surface"
    );
    assert_eq!(by("auth-flows").actual, 4, "all four flows documented");
    assert_eq!(by("manager-docs").actual, 88, "one doc page per manager");
    assert_eq!(by("docs-guides").actual, 5, "index + 4 topic guides");
    assert_eq!(
        by("webhook-signatures").actual,
        1,
        "the runtime verifies webhooks"
    );
    // The generated behavioral tests: the serialization baseline plus one
    // round-trip test per discriminated union.
    assert!(
        by("round-trip-tests").actual >= 2,
        "serialization baseline + per-union tests"
    );

    // The one documented platform exclusion: the erased serialization layer
    // (the tri-state maps onto `?:`/`| null`, TR-TS.2). It passes as
    // not-applicable, never a failure.
    use gantry_verify::CheckStatus;
    assert_eq!(by("serialization").status, CheckStatus::Excluded);
    assert_eq!(report.excluded(), 1, "{}", report.report());
    assert_eq!(report.failures(), 0);
}

/// Every generated markdown file's code examples keep their indentation (a
/// guide's `\` line continuation strips leading spaces unless escaped).
fn assert_docs_keep_indentation(files: &[(String, String)]) {
    let flush_left: Vec<String> = files
        .iter()
        .filter(|(path, _)| path.ends_with(".md"))
        .flat_map(|(path, content)| {
            unindented_block_bodies(content)
                .into_iter()
                .map(move |line| format!("{path} {line}"))
        })
        .collect();
    assert!(
        flush_left.is_empty(),
        "unindented code-example lines:\n{}",
        flush_left.join("\n")
    );
}
