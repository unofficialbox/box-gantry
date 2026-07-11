//! Schema → IR lowering semantics (the D-105 conventions).

use std::path::PathBuf;

use gantry_ir as ir;
use gantry_spec::{IngestError, Lowering, SpecSet};

/// Wrap `schemas` in a minimal valid document, write it, load it, lower it.
fn lower_schemas(schemas: serde_json::Value) -> Result<Lowering, IngestError> {
    let spec = serde_json::json!({
        "openapi": "3.0.2",
        "info": { "title": "Box Platform API", "version": "2024.0" },
        "paths": {
            "/files/{file_id}": {
                "get": { "operationId": "get_files_id", "x-box-tag": "files" }
            }
        },
        "components": { "schemas": schemas }
    });
    let dir = std::env::temp_dir().join(format!(
        "gantry-lowering-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("spec.json");
    std::fs::write(&file, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    let set = SpecSet::load(&[file])?;
    gantry_spec::lower(&set)
}

fn find<'p>(program: &'p ir::Program, name: &str) -> &'p ir::Decl {
    program
        .decls
        .iter()
        .find(|d| d.name.as_str() == name)
        .unwrap_or_else(|| panic!("no declaration named {name}"))
}

fn struct_decl(decl: &ir::Decl) -> &ir::StructDecl {
    let ir::DeclKind::Struct(s) = &decl.kind else {
        panic!("{} is not a struct: {:?}", decl.name.as_str(), decl.kind)
    };
    s
}

#[test]
fn wrapper_idiom_is_a_reference_not_a_new_type() {
    let lowering = lower_schemas(serde_json::json!({
        "FolderMini": { "type": "object", "properties": { "id": { "type": "string" } } },
        "Folder": {
            "type": "object",
            "properties": {
                "parent": {
                    "allOf": [
                        { "$ref": "#/components/schemas/FolderMini" },
                        { "description": "The parent folder.", "nullable": true }
                    ]
                }
            }
        }
    }))
    .unwrap();
    let folder = struct_decl(find(&lowering.program, "Folder"));
    let parent = &folder.fields[0];
    // nullable came from the annotation part; the type is the referenced
    // decl, not a synthesized wrapper.
    let ir::Type::Optional(inner) = &parent.ty else {
        panic!("parent must be optional: {:?}", parent.ty)
    };
    let ir::Type::Decl(id) = **inner else {
        panic!("parent must reference FolderMini: {inner:?}")
    };
    assert_eq!(lowering.program.decl(id).name.as_str(), "FolderMini");
    assert_eq!(lowering.stats.synthesized, 0);
}

#[test]
fn all_of_composition_flattens_with_later_parts_overriding() {
    let lowering = lower_schemas(serde_json::json!({
        "Base": {
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "kind": { "type": "string" }
            },
            "required": ["id"]
        },
        "Extended": {
            "allOf": [
                { "$ref": "#/components/schemas/Base" },
                {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "boolean" },
                        "size": { "type": "integer" }
                    },
                    "required": ["size"]
                }
            ]
        }
    }))
    .unwrap();
    let extended = struct_decl(find(&lowering.program, "Extended"));
    let names: Vec<&str> = extended
        .fields
        .iter()
        .map(|f| f.wire_name.as_str())
        .collect();
    assert_eq!(names, ["id", "kind", "size"]);
    // `id` stays required (from Base), `size` is required (extension),
    // `kind` was overridden to boolean and stays optional.
    assert_eq!(extended.fields[0].ty, ir::Type::String);
    assert_eq!(
        extended.fields[1].ty,
        ir::Type::Optional(Box::new(ir::Type::Bool))
    );
    assert_eq!(extended.fields[2].ty, ir::Type::Int64);
}

#[test]
fn one_of_with_type_constants_is_discriminated() {
    let lowering = lower_schemas(serde_json::json!({
        "File": {
            "type": "object",
            "properties": { "type": { "type": "string", "enum": ["file"] } }
        },
        "Folder": {
            "type": "object",
            "properties": { "type": { "type": "string", "enum": ["folder"] } }
        },
        "Item": {
            "oneOf": [
                { "$ref": "#/components/schemas/File" },
                { "$ref": "#/components/schemas/Folder" }
            ]
        }
    }))
    .unwrap();
    let ir::DeclKind::Union(union) = &find(&lowering.program, "Item").kind else {
        panic!("Item must be a union")
    };
    assert_eq!(union.discriminator.as_deref(), Some("type"));
    let values: Vec<Option<&str>> = union
        .variants
        .iter()
        .map(|v| v.discriminator_value.as_deref())
        .collect();
    assert_eq!(values, [Some("file"), Some("folder")]);
    assert_eq!(lowering.stats.discriminated_unions, 1);
}

#[test]
fn one_of_without_type_constants_is_structural() {
    let lowering = lower_schemas(serde_json::json!({
        "A": { "type": "object", "properties": { "a": { "type": "string" } } },
        "B": { "type": "object", "properties": { "b": { "type": "string" } } },
        "Either": {
            "oneOf": [
                { "$ref": "#/components/schemas/A" },
                { "$ref": "#/components/schemas/B" }
            ]
        }
    }))
    .unwrap();
    let ir::DeclKind::Union(union) = &find(&lowering.program, "Either").kind else {
        panic!("Either must be a union")
    };
    assert_eq!(union.discriminator, None);
    assert!(
        union
            .variants
            .iter()
            .all(|v| v.discriminator_value.is_none())
    );
    assert_eq!(lowering.stats.discriminated_unions, 0);
}

#[test]
fn enums_are_open_and_null_entries_encode_nullability() {
    let lowering = lower_schemas(serde_json::json!({
        "Thing": {
            "type": "object",
            "properties": {
                "role": { "type": "string", "enum": ["editor", "viewer", null] }
            },
            "required": ["role"]
        }
    }))
    .unwrap();
    let synthesized = find(&lowering.program, "ThingRole");
    let ir::DeclKind::Enum(decl) = &synthesized.kind else {
        panic!("ThingRole must be an enum")
    };
    assert_eq!(decl.values, ["editor", "viewer"]);
    assert_eq!(decl.extensibility, ir::Extensibility::Open);
    assert_eq!(lowering.stats.synthesized, 1);
}

#[test]
fn unresolved_refs_fail_loudly_with_a_location() {
    let err = lower_schemas(serde_json::json!({
        "Broken": {
            "type": "object",
            "properties": { "x": { "$ref": "#/components/schemas/DoesNotExist" } }
        }
    }))
    .unwrap_err();
    let IngestError::UnresolvedRef {
        location,
        reference,
        ..
    } = &err
    else {
        panic!("expected UnresolvedRef, got {err}")
    };
    assert_eq!(location, "components.schemas.Broken.properties.x");
    assert_eq!(reference, "#/components/schemas/DoesNotExist");
}

#[test]
fn synthesized_names_disambiguate_deterministically() {
    let lowering = lower_schemas(serde_json::json!({
        // The natural synthesized name for Widget.status is WidgetStatus —
        // which is already taken by a real schema.
        "WidgetStatus": { "type": "object", "properties": { "id": { "type": "string" } } },
        "Widget": {
            "type": "object",
            "properties": {
                "status": { "type": "string", "enum": ["on", "off"] }
            }
        }
    }))
    .unwrap();
    let ir::DeclKind::Enum(decl) = &find(&lowering.program, "WidgetStatus2").kind else {
        panic!("collision must produce WidgetStatus2 as an enum")
    };
    assert_eq!(decl.values, ["on", "off"]);
}

#[test]
fn versioned_documents_get_their_own_module() {
    let dir = std::env::temp_dir().join(format!(
        "gantry-lowering-versioned-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut files: Vec<PathBuf> = Vec::new();
    for version in ["2024.0", "2025.0"] {
        let spec = serde_json::json!({
            "openapi": "3.0.2",
            "info": { "title": "Box Platform API", "version": version },
            "paths": {},
            "components": { "schemas": {
                "Widget": { "type": "object", "properties": { "id": { "type": "string" } } }
            } }
        });
        let file = dir.join(format!("{version}.json"));
        std::fs::write(&file, serde_json::to_string(&spec).unwrap()).unwrap();
        files.push(file);
    }
    let lowering = gantry_spec::lower(&SpecSet::load(&files).unwrap()).unwrap();
    let modules: Vec<String> = lowering
        .program
        .decls
        .iter()
        .map(|d| {
            d.module
                .0
                .iter()
                .map(ir::Identifier::as_str)
                .collect::<Vec<_>>()
                .join("::")
        })
        .collect();
    assert_eq!(modules, ["schemas", "schemas::v2025_0"]);
}
