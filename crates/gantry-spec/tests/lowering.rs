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
    // The full tri-state (D-110): the key may be absent (not required)
    // AND the value may be an explicit null (annotation-part nullable) —
    // canonically Optional<Nullable<T>>. The type is the referenced decl,
    // not a synthesized wrapper.
    let ir::Type::Optional(nullable) = &parent.ty else {
        panic!("parent must be optional: {:?}", parent.ty)
    };
    let ir::Type::Nullable(inner) = &**nullable else {
        panic!("parent must be nullable: {nullable:?}")
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
    // The null entry marks the field nullable (D-110); required keeps it
    // from also being Optional.
    let thing = struct_decl(find(&lowering.program, "Thing"));
    assert!(
        matches!(&thing.fields[0].ty, ir::Type::Nullable(inner)
            if matches!(**inner, ir::Type::Decl(_))),
        "role must be Nullable(Decl): {:?}",
        thing.fields[0].ty
    );
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
fn identical_inline_shapes_dedupe_to_one_declaration() {
    // D-127: two inline objects with the same structure collapse to a single
    // synthesized decl; both fields reference it. The repeated `{id}` refs
    // that pepper Box request bodies become one shared type, not N copies.
    let lowering = lower_schemas(serde_json::json!({
        "Thing": {
            "type": "object",
            "properties": {
                "parent": { "type": "object", "properties": { "id": { "type": "string" } } },
                "child":  { "type": "object", "properties": { "id": { "type": "string" } } },
                "other":  { "type": "object", "properties": { "name": { "type": "string" } } }
            }
        }
    }))
    .unwrap();
    // `parent` and `child` share a shape → one synthesized struct; `other`
    // differs → a second. Two synthesized total, not three.
    assert_eq!(lowering.stats.synthesized, 2);
    let thing = struct_decl(find(&lowering.program, "Thing"));
    let decl_id = |field: &ir::Field| -> ir::DeclId {
        // Fields are optional (not required), so unwrap the Optional wrapper.
        let ty = if let ir::Type::Optional(inner) = &field.ty {
            inner.as_ref()
        } else {
            &field.ty
        };
        let ir::Type::Decl(id) = ty else {
            panic!("inline object must be a Decl: {ty:?}")
        };
        *id
    };
    let parent = decl_id(&thing.fields[0]);
    let child = decl_id(&thing.fields[1]);
    let other = decl_id(&thing.fields[2]);
    assert_eq!(parent, child, "identical inline shapes share one decl");
    assert_ne!(parent, other, "a different shape is a distinct decl");
}

#[test]
fn nested_inline_names_use_immediate_context_not_the_full_ancestry() {
    // Deeply-nested inline objects must be named from their immediate parent
    // + leaf (2 segments), never the accumulated path — the box-node-sdk
    // failure mode that produced 100+ char identifiers. Here the deepest
    // enum is `StaticConfigClassification`, NOT
    // `OuterDataStaticConfigClassification`.
    let lowering = lower_schemas(serde_json::json!({
        "Outer": {
            "type": "object",
            "properties": {
                "data": {
                    "type": "object",
                    "properties": {
                        "static_config": {
                            "type": "object",
                            "properties": {
                                "classification": { "type": "string", "enum": ["a", "b"] }
                            }
                        }
                    }
                }
            }
        }
    }))
    .unwrap();
    // Each level is its parent's leaf + its own leaf.
    let _ = find(&lowering.program, "OuterData");
    let _ = find(&lowering.program, "DataStaticConfig");
    let ir::DeclKind::Enum(decl) = &find(&lowering.program, "StaticConfigClassification").kind
    else {
        panic!("the deepest inline enum must be StaticConfigClassification")
    };
    assert_eq!(decl.values, ["a", "b"]);
    // The full-ancestry name must NOT exist.
    assert!(
        lowering
            .program
            .decls
            .iter()
            .all(|d| d.name.as_str() != "OuterDataStaticConfigClassification"),
        "immediate-context naming must not emit the full-ancestry name"
    );
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

/// Wrap `paths` (and optional shared parameters) in a minimal document.
fn lower_paths(
    paths: serde_json::Value,
    parameters: serde_json::Value,
) -> Result<Lowering, IngestError> {
    let spec = serde_json::json!({
        "openapi": "3.0.2",
        "info": { "title": "Box Platform API", "version": "2025.0" },
        "paths": paths,
        "components": { "schemas": {}, "parameters": parameters }
    });
    let dir = std::env::temp_dir().join(format!(
        "gantry-op-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("spec.json");
    std::fs::write(&file, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    gantry_spec::lower(&SpecSet::load(&[file])?)
}

#[test]
fn method_names_are_shortened_and_collisions_fall_back() {
    // D-126: drop the manager-tag echo and opaque id handles, map the HTTP
    // verb (`get`→`get`, `post`→`create`, …), a trailing id → `ById`, interior
    // ids drop. A one-vs-two-`{id}` collision falls back to keeping both ids.
    let lowering = lower_paths(
        serde_json::json!({
            "/files/{file_id}": {
                "get": { "operationId": "get_files_id", "x-box-tag": "files",
                    "parameters": [{"name":"file_id","in":"path","required":true,
                        "schema":{"type":"string"}}],
                    "responses": { "204": {} } }
            },
            "/files/{file_id}/copy": {
                "post": { "operationId": "post_files_id_copy", "x-box-tag": "files",
                    "parameters": [{"name":"file_id","in":"path","required":true,
                        "schema":{"type":"string"}}],
                    "responses": { "204": {} } }
            },
            "/metadata_taxonomies/{scope}/{id}": {
                "get": { "operationId": "get_metadata_taxonomies_id_id",
                    "x-box-tag": "metadata_taxonomies",
                    "parameters": [
                        {"name":"scope","in":"path","required":true,"schema":{"type":"string"}},
                        {"name":"id","in":"path","required":true,"schema":{"type":"string"}}],
                    "responses": { "204": {} } }
            },
            "/metadata_taxonomies/{id}": {
                "get": { "operationId": "get_metadata_taxonomies_id",
                    "x-box-tag": "metadata_taxonomies",
                    "parameters": [{"name":"id","in":"path","required":true,
                        "schema":{"type":"string"}}],
                    "responses": { "204": {} } }
            }
        }),
        serde_json::json!({}),
    )
    .unwrap();
    let names_for = |manager: &str| -> Vec<String> {
        lowering
            .program
            .operations
            .iter()
            .filter(|o| o.manager.as_str() == manager)
            .map(|o| o.name.as_str().to_string())
            .collect()
    };
    // files: GET by id → `get_by_id`; POST .../copy → `copy_by_id` (the
    // curated action verb `copy` leads, the HTTP verb drops, D-126).
    let files_ops = names_for("files");
    assert!(
        files_ops.contains(&"get_by_id".to_string()),
        "{files_ops:?}"
    );
    assert!(
        files_ops.contains(&"copy_by_id".to_string()),
        "{files_ops:?}"
    );
    // metadata_taxonomies: the one-id and two-id GETs both want `get_by_id`;
    // the collision keeps them distinct (which one keeps the short name
    // depends on spec order, so assert distinctness, not a specific pair).
    let tax = names_for("metadata_taxonomies");
    assert_eq!(tax.len(), 2, "{tax:?}");
    assert!(tax.contains(&"get_by_id".to_string()), "{tax:?}");
    assert!(
        tax[0] != tax[1],
        "the collision must stay distinct: {tax:?}"
    );
    assert!(
        tax.iter().all(|n| n.starts_with("get_by_id")),
        "both target-by-id names share the prefix: {tax:?}"
    );
}

#[test]
fn custom_action_verbs_lead_the_method_name() {
    // A curated action verb that trails the operationId leads the method name
    // (the HTTP verb drops); the `:` custom-method separator is split like
    // `_`, so `levels:append` yields the `append` action token.
    let lowering = lower_paths(
        serde_json::json!({
            "/ai/ask": {
                "post": { "operationId": "post_ai_ask", "x-box-tag": "ai",
                    "responses": { "204": {} } }
            },
            "/metadata_taxonomies/{scope}/{id}/levels": {
                "post": { "operationId": "post_metadata_taxonomies_id_id_levels:append",
                    "x-box-tag": "metadata_taxonomies",
                    "parameters": [
                        {"name":"scope","in":"path","required":true,"schema":{"type":"string"}},
                        {"name":"id","in":"path","required":true,"schema":{"type":"string"}}],
                    "responses": { "204": {} } }
            }
        }),
        serde_json::json!({}),
    )
    .unwrap();
    let name_of = |manager: &str| {
        lowering
            .program
            .operations
            .iter()
            .find(|o| o.manager.as_str() == manager)
            .map(|o| o.name.as_str().to_string())
            .unwrap()
    };
    // `post_ai_ask` → the action `ask` leads, no HTTP verb: `ask`.
    assert_eq!(name_of("ai"), "ask");
    // `..._levels:append` → action `append` leads, interior ids drop, the
    // `levels` sub-resource stays: `append_levels`.
    assert_eq!(name_of("metadata_taxonomies"), "append_levels");
}

#[test]
fn variation_and_version_suffix_become_structure() {
    let lowering = lower_paths(
        serde_json::json!({
            "/oauth2/token": {
                "post": {
                    "operationId": "post_oauth2_token_v2025.0", "x-box-tag": "authorization",
                    "responses": { "200": { "content": { "application/json": {} } } }
                }
            },
            "/oauth2/token#refresh": {
                "post": {
                    "operationId": "post_oauth2_token#refresh", "x-box-tag": "authorization",
                    "servers": [ { "url": "https://api.box.com" } ],
                    "responses": { "204": {} }
                }
            }
        }),
        serde_json::json!({}),
    )
    .unwrap();
    let ops = &lowering.program.operations;
    assert_eq!(ops.len(), 2);
    // `_v2025.0` stripped (the document already carries the version);
    // `#refresh` split into structured variation data. The method name maps
    // the HTTP verb to a semantic one (`post`→`create`, D-126); the two share
    // a name but differ by variation, so they don't collide.
    assert_eq!(ops[0].name.as_str(), "create_oauth2_token");
    assert_eq!(ops[0].variation, None);
    assert_eq!(ops[0].base_url, ir::BaseUrl::Api);
    assert_eq!(ops[1].name.as_str(), "create_oauth2_token");
    assert_eq!(ops[1].variation.as_ref().unwrap().as_str(), "refresh");
    assert_eq!(ops[1].base_url, ir::BaseUrl::ApiRoot);
    // The `#` fragment is not part of the request path.
    assert_eq!(
        ops[1].path,
        vec![
            ir::PathSegment::Literal("oauth2".into()),
            ir::PathSegment::Literal("token".into())
        ]
    );
    assert_eq!(ops[1].response, ir::ResponseShape::None);
}

#[test]
fn params_resolve_including_component_refs() {
    let lowering = lower_paths(
        serde_json::json!({
            "/files/{file_id}": {
                "get": {
                    "operationId": "get_files_id", "x-box-tag": "files",
                    "parameters": [
                        { "name": "file_id", "in": "path", "required": true,
                          "schema": { "type": "string" } },
                        { "name": "fields", "in": "query",
                          "schema": { "type": "array", "items": { "type": "string" } } },
                        { "$ref": "#/components/parameters/boxapi" }
                    ],
                    "responses": { "200": { "content": { "application/json": {
                        "schema": { "type": "object", "properties": { "id": { "type": "string" } } }
                    } } } }
                }
            }
        }),
        serde_json::json!({
            "boxapi": { "name": "boxapi", "in": "header", "schema": { "type": "string" } }
        }),
    )
    .unwrap();
    let op = &lowering.program.operations[0];
    let kinds: Vec<(&str, ir::ParamLocation)> = op
        .params
        .iter()
        .map(|p| (p.wire_name.as_str(), p.location))
        .collect();
    assert_eq!(
        kinds,
        [
            ("file_id", ir::ParamLocation::Path),
            ("fields", ir::ParamLocation::Query),
            ("boxapi", ir::ParamLocation::Header)
        ]
    );
    // Path params are bare; optional query params wrap in Optional.
    assert_eq!(op.params[0].ty, ir::Type::String);
    assert_eq!(
        op.params[1].ty,
        ir::Type::Optional(Box::new(ir::Type::List(Box::new(ir::Type::String))))
    );
    assert_eq!(
        op.path,
        vec![
            ir::PathSegment::Literal("files".into()),
            ir::PathSegment::Parameter(ir::Identifier::new("file_id").unwrap())
        ]
    );
}

#[test]
fn composite_path_segments_parse_into_parts() {
    let lowering = lower_paths(
        serde_json::json!({
            "/files/{file_id}/thumbnail.{extension}": {
                "get": {
                    "operationId": "get_files_id_thumbnail_id", "x-box-tag": "files",
                    "parameters": [
                        { "name": "file_id", "in": "path", "required": true,
                          "schema": { "type": "string" } },
                        { "name": "extension", "in": "path", "required": true,
                          "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": { "content": { "image/png": {} } },
                        "202": {},
                        "302": {}
                    }
                }
            }
        }),
        serde_json::json!({}),
    )
    .unwrap();
    let op = &lowering.program.operations[0];
    assert_eq!(
        op.path[2],
        ir::PathSegment::Composite(vec![
            ir::PathPart::Literal("thumbnail.".into()),
            ir::PathPart::Parameter(ir::Identifier::new("extension").unwrap())
        ])
    );
    // 200 image/png beats the content-free 202/302: binary download.
    assert_eq!(op.response, ir::ResponseShape::Binary);
}

#[test]
fn redirect_only_success_is_a_redirect() {
    let lowering = lower_paths(
        serde_json::json!({
            "/gone": {
                "get": {
                    "operationId": "get_gone", "x-box-tag": "files",
                    "responses": { "302": {} }
                }
            }
        }),
        serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(
        lowering.program.operations[0].response,
        ir::ResponseShape::Redirect
    );
}

#[test]
fn undeclared_path_parameter_fails_loudly() {
    let err = lower_paths(
        serde_json::json!({
            "/files/{file_id}": {
                "get": {
                    "operationId": "get_files_id", "x-box-tag": "files",
                    "responses": { "204": {} }
                }
            }
        }),
        serde_json::json!({}),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("{file_id}") && err.to_string().contains("no declaration"),
        "{err}"
    );
}

#[test]
fn mismatched_version_marker_fails_loudly() {
    let err = lower_paths(
        serde_json::json!({
            "/widgets": {
                "get": {
                    "operationId": "get_widgets_v2099.0", "x-box-tag": "widgets",
                    "responses": { "204": {} }
                }
            }
        }),
        serde_json::json!({}),
    )
    .unwrap_err();
    assert!(err.to_string().contains("version marker"), "{err}");
}
