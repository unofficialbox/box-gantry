//! Structural + determinism fixtures for the Apex model layer.
//!
//! No Apex toolchain runs here, so — until the scratch-org gate (VR-1.3) —
//! these assert the shape each lowering rule produces and that the whole
//! real spec lowers deterministically within the platform's identifier
//! limit. The case list mirrors the Go per-node fixtures (VR-2), retargeted
//! to Apex semantics.

use std::path::PathBuf;

use gantry_backend_apex::{BuildInfo, GeneratedFile, generate, generate_models};

const CLASSES: &str = "force-app/main/default/classes";
use gantry_ir as ir;
use gantry_manifest::{ModuleSystem, apex};
use gantry_spec::SpecSet;

fn ident(name: &str) -> ir::Identifier {
    ir::Identifier::new(name).unwrap()
}

fn schemas() -> ir::ModulePath {
    ir::ModulePath(vec![ident("schemas")])
}

fn field(wire: &str, ty: ir::Type) -> ir::Field {
    ir::Field {
        name: ident(wire),
        wire_name: wire.to_string(),
        ty,
    }
}

fn struct_decl(name: &str, fields: Vec<ir::Field>) -> ir::Decl {
    ir::Decl {
        name: ident(name),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Struct(ir::StructDecl { fields }),
    }
}

/// Render a program's model classes with the Apex manifest.
fn render(decls: Vec<ir::Decl>) -> Vec<GeneratedFile> {
    let mut program = ir::Program::default();
    for decl in decls {
        program.add(decl);
    }
    let program = Box::leak(Box::new(program));
    let analysis = gantry_sema::analyze(program).expect("fixture must analyze");
    generate_models(&analysis, &apex())
}

fn only(decls: Vec<ir::Decl>) -> String {
    let files = render(decls);
    assert_eq!(files.len(), 1, "expected exactly one class file");
    files.into_iter().next().unwrap().content
}

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find:\n  {needle}\nin:\n{haystack}"
    );
}

// --- scalar + container mappings -----------------------------------------

#[test]
fn scalars_map_to_apex_types() {
    let go = only(vec![struct_decl(
        "S",
        vec![
            field("b", ir::Type::Bool),
            field("i", ir::Type::Int64),
            field("f", ir::Type::Float64),
            field("s", ir::Type::String),
            field("d", ir::Type::Date),
            field("dt", ir::Type::DateTime),
            field("bin", ir::Type::Binary),
            field("j", ir::Type::JsonValue),
        ],
    )]);
    assert_contains(&go, "public Boolean b; // wire: b");
    assert_contains(&go, "public Long i; // wire: i");
    assert_contains(&go, "public Double f; // wire: f");
    assert_contains(&go, "public String s; // wire: s");
    assert_contains(&go, "public Date d; // wire: d");
    assert_contains(&go, "public Datetime dt; // wire: dt");
    assert_contains(&go, "public Blob bin; // wire: bin"); // buffered platform
    assert_contains(&go, "public Object j; // wire: j");
}

#[test]
fn tri_state_wrappers_erase_at_the_type_level() {
    // Optional<T>, Nullable<T>, and Optional<Nullable<T>> all render as the
    // bare Apex type — every reference is nullable; the serializer carries
    // absent-vs-null (D-110), not the type.
    let opt = ir::Type::Optional(Box::new(ir::Type::String));
    let null = ir::Type::Nullable(Box::new(ir::Type::String));
    let tri = ir::Type::Optional(Box::new(ir::Type::Nullable(Box::new(ir::Type::String))));
    let go = only(vec![struct_decl(
        "S",
        vec![field("a", opt), field("b", null), field("c", tri)],
    )]);
    assert_contains(&go, "public String a;");
    assert_contains(&go, "public String b;");
    assert_contains(&go, "public String c;");
}

#[test]
fn collections_use_builtin_generics() {
    let go = only(vec![struct_decl(
        "S",
        vec![
            field("xs", ir::Type::List(Box::new(ir::Type::Int64))),
            field("m", ir::Type::Map(Box::new(ir::Type::String))),
        ],
    )]);
    assert_contains(&go, "public List<Long> xs;");
    assert_contains(&go, "public Map<String, String> m;");
}

#[test]
fn reserved_field_names_are_mangled_wire_name_preserved() {
    // `limit` and `group` are Apex reserved words; the field gains a `_r`
    // suffix (Apex forbids a trailing `_`), but the wire name (the JSON key)
    // is untouched.
    let go = only(vec![struct_decl(
        "S",
        vec![
            field("limit", ir::Type::Int64),
            field("group", ir::Type::String),
        ],
    )]);
    assert_contains(&go, "public Long limit_r; // wire: limit");
    assert_contains(&go, "public String group_r; // wire: group");
}

#[test]
fn field_names_are_shaped_into_valid_apex_identifiers() {
    // Box wire names break Apex's identifier rules: runs of `__` (metadata
    // keys like `Box__Security__Classification__Key`) and leading digits are
    // both rejected. The identifier is folded to a legal shape; the wire
    // name (JSON key) is preserved verbatim in the trailing comment.
    let go = only(vec![struct_decl(
        "S",
        vec![
            field("Box__Security__Classification__Key", ir::Type::String),
            field("2fa_enabled", ir::Type::Bool),
        ],
    )]);
    assert_contains(
        &go,
        "public String Box_Security_Classification_Key; // wire: Box__Security__Classification__Key",
    );
    assert_contains(&go, "public Boolean x2fa_enabled; // wire: 2fa_enabled");
}

// --- field ↔ wire serialization remap ------------------------------------

#[test]
fn a_struct_with_a_renamed_field_gets_key_remap_methods() {
    // `limit` → `limit_r` can't round-trip through native `JSON.deserialize`
    // (which matches on the field name), so the struct is "affected" and gains
    // `normalizeKeys` (wire → Apex) and `denormalizeKeys` (Apex → wire) that
    // rename the key on the untyped JSON tree.
    let go = only(vec![struct_decl(
        "S",
        vec![field("limit", ir::Type::Int64)],
    )]);
    assert_contains(
        &go,
        "public static Map<String, Object> normalizeKeys(Map<String, Object> raw) {",
    );
    assert_contains(&go, "if (raw.containsKey('limit')) {");
    assert_contains(&go, "raw.put('limit_r', v);");
    assert_contains(
        &go,
        "public static Map<String, Object> denormalizeKeys(Map<String, Object> raw) {",
    );
    assert_contains(&go, "if (raw.containsKey('limit_r')) {");
    assert_contains(&go, "raw.put('limit', v);");
}

/// Render the full SDK for a program with a single struct optionally used as a
/// POST request body, returning the struct's class source.
fn struct_with_optional_body(fields: Vec<ir::Field>, as_body: bool) -> String {
    let mut program = ir::Program::default();
    let s = program.add(struct_decl("S", fields));
    if as_body {
        program.operations.push(ir::Operation {
            name: ident("post_s"),
            variation: None,
            manager: ident("things"),
            api_version: None,
            method: ir::HttpMethod::Post,
            base_url: ir::BaseUrl::Api,
            path: vec![ir::PathSegment::Literal("things".into())],
            params: vec![],
            request: Some(ir::RequestBody {
                media: ir::RequestMedia::Json,
                ty: ir::Type::Decl(s),
            }),
            response: ir::ResponseShape::None,
            deprecated: false,
        });
    }
    let program = Box::leak(Box::new(program));
    let analysis = gantry_sema::analyze(program).expect("fixture must analyze");
    let files = generate(&analysis, &apex(), &BuildInfo::new("testfp"));
    files
        .into_iter()
        .find(|f| f.path == format!("{CLASSES}/S.cls"))
        .expect("S class")
        .content
}

#[test]
fn a_nullable_field_on_a_body_struct_gets_explicit_null_control() {
    // `note` is `Nullable` (the wire value may be an explicit `null`) and `S` is
    // a request body, so `S` gains a `fieldsToNull` control field and its
    // `denormalizeKeys` injects an explicit `null` for `note` when listed, then
    // drops the control key. The absent-only `id` field gets no injection.
    let s = struct_with_optional_body(
        vec![
            field("id", ir::Type::String),
            field("note", ir::Type::Nullable(Box::new(ir::Type::String))),
        ],
        true,
    );
    assert_contains(&s, "public Set<String> fieldsToNull = new Set<String>();");
    assert_contains(&s, "Object toNull = raw.remove('fieldsToNull');");
    assert_contains(&s, "if (nfName == 'note') raw.put('note', null);");
    // An absent-only field is never force-nulled.
    assert!(
        !s.contains("raw.put('id', null)"),
        "a non-nullable field must not be injectable as null:\n{s}"
    );
}

#[test]
fn a_nullable_field_off_the_request_path_gets_no_explicit_null_control() {
    // The same struct with a `Nullable` field, but never used as a request body,
    // has no explicit-null concern — no `fieldsToNull`, no injection.
    let s = struct_with_optional_body(
        vec![field(
            "note",
            ir::Type::Nullable(Box::new(ir::Type::String)),
        )],
        false,
    );
    assert!(
        !s.contains("fieldsToNull"),
        "a response-only struct must not carry explicit-null control:\n{s}"
    );
}

#[test]
fn the_null_control_field_dodges_a_colliding_schema_field() {
    // A real schema field named `fieldsToNull` must not be shadowed by the
    // control field (Apex identifiers are case-insensitive) nor have its value
    // consumed by the control-key removal — the control name is disambiguated.
    let s = struct_with_optional_body(
        vec![
            field("fieldsToNull", ir::Type::String),
            field("note", ir::Type::Nullable(Box::new(ir::Type::String))),
        ],
        true,
    );
    assert_contains(&s, "public String fieldsToNull; // wire: fieldsToNull");
    assert_contains(&s, "public Set<String> fieldsToNull2 = new Set<String>();");
    // The removal targets the minted control name, not the real field.
    assert_contains(&s, "Object toNull = raw.remove('fieldsToNull2');");
    assert!(
        !s.contains("raw.remove('fieldsToNull')"),
        "the real `fieldsToNull` wire field must not be consumed as control:\n{s}"
    );
}

#[test]
fn a_clean_struct_has_no_remap_methods() {
    // Every field's Apex name equals its wire key, so native (de)serialization
    // already round-trips — no remap code is generated (872 of 991 classes).
    let go = only(vec![struct_decl(
        "S",
        vec![
            field("id", ir::Type::String),
            field("name", ir::Type::String),
        ],
    )]);
    assert!(
        !go.contains("normalizeKeys"),
        "a clean struct must not carry remap methods:\n{go}"
    );
}

#[test]
fn remap_recurses_into_affected_children_only() {
    // A parent that is itself clean but holds a list of an affected child is
    // still affected (native deserialize would drop the child's keys), and its
    // remap descends into the list, delegating to the child's remap.
    let child = struct_decl("Child", vec![field("limit", ir::Type::Int64)]);
    let parent = struct_decl(
        "Parent",
        vec![
            field("id", ir::Type::String),
            field(
                "kids",
                ir::Type::List(Box::new(ir::Type::Decl(ir::DeclId(0)))),
            ),
        ],
    );
    let files = render(vec![child, parent]);
    let parent_src = &files
        .iter()
        .find(|f| f.path.ends_with("/Parent.cls"))
        .expect("Parent class")
        .content;
    // The clean `id` field is not remapped; only the affected `kids` list is,
    // and it delegates to the child's remap per element.
    assert_contains(parent_src, "if (raw.containsKey('kids')) {");
    assert_contains(
        parent_src,
        "wElem0 = Child.normalizeKeys((Map<String, Object>) wElem0);",
    );
    assert!(
        !parent_src.contains("raw.remove('id')"),
        "a matching field must not be remapped:\n{parent_src}"
    );
}

// --- Object-field deserialization (D-140) ---------------------------------

#[test]
fn an_object_leaf_field_gets_a_deserialize_builder() {
    // A `JsonValue` field erases to `Object`, which native `JSON.deserialize`
    // can't populate. The struct gets a `deserialize(Object)` that detaches the
    // field, deserializes the typed shell, and reattaches the raw value (D-140).
    let go = only(vec![struct_decl(
        "S",
        vec![
            field("id", ir::Type::String),
            field("data", ir::Type::JsonValue),
        ],
    )]);
    assert_contains(&go, "public static S deserialize(Object rawInput) {");
    assert_contains(&go, "Object d_data = raw.remove('data');");
    assert_contains(
        &go,
        "S result = (S) JSON.deserialize(JSON.serialize(raw), S.class);",
    );
    assert_contains(&go, "result.data = d_data;");
    // A clean scalar field stays in the typed shell — never detached.
    assert!(
        !go.contains("d_id"),
        "a clean field must not be detached:\n{go}"
    );
}

#[test]
fn a_list_of_object_leaves_reattaches_as_a_raw_list() {
    // `List<Object>` is reattached whole (the elements are inspected at the call
    // site); no per-element work.
    let go = only(vec![struct_decl(
        "S",
        vec![field(
            "items",
            ir::Type::List(Box::new(ir::Type::JsonValue)),
        )],
    )]);
    assert_contains(&go, "result.items = (List<Object>) d_items;");
}

#[test]
fn a_nested_object_bearing_struct_deserializes_through_the_child() {
    // A parent that holds an object-bearing child is itself object-bearing, and
    // its builder recurses into the child's `deserialize` for that field.
    let child = struct_decl("Child", vec![field("data", ir::Type::JsonValue)]);
    let parent = struct_decl(
        "Parent",
        vec![
            field("id", ir::Type::String),
            field("child", ir::Type::Decl(ir::DeclId(0))),
        ],
    );
    let files = render(vec![child, parent]);
    let parent_src = &files
        .iter()
        .find(|f| f.path.ends_with("/Parent.cls"))
        .expect("Parent class")
        .content;
    assert_contains(parent_src, "Object d_child = raw.remove('child');");
    assert_contains(parent_src, "result.child = Child.deserialize(d_child);");
}

// --- enums / unions / aliases --------------------------------------------

#[test]
fn open_enum_is_a_string_class_with_constants() {
    let go = only(vec![ir::Decl {
        name: ident("ItemType"),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Enum(ir::EnumDecl {
            values: vec!["file".into(), "web_link".into()],
            extensibility: ir::Extensibility::Open,
        }),
    }]);
    assert_contains(&go, "public class ItemType {");
    // A constants namespace — no per-instance `value`; enum *fields* are
    // typed `String` (see the field-representation test).
    assert!(
        !go.contains("public String value;"),
        "the enum class must be constants-only"
    );
    // Constant names are the shared PascalCase identifier form; the wire
    // value is the string literal.
    assert_contains(&go, "public static final String File = 'file';");
    assert_contains(&go, "public static final String WebLink = 'web_link';");
}

#[test]
fn enum_constants_that_are_reserved_words_are_mangled() {
    // A wire value whose identifier form is an Apex reserved word (`asc`,
    // `date`, `group`) can't be a bare constant name — the platform rejects
    // it ("Identifier name is reserved" / "Unexpected token"). It gains the
    // same `_r` suffix as reserved fields; the wire literal is untouched.
    let go = only(vec![ir::Decl {
        name: ident("Sort"),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Enum(ir::EnumDecl {
            values: vec!["asc".into(), "date".into(), "group".into()],
            extensibility: ir::Extensibility::Open,
        }),
    }]);
    assert_contains(&go, "public static final String Asc_r = 'asc';");
    assert_contains(&go, "public static final String Date_r = 'date';");
    assert_contains(&go, "public static final String Group_r = 'group';");
}

#[test]
fn enum_constant_dedup_is_case_insensitive() {
    // Apex identifiers are case-insensitive, so `ASC` and `asc` both escape
    // to `Asc_r` and would be a duplicate field. The dedup must key on the
    // lowercased name and disambiguate the second with a numeric suffix.
    let go = only(vec![ir::Decl {
        name: ident("Direction"),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Enum(ir::EnumDecl {
            values: vec!["ASC".into(), "asc".into()],
            extensibility: ir::Extensibility::Open,
        }),
    }]);
    assert_contains(&go, "public static final String ASC_r = 'ASC';");
    assert_contains(&go, "public static final String Asc_r2 = 'asc';");
}

#[test]
fn a_schema_named_for_a_reserved_type_is_a_safe_class_name() {
    // `Group` is a reserved Apex type; a schema of that name can't be a bare
    // class. The class identifier gets the same `_r` escape as a field.
    let go = only(vec![struct_decl(
        "Group",
        vec![field("id", ir::Type::String)],
    )]);
    assert_contains(&go, "public class Group_r {");
}

#[test]
fn enum_and_union_typed_fields_use_native_json_types() {
    // A field typed as an open enum is a `String`; as a union it is an
    // `Object` (raw map for the caller to dispatch) — so a struct
    // round-trips through JSON.deserialize natively.
    let files = render(vec![
        ir::Decl {
            name: ident("Role"),
            module: schemas(),
            api_version: None,
            kind: ir::DeclKind::Enum(ir::EnumDecl {
                values: vec!["admin".into()],
                extensibility: ir::Extensibility::Open,
            }),
        },
        struct_decl("Sub", vec![field("id", ir::Type::String)]),
        ir::Decl {
            name: ident("Payload"),
            module: schemas(),
            api_version: None,
            kind: ir::DeclKind::Union(ir::UnionDecl {
                discriminator: Some("type".into()),
                variants: vec![ir::UnionVariant {
                    discriminator_value: Some("sub".into()),
                    ty: ir::Type::Decl(ir::DeclId(1)),
                }],
                extensibility: ir::Extensibility::Open,
            }),
        },
        struct_decl(
            "Holder",
            vec![
                field("role", ir::Type::Decl(ir::DeclId(0))),
                field("payload", ir::Type::Decl(ir::DeclId(2))),
                field(
                    "roles",
                    ir::Type::List(Box::new(ir::Type::Decl(ir::DeclId(0)))),
                ),
                field("sub", ir::Type::Decl(ir::DeclId(1))), // a struct ref stays typed
            ],
        ),
    ]);
    let holder = files
        .iter()
        .find(|f| f.path == format!("{CLASSES}/Holder.cls"))
        .unwrap();
    assert_contains(&holder.content, "public String role; // wire: role");
    assert_contains(&holder.content, "public Object payload; // wire: payload");
    assert_contains(&holder.content, "public List<String> roles; // wire: roles");
    assert_contains(&holder.content, "public Sub sub; // wire: sub");
}

#[test]
fn discriminated_union_gets_deserialize_untyped_dispatch() {
    let files = render(vec![
        struct_decl("File", vec![field("id", ir::Type::String)]),
        struct_decl("Folder", vec![field("id", ir::Type::String)]),
        ir::Decl {
            name: ident("Item"),
            module: schemas(),
            api_version: None,
            kind: ir::DeclKind::Union(ir::UnionDecl {
                discriminator: Some("type".into()),
                variants: vec![
                    ir::UnionVariant {
                        discriminator_value: Some("file".into()),
                        ty: ir::Type::Decl(ir::DeclId(0)),
                    },
                    ir::UnionVariant {
                        discriminator_value: Some("folder".into()),
                        ty: ir::Type::Decl(ir::DeclId(1)),
                    },
                ],
                extensibility: ir::Extensibility::Open,
            }),
        },
    ]);
    let union = files
        .iter()
        .find(|f| f.path == format!("{CLASSES}/Item.cls"))
        .expect("union class");
    assert_contains(
        &union.content,
        "public static Object parse(Map<String, Object> untyped) {",
    );
    assert_contains(&union.content, "String tag = (String) untyped.get('type');");
    assert_contains(
        &union.content,
        "if (tag == 'file') return (File) JSON.deserialize(JSON.serialize(untyped), File.class);",
    );
    // Open union: unknown tag round-trips as the raw map.
    assert_contains(&union.content, "return untyped;");
}

#[test]
fn a_union_variant_with_renamed_keys_is_normalized_before_dispatch() {
    // The `file` variant is a clean struct; the `folder` variant has a renamed
    // field (`limit`→`limit_r`), so it must be `normalizeKeys`'d before native
    // deserialize — otherwise the union dispatch would drop its keys (D-132).
    let files = render(vec![
        struct_decl("File", vec![field("id", ir::Type::String)]),
        struct_decl("Folder", vec![field("limit", ir::Type::Int64)]),
        ir::Decl {
            name: ident("Item"),
            module: schemas(),
            api_version: None,
            kind: ir::DeclKind::Union(ir::UnionDecl {
                discriminator: Some("type".into()),
                variants: vec![
                    ir::UnionVariant {
                        discriminator_value: Some("file".into()),
                        ty: ir::Type::Decl(ir::DeclId(0)),
                    },
                    ir::UnionVariant {
                        discriminator_value: Some("folder".into()),
                        ty: ir::Type::Decl(ir::DeclId(1)),
                    },
                ],
                extensibility: ir::Extensibility::Open,
            }),
        },
    ]);
    let union = &files
        .iter()
        .find(|f| f.path == format!("{CLASSES}/Item.cls"))
        .expect("union class")
        .content;
    // Clean variant: dispatched on the raw map.
    assert_contains(
        union,
        "if (tag == 'file') return (File) JSON.deserialize(JSON.serialize(untyped), File.class);",
    );
    // Affected variant: keys normalized first.
    assert_contains(
        union,
        "if (tag == 'folder') return (Folder) JSON.deserialize(JSON.serialize(Folder.normalizeKeys(untyped)), Folder.class);",
    );
}

#[test]
fn structural_union_erases_to_object() {
    let go = only(vec![ir::Decl {
        name: ident("AnyValue"),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Union(ir::UnionDecl {
            discriminator: None,
            variants: vec![ir::UnionVariant {
                discriminator_value: None,
                ty: ir::Type::String,
            }],
            extensibility: ir::Extensibility::Open,
        }),
    }]);
    assert_contains(&go, "public class AnyValue {");
    assert_contains(&go, "public Object value;");
}

#[test]
fn alias_emits_no_class_and_resolves_through() {
    // An alias produces no file; a field referencing it renders as the
    // target type.
    let files = render(vec![
        ir::Decl {
            name: ident("Token"),
            module: schemas(),
            api_version: None,
            kind: ir::DeclKind::Alias(ir::Type::String),
        },
        struct_decl(
            "Holder",
            vec![field("token", ir::Type::Decl(ir::DeclId(0)))],
        ),
    ]);
    assert!(
        !files
            .iter()
            .any(|f| f.path == format!("{CLASSES}/Token.cls")),
        "alias must not emit a class"
    );
    let holder = files
        .iter()
        .find(|f| f.path == format!("{CLASSES}/Holder.cls"))
        .unwrap();
    assert_contains(&holder.content, "public String token; // wire: token");
}

// --- the whole real spec -------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

fn real_spec_files() -> Vec<GeneratedFile> {
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
    generate_models(&analysis, &apex())
}

#[test]
fn the_real_spec_lowers_to_apex_classes() {
    let files = real_spec_files();

    // Every struct/union/enum decl becomes one class; aliases (2 in the
    // real spec) do not. After structural dedupe (D-127), the version merge
    // (D-190), and stripping the box-version header enums (D-191) the spec
    // lowers to 877 decls − 2 aliases = 875 classes. Pinned so the count only
    // moves deliberately with the spec (VR-6 lineage).
    assert_eq!(files.len(), 875, "expected one class per non-alias decl");

    // Every class name obeys the platform identifier limit (TR-Apex.1) and
    // is globally unique (flat namespace), and every file carries the
    // do-not-edit header (FR-6.3).
    let ModuleSystem::Flat { identifier_limit } = apex().modules else {
        unreachable!()
    };
    let mut class_names = std::collections::HashSet::new();
    for file in &files {
        let name = file
            .path
            .strip_prefix(&format!("{CLASSES}/"))
            .and_then(|p| p.strip_suffix(".cls"))
            .expect("class path shape");
        assert!(
            name.len() <= identifier_limit as usize,
            "identifier {name:?} exceeds the {identifier_limit}-char limit"
        );
        assert!(class_names.insert(name), "duplicate class name {name:?}");
        assert!(
            file.content.starts_with("// Code generated by box-gantry "),
            "{} lacks the header",
            file.path
        );
    }

    // The generated deserializeUntyped dispatch appears for every
    // discriminated union (23 in the real spec — matches the IR stats).
    let dispatch = files
        .iter()
        .filter(|f| {
            f.content
                .contains("public static Object parse(Map<String, Object> untyped)")
        })
        .count();
    assert_eq!(
        dispatch, 23,
        "expected one dispatch per discriminated union"
    );
}

#[test]
fn generation_is_deterministic() {
    let once = real_spec_files();
    let twice = real_spec_files();
    assert_eq!(once.len(), twice.len());
    for (a, b) in once.iter().zip(&twice) {
        assert_eq!(a.path, b.path);
        assert_eq!(a.content, b.content, "nondeterministic: {}", a.path);
    }
}

#[test]
fn the_generated_tree_is_a_deployable_sfdx_project() {
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
    let files = generate(&analysis, &apex(), &BuildInfo::new("testfp"));

    // The project manifest exists and is valid JSON naming the source dir.
    let project = files
        .iter()
        .find(|f| f.path == "sfdx-project.json")
        .expect("sfdx-project.json");
    let parsed: serde_json::Value = serde_json::from_str(&project.content).expect("valid JSON");
    assert_eq!(parsed["packageDirectories"][0]["path"], "force-app");
    // NF-8 (D-142): the namespace + unlocked-package definition ride on the
    // generated project so `sf package version create` builds the ship artifact.
    assert_eq!(parsed["namespace"], "unbox");
    assert_eq!(
        parsed["packageDirectories"][0]["package"],
        "Unbox Salesforce SDK"
    );
    // `.NEXT` auto-increments the build segment; major.minor is set at release
    // from the FR-9 spec-diff.
    assert_eq!(
        parsed["packageDirectories"][0]["versionNumber"],
        "0.1.0.NEXT"
    );
    // Emitted as an empty object: the generated project is overwritten on every
    // run, so it carries no persisted alias — the durable handle is the `0Ho…`
    // package id supplied to the release build (D-142).
    assert_eq!(parsed["packageAliases"], serde_json::json!({}));

    // Every class has exactly one matching -meta.xml sidecar (source
    // format), so the tree deploys as-is. After dedupe (D-127): 898 model
    // classes + 85 managers + the Box client + 3 contract stubs + 12
    // hand-written runtime classes (the caching base, CCG + JWT providers, the
    // chunked-upload helper, and their tests — D-134/D-135/D-136 — plus the
    // HTTP client's own HttpCalloutMock test) = 999
    // (pagination adds no classes — the base method's envelope is the page,
    // D-131). Plus the generated `@isTest` suite for the 75% coverage gate: 85
    // per-manager tests + the mock client + the unions test = 87, the
    // `BoxBuildInfo` provenance class (NF-7, D-141) = 1, and the model wire-hook
    // suite = 4 (D-146): the 220 structs that carry a generated wire static
    // (`normalizeKeys`/`denormalizeKeys`/`deserialize`) exercised with populated
    // inputs, chunked ≤ 60 structs per class so no method overruns Apex's
    // compiled-size limit. The version merge (D-190) drops 21 model classes and
    // the box-version strip (D-191) drops 2 more. 976 + 87 + 1 + 4 = 1068 total.
    let classes: Vec<&str> = files
        .iter()
        .filter(|f| f.path.ends_with(".cls"))
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(
        classes.len(),
        976 + 87 + 1 + 4,
        "models + managers + client + stubs + runtime + @isTest suite + BoxBuildInfo + wire-hook suite"
    );
    // The generated test suite ships with the deployable tree.
    for test in ["BoxCalloutMock", "BoxFilesTest", "BoxUnionsTest"] {
        assert!(
            classes
                .iter()
                .any(|c| *c == format!("force-app/main/default/classes/{test}.cls")),
            "missing test class {test}"
        );
    }
    // The hand-written runtime ships inside the deployable tree (Apex is one
    // flat namespace), behind the generated `BoxClient` contract.
    for runtime in [
        "BoxHttpClient",
        "BoxTokenProvider",
        "BoxDeveloperTokenProvider",
        "BoxCcgTokenProvider",
        "BoxJwtTokenProvider",
        "BoxChunkedUpload",
        "BoxApiException",
    ] {
        assert!(
            classes
                .iter()
                .any(|c| *c == format!("force-app/main/default/classes/{runtime}.cls")),
            "missing runtime class {runtime}"
        );
    }
    let mut names = std::collections::HashSet::new();
    for class in &classes {
        let meta = format!("{class}-meta.xml");
        assert!(
            files.iter().any(|f| f.path == meta),
            "class {class} has no -meta.xml sidecar"
        );
        assert!(class.starts_with("force-app/main/default/classes/"));
        // Flat namespace: every top-level class name is globally unique,
        // including managers/client/stubs vs model classes.
        assert!(names.insert(*class), "duplicate class {class}");
    }
    // The standard SFDX scaffolding ships with the project.
    for scaffold in [
        "sfdx-project.json",
        "config/project-scratch-def.json",
        ".forceignore",
        "manifest/package.xml",
        "README.md",
    ] {
        assert!(
            files.iter().any(|f| f.path == scaffold),
            "missing scaffolding file {scaffold}"
        );
    }
    // A Remote Site Setting per Box host — without these the deployed SDK can't
    // make a single callout (Apex blocks unlisted endpoints).
    for host in [
        "Box_api_box_com",
        "Box_upload_box_com",
        "Box_account_box_com",
        "Box_dl_boxcloud_com",
    ] {
        // Source-format suffix `.remoteSite-meta.xml` (SDR registry canonical),
        // not the MDAPI `.remoteSiteSetting` — required by 2GP packaging (D-144).
        let path = format!("force-app/main/default/remoteSiteSettings/{host}.remoteSite-meta.xml");
        assert!(
            files.iter().any(|f| f.path == path),
            "missing remote site setting {host}"
        );
    }
    // One Markdown doc per endpoint, plus a per-manager index and the top
    // index — none of it under the package directory, so it never deploys.
    let docs: Vec<&str> = files
        .iter()
        .filter(|f| f.path.starts_with("docs/") || f.path == "docs/README.md")
        .map(|f| f.path.as_str())
        .collect();
    assert!(
        docs.contains(&"docs/README.md"),
        "the docs index is missing"
    );
    assert!(
        docs.iter().all(|d| d.ends_with(".md")),
        "docs must be Markdown only"
    );
    // 336 endpoint pages + 85 manager indexes + 1 top index + 3 topic guides
    // (auth/pagination/errors) = 425.
    assert_eq!(
        docs.len(),
        425,
        "endpoint + manager + top-index + guide docs"
    );
    // 5 base scaffolding + 4 Remote Site Settings + 1068 classes + 1068 metas
    // + 425 docs.
    assert_eq!(files.len(), 5 + 4 + (976 + 87 + 1 + 4) * 2 + 425);

    // Deterministic and path-sorted.
    let sorted: Vec<&String> = {
        let mut p: Vec<&String> = files.iter().map(|f| &f.path).collect();
        p.sort();
        p
    };
    assert_eq!(
        files.iter().map(|f| &f.path).collect::<Vec<_>>(),
        sorted,
        "generate() output must be path-sorted"
    );
}

#[test]
fn the_generated_test_suite_exercises_the_managers_through_a_mock() {
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
    let files = generate(&analysis, &apex(), &BuildInfo::new("testfp"));
    let get = |name: &str| {
        &files
            .iter()
            .find(|f| f.path == format!("{CLASSES}/{name}.cls"))
            .unwrap_or_else(|| panic!("missing {name}"))
            .content
    };

    // The mock implements the runtime contract with no real callout.
    let mock = get("BoxCalloutMock");
    assert_contains(mock, "@isTest");
    assert_contains(mock, "public class BoxCalloutMock implements BoxClient {");
    assert_contains(mock, "public BoxResponse send(BoxRequest request) {");

    // A per-manager test constructs the manager with the mock and drives every
    // operation inside a Test.startTest/stopTest window.
    let files_test = get("BoxFilesTest");
    assert_contains(files_test, "@isTest\nprivate class BoxFilesTest {");
    assert_contains(files_test, "BoxCalloutMock mock = new BoxCalloutMock();");
    assert_contains(files_test, "BoxFiles svc = new BoxFiles(mock);");
    assert_contains(files_test, "Test.startTest();");
    assert_contains(files_test, "svc.get(");
    assert_contains(files_test, "Test.stopTest();");
    // A binary (Blob) response is fed as a Blob, an array response as `[]`.
    assert_contains(files_test, "mock.bodyBlob = Blob.valueOf('x');");

    // The unions test drives each discriminated union's parse dispatch.
    let unions = get("BoxUnionsTest");
    assert_contains(unions, "@isTest\nprivate class BoxUnionsTest {");
    assert_contains(unions, ".parse(new Map<String, Object>{");
}

#[test]
fn each_endpoint_has_a_markdown_doc_with_a_runnable_snippet() {
    let lowering = gantry_spec::lower(
        &SpecSet::load(&[
            fixture("openapi.json"),
            fixture("openapi-v2025.0.json"),
            fixture("openapi-v2026.0.json"),
        ])
        .unwrap(),
    )
    .unwrap();
    let op_count = lowering.program.operations.len();
    let program = Box::leak(Box::new(lowering.program));
    let analysis = gantry_sema::analyze(program).unwrap();
    let files = generate(&analysis, &apex(), &BuildInfo::new("testfp"));

    // One endpoint page per operation. Endpoint pages live at
    // `docs/<manager>/<op>.md`; the per-manager `README.md` indexes, the top
    // `docs/README.md`, and the top-level topic guides (`docs/auth.md` etc.)
    // are the non-endpoint Markdown, excluded by requiring a manager subpath.
    let endpoint_pages = files
        .iter()
        .filter(|f| {
            f.path.starts_with("docs/")
                && f.path.ends_with(".md")
                && !f.path.ends_with("README.md")
                && f.path["docs/".len()..].contains('/')
        })
        .count();
    assert_eq!(endpoint_pages, op_count, "one endpoint page per operation");

    // A known endpoint reads as expected: the import/setup section, the SDK
    // types it touches, and a copy-pasteable example calling the real method.
    let get_file = files
        .iter()
        .find(|f| f.path == "docs/files/get.md")
        .expect("docs/files/get.md");
    let body = &get_file.content;
    assert_contains(body, "`GET /files/{file_id}`");
    assert_contains(body, "## Imports & setup");
    assert_contains(body, "Apex has no `import` statement");
    assert_contains(body, "**SDK types used:** `Box`, `BoxFiles`, `FileFull`");
    assert_contains(body, "Box client = new Box(myBoxClient);");
    assert_contains(
        body,
        "FileFull result = client.files.get(fileId, null, null, null, null);",
    );
    // A non-paged endpoint has no pagination section.
    assert!(!body.contains("## Pagination"));

    // A paged endpoint documents the cursor loop (no page classes — the
    // envelope is the page, D-131). Folders' `listItems` is marker-paged.
    let get_items = files
        .iter()
        .find(|f| f.path == "docs/folders/listItems.md")
        .expect("docs/folders/listItems.md");
    let paged = &get_items.content;
    assert_contains(paged, "## Pagination");
    assert_contains(paged, "while (String.isNotBlank(page.next_marker)) {");
    assert_contains(paged, "page = client.folders.listItems(");
}
