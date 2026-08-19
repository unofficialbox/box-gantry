//! The `gantry names` report (`gantry_spec::long_names`): grouping by
//! shared component prefix, word breakdown, and deterministic ordering.

use std::path::PathBuf;

use gantry_spec::SpecSet;

fn lower(schemas: serde_json::Value) -> gantry_spec::Lowering {
    let spec = serde_json::json!({
        "openapi": "3.0.2",
        "info": { "title": "Box Platform API", "version": "2024.0" },
        "paths": {
            "/files/{file_id}": {
                "get": {
                    "operationId": "get_files_id",
                    "x-box-tag": "files",
                    "parameters": [
                        { "name": "file_id", "in": "path", "required": true,
                          "schema": { "type": "string" } }
                    ]
                }
            }
        },
        "components": { "schemas": schemas }
    });
    let dir = std::env::temp_dir().join(format!(
        "gantry-report-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file: PathBuf = dir.join("spec.json");
    std::fs::write(&file, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    let set = SpecSet::load(&[file]).unwrap();
    gantry_spec::lower(&set).unwrap()
}

/// Two long names sharing one long component prefix, plus one long name
/// with no component prefix (an inline enum nested two levels deep, so its
/// name is `leaf`-seeded from its immediate parent field, not the
/// top-level component).
fn fixture() -> serde_json::Value {
    serde_json::json!({
        "AVeryLongComponentSchemaNameForTesting": {
            "type": "object",
            "properties": {
                "first_long_field_name": {
                    "type": "object",
                    "properties": { "a": { "type": "string" } }
                },
                "second_long_field_name": {
                    "type": "object",
                    "properties": { "b": { "type": "string" } }
                },
                "nested": {
                    "type": "object",
                    "properties": {
                        "another_quite_long_inline_enum_field": {
                            "type": "string",
                            "enum": ["x", "y"]
                        }
                    }
                }
            }
        }
    })
}

#[test]
fn long_names_below_the_threshold_are_excluded() {
    let lowering = lower(fixture());
    let report = gantry_spec::long_names(&lowering.synthesis_log, 1000);
    assert_eq!(report.total(), 0, "{}", report.report());
}

#[test]
fn names_sharing_a_component_prefix_are_grouped_under_it() {
    let lowering = lower(fixture());
    let report = gantry_spec::long_names(&lowering.synthesis_log, 30);
    assert_eq!(report.grouped.len(), 1, "{}", report.report());
    let (component, _len, names) = &report.grouped[0];
    assert_eq!(component, "AVeryLongComponentSchemaNameForTesting");
    let names: Vec<&str> = names.iter().map(|n| n.name.as_str()).collect();
    assert!(
        names.contains(&"AVeryLongComponentSchemaNameForTestingFirstLongFieldName"),
        "{names:?}"
    );
    assert!(
        names.contains(&"AVeryLongComponentSchemaNameForTestingSecondLongFieldName"),
        "{names:?}"
    );
}

#[test]
fn a_name_with_no_component_prefix_is_ungrouped() {
    let lowering = lower(fixture());
    let report = gantry_spec::long_names(&lowering.synthesis_log, 30);
    let ungrouped: Vec<&str> = report.ungrouped.iter().map(|n| n.name.as_str()).collect();
    assert!(
        ungrouped
            .iter()
            .any(|n| n.contains("AnotherQuiteLongInlineEnumField")),
        "{ungrouped:?}"
    );
}

#[test]
fn word_breakdown_splits_on_word_boundaries() {
    let lowering = lower(fixture());
    let report = gantry_spec::long_names(&lowering.synthesis_log, 30);
    let entry = report
        .grouped
        .iter()
        .flat_map(|(_, _, names)| names)
        .find(|n| n.name.ends_with("FirstLongFieldName"))
        .unwrap();
    assert_eq!(
        entry.words,
        vec![
            "a",
            "very",
            "long",
            "component",
            "schema",
            "name",
            "for",
            "testing",
            "first",
            "long",
            "field",
            "name"
        ]
    );
}

#[test]
fn the_report_text_is_deterministic_across_runs() {
    let first = gantry_spec::long_names(&lower(fixture()).synthesis_log, 30).report();
    let second = gantry_spec::long_names(&lower(fixture()).synthesis_log, 30).report();
    assert_eq!(first, second);
}
