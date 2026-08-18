//! Per-manager schema-file split (D-201): a declaration exclusively reached
//! by one manager's operations lands in that manager's own
//! `src/models/schemas/<manager>.ts` file; a declaration shared by
//! two-or-more managers (or reached by none) stays in the catch-all
//! `src/models/schemas.ts` — never lost, never duplicated, and every type
//! stays reachable as `models.schemas.<Type>` regardless of which file
//! declares it.

use gantry_backend_typescript::BuildInfo;
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
/// field references `Shared`, so it needs an `import type` back to the
/// catch-all) by `files` alone; `FolderOnly` (which references nothing
/// outside itself) by `folders` alone; `Orphan` by neither.
fn generate() -> Vec<gantry_backend_typescript::GeneratedFile> {
    let decls = vec![
        empty_struct("Shared"), // 0
        struct_decl(
            "FileOnly",
            vec![field("shared", ir::Type::Decl(ir::DeclId(0)))],
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
    gantry_backend_typescript::generate_models(&analysis, &build)
}

fn file(files: &[gantry_backend_typescript::GeneratedFile], path: &str) -> String {
    files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("expected a generated file at {path:?}"))
        .content
        .clone()
}

#[test]
fn the_barrel_is_unaffected_by_the_split() {
    let files = generate();
    let index = file(&files, "src/models/index.ts");
    assert!(
        index.contains("export * as schemas from './schemas.js';"),
        "the public models.schemas.* path must not move:\n{index}"
    );
}

#[test]
fn the_catch_all_reexports_every_bucket() {
    let files = generate();
    let catch_all = file(&files, "src/models/schemas.ts");
    assert!(
        catch_all.contains("export * from './schemas/files.js';"),
        "{catch_all}"
    );
    assert!(
        catch_all.contains("export * from './schemas/folders.js';"),
        "{catch_all}"
    );
    assert!(catch_all.contains("export type Shared ="), "{catch_all}");
    assert!(catch_all.contains("export type Orphan ="), "{catch_all}");
    assert!(!catch_all.contains("FileOnly"), "{catch_all}");
}

#[test]
fn a_bucket_that_references_the_catch_all_imports_it() {
    let files = generate();
    let bucket = file(&files, "src/models/schemas/files.ts");
    assert!(bucket.contains("export interface FileOnly"), "{bucket}");
    assert!(
        bucket.contains("import type { Shared } from '../schemas.js';"),
        "FileOnly references Shared, which lives in the catch-all:\n{bucket}"
    );
}

#[test]
fn a_bucket_needing_nothing_outside_itself_has_no_import() {
    let files = generate();
    let bucket = file(&files, "src/models/schemas/folders.ts");
    assert!(bucket.contains("export type FolderOnly ="), "{bucket}");
    assert!(
        !bucket.contains("import type"),
        "FolderOnly references nothing outside itself:\n{bucket}"
    );
}

/// The reverse direction: an orphan in the catch-all that references a
/// bucketed type still needs the import — unlike Rust's `pub use`,
/// TypeScript's `export *` does not bring names into the re-exporting
/// module's own scope.
#[test]
fn the_catch_all_imports_from_a_bucket_when_it_references_one() {
    let decls = vec![
        empty_struct("FileOnly"), // 0
        struct_decl(
            "UsesFileOnly",
            vec![field("f", ir::Type::Decl(ir::DeclId(0)))],
        ), // 1
    ];
    let program = ir::Program {
        decls,
        operations: vec![
            op("getFileOnly", "files", ir::Type::Decl(ir::DeclId(0))),
            // UsesFileOnly is reached by no operation, so it stays in the
            // catch-all, but its field references the "files" bucket.
        ],
    };
    let program = Box::leak(Box::new(program));
    let analysis = gantry_sema::analyze(program).expect("fixture program must analyze");
    let build = BuildInfo::new("fixtures");
    let files = gantry_backend_typescript::generate_models(&analysis, &build);
    let catch_all = file(&files, "src/models/schemas.ts");
    assert!(
        catch_all.contains("import type { FileOnly } from './schemas/files.js';"),
        "{catch_all}"
    );
    assert!(
        catch_all.contains("export interface UsesFileOnly"),
        "{catch_all}"
    );
}

/// The partition safety net: every declared type name appears in exactly one
/// `src/models/schemas*.ts` file.
#[test]
fn every_decl_appears_in_exactly_one_file() {
    let files = generate();
    // Empty structs render as a `Record<string, never>` alias; FileOnly has
    // a field, so it renders as a real `interface`.
    for (name, needle) in [
        ("Shared", "export type Shared ="),
        ("FileOnly", "export interface FileOnly"),
        ("FolderOnly", "export type FolderOnly ="),
        ("Orphan", "export type Orphan ="),
    ] {
        let owners: Vec<&str> = files
            .iter()
            .filter(|f| {
                f.path == "src/models/schemas.ts" || f.path.starts_with("src/models/schemas/")
            })
            .filter(|f| f.content.contains(needle))
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
        "src/models/schemas.ts",
        "src/models/schemas/files.ts",
        "src/models/schemas/folders.ts",
    ] {
        let content = file(&files, path);
        assert!(
            content.starts_with("// Code generated by box-gantry "),
            "{path} is missing the generated-file header:\n{content}"
        );
    }
}
