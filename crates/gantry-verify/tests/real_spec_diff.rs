//! FR-9 over the real vendored specs: adding a version overlay to the base
//! spec is an additive change (new operations + versioned schemas), never a
//! breaking one, and a spec set diffed against itself is empty.

use std::path::PathBuf;

use gantry_spec::SpecSet;
use gantry_verify::{VersionBump, diff};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

fn lower(names: &[&str]) -> gantry_ir::Program {
    let paths: Vec<PathBuf> = names.iter().map(|n| fixture(n)).collect();
    gantry_spec::lower(&SpecSet::load(&paths).unwrap())
        .unwrap()
        .program
}

#[test]
fn a_program_does_not_differ_from_itself() {
    let program = lower(&["openapi.json"]);
    let result = diff(&program, &program);
    assert!(
        result.changes.is_empty(),
        "a program must not differ from itself:\n{}",
        result.report()
    );
    assert_eq!(result.bump(), VersionBump::None);
}

#[test]
fn adding_a_version_overlay_is_a_compatible_change() {
    let base = lower(&["openapi.json"]);
    let with_2025 = lower(&["openapi.json", "openapi-v2025.0.json"]);

    let result = diff(&base, &with_2025);
    // The overlay only adds surface (2025.0 operations and versioned
    // schemas), so nothing is breaking.
    assert_eq!(
        result.breaking(),
        0,
        "the 2025.0 overlay should be purely additive:\n{}",
        result.report()
    );
    assert!(
        result.compatible() > 0,
        "the overlay must introduce new operations/schemas"
    );
    assert_eq!(result.bump(), VersionBump::Minor);
}

#[test]
fn dropping_a_version_overlay_is_breaking() {
    // The reverse direction removes the operations/schemas the overlay
    // added — a major bump.
    let base = lower(&["openapi.json"]);
    let with_2025 = lower(&["openapi.json", "openapi-v2025.0.json"]);

    let result = diff(&with_2025, &base);
    assert!(result.breaking() > 0, "removals must be breaking");
    assert_eq!(result.bump(), VersionBump::Major);
}
