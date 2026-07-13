//! Structural + determinism fixtures for the Apex manager/client layer.

use std::path::PathBuf;

use gantry_backend_apex::{GeneratedFile, generate_managers};
use gantry_ir as ir;
use gantry_manifest::{ModuleSystem, apex};
use gantry_spec::SpecSet;

const CLASSES: &str = "force-app/main/default/classes";

fn ident(name: &str) -> ir::Identifier {
    ir::Identifier::new(name).unwrap()
}

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected to find:\n  {needle}\nin:\n{haystack}"
    );
}

/// A one-manager program: a `GET /files/{fileId}` returning a struct, with a
/// query param.
fn sample() -> Vec<GeneratedFile> {
    let mut program = ir::Program::default();
    let file = program.add(ir::Decl {
        name: ident("FileFull"),
        module: ir::ModulePath(vec![ident("schemas")]),
        api_version: None,
        kind: ir::DeclKind::Struct(ir::StructDecl {
            fields: vec![ir::Field {
                name: ident("id"),
                wire_name: "id".into(),
                ty: ir::Type::String,
            }],
        }),
    });
    program.operations.push(ir::Operation {
        name: ident("get_files_id"),
        variation: None,
        manager: ident("files"),
        api_version: None,
        method: ir::HttpMethod::Get,
        base_url: ir::BaseUrl::Api,
        path: vec![
            ir::PathSegment::Literal("files".into()),
            ir::PathSegment::Parameter(ident("file_id")),
        ],
        params: vec![
            ir::Param {
                name: ident("file_id"),
                wire_name: "file_id".into(),
                location: ir::ParamLocation::Path,
                ty: ir::Type::String,
            },
            ir::Param {
                name: ident("fields"),
                wire_name: "fields".into(),
                location: ir::ParamLocation::Query,
                ty: ir::Type::Optional(Box::new(ir::Type::String)),
            },
        ],
        request: None,
        response: ir::ResponseShape::Json(ir::Type::Decl(file)),
        deprecated: false,
    });
    let program = Box::leak(Box::new(program));
    let analysis = gantry_sema::analyze(program).unwrap();
    generate_managers(&analysis, &apex())
}

#[test]
fn a_manager_method_builds_the_request_and_deserializes() {
    let files = sample();
    let manager = files
        .iter()
        .find(|f| f.path == format!("{CLASSES}/BoxFiles.cls"))
        .expect("BoxFiles class");
    let src = &manager.content;

    // Class shape: holds the runtime client, constructed from it.
    assert_contains(src, "public with sharing class BoxFiles {");
    assert_contains(src, "private final BoxClient client;");
    assert_contains(src, "public BoxFiles(BoxClient client) {");

    // Method: path params first, then query; correct request line + path.
    assert_contains(
        src,
        "public FileFull getFilesId(String fileId, String fields) {",
    );
    assert_contains(src, "request.method = 'GET';");
    assert_contains(src, "request.baseUrl = 'api';");
    assert_contains(src, "request.path = '/' + 'files' + '/' + fileId;");
    assert_contains(
        src,
        "if (fields != null) request.query.put('fields', String.valueOf(fields));",
    );
    assert_contains(src, "BoxResponse response = client.send(request);");
    assert_contains(
        src,
        "return (FileFull) JSON.deserialize(response.body, FileFull.class);",
    );
}

#[test]
fn the_client_wires_every_manager_and_the_stubs_exist() {
    let files = sample();
    let client = files
        .iter()
        .find(|f| f.path == format!("{CLASSES}/Box.cls"))
        .expect("Box client");
    assert_contains(&client.content, "public final BoxFiles files;");
    assert_contains(&client.content, "public Box(BoxClient client) {");
    assert_contains(&client.content, "this.files = new BoxFiles(client);");

    for stub in ["BoxRequest", "BoxResponse", "BoxClient"] {
        assert!(
            files
                .iter()
                .any(|f| f.path == format!("{CLASSES}/{stub}.cls")),
            "missing runtime stub {stub}"
        );
    }
    assert_contains(
        &files
            .iter()
            .find(|f| f.path == format!("{CLASSES}/BoxClient.cls"))
            .unwrap()
            .content,
        "public interface BoxClient {",
    );
}

// --- the whole real spec -------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/specs")
        .join(name)
}

fn real_spec_managers() -> Vec<GeneratedFile> {
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
    generate_managers(&analysis, &apex())
}

#[test]
fn the_real_spec_yields_one_class_per_manager_and_a_method_per_operation() {
    let files = real_spec_managers();

    // 85 managers + the Box client + 3 runtime stubs.
    assert_eq!(files.len(), 85 + 1 + 3);
    let managers = files
        .iter()
        .filter(|f| {
            let n = f.path.trim_start_matches(&format!("{CLASSES}/"));
            n.starts_with("Box") && n.ends_with(".cls") && n != "Box.cls"
        })
        .filter(|f| f.content.contains("with sharing class"))
        .count();
    assert_eq!(managers, 85, "one class per manager");

    // One request per operation → 336 methods across all managers.
    let methods: usize = files
        .iter()
        .map(|f| f.content.matches("new BoxRequest();").count())
        .sum();
    assert_eq!(methods, 336, "one method per operation");

    // Every method and class name obeys the 40-char identifier limit.
    let ModuleSystem::Flat { identifier_limit } = apex().modules else {
        unreachable!()
    };
    for file in &files {
        let name = file
            .path
            .trim_start_matches(&format!("{CLASSES}/"))
            .trim_end_matches(".cls");
        assert!(
            name.len() <= identifier_limit as usize,
            "class {name} exceeds the identifier limit"
        );
    }
}

#[test]
fn generation_is_deterministic() {
    let once = real_spec_managers();
    let twice = real_spec_managers();
    for (a, b) in once.iter().zip(&twice) {
        assert_eq!(a.path, b.path);
        assert_eq!(a.content, b.content, "nondeterministic: {}", a.path);
    }
}
