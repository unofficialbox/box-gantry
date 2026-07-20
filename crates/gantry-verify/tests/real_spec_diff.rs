//! FR-9 over the real vendored specs: adding a version overlay to the base
//! spec is almost entirely additive (new operations + versioned schemas). The
//! one exception is the D-190 superset merge: where a schema name is shared
//! across versions, the merged type takes the looser union, so a field the base
//! required can become optional — a real (intended) contract change the diff
//! honestly reports as breaking. A spec set diffed against itself is empty.

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
    // The overlay adds surface (2025.0 operations and schemas) — overwhelmingly
    // compatible — but the D-190 superset merge loosens the one genuinely-
    // different shared schema, `SharedLinkPermissions`: its base-required fields
    // become optional in the union. The diff reports that single contract
    // loosening as breaking (a field the base guaranteed is no longer certain).
    assert_eq!(
        result.breaking(),
        1,
        "only the SharedLinkPermissions superset loosening is breaking:\n{}",
        result.report()
    );
    assert!(
        result.report().contains("SharedLinkPermissions"),
        "the lone breaking change is the merged SharedLinkPermissions:\n{}",
        result.report()
    );
    assert!(
        result.compatible() > 0,
        "the overlay must introduce new operations/schemas"
    );
    // One breaking change → a major bump (the looser contract, per D-190).
    assert_eq!(result.bump(), VersionBump::Major);
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
