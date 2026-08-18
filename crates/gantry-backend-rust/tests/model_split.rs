//! Per-manager schema-file split (D-201): a declaration exclusively reached
//! by one manager's operations lands in that manager's own
//! `src/models/schemas/<manager>.rs` submodule; a declaration shared by
//! two-or-more managers (or reached by none) stays in the catch-all
//! `src/models/schemas.rs` — never lost, never duplicated, and every type
//! stays reachable as `models::schemas::<Type>` regardless of which file
//! declares it.

use gantry_backend_rust::BuildInfo;
use gantry_ir as ir;

fn ident(name: &str) -> ir::Identifier {
    ir::Identifier::new(name).unwrap()
}

fn schemas() -> ir::ModulePath {
    ir::ModulePath(vec![ident("schemas")])
}

fn empty_struct(name: &str) -> ir::Decl {
    struct_decl(name, vec![])
}

fn struct_decl(name: &str, fields: Vec<ir::Field>) -> ir::Decl {
    ir::Decl {
        name: ident(name),
        module: schemas(),
        api_version: None,
        kind: ir::DeclKind::Struct(ir::StructDecl {
            fields,
            extra: None,
        }),
    }
}

fn field(name: &str, ty: ir::Type) -> ir::Field {
    ir::Field {
        name: ident(name),
        wire_name: name.to_string(),
        ty,
    }
}

fn op(name: &str, manager: &str, response: ir::Type) -> ir::Operation {
    ir::Operation {
        name: ident(name),
        variation: None,
        manager: ident(manager),
        api_version: None,
        method: ir::HttpMethod::Get,
        base_url: ir::BaseUrl::Api,
        path: vec![],
        params: vec![],
        request: None,
        response: ir::ResponseShape::Json(response),
        deprecated: false,
    }
}

/// `Shared` is reached by both `files` and `folders`; `FileOnly` (whose one
/// field references `Shared`, so it also needs `use super::*;`) by `files`
/// alone; `FolderOnly` (which references nothing outside itself) by
/// `folders` alone; `Orphan` by neither.
fn generate() -> Vec<gantry_backend_rust::GeneratedFile> {
    let decls = vec![
        empty_struct("Shared"), // 0
        struct_decl(
            "FileOnly",
            vec![
                field("shared", ir::Type::Decl(ir::DeclId(0))),
                field("when", ir::Type::Optional(Box::new(ir::Type::DateTime))),
            ],
        ), // 1
        empty_struct("FolderOnly"), // 2
        empty_struct("Orphan"), // 3
    ];
    let program = ir::Program {
        decls,
        operations: vec![
            op("getShared1", "files", ir::Type::Decl(ir::DeclId(0))),
            op("getShared2", "folders", ir::Type::Decl(ir::DeclId(0))),
            op("getFileOnly", "files", ir::Type::Decl(ir::DeclId(1))),
            op("getFolderOnly", "folders", ir::Type::Decl(ir::DeclId(2))),
        ],
    };
    let program = Box::leak(Box::new(program));
    let analysis = gantry_sema::analyze(program).expect("fixture program must analyze");
    let build = BuildInfo::new("fixtures");
    gantry_backend_rust::generate_models(&analysis, &build)
}

fn file(files: &[gantry_backend_rust::GeneratedFile], path: &str) -> String {
    files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("expected a generated file at {path:?}"))
        .content
        .clone()
}

#[test]
fn mod_rs_only_declares_the_top_level_schemas_module() {
    let files = generate();
    let mod_rs = file(&files, "src/models/mod.rs");
    assert!(mod_rs.contains("pub mod schemas;"), "{mod_rs}");
}

#[test]
fn the_catch_all_declares_and_reexports_every_bucket() {
    let files = generate();
    let catch_all = file(&files, "src/models/schemas.rs");
    assert!(catch_all.contains("mod files;"), "{catch_all}");
    assert!(catch_all.contains("mod folders;"), "{catch_all}");
    assert!(catch_all.contains("pub use files::*;"), "{catch_all}");
    assert!(catch_all.contains("pub use folders::*;"), "{catch_all}");
    assert!(catch_all.contains("pub struct Shared"), "{catch_all}");
    assert!(catch_all.contains("pub struct Orphan"), "{catch_all}");
    assert!(!catch_all.contains("pub struct FileOnly"), "{catch_all}");
    // mod/pub use lines are sorted, not hash-order.
    let mod_files = catch_all.find("mod files;").unwrap();
    let mod_folders = catch_all.find("mod folders;").unwrap();
    assert!(mod_files < mod_folders, "{catch_all}");
}

#[test]
fn a_bucket_that_references_the_catch_all_gets_use_super() {
    let files = generate();
    let bucket = file(&files, "src/models/schemas/files.rs");
    assert!(bucket.contains("pub struct FileOnly"), "{bucket}");
    assert!(
        bucket.contains("use super::*;"),
        "FileOnly references Shared, which lives in the catch-all:\n{bucket}"
    );
}

#[test]
fn a_bucket_needing_nothing_outside_itself_has_no_use_super() {
    let files = generate();
    let bucket = file(&files, "src/models/schemas/folders.rs");
    assert!(bucket.contains("pub struct FolderOnly"), "{bucket}");
    assert!(
        !bucket.contains("use super::*;"),
        "FolderOnly references nothing outside itself, so the glob would be \
         an unused import under -D warnings:\n{bucket}"
    );
}

/// The partition safety net: every declared type name appears in exactly one
/// `src/models/schemas*.rs` file.
#[test]
fn every_decl_appears_in_exactly_one_file() {
    let files = generate();
    for name in ["Shared", "FileOnly", "FolderOnly", "Orphan"] {
        let owners: Vec<&str> = files
            .iter()
            .filter(|f| {
                f.path == "src/models/schemas.rs" || f.path.starts_with("src/models/schemas/")
            })
            .filter(|f| f.content.contains(&format!("pub struct {name}")))
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(
            owners.len(),
            1,
            "{name} should appear in exactly one file, found in {owners:?}"
        );
    }
}

#[test]
fn every_split_file_has_the_generated_header() {
    let files = generate();
    for path in [
        "src/models/schemas.rs",
        "src/models/schemas/files.rs",
        "src/models/schemas/folders.rs",
    ] {
        let content = file(&files, path);
        assert!(
            content.starts_with("// Code generated by box-gantry "),
            "{path} is missing the generated-file header:\n{content}"
        );
    }
}
