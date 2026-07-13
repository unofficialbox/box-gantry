//! Apex manager + client lowering: one class per `x-box-tag` manager, a
//! method per operation, plus the `Box` client entry point and the runtime
//! contract stubs.
//!
//! Managers call only the runtime contract (`BoxClient` / `BoxRequest` /
//! `BoxResponse`), so — exactly like the Go backend against `gantryruntime`
//! — the generated code compiles against compilable stubs, and a
//! hand-written Apex runtime (a later slice) drops in behind the same
//! surface. Methods build the request structurally (method, base-URL class,
//! path segments, typed query/header params, body) and deserialize the
//! response into the model type; errors are exceptions (manifest
//! `ErrorModel::Exceptions`), thrown by the runtime.

use std::collections::HashSet;
use std::fmt::Write as _;

use gantry_ir as ir;
use gantry_ir::naming::{camel, pascal};
use gantry_manifest::CapabilityManifest;
use gantry_sema::Analysis;

use crate::models::{ClassNames, apex_type, mint_unique};
use crate::{CLASSES_DIR, GeneratedFile, safe_word};

/// The fixed runtime-contract class names, reserved so no manager or model
/// class can collide with them in the flat namespace.
const RUNTIME_NAMES: &[&str] = &["Box", "BoxRequest", "BoxResponse", "BoxClient"];

/// Generate the manager classes, the `Box` client, and the runtime stubs.
pub fn generate_managers(
    analysis: &Analysis<'_>,
    manifest: &CapabilityManifest,
) -> Vec<GeneratedFile> {
    let program = analysis.program;
    let limit = match manifest.modules {
        gantry_manifest::ModuleSystem::Flat { identifier_limit } => identifier_limit as usize,
        gantry_manifest::ModuleSystem::Hierarchical => {
            panic!("the Apex backend requires the flat-namespace manifest axis")
        }
    };
    let names = ClassNames::build(program, limit);

    // Manager/client/stub names share the flat namespace with the model
    // classes, so mint them into a set seeded with every model name plus
    // the reserved runtime names — globally unique, within the limit.
    let mut used: std::collections::HashSet<String> = names.names().map(str::to_string).collect();
    for reserved in RUNTIME_NAMES {
        used.insert((*reserved).to_string());
    }

    let mut files = Vec::new();
    // Managers appear in analysis order (BTreeMap → sorted, deterministic).
    let mut client_fields: Vec<(String, String)> = Vec::new();
    for (manager, op_indices) in &analysis.managers {
        let class = mint_unique(&format!("Box{}", pascal(manager)), limit, &mut used);
        let field = safe_word(&camel(manager));
        client_fields.push((field, class.clone()));
        files.push(GeneratedFile {
            path: format!("{CLASSES_DIR}/{class}.cls"),
            content: manager_class(program, &names, &class, op_indices),
        });
    }

    files.push(GeneratedFile {
        path: format!("{CLASSES_DIR}/Box.cls"),
        content: client_class(&client_fields),
    });
    for stub in runtime_stubs() {
        files.push(stub);
    }
    files
}

fn manager_class(
    program: &ir::Program,
    names: &ClassNames,
    class: &str,
    op_indices: &[usize],
) -> String {
    let mut out = header();
    let _ = writeln!(out, "public with sharing class {class} {{");
    let _ = writeln!(out, "    private final BoxClient client;");
    let _ = writeln!(out, "    public {class}(BoxClient client) {{");
    let _ = writeln!(out, "        this.client = client;");
    let _ = writeln!(out, "    }}");

    // Method names must be unique within the class.
    let mut used: HashSet<String> = HashSet::new();
    for &index in op_indices {
        let op = &program.operations[index];
        out.push('\n');
        render_operation(&mut out, program, names, op, &mut used);
    }

    let _ = writeln!(out, "}}");
    out
}

fn render_operation(
    out: &mut String,
    program: &ir::Program,
    names: &ClassNames,
    op: &ir::Operation,
    used: &mut HashSet<String>,
) {
    let method_name = unique_method_name(op, used);
    let return_ty = response_type(program, names, &op.response);

    // Signature: path + non-path params, then the request body.
    let mut args: Vec<String> = Vec::new();
    for param in path_params(op) {
        args.push(format!("String {}", arg_name(param)));
    }
    for param in &op.params {
        if param.location != ir::ParamLocation::Path {
            args.push(format!(
                "{} {}",
                apex_type(program, names, &param.ty),
                arg_name(param)
            ));
        }
    }
    if let Some(body) = &op.request {
        let ty = request_type(program, names, body);
        args.push(format!("{ty} body"));
    }

    let _ = writeln!(
        out,
        "    public {return_ty} {method_name}({}) {{",
        args.join(", ")
    );
    let _ = writeln!(out, "        BoxRequest request = new BoxRequest();");
    let _ = writeln!(
        out,
        "        request.method = '{}';",
        http_method(op.method)
    );
    let _ = writeln!(
        out,
        "        request.baseUrl = '{}';",
        base_url_key(op.base_url)
    );
    let _ = writeln!(out, "        request.path = {};", path_expr(&op.path));

    for param in &op.params {
        match param.location {
            ir::ParamLocation::Query => {
                let _ = writeln!(
                    out,
                    "        if ({0} != null) request.query.put('{1}', String.valueOf({0}));",
                    arg_name(param),
                    escape(&param.wire_name)
                );
            }
            ir::ParamLocation::Header => {
                let _ = writeln!(
                    out,
                    "        if ({0} != null) request.headers.put('{1}', String.valueOf({0}));",
                    arg_name(param),
                    escape(&param.wire_name)
                );
            }
            ir::ParamLocation::Path => {}
        }
    }
    if let Some(body) = &op.request {
        match body.media {
            ir::RequestMedia::OctetStream => {
                let _ = writeln!(out, "        request.binaryBody = body;");
            }
            ir::RequestMedia::Json
            | ir::RequestMedia::JsonPatch
            | ir::RequestMedia::UrlEncoded
            | ir::RequestMedia::Multipart => {
                let _ = writeln!(out, "        request.body = body;");
            }
        }
    }

    let _ = writeln!(out, "        BoxResponse response = client.send(request);");
    if let Some(expr) = deserialize_expr(program, names, &op.response) {
        let _ = writeln!(out, "        return {expr};");
    }
    let _ = writeln!(out, "    }}");
}

/// The Apex return type for a response shape.
fn response_type(program: &ir::Program, names: &ClassNames, shape: &ir::ResponseShape) -> String {
    match shape {
        ir::ResponseShape::None => "void".to_string(),
        ir::ResponseShape::Json(ty) => apex_type(program, names, ty),
        // Buffered platform: bytes are a Blob, never a stream.
        ir::ResponseShape::Binary => "Blob".to_string(),
        ir::ResponseShape::Text | ir::ResponseShape::Redirect => "String".to_string(),
    }
}

/// The deserialization expression for the response, or `None` for `void`.
fn deserialize_expr(
    program: &ir::Program,
    names: &ClassNames,
    shape: &ir::ResponseShape,
) -> Option<String> {
    match shape {
        ir::ResponseShape::None => None,
        ir::ResponseShape::Binary => Some("response.binaryBody".to_string()),
        ir::ResponseShape::Text | ir::ResponseShape::Redirect => Some("response.body".to_string()),
        ir::ResponseShape::Json(ty) => {
            let apex = apex_type(program, names, ty);
            // Object/String/plain values deserialize untyped; typed classes
            // and collections go through JSON.deserialize with a class token.
            if apex == "Object" {
                Some("JSON.deserializeUntyped(response.body)".to_string())
            } else if apex == "String" {
                Some("response.body".to_string())
            } else {
                Some(format!(
                    "({apex}) JSON.deserialize(response.body, {apex}.class)"
                ))
            }
        }
    }
}

fn request_type(program: &ir::Program, names: &ClassNames, body: &ir::RequestBody) -> String {
    match body.media {
        ir::RequestMedia::OctetStream => "Blob".to_string(),
        ir::RequestMedia::Json
        | ir::RequestMedia::JsonPatch
        | ir::RequestMedia::UrlEncoded
        | ir::RequestMedia::Multipart => apex_type(program, names, &body.ty),
    }
}

/// The client entry point: one field per manager, wired from a `BoxClient`.
fn client_class(fields: &[(String, String)]) -> String {
    let mut out = header();
    let _ = writeln!(out, "public with sharing class Box {{");
    for (field, class) in fields {
        let _ = writeln!(out, "    public final {class} {field};");
    }
    let _ = writeln!(out, "    public Box(BoxClient client) {{");
    for (field, class) in fields {
        let _ = writeln!(out, "        this.{field} = new {class}(client);");
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
    out
}

/// The runtime contract stubs. The generated managers compile against
/// these; the hand-written Apex runtime (a later slice) implements
/// `BoxClient` behind the same surface (auth, callout, retry, governor
/// limits) — the Go `gantryruntime` pattern.
fn runtime_stubs() -> Vec<GeneratedFile> {
    let request = format!(
        "{header}public class BoxRequest {{\n\
         \x20   public String method;\n\
         \x20   public String baseUrl;\n\
         \x20   public String path;\n\
         \x20   public Map<String, String> query = new Map<String, String>();\n\
         \x20   public Map<String, String> headers = new Map<String, String>();\n\
         \x20   public Object body;\n\
         \x20   public Blob binaryBody;\n\
         }}\n",
        header = header()
    );
    let response = format!(
        "{header}public class BoxResponse {{\n\
         \x20   public Integer statusCode;\n\
         \x20   public String body;\n\
         \x20   public Blob binaryBody;\n\
         }}\n",
        header = header()
    );
    let client = format!(
        "{header}// The runtime contract the generated managers call. The\n\
         // hand-written Apex runtime implements this (auth + callout + retry\n\
         // + governor limits); a non-2xx response is raised as an exception.\n\
         public interface BoxClient {{\n\
         \x20   BoxResponse send(BoxRequest request);\n\
         }}\n",
        header = header()
    );
    vec![
        stub("BoxRequest", request),
        stub("BoxResponse", response),
        stub("BoxClient", client),
    ]
}

fn stub(name: &str, content: String) -> GeneratedFile {
    GeneratedFile {
        path: format!("{CLASSES_DIR}/{name}.cls"),
        content,
    }
}

// --- helpers --------------------------------------------------------------

fn header() -> String {
    format!(
        "// Code generated by box-gantry {}. DO NOT EDIT.\n",
        env!("CARGO_PKG_VERSION")
    )
}

fn path_params(op: &ir::Operation) -> impl Iterator<Item = &ir::Param> {
    op.params
        .iter()
        .filter(|p| p.location == ir::ParamLocation::Path)
}

/// A method name unique within its manager class: `camelName` + Pascal
/// variation, then a numeric suffix on collision.
fn unique_method_name(op: &ir::Operation, used: &mut HashSet<String>) -> String {
    let mut base = camel(op.name.as_str());
    if let Some(variation) = &op.variation {
        base.push_str(&pascal(variation.as_str()));
    }
    if let Some(version) = &op.api_version {
        // Distinguish same-named operations across API versions.
        let suffix = version.0.replace(['.', '-'], "");
        if used.contains(&base) {
            base.push_str(&format!("V{suffix}"));
        }
    }
    let mut candidate = base.clone();
    for n in 2u32.. {
        if used.insert(candidate.clone()) {
            return candidate;
        }
        candidate = format!("{base}{n}");
    }
    unreachable!()
}

fn arg_name(param: &ir::Param) -> String {
    safe_word(&camel(param.name.as_str()))
}

/// Build the Apex path expression from structured segments (FR-2.2): string
/// concatenation with the path-parameter variables interpolated.
fn path_expr(path: &[ir::PathSegment]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for segment in path {
        parts.push("'/'".to_string());
        match segment {
            ir::PathSegment::Literal(text) => parts.push(format!("'{}'", escape(text))),
            ir::PathSegment::Parameter(name) => parts.push(camel(name.as_str())),
            ir::PathSegment::Composite(pieces) => {
                for piece in pieces {
                    match piece {
                        ir::PathPart::Literal(text) => parts.push(format!("'{}'", escape(text))),
                        ir::PathPart::Parameter(name) => parts.push(camel(name.as_str())),
                    }
                }
            }
        }
    }
    if parts.is_empty() {
        "''".to_string()
    } else {
        parts.join(" + ")
    }
}

fn http_method(method: ir::HttpMethod) -> &'static str {
    match method {
        ir::HttpMethod::Get => "GET",
        ir::HttpMethod::Put => "PUT",
        ir::HttpMethod::Post => "POST",
        ir::HttpMethod::Delete => "DELETE",
        ir::HttpMethod::Options => "OPTIONS",
        ir::HttpMethod::Head => "HEAD",
        ir::HttpMethod::Patch => "PATCH",
        ir::HttpMethod::Trace => "TRACE",
    }
}

fn base_url_key(base: ir::BaseUrl) -> &'static str {
    match base {
        ir::BaseUrl::Api => "api",
        ir::BaseUrl::ApiRoot => "api_root",
        ir::BaseUrl::Upload => "upload",
        ir::BaseUrl::UploadSession => "upload_session",
        ir::BaseUrl::OAuthAuthorize => "oauth_authorize",
        ir::BaseUrl::Download => "download",
    }
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}
