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
            extra: None,
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
        conditional: None,
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

/// A one-manager program whose operation both sends and returns a struct with
/// a renamed field (`limit` → `limit_r`), so the manager must route through the
/// generated key remap on both the request and response paths.
fn remap_sample() -> Vec<GeneratedFile> {
    let mut program = ir::Program::default();
    let paged = program.add(ir::Decl {
        name: ident("Paged"),
        module: ir::ModulePath(vec![ident("schemas")]),
        api_version: None,
        kind: ir::DeclKind::Struct(ir::StructDecl {
            fields: vec![ir::Field {
                name: ident("limit"),
                wire_name: "limit".into(),
                ty: ir::Type::Int64,
            }],
            extra: None,
        }),
    });
    program.operations.push(ir::Operation {
        name: ident("post_search"),
        variation: None,
        manager: ident("search"),
        api_version: None,
        method: ir::HttpMethod::Post,
        base_url: ir::BaseUrl::Api,
        path: vec![ir::PathSegment::Literal("search".into())],
        params: vec![],
        request: Some(ir::RequestBody {
            media: ir::RequestMedia::Json,
            ty: ir::Type::Decl(paged),
        }),
        response: ir::ResponseShape::Json(ir::Type::Decl(paged)),
        deprecated: false,
        conditional: None,
    });
    let program = Box::leak(Box::new(program));
    let analysis = gantry_sema::analyze(program).unwrap();
    generate_managers(&analysis, &apex())
}

#[test]
fn a_renamed_field_routes_through_the_key_remap_both_ways() {
    let files = remap_sample();
    let manager = files
        .iter()
        .find(|f| f.path == format!("{CLASSES}/BoxSearch.cls"))
        .expect("BoxSearch class");
    let src = &manager.content;

    // Request: reduce the body to its set keys, remap Apex → wire, hand the map
    // to the runtime with null-suppression off.
    assert_contains(
        src,
        "Object wireBody = JSON.deserializeUntyped(JSON.serialize(body, true));",
    );
    assert_contains(
        src,
        "wireBody = Paged.denormalizeKeys((Map<String, Object>) wireBody);",
    );
    assert_contains(src, "request.body = wireBody;");
    assert_contains(src, "request.suppressNulls = false;");

    // Response: parse untyped, remap wire → Apex, then native-deserialize.
    assert_contains(
        src,
        "Object parsed = JSON.deserializeUntyped(response.body);",
    );
    assert_contains(
        src,
        "parsed = Paged.normalizeKeys((Map<String, Object>) parsed);",
    );
    assert_contains(
        src,
        "return (Paged) JSON.deserialize(JSON.serialize(parsed), Paged.class);",
    );
}

/// A one-manager program whose operation sends a struct with **no** renamed
/// fields but one `Nullable` field, so null-writability alone must route the
/// body through the write transform.
fn null_only_sample() -> Vec<GeneratedFile> {
    let mut program = ir::Program::default();
    let patch = program.add(ir::Decl {
        name: ident("Patch"),
        module: ir::ModulePath(vec![ident("schemas")]),
        api_version: None,
        kind: ir::DeclKind::Struct(ir::StructDecl {
            fields: vec![ir::Field {
                name: ident("note"),
                wire_name: "note".into(),
                ty: ir::Type::Nullable(Box::new(ir::Type::String)),
            }],
            extra: None,
        }),
    });
    program.operations.push(ir::Operation {
        name: ident("patch_thing"),
        variation: None,
        manager: ident("things"),
        api_version: None,
        method: ir::HttpMethod::Put,
        base_url: ir::BaseUrl::Api,
        path: vec![ir::PathSegment::Literal("things".into())],
        params: vec![],
        request: Some(ir::RequestBody {
            media: ir::RequestMedia::Json,
            ty: ir::Type::Decl(patch),
        }),
        response: ir::ResponseShape::None,
        deprecated: false,
        conditional: None,
    });
    let program = Box::leak(Box::new(program));
    let analysis = gantry_sema::analyze(program).unwrap();
    generate_managers(&analysis, &apex())
}

#[test]
fn null_writability_alone_routes_the_body_through_the_write_transform() {
    // No field renames — the only reason `Patch` is write-affected is its
    // `Nullable` field. The manager must still reduce, denormalize, and drop
    // null-suppression so an explicit null can reach Box.
    let files = null_only_sample();
    let src = &files
        .iter()
        .find(|f| f.path == format!("{CLASSES}/BoxThings.cls"))
        .expect("BoxThings class")
        .content;
    assert_contains(
        src,
        "Object wireBody = JSON.deserializeUntyped(JSON.serialize(body, true));",
    );
    assert_contains(
        src,
        "wireBody = Patch.denormalizeKeys((Map<String, Object>) wireBody);",
    );
    assert_contains(src, "request.body = wireBody;");
    assert_contains(src, "request.suppressNulls = false;");
}

/// A one-manager program whose operation returns a struct with an `Object`
/// (`JsonValue`) field, so the manager must route the response through the
/// generated `deserialize` builder rather than native `JSON.deserialize`.
fn object_response_sample() -> Vec<GeneratedFile> {
    let mut program = ir::Program::default();
    let envelope = program.add(ir::Decl {
        name: ident("Envelope"),
        module: ir::ModulePath(vec![ident("schemas")]),
        api_version: None,
        kind: ir::DeclKind::Struct(ir::StructDecl {
            fields: vec![ir::Field {
                name: ident("payload"),
                wire_name: "payload".into(),
                ty: ir::Type::JsonValue,
            }],
            extra: None,
        }),
    });
    program.operations.push(ir::Operation {
        name: ident("get_thing"),
        variation: None,
        manager: ident("things"),
        api_version: None,
        method: ir::HttpMethod::Get,
        base_url: ir::BaseUrl::Api,
        path: vec![ir::PathSegment::Literal("things".into())],
        params: vec![],
        request: None,
        response: ir::ResponseShape::Json(ir::Type::Decl(envelope)),
        deprecated: false,
        conditional: None,
    });
    let program = Box::leak(Box::new(program));
    let analysis = gantry_sema::analyze(program).unwrap();
    generate_managers(&analysis, &apex())
}

#[test]
fn an_object_bearing_response_routes_through_the_deserialize_builder() {
    let files = object_response_sample();
    let src = &files
        .iter()
        .find(|f| f.path == format!("{CLASSES}/BoxThings.cls"))
        .expect("BoxThings class")
        .content;
    assert_contains(
        src,
        "Object parsed = JSON.deserializeUntyped(response.body);",
    );
    assert_contains(src, "deserialized = Envelope.deserialize(parsed);");
    assert_contains(src, "return deserialized;");
    // It must NOT try native typed deserialize into the Object-bearing type.
    assert!(
        !src.contains("(Envelope) JSON.deserialize(response.body"),
        "an object-bearing response must not use native typed deserialize:\n{src}"
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

    // 88 managers + the Box client + 3 runtime stubs. Pagination adds no
    // classes — the base method's envelope is the page (D-131).
    assert_eq!(files.len(), 88 + 1 + 3);
    let managers = files
        .iter()
        .filter(|f| {
            let n = f.path.trim_start_matches(&format!("{CLASSES}/"));
            n.starts_with("Box") && n.ends_with(".cls") && n != "Box.cls"
        })
        .filter(|f| f.content.contains("with sharing class"))
        .count();
    assert_eq!(managers, 88, "one class per manager");

    // One request per operation → 381 methods across all managers.
    let methods: usize = files
        .iter()
        .map(|f| f.content.matches("new BoxRequest();").count())
        .sum();
    assert_eq!(methods, 381, "one method per operation");

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

/// The Apex live smoke (`runtimes/apex/smoke/livesmoke.apex`, VR-1.4) runs as
/// anonymous Apex, so nothing compiles it until a credentialed run — a renamed
/// method (`getItems` → `listItems`) surfaced as a compile error months later.
/// Pin every `client.<manager>.<method>(…)` it calls to a generated method of
/// the same arity.
#[test]
fn the_apex_live_smoke_calls_methods_the_sdk_generates() {
    let smoke = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtimes/apex/smoke/livesmoke.apex"),
    )
    .unwrap();
    let files = real_spec_managers();
    let class_source = |class: &str| {
        let path = format!("{CLASSES}/{class}.cls");
        files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("no generated {path}"))
            .content
            .as_str()
    };
    let client = class_source("Box");

    let mut calls = 0;
    // `client` is the smoke's session; `narrow` the client built from a
    // downscoped token.
    let receivers = smoke
        .match_indices("client.")
        .chain(smoke.match_indices("narrow."));
    for (at, receiver) in receivers {
        let rest = &smoke[at + receiver.len()..];
        let Some((manager, rest)) = rest.split_once('.') else {
            continue;
        };
        let Some((method, args)) = rest.split_once('(') else {
            continue;
        };
        if !is_identifier(manager) || !is_identifier(method) {
            continue;
        }
        let field = format!(" {manager};");
        let class = client
            .lines()
            .find_map(|l| l.trim().strip_prefix("public final ")?.strip_suffix(&field))
            .unwrap_or_else(|| panic!("Box has no `{manager}` manager"));
        let signature = format!(" {method}(");
        let params = class_source(class)
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("public "))
            .find_map(|l| l.split_once(&signature).map(|(_, params)| arity(params)))
            .unwrap_or_else(|| panic!("{class} has no method `{method}`"));
        assert_eq!(
            arity(args),
            params,
            "client.{manager}.{method}: the smoke's argument count doesn't match {class}.{method}"
        );
        calls += 1;
    }
    assert_eq!(
        calls, 18,
        "the smoke's SDK calls (getMe, listItems, then a scratch folder's and a scratch \
         web link's create, getIfModified ×2, update_r ×2, delete_r, the folder's \
         downscopeToken and the narrow client's update_r and delete_r, then events.stream)"
    );
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// How many comma-separated items precede the `)` closing a call or
/// parameter list, given the text just after its `(`.
fn arity(list: &str) -> usize {
    let mut depth = 0;
    let mut items = 0;
    for c in list.chars() {
        match c {
            '(' | '<' => depth += 1,
            '>' => depth -= 1,
            ')' if depth == 0 => break,
            ')' => depth -= 1,
            ',' if depth == 0 => items += 1,
            c if !c.is_whitespace() && items == 0 => items = 1,
            _ => {}
        }
    }
    items
}
