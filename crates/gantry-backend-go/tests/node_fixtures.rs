//! VR-2: per-node lowering fixtures — an IR fragment → its expected Go
//! source, one node kind (and quirk) at a time.
//!
//! Where the compile-the-output loop (VR-1.1) proves the *whole* real spec
//! builds, these prove *each* lowering rule renders the exact source the
//! contract intends: the D-110 tri-state pointer/wrapper shapes, open-enum
//! constants, discriminated-union dispatch with unknown-tag retention, the
//! special scalar mappings (Date/DateTime/Binary/JSON), and aliases. The
//! case list and expected semantics are informed by box-codegen's 54-case
//! Go suite, authored fresh against this IR.
//!
//! Each fixture asserts the *semantics* (the type expression, the tag, the
//! generated method) rather than byte-exact output — determinism and gofmt
//! cleanliness are already covered by `compile_output.rs`.

use gantry_backend_go::BuildInfo;
use gantry_ir as ir;

// --- harness --------------------------------------------------------------

fn ident(name: &str) -> ir::Identifier {
    ir::Identifier::new(name).unwrap()
}

fn schemas() -> ir::ModulePath {
    ir::ModulePath(vec![ident("schemas")])
}

fn struct_decl(name: &str, fields: Vec<ir::Field>) -> ir::Decl {
    ir::Decl {
        name: ident(name),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Struct(ir::StructDecl { fields }),
    }
}

fn field(wire: &str, ty: ir::Type) -> ir::Field {
    ir::Field {
        name: ident(wire),
        wire_name: wire.to_string(),
        ty,
    }
}

fn opt(ty: ir::Type) -> ir::Type {
    ir::Type::Optional(Box::new(ty))
}
fn nullable(ty: ir::Type) -> ir::Type {
    ir::Type::Nullable(Box::new(ty))
}
fn list(ty: ir::Type) -> ir::Type {
    ir::Type::List(Box::new(ty))
}
fn map(ty: ir::Type) -> ir::Type {
    ir::Type::Map(Box::new(ty))
}

/// Render a program's `schemas` module to Go source.
fn render(decls: Vec<ir::Decl>) -> String {
    let mut program = ir::Program::default();
    for decl in decls {
        program.add(decl);
    }
    let program = Box::leak(Box::new(program));
    let analysis = gantry_sema::analyze(program).expect("fixture program must analyze");
    let build = BuildInfo::new("fixtures");
    gantry_backend_go::generate_models(&analysis, &build)
        .into_iter()
        .find(|f| f.path == "schemas/schemas.go")
        .expect("the schemas module must be generated")
        .content
}

/// Render a single struct whose one field has the given type.
fn render_field(ty: ir::Type) -> String {
    render(vec![struct_decl("S", vec![field("f", ty)])])
}

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find:\n  {needle}\nin generated source:\n{haystack}"
    );
}

/// Collapse runs of spaces/tabs to a single space, so an assertion is
/// insensitive to gofmt's column alignment within a field block.
fn squeeze(s: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for c in s.chars() {
        if c == ' ' || c == '\t' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

fn assert_contains_aligned(haystack: &str, needle: &str) {
    assert!(
        squeeze(haystack).contains(&squeeze(needle)),
        "expected (ignoring column alignment) to find:\n  {needle}\nin generated source:\n{haystack}"
    );
}

// --- scalar mappings ------------------------------------------------------

#[test]
fn primitive_scalars_map_directly() {
    assert_contains(&render_field(ir::Type::Bool), "F bool `json:\"f\"`");
    assert_contains(&render_field(ir::Type::Int64), "F int64 `json:\"f\"`");
    assert_contains(&render_field(ir::Type::Float64), "F float64 `json:\"f\"`");
    assert_contains(&render_field(ir::Type::String), "F string `json:\"f\"`");
}

#[test]
fn date_maps_to_the_serialization_date_type() {
    let go = render_field(ir::Type::Date);
    assert_contains(&go, "F serialization.Date `json:\"f\"`");
    assert_contains(&go, "boxgantry.invalid/boxsdk/serialization");
}

#[test]
fn datetime_maps_to_time_time() {
    let go = render_field(ir::Type::DateTime);
    assert_contains(&go, "F time.Time `json:\"f\"`");
    assert_contains(&go, "import \"time\"");
}

#[test]
fn binary_is_an_io_reader_excluded_from_json() {
    // Binary parts travel via multipart assembly, so they carry `json:"-"`.
    let go = render_field(ir::Type::Binary);
    assert_contains(&go, "F io.Reader `json:\"-\"`");
    assert_contains(&go, "import \"io\"");
}

#[test]
fn schemaless_json_maps_to_any() {
    assert_contains(&render_field(ir::Type::JsonValue), "F any `json:\"f\"`");
}

// --- optionality: the D-110 tri-state ------------------------------------

#[test]
fn optional_scalar_is_a_pointer_with_omitempty() {
    assert_contains(
        &render_field(opt(ir::Type::String)),
        "F *string `json:\"f,omitempty\"`",
    );
}

#[test]
fn optional_nullable_is_the_full_tristate_wrapper() {
    // Optional<Nullable<T>> → *serialization.Nullable[T] with omitempty:
    // nil = absent, &{Valid:false} = null, &{Valid:true} = value (BG-1).
    assert_contains(
        &render_field(opt(nullable(ir::Type::String))),
        "F *serialization.Nullable[string] `json:\"f,omitempty\"`",
    );
}

#[test]
fn bare_nullable_is_the_wrapper_by_value() {
    // Key always present, value may be null → wrapper by value, no omitempty.
    assert_contains(
        &render_field(nullable(ir::Type::String)),
        "F serialization.Nullable[string] `json:\"f\"`",
    );
}

#[test]
fn optional_slice_stays_bare_but_omitempty() {
    // A slice is already nilable, so no pointer — but still omitempty.
    assert_contains(
        &render_field(opt(list(ir::Type::String))),
        "F []string `json:\"f,omitempty\"`",
    );
}

// --- containers -----------------------------------------------------------

#[test]
fn list_and_map_map_to_go_collections() {
    assert_contains(
        &render_field(list(ir::Type::Int64)),
        "F []int64 `json:\"f\"`",
    );
    assert_contains(
        &render_field(map(ir::Type::String)),
        "F map[string]string `json:\"f\"`",
    );
}

#[test]
fn nullable_inside_a_list_keeps_its_wrapper_per_element() {
    assert_contains(
        &render_field(list(nullable(ir::Type::String))),
        "F []serialization.Nullable[string] `json:\"f\"`",
    );
}

#[test]
fn a_decl_reference_renders_as_the_type_name() {
    let go = render(vec![
        struct_decl("Inner", vec![field("id", ir::Type::String)]),
        struct_decl("Outer", vec![field("inner", ir::Type::Decl(ir::DeclId(0)))]),
    ]);
    assert_contains(&go, "Inner Inner `json:\"inner\"`");
}

// --- enums ----------------------------------------------------------------

#[test]
fn open_enum_is_a_string_type_with_prefixed_constants() {
    let go = render(vec![ir::Decl {
        name: ident("ItemType"),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Enum(ir::EnumDecl {
            values: vec!["file".into(), "web_link".into()],
            extensibility: ir::Extensibility::Open,
        }),
    }]);
    // Named string type: unknown values round-trip for free (no serializer).
    assert_contains(&go, "type ItemType string");
    assert_contains(&go, "ItemTypeFile    ItemType = \"file\"");
    assert_contains(&go, "ItemTypeWebLink ItemType = \"web_link\"");
}

// --- unions ---------------------------------------------------------------

#[test]
fn discriminated_union_dispatches_and_retains_unknown_tags() {
    let go = render(vec![
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
    // Variant struct: one pointer per variant plus the open-union catch-all.
    // (Field columns are gofmt-aligned, so ignore inter-column spacing.)
    assert_contains(&go, "type Item struct {");
    assert_contains_aligned(&go, "File *File");
    assert_contains_aligned(&go, "Folder *Folder");
    assert_contains_aligned(&go, "Unknown json.RawMessage");
    // Marshal delegates to the set variant.
    assert_contains(&go, "case u.File != nil:\n\t\treturn json.Marshal(u.File)");
    // Unmarshal dispatches on the tag and retains unknown tags raw.
    assert_contains(&go, "case \"file\":\n\t\tu.File = new(File)");
    assert_contains(&go, "u.Unknown = append(u.Unknown[:0], data...)");
}

#[test]
fn structural_union_round_trips_raw_bytes() {
    let go = render(vec![ir::Decl {
        name: ident("AnyValue"),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Union(ir::UnionDecl {
            // No discriminator → structural: passthrough raw JSON.
            discriminator: None,
            variants: vec![ir::UnionVariant {
                discriminator_value: None,
                ty: ir::Type::String,
            }],
            extensibility: ir::Extensibility::Open,
        }),
    }]);
    assert_contains(&go, "type AnyValue struct {\n\tRaw json.RawMessage\n}");
    assert_contains(&go, "return u.Raw, nil");
}

// --- aliases and the empty struct ----------------------------------------

#[test]
fn alias_renders_as_a_go_type_alias() {
    let go = render(vec![ir::Decl {
        name: ident("Token"),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Alias(ir::Type::String),
    }]);
    assert_contains(&go, "type Token = string");
}

#[test]
fn empty_struct_renders_without_a_body() {
    let go = render(vec![struct_decl("Empty", vec![])]);
    assert_contains(&go, "type Empty struct{}");
}
