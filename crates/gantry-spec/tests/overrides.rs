//! Name overrides (`NameOverrides`): a component-level override cascades
//! into every field synthesized under it; a location override replaces
//! exactly one site; a stale or colliding override fails loudly (NF-1).

use std::path::PathBuf;

use gantry_ir as ir;
use gantry_spec::{IngestError, Lowering, NameOverrides, SpecSet};

fn write_spec(schemas: serde_json::Value) -> PathBuf {
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
        "gantry-overrides-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("spec.json");
    std::fs::write(&file, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    file
}

fn write_overrides(dir_hint: &std::path::Path, overrides: serde_json::Value) -> PathBuf {
    let file = dir_hint.with_file_name("overrides.json");
    std::fs::write(&file, serde_json::to_string_pretty(&overrides).unwrap()).unwrap();
    file
}

fn lower(schemas: serde_json::Value, overrides: &NameOverrides) -> Result<Lowering, IngestError> {
    let spec_file = write_spec(schemas);
    let set = SpecSet::load(&[spec_file]).unwrap();
    gantry_spec::lower_with_overrides(&set, overrides)
}

fn find<'p>(program: &'p ir::Program, name: &str) -> &'p ir::Decl {
    program
        .decls
        .iter()
        .find(|d| d.name.as_str() == name)
        .unwrap_or_else(|| {
            panic!(
                "no declaration named {name}, have: {:?}",
                all_names(program)
            )
        })
}

fn all_names(program: &ir::Program) -> Vec<&str> {
    program.decls.iter().map(|d| d.name.as_str()).collect()
}

/// A component whose two long-named siblings share one verbose owner:
/// `Widget` has an inline `budget_report` field, synthesized as
/// `WidgetBudgetReport` (a struct with one field, `total`), and a
/// `budget_summary` field, synthesized as `WidgetBudgetSummary`.
fn widget_schemas() -> serde_json::Value {
    serde_json::json!({
        "Widget": {
            "type": "object",
            "properties": {
                "budget_report": {
                    "type": "object",
                    "properties": { "total": { "type": "string" } }
                },
                "budget_summary": {
                    "type": "object",
                    "properties": { "count": { "type": "integer" } }
                }
            }
        }
    })
}

#[test]
fn a_component_override_cascades_to_every_field_synthesized_under_it() {
    let spec_file = write_spec(widget_schemas());
    let overrides_file = write_overrides(
        &spec_file,
        serde_json::json!({ "components": { "Widget": "Wdg" } }),
    );
    let overrides = NameOverrides::load(&overrides_file).unwrap();
    let set = SpecSet::load(&[spec_file]).unwrap();
    let lowering = gantry_spec::lower_with_overrides(&set, &overrides).unwrap();

    find(&lowering.program, "Wdg");
    // Both children inherited the shortened owner, not the original name.
    find(&lowering.program, "WdgBudgetReport");
    find(&lowering.program, "WdgBudgetSummary");
    assert!(
        !all_names(&lowering.program).contains(&"Widget"),
        "the original component name should not survive alongside the override: {:?}",
        all_names(&lowering.program)
    );
    assert!(
        !all_names(&lowering.program)
            .iter()
            .any(|n| n.starts_with("Widget")),
        "no descendant should keep the un-overridden owner prefix: {:?}",
        all_names(&lowering.program)
    );
}

#[test]
fn a_location_override_replaces_exactly_one_site() {
    let spec_file = write_spec(widget_schemas());
    let overrides_file = write_overrides(
        &spec_file,
        serde_json::json!({
            "locations": {
                "components.schemas.Widget.properties.budget_report": "Report"
            }
        }),
    );
    let overrides = NameOverrides::load(&overrides_file).unwrap();
    let set = SpecSet::load(&[spec_file]).unwrap();
    let lowering = gantry_spec::lower_with_overrides(&set, &overrides).unwrap();

    find(&lowering.program, "Report");
    // The un-overridden sibling still gets its normal, un-cascaded name.
    find(&lowering.program, "WidgetBudgetSummary");
    assert!(!all_names(&lowering.program).contains(&"WidgetBudgetReport"));
}

#[test]
fn an_override_key_that_matches_nothing_is_a_loud_error() {
    let overrides_file = write_overrides(
        &write_spec(widget_schemas()),
        serde_json::json!({ "components": { "NoSuchSchema": "Whatever" } }),
    );
    let overrides = NameOverrides::load(&overrides_file).unwrap();
    let err = lower(widget_schemas(), &overrides).unwrap_err();
    assert!(
        matches!(&err, IngestError::UnusedOverride { kind, key } if *kind == "component" && key == "NoSuchSchema"),
        "{err}"
    );
}

#[test]
fn an_override_value_colliding_with_an_existing_name_is_a_loud_error() {
    // "Taken" is a real, separate top-level component — reserved before any
    // field synthesis runs. Overriding an unrelated field to the same text
    // must fail loudly, not silently numeral-suffix around the collision
    // (a numeral would silently give the human a name they didn't ask for).
    let mut schemas = widget_schemas();
    schemas["Taken"] = serde_json::json!({
        "type": "object",
        "properties": { "x": { "type": "string" } }
    });
    let overrides_file = write_overrides(
        &write_spec(schemas.clone()),
        serde_json::json!({
            "locations": {
                "components.schemas.Widget.properties.budget_report": "Taken"
            }
        }),
    );
    let overrides = NameOverrides::load(&overrides_file).unwrap();
    let err = lower(schemas, &overrides).unwrap_err();
    assert!(
        matches!(&err, IngestError::OverrideCollision { .. }),
        "{err}"
    );
}

#[test]
fn an_invalid_override_value_fails_at_load_time() {
    let overrides_file = write_overrides(
        &write_spec(widget_schemas()),
        serde_json::json!({ "components": { "Widget": "not a valid identifier" } }),
    );
    let err = NameOverrides::load(&overrides_file).unwrap_err();
    assert!(
        matches!(&err, IngestError::InvalidOverrideName { .. }),
        "{err}"
    );
}

#[test]
fn empty_overrides_behaves_identically_to_no_overrides_flag() {
    let with_empty = lower(widget_schemas(), &NameOverrides::empty()).unwrap();
    let plain = {
        let spec_file = write_spec(widget_schemas());
        let set = SpecSet::load(&[spec_file]).unwrap();
        gantry_spec::lower(&set).unwrap()
    };
    assert_eq!(all_names(&with_empty.program), all_names(&plain.program));
}

#[test]
fn the_synthesis_log_records_every_named_declaration_with_its_location() {
    let lowering = lower(widget_schemas(), &NameOverrides::empty()).unwrap();
    let entry = lowering
        .synthesis_log
        .iter()
        .find(|(_, name)| name == "WidgetBudgetReport")
        .unwrap_or_else(|| {
            panic!(
                "expected an entry for WidgetBudgetReport in {:?}",
                lowering.synthesis_log
            )
        });
    assert_eq!(
        entry.0,
        "components.schemas.Widget.properties.budget_report"
    );
}
