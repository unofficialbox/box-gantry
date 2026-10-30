//! Zip-download detection over Box's real specs.

use std::path::PathBuf;

use gantry_spec::SpecSet;
use gantry_synth::detect_zip_downloads;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

/// Box has exactly one zip download: `POST /zip_downloads`, answered with a
/// `download_url`, paired with `GET /zip_downloads/{id}/content` on the
/// download host. A spec change that breaks the shape drops it, and this fails.
#[test]
fn zip_downloads_create_and_get_content_are_the_only_zip_download() {
    let set = SpecSet::load(&[
        fixture("openapi.json"),
        fixture("openapi-v2025.0.json"),
        fixture("openapi-v2026.0.json"),
    ])
    .unwrap();
    let lowering = gantry_spec::lower(&set).unwrap();
    let analysis = gantry_sema::analyze(&lowering.program).unwrap();
    let ops = &lowering.program.operations;
    let name = |index: usize| {
        format!(
            "{}.{}",
            ops[index].manager.as_str(),
            ops[index].name.as_str()
        )
    };
    let found: Vec<String> = detect_zip_downloads(&analysis)
        .iter()
        .map(|z| {
            format!(
                "{} + {} ({})",
                name(z.create),
                name(z.content),
                z.download_url.wire_name
            )
        })
        .collect();
    assert_eq!(
        found,
        ["zip_downloads.create + zip_downloads.get_content (download_url)"]
    );
}
