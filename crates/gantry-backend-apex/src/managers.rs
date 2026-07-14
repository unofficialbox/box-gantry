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

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use gantry_ir as ir;
use gantry_ir::naming::{camel, pascal};
use gantry_manifest::CapabilityManifest;
use gantry_sema::Analysis;
use gantry_synth::{PageStyle, PagedOperation};

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
    let infos = manager_infos(analysis, &names, limit);

    // Pagination is detected once, structurally, from the shared synth pass
    // (FR-7.3) — the same source the Go backend uses.
    let paged_ops = gantry_synth::detect_pagination(analysis);
    let paged: HashMap<usize, &PagedOperation> =
        paged_ops.iter().map(|p| (p.operation, p)).collect();

    // Page-class names share the flat namespace, so mint them into a set of
    // every name already taken (models + runtime + managers + client).
    let mut used_classes: HashSet<String> = names.names().map(str::to_string).collect();
    for reserved in RUNTIME_NAMES
        .iter()
        .chain(crate::runtime::RUNTIME_CLASS_NAMES)
    {
        used_classes.insert((*reserved).to_string());
    }
    for info in &infos {
        used_classes.insert(info.class.clone());
    }

    let mut files = Vec::new();
    let mut pages: Vec<GeneratedFile> = Vec::new();
    for info in &infos {
        let op_indices = &analysis.managers[&info.manager];
        files.push(GeneratedFile {
            path: format!("{CLASSES_DIR}/{}.cls", info.class),
            content: manager_class(
                program,
                &names,
                &info.class,
                &info.manager,
                op_indices,
                &paged,
                limit,
                &mut used_classes,
                &mut pages,
            ),
        });
    }
    files.extend(pages);

    let client_fields: Vec<(String, String)> = infos
        .iter()
        .map(|i| (i.field.clone(), i.class.clone()))
        .collect();
    files.push(GeneratedFile {
        path: format!("{CLASSES_DIR}/Box.cls"),
        content: client_class(&client_fields),
    });
    for stub in runtime_stubs() {
        files.push(stub);
    }
    files
}

/// A manager's stable names in the flat namespace: the Box tag, its minted
/// class name (`BoxFiles`, abbreviated to the identifier limit if needed),
/// and the `Box` client field (`files`). The single source both the manager
/// generator and the docs generator mint from, so they never disagree.
pub(crate) struct ManagerInfo {
    pub manager: String,
    pub class: String,
    pub field: String,
}

pub(crate) fn manager_infos(
    analysis: &Analysis<'_>,
    names: &ClassNames,
    limit: usize,
) -> Vec<ManagerInfo> {
    // Manager/client/stub names share the flat namespace with the model
    // classes, so mint them into a set seeded with every model name plus the
    // reserved contract + hand-written-runtime names — globally unique, within
    // the limit. Managers appear in analysis order (BTreeMap → sorted,
    // deterministic).
    let mut used: HashSet<String> = names.names().map(str::to_string).collect();
    for reserved in RUNTIME_NAMES
        .iter()
        .chain(crate::runtime::RUNTIME_CLASS_NAMES)
    {
        used.insert((*reserved).to_string());
    }
    analysis
        .managers
        .keys()
        .map(|manager| ManagerInfo {
            manager: manager.clone(),
            class: mint_unique(&format!("Box{}", pascal(manager)), limit, &mut used),
            field: safe_word(&camel(manager)),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn manager_class(
    program: &ir::Program,
    names: &ClassNames,
    class: &str,
    manager: &str,
    op_indices: &[usize],
    paged: &HashMap<usize, &PagedOperation>,
    limit: usize,
    used_classes: &mut HashSet<String>,
    pages: &mut Vec<GeneratedFile>,
) -> String {
    let mut out = header();
    let _ = writeln!(
        out,
        "/**\n * The `{manager}` resource manager — {count} operation(s).\n \
         * Reached through the `Box` client (`client.{field}`), never\n \
         * constructed directly.\n */",
        count = op_indices.len(),
        field = safe_word(&camel(manager)),
    );
    let _ = writeln!(out, "public with sharing class {class} {{");
    let _ = writeln!(out, "    private final BoxClient client;");
    let _ = writeln!(
        out,
        "    /** Wire the manager to the shared runtime client. */"
    );
    let _ = writeln!(out, "    public {class}(BoxClient client) {{");
    let _ = writeln!(out, "        this.client = client;");
    let _ = writeln!(out, "    }}");

    // Method names must be unique within the class.
    let mut used: HashSet<String> = HashSet::new();
    for &index in op_indices {
        let op = &program.operations[index];
        out.push('\n');
        let method_name = render_operation(&mut out, program, names, op, &mut used);
        // A paginated operation gains a governor-limit-aware `…Page` helper
        // and its own page class (D-131).
        if let Some(page) = paged.get(&index) {
            let page_class = mint_unique(
                &format!("{class}{}Page", pascal(&method_name)),
                limit,
                used_classes,
            );
            out.push('\n');
            render_paginate(
                &mut out,
                program,
                names,
                op,
                &method_name,
                &page_class,
                page,
            );
            pages.push(GeneratedFile {
                path: format!("{CLASSES_DIR}/{page_class}.cls"),
                content: page_class_source(program, names, &page_class, &method_name, page),
            });
        }
    }

    let _ = writeln!(out, "}}");
    out
}

/// One parameter of a generated method, in call order.
pub(crate) struct OpParam {
    pub name: String,
    pub apex_type: String,
    pub location: ir::ParamLocation,
    pub optional: bool,
}

/// The full shape of a generated manager method — the single source of truth
/// for the method signature, its ApexDoc, and the per-endpoint reference docs
/// (so the three can never drift). `method_name` is resolved by the caller
/// (it is stateful within a manager); everything else derives from the IR.
pub(crate) struct OpSignature {
    pub method_name: String,
    pub http: &'static str,
    pub path_display: String,
    pub return_ty: String,
    pub params: Vec<OpParam>,
    pub body_type: Option<String>,
    pub deprecated: bool,
}

pub(crate) fn op_signature(
    program: &ir::Program,
    names: &ClassNames,
    op: &ir::Operation,
    method_name: String,
) -> OpSignature {
    let mut params: Vec<OpParam> = Vec::new();
    // Path params first (always required, always `String`), then the rest in
    // spec order — matching the emitted signature.
    for param in path_params(op) {
        params.push(OpParam {
            name: arg_name(param),
            apex_type: "String".to_string(),
            location: ir::ParamLocation::Path,
            optional: false,
        });
    }
    for param in &op.params {
        if param.location != ir::ParamLocation::Path {
            params.push(OpParam {
                name: arg_name(param),
                apex_type: apex_type(program, names, &param.ty),
                location: param.location,
                optional: matches!(param.ty, ir::Type::Optional(_)),
            });
        }
    }
    OpSignature {
        method_name,
        http: http_method(op.method),
        path_display: path_display(&op.path),
        return_ty: response_type(program, names, &op.response),
        params,
        body_type: op.request.as_ref().map(|b| request_type(program, names, b)),
        deprecated: op.deprecated,
    }
}

impl OpSignature {
    /// The Apex parameter list (`Type name, …`) for the method signature.
    fn arg_list(&self) -> String {
        let mut args: Vec<String> = self
            .params
            .iter()
            .map(|p| format!("{} {}", p.apex_type, p.name))
            .collect();
        if let Some(body) = &self.body_type {
            args.push(format!("{body} body"));
        }
        args.join(", ")
    }

    /// The argument names to forward this signature to another method
    /// (a `…Page` helper delegating to the base method).
    fn forward_args(&self) -> String {
        let mut args: Vec<String> = self.params.iter().map(|p| p.name.clone()).collect();
        if self.body_type.is_some() {
            args.push("body".to_string());
        }
        args.join(", ")
    }

    /// The ApexDoc block for the method (structural — the spec carries no
    /// per-operation prose in the IR, so this documents the wire shape).
    fn apexdoc(&self) -> String {
        let mut doc = String::from("    /**\n");
        let _ = writeln!(
            doc,
            "     * `{http} {path}`",
            http = self.http,
            path = self.path_display
        );
        if self.deprecated {
            let _ = writeln!(doc, "     *\n     * @deprecated by the Box API");
        }
        if !self.params.is_empty() || self.body_type.is_some() {
            doc.push_str("     *\n");
        }
        for param in &self.params {
            let opt = if param.optional { ", optional" } else { "" };
            let _ = writeln!(
                doc,
                "     * @param {name} {loc} parameter{opt}",
                name = param.name,
                loc = param_location(param.location),
            );
        }
        if let Some(body) = &self.body_type {
            let _ = writeln!(doc, "     * @param body the request body (`{body}`)");
        }
        if self.return_ty != "void" {
            let _ = writeln!(doc, "     * @return `{}`", self.return_ty);
        }
        doc.push_str("     */\n");
        doc
    }
}

fn param_location(loc: ir::ParamLocation) -> &'static str {
    match loc {
        ir::ParamLocation::Path => "path",
        ir::ParamLocation::Query => "query",
        ir::ParamLocation::Header => "header",
    }
}

/// Render the path template for display/docs: `/files/{file_id}`.
pub(crate) fn path_display(path: &[ir::PathSegment]) -> String {
    let mut out = String::new();
    for segment in path {
        out.push('/');
        match segment {
            ir::PathSegment::Literal(text) => out.push_str(text),
            ir::PathSegment::Parameter(name) => {
                let _ = write!(out, "{{{}}}", name.as_str());
            }
            ir::PathSegment::Composite(pieces) => {
                for piece in pieces {
                    match piece {
                        ir::PathPart::Literal(text) => out.push_str(text),
                        ir::PathPart::Parameter(name) => {
                            let _ = write!(out, "{{{}}}", name.as_str());
                        }
                    }
                }
            }
        }
    }
    out
}

/// Render the operation's method; returns the (unique) method name so the
/// caller can wire a pagination helper to it.
fn render_operation(
    out: &mut String,
    program: &ir::Program,
    names: &ClassNames,
    op: &ir::Operation,
    used: &mut HashSet<String>,
) -> String {
    let method_name = unique_method_name(op, used);
    let sig = op_signature(program, names, op, method_name.clone());

    out.push_str(&sig.apexdoc());
    let _ = writeln!(
        out,
        "    public {return_ty} {method_name}({args}) {{",
        return_ty = sig.return_ty,
        method_name = sig.method_name,
        args = sig.arg_list(),
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
    method_name
}

/// Render the governor-limit-aware `…Page` helper: it delegates to the base
/// method for one page, then repackages the envelope's entries + cursor into
/// a typed page object (D-131). Apex has no lazy iterators and governor
/// limits forbid auto-fetching every page, so the caller loops explicitly:
/// call, check `hasMore`, pass the cursor back.
fn render_paginate(
    out: &mut String,
    program: &ir::Program,
    names: &ClassNames,
    op: &ir::Operation,
    base_method: &str,
    page_class: &str,
    page: &PagedOperation,
) {
    let sig = op_signature(program, names, op, format!("{base_method}Page"));
    let envelope = response_type(program, names, &op.response);
    let entries_field = safe_word(page.entries_wire.as_str());
    let cursor_field = safe_word(page.cursor_wire.as_str());
    let cont = match page.style {
        PageStyle::Marker => "pass `nextMarker` back as the cursor",
        PageStyle::Offset => "pass `nextOffset` back as the cursor",
    };

    let _ = writeln!(
        out,
        "    /**\n     * Fetch one page of `{base_method}` (governor-limit-aware\n     \
         * pagination). Read `hasMore` and {cont} to continue — Apex cannot\n     \
         * lazily iterate, and governor limits cap callouts per transaction.\n     \
         * @return `{page_class}`\n     */"
    );
    let _ = writeln!(
        out,
        "    public {page_class} {method}({args}) {{",
        method = sig.method_name,
        args = sig.arg_list(),
    );
    let _ = writeln!(
        out,
        "        {envelope} envelope = this.{base_method}({});",
        sig.forward_args()
    );
    let _ = writeln!(out, "        {page_class} page = new {page_class}();");
    let _ = writeln!(out, "        page.items = envelope.{entries_field};");
    match page.style {
        PageStyle::Marker => {
            let _ = writeln!(out, "        page.nextMarker = envelope.{cursor_field};");
            let _ = writeln!(
                out,
                "        page.hasMore = String.isNotBlank(envelope.{cursor_field});"
            );
        }
        PageStyle::Offset => {
            let _ = writeln!(out, "        Long current = 0;");
            let _ = writeln!(
                out,
                "        if (envelope.{cursor_field} != null) current = envelope.{cursor_field};"
            );
            let _ = writeln!(
                out,
                "        Integer count = (envelope.{entries_field} == null) ? 0 : envelope.{entries_field}.size();"
            );
            let _ = writeln!(out, "        page.nextOffset = current + count;");
            let _ = writeln!(out, "        page.hasMore = count > 0;");
        }
    }
    let _ = writeln!(out, "        return page;");
    let _ = writeln!(out, "    }}");
}

/// The per-operation page class: the typed slice + the next cursor + a
/// `hasMore` flag (D-131). No generics in Apex, so one class per paged op.
fn page_class_source(
    program: &ir::Program,
    names: &ClassNames,
    page_class: &str,
    base_method: &str,
    page: &PagedOperation,
) -> String {
    let element = apex_type(program, names, &page.element);
    let mut out = header();
    let _ = writeln!(
        out,
        "/** One page of `{base_method}` results (governor-limit-aware\n \
         * pagination). Loop while `hasMore`, feeding the cursor back. */"
    );
    let _ = writeln!(out, "public class {page_class} {{");
    let _ = writeln!(out, "    /** The items on this page. */");
    let _ = writeln!(out, "    public List<{element}> items;");
    match page.style {
        PageStyle::Marker => {
            let _ = writeln!(
                out,
                "    /** The cursor for the next page; blank when exhausted. */"
            );
            let _ = writeln!(out, "    public String nextMarker;");
        }
        PageStyle::Offset => {
            let _ = writeln!(out, "    /** The offset to request for the next page. */");
            let _ = writeln!(out, "    public Long nextOffset;");
        }
    }
    let _ = writeln!(out, "    /** Whether another page is available. */");
    let _ = writeln!(out, "    public Boolean hasMore;");
    let _ = writeln!(out, "}}");
    out
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
    let _ = writeln!(
        out,
        "/**\n * The Box SDK entry point: one field per resource manager.\n \
         * Construct it with a `BoxClient` (the hand-written runtime that\n \
         * performs auth + HTTP callouts), then reach an endpoint through its\n \
         * manager, e.g. `new Box(client).files.getById(...)`.\n */"
    );
    let _ = writeln!(out, "public with sharing class Box {{");
    for (field, class) in fields {
        let _ = writeln!(out, "    /** The `{field}` resource manager. */");
        let _ = writeln!(out, "    public final {class} {field};");
    }
    let _ = writeln!(
        out,
        "    /** Wire every manager to one shared runtime client. */"
    );
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
pub(crate) fn unique_method_name(op: &ir::Operation, used: &mut HashSet<String>) -> String {
    let mut base = camel(op.name.as_str());
    if let Some(variation) = &op.variation {
        base.push_str(&pascal(variation.as_str()));
    }
    // A method named for a reserved word (`delete`, `update`, …) is rejected
    // by the platform just like a field is; give it the same `_r` escape.
    base = safe_word(&base);
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
