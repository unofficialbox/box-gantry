//! Manager / client generation: IR operations → Java manager classes (TR-Java.3).
//!
//! One `<Pascal>Manager` class per `x-box-tag` (the `Analysis::managers`
//! grouping, shared with every backend), one **blocking** method per operation
//! that builds the URL + params + body and reaches the network **only** through
//! the runtime contract (`session.fetch(...)`, FR-5.2) — mirroring the Rust
//! (D-149) and TypeScript (D-159) managers, but synchronous and exception-based
//! per the Java manifest (Sync + Exceptions). A `Client` entry point holds one
//! manager per tag over a shared runtime `Session`; optional parameters bundle
//! into a per-operation options object.
//!
//! Everything the method bodies reference — the runtime (`com.box.sdk.runtime`),
//! the JSON codec (`com.box.sdk.core.Json`), the `Internal` helpers, and the
//! model types — is named by its fully-qualified name, so a manager file needs
//! no imports and can't collide with a schema type.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use gantry_ir as ir;
use gantry_ir::naming::{camel, pascal, snake};
use gantry_sema::Analysis;
use gantry_synth::{PageStyle, PagedOperation, detect_pagination};

use crate::models::{
    JAVA_KEYWORDS, component_ident, dedupe, package_names, struct_components, type_names,
};
use crate::{BuildInfo, GeneratedFile, INTERNAL_PKG, MANAGERS_PKG, MODEL_PKG, ROOT_PKG, java_path};

/// Fully-qualified runtime references used across the generated managers.
const RUNTIME: &str = "com.box.sdk.runtime.Runtime";
const REQUEST: &str = "com.box.sdk.runtime.Runtime.Request";
const SESSION: &str = "com.box.sdk.runtime.Runtime.Session";
const STREAM: &str = "com.box.sdk.runtime.Runtime.Stream";
const AUTH: &str = "com.box.sdk.runtime.Runtime.Auth";
const JSON: &str = "com.box.sdk.core.Json";
const INTERNAL: &str = "com.box.sdk.internal.Internal";
const UTF_8: &str = "java.nio.charset.StandardCharsets.UTF_8";

/// One planned manager: its grouping key, operation indices, and the
/// collision-free class + field names it lowers to.
pub(crate) struct ManagerPlan {
    pub(crate) ops: Vec<usize>,
    /// The manager class name, e.g. `FilesManager`.
    pub(crate) class: String,
    /// The `Client` field / constructor accessor, e.g. `files`.
    pub(crate) field: String,
}

/// The method name for an operation: `snake` base + variation + a `_v<version>`
/// suffix for a non-base API version (so base and versioned surfaces never
/// collide, FR-7.5), camelCased and keyword-guarded. Free-standing so the docs
/// generator names methods exactly as the manager printer does.
pub(crate) fn method_name(op: &ir::Operation, base_version: Option<&ir::ApiVersion>) -> String {
    let mut name = snake(op.name.as_str());
    if let Some(variation) = &op.variation {
        name.push('_');
        name.push_str(&snake(variation.as_str()));
    }
    if op.api_version.as_ref() != base_version
        && let Some(version) = &op.api_version
    {
        name.push_str("_v");
        name.push_str(&version.0.replace(['.', '-'], "_"));
    }
    keyword_safe(&camel(&name))
}

/// The deduped `(op index, method name)` list for a manager's operations — the
/// single source of truth the manager printer and docs both use, so a method
/// heading in the docs matches the emitted method name exactly.
pub(crate) fn deduped_methods(
    program: &ir::Program,
    ops: &[usize],
    base_version: Option<&ir::ApiVersion>,
) -> Vec<(usize, String)> {
    let mut used: Vec<String> = Vec::new();
    ops.iter()
        .map(|&i| {
            let name = dedupe(&mut used, method_name(&program.operations[i], base_version));
            (i, name)
        })
        .collect()
}

/// A field type's optionality wrapper in the model layer (D-110): a plain
/// value, a bare nullable reference, an `Optional<T>`, or a `Tristate<T>`.
enum OptLayer {
    Plain,
    Nullable,
    Optional,
    Tristate,
}

/// Everything needed to synthesize one paged operation's paginator class and
/// its `<method>Paginate` constructor (FR-7.3).
struct PaginationPlan {
    /// The top-level paginator class, e.g. `FilesGetFolderItemsPaginator`.
    class: String,
    /// The owning manager class, e.g. `FilesManager`.
    manager_class: String,
    /// The `<method>Paginate` constructor name.
    paginate: String,
    /// The plain method the paginator drives, e.g. `getFolderItems`.
    method: String,
    /// The nested options type, e.g. `FilesManager.GetFolderItemsOptions`.
    options_ty: String,
    /// The response envelope Java type.
    envelope: String,
    /// The element Java type yielded across pages.
    element: String,
    /// Stored constructor fields = required params (owned), then the body:
    /// `(ident, java_type)`.
    stored: Vec<(String, String)>,
    /// The forwarded arguments to the plain method (stored idents).
    forward: Vec<String>,
    /// The optional-parameter fields (the cursor included) copied into the
    /// paginator's private working options, so the caller's object is untouched.
    optional_fields: Vec<String>,
    /// The iterator's cursor-state field declaration (marker or offset).
    cursor_decl: String,
    /// The statement writing the cursor into the working options each page.
    cursor_set: String,
    /// The per-page cursor-advance statements (marker or offset style).
    cursor_advance: String,
    /// The expression extracting the page's element `List` from `_page`.
    entries_expr: String,
}

/// Allocate a collision-free class + field name per manager. `Analysis::managers`
/// is a `BTreeMap`, so managers are planned in sorted-key order (deterministic,
/// FR-6.2); the dedup accumulator runs across all managers so two keys that
/// normalize together still get distinct names.
pub(crate) fn plan_managers(analysis: &Analysis<'_>) -> Vec<ManagerPlan> {
    let mut used: Vec<String> = Vec::new();
    analysis
        .managers
        .iter()
        .map(|(key, ops)| {
            let base = dedupe(&mut used, manager_base(key));
            ManagerPlan {
                ops: ops.clone(),
                class: format!("{}Manager", pascal(&base)),
                field: keyword_safe(&camel(&base)),
            }
        })
        .collect()
}

/// The normalized, guarded base name for a manager key — the single root the
/// class (`Pascal…Manager`) and field (`camel…`) both derive from, so they
/// can't disagree.
fn manager_base(key: &str) -> String {
    let base = snake(key);
    if base.is_empty() {
        "manager".to_string()
    } else if base.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("m{base}")
    } else {
        base
    }
}

/// Generate the manager classes, the `Client`, and the `Internal` helper. The
/// runtime stub is emitted by the crate root (it comes from the contract, not
/// the IR).
pub fn generate_managers(analysis: &Analysis<'_>, build: &BuildInfo) -> Vec<GeneratedFile> {
    let program = analysis.program;
    let base_version = program
        .operations
        .first()
        .and_then(|op| op.api_version.as_ref());
    let printer = ManagerPrinter {
        program,
        base_version,
        packages: package_names(program),
        names: type_names(program),
        paged: detect_pagination(analysis)
            .into_iter()
            .map(|p| (p.operation, p))
            .collect(),
    };
    let plans = plan_managers(analysis);

    let mut files = Vec::new();
    for plan in &plans {
        let (content, paginators) = printer.manager_file(plan, build);
        files.push(GeneratedFile {
            path: java_path(MANAGERS_PKG, &plan.class),
            content,
        });
        files.extend(paginators);
    }
    files.push(GeneratedFile {
        path: java_path(ROOT_PKG, "Client"),
        content: client_file(&plans, build),
    });
    files.push(GeneratedFile {
        path: java_path(INTERNAL_PKG, "Internal"),
        content: internal_file(build),
    });
    files
}

/// The top-level `Client`: one field per manager over a shared runtime session,
/// constructed from an authentication flow.
fn client_file(plans: &[ManagerPlan], build: &BuildInfo) -> String {
    let mut out = header(build);
    let _ = writeln!(out, "package {ROOT_PKG};\n");
    out.push_str("/** The Box SDK entry point: a manager per API area over one session. */\n");
    out.push_str("public final class Client {\n");
    for plan in plans {
        let _ = writeln!(
            out,
            "    /** The {} API. */\n    public final {MANAGERS_PKG}.{} {};",
            plan.field, plan.class, plan.field
        );
    }
    out.push('\n');
    out.push_str("    /** Build a client for an authentication flow. */\n");
    let _ = writeln!(out, "    public Client({AUTH} auth) {{");
    let _ = writeln!(out, "        {SESSION} session = new {SESSION}(auth);");
    for plan in plans {
        let _ = writeln!(
            out,
            "        this.{} = new {MANAGERS_PKG}.{}(session);",
            plan.field, plan.class
        );
    }
    out.push_str("    }\n}\n");
    out
}

/// Renders operations to manager classes; carries the shared naming maps so
/// manager references agree with the model files.
struct ManagerPrinter<'p> {
    program: &'p ir::Program,
    base_version: Option<&'p ir::ApiVersion>,
    packages: BTreeMap<ir::ModulePath, String>,
    names: BTreeMap<ir::DeclId, String>,
    /// Paged operations by operation index (FR-7.3), so a manager method that
    /// pages also gets a `<method>Paginate` constructor + a paginator class.
    paged: HashMap<usize, PagedOperation>,
}

impl ManagerPrinter<'_> {
    /// One manager's complete `.java` file: the class, its shared session, an
    /// operation method per grouped op, and a nested options class per op that
    /// has optional parameters.
    fn manager_file(&self, plan: &ManagerPlan, build: &BuildInfo) -> (String, Vec<GeneratedFile>) {
        // Method names dedup per manager, so distinct ops that normalize to the
        // same name stay distinct — and the options class reuses the name. The
        // docs generator reuses this exact list (`deduped_methods`).
        let methods = deduped_methods(self.program, &plan.ops, self.base_version);

        let mut out = header(build);
        let _ = writeln!(out, "package {MANAGERS_PKG};\n");
        let _ = writeln!(out, "/** Operations for the {} API area. */", plan.field);
        let _ = writeln!(out, "public final class {} {{", plan.class);
        let _ = writeln!(out, "    private final {SESSION} session;\n");
        out.push_str("    /** Construct over a shared runtime session (used by {@link com.box.sdk.Client}). */\n");
        let _ = writeln!(
            out,
            "    public {}({SESSION} session) {{\n        this.session = session;\n    }}\n",
            plan.class
        );
        let mut paginators = Vec::new();
        for (i, name) in &methods {
            let op = &self.program.operations[*i];
            out.push_str(&self.operation(op, name));
            out.push('\n');
            // A paged operation also gets a `<method>Paginate` constructor plus a
            // top-level paginator class (FR-7.3) — right after the plain method,
            // which the paginator drives (URL/param/body logic is never dupli-
            // cated). A cursor shape we don't synthesize skips only the paginator
            // (the plain method still ships, VR-6), the same rule Rust/TS follow.
            if let Some(pplan) = self
                .paged
                .get(i)
                .and_then(|paged| self.pagination_plan(op, plan, name, paged))
            {
                out.push_str(&self.paginate_method(op, name, &pplan));
                out.push('\n');
                paginators.push(GeneratedFile {
                    path: java_path(MANAGERS_PKG, &pplan.class),
                    content: self.paginator_file(&pplan, build),
                });
            }
        }
        // Nested options classes, after the methods that reference them.
        for (i, name) in &methods {
            if let Some(options) = self.options_class(&self.program.operations[*i], name) {
                out.push_str(&options);
                out.push('\n');
            }
        }
        out.truncate(out.trim_end().len());
        out.push_str("\n}\n");
        (out, paginators)
    }

    /// One operation → a blocking method routing through the runtime contract.
    fn operation(&self, op: &ir::Operation, method: &str) -> String {
        let required: Vec<&ir::Param> = op
            .params
            .iter()
            .filter(|p| !matches!(p.ty, ir::Type::Optional(_)))
            .collect();
        let optional: Vec<&ir::Param> = op
            .params
            .iter()
            .filter(|p| matches!(p.ty, ir::Type::Optional(_)))
            .collect();

        // Required params — path params included — are positional method args.
        let mut sig: Vec<String> = required
            .iter()
            .map(|p| {
                format!(
                    "{} {}",
                    self.java_type(&p.ty),
                    component_ident(p.name.as_str())
                )
            })
            .collect();
        if let Some(body) = &op.request {
            sig.push(format!(
                "{} body",
                self.java_type(unwrap_optionality(&body.ty))
            ));
        }
        if !optional.is_empty() {
            sig.push(format!("{}Options options", pascal(method)));
        }

        let mut out = String::new();
        let _ = writeln!(
            out,
            "    /** {} {}. */",
            http_method(op.method),
            wire_path(op)
        );
        let _ = writeln!(
            out,
            "    public {} {method}({}) {{",
            self.return_type(op),
            sig.join(", ")
        );
        self.url(&mut out, op);
        let _ = writeln!(
            out,
            "        {REQUEST} _req = session.newRequest({:?}, _url.toString());",
            http_method(op.method)
        );
        self.apply_params(&mut out, &required, false);
        self.apply_params(&mut out, &optional, true);
        self.request_body(&mut out, op);
        self.fetch_and_decode(&mut out, op);
        out.push_str("    }\n");
        out
    }

    /// Build the request URL into a `StringBuilder _url` from the base-URL class
    /// and the structured path segments (path params percent-escaped).
    fn url(&self, out: &mut String, op: &ir::Operation) {
        let _ = writeln!(
            out,
            "        StringBuilder _url = new StringBuilder(session.baseUrl({:?}));",
            base_class(op.base_url)
        );
        for segment in &op.path {
            match segment {
                ir::PathSegment::Literal(text) => {
                    let _ = writeln!(out, "        _url.append({:?});", format!("/{text}"));
                }
                ir::PathSegment::Parameter(name) => {
                    let value = self.path_value(op, name);
                    let _ = writeln!(
                        out,
                        "        _url.append(\"/\").append({INTERNAL}.pathEscape({value}));"
                    );
                }
                ir::PathSegment::Composite(parts) => {
                    out.push_str("        _url.append(\"/\");\n");
                    for part in parts {
                        match part {
                            ir::PathPart::Literal(text) => {
                                let _ = writeln!(out, "        _url.append({text:?});");
                            }
                            ir::PathPart::Parameter(name) => {
                                let value = self.path_value(op, name);
                                let _ = writeln!(
                                    out,
                                    "        _url.append({INTERNAL}.pathEscape({value}));"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// The string form of a path parameter's value (path params are always
    /// present, so no optionality handling).
    fn path_value(&self, op: &ir::Operation, name: &ir::Identifier) -> String {
        let param = op
            .params
            .iter()
            .find(|p| p.location == ir::ParamLocation::Path && p.name == *name)
            .expect("a path segment names a declared path parameter");
        self.string_value(&component_ident(param.name.as_str()), &param.ty)
    }

    /// Apply query/header params to `_req`. Required params are applied
    /// unconditionally; optional ones are read from the options object and
    /// applied only when set. Path params were consumed building the URL.
    fn apply_params(&self, out: &mut String, params: &[&ir::Param], from_options: bool) {
        for param in params {
            let call = match param.location {
                ir::ParamLocation::Query => "withQuery",
                ir::ParamLocation::Header => "withHeader",
                ir::ParamLocation::Path => continue,
            };
            let wire = format!("{:?}", param.wire_name);
            if from_options {
                let ident = component_ident(param.name.as_str());
                let value = self.string_value("_v", unwrap_optionality(&param.ty));
                let _ = writeln!(
                    out,
                    "        if (options != null && options.{ident} != null) {{"
                );
                let _ = writeln!(
                    out,
                    "            {} _v = options.{ident};",
                    self.java_type(unwrap_optionality(&param.ty))
                );
                let _ = writeln!(
                    out,
                    "            _req = {RUNTIME}.{call}(_req, {wire}, {value});"
                );
                out.push_str("        }\n");
            } else {
                let value = self.string_value(&component_ident(param.name.as_str()), &param.ty);
                let _ = writeln!(
                    out,
                    "        _req = {RUNTIME}.{call}(_req, {wire}, {value});"
                );
            }
        }
    }

    /// Encode and attach the request body per its media type.
    fn request_body(&self, out: &mut String, op: &ir::Operation) {
        let Some(body) = &op.request else { return };
        let inner = unwrap_optionality(&body.ty);
        match body.media {
            ir::RequestMedia::Json | ir::RequestMedia::JsonPatch => {
                let _ = writeln!(
                    out,
                    "        byte[] _payload = {JSON}.write({}).getBytes({UTF_8});",
                    self.encode_value(inner, "body", 0)
                );
                let _ = writeln!(
                    out,
                    "        _req = {RUNTIME}.withJsonBody(_req, _payload);"
                );
            }
            ir::RequestMedia::OctetStream => {
                // The binary body is a `byte[]` parameter; send it as the request
                // stream (previously stubbed to `Stream.empty()`, which silently
                // dropped every octet-stream upload — e.g. chunked-upload parts).
                let _ = writeln!(
                    out,
                    "        _req = {RUNTIME}.withStreamBody(_req, {STREAM}.fromBytes(body), \"application/octet-stream\");"
                );
            }
            ir::RequestMedia::Multipart => {
                let _ = writeln!(
                    out,
                    "        byte[] _attributes = {JSON}.write({}).getBytes({UTF_8});",
                    self.encode_value(inner, "body", 0)
                );
                let _ = writeln!(
                    out,
                    "        _req = {RUNTIME}.withMultipartBody(_req, _attributes, \"file\", {STREAM}.empty());"
                );
            }
            ir::RequestMedia::UrlEncoded => self.url_encoded_body(out, inner),
        }
    }

    /// An `application/x-www-form-urlencoded` body (the OAuth2 token endpoints).
    /// A struct body builds an ordered field map (each optional/nullable field
    /// applied only when set); anything else falls back to a JSON form value.
    fn url_encoded_body(&self, out: &mut String, ty: &ir::Type) {
        if let ir::Type::Decl(id) = ty
            && let ir::DeclKind::Struct(s) = &self.program.decl(*id).kind
        {
            out.push_str(
                "        java.util.Map<String, String> _form = new java.util.LinkedHashMap<>();\n",
            );
            for (ident, field) in struct_components(s) {
                self.form_field(out, &ident, field);
            }
            let _ = writeln!(
                out,
                "        byte[] _payload = {INTERNAL}.formEncode(_form).getBytes({UTF_8});"
            );
        } else {
            let _ = writeln!(
                out,
                "        byte[] _payload = {JSON}.write({}).getBytes({UTF_8});",
                self.encode_value(ty, "body", 0)
            );
        }
        let _ = writeln!(
            out,
            "        _req = {RUNTIME}.withFormBody(_req, _payload);"
        );
    }

    /// One form-body field → a conditional `_form.put`, matching the accessor
    /// shape the model layer gives the field: a tri-state / plain optional is
    /// applied only when it carries a value, a nullable only when non-null, a
    /// required field always.
    fn form_field(&self, out: &mut String, ident: &str, field: &ir::Field) {
        let wire = format!("{:?}", field.wire_name);
        let acc = format!("body.{ident}()");
        // `if let` chains (not a `match`) so there is no wildcard over `ir::Type`.
        let ty = self.resolve_alias(&field.ty);
        if let ir::Type::Optional(inner) = &ty {
            if let ir::Type::Nullable(n) = &**inner {
                let value = self.form_value(&format!("{acc}.value()"), n);
                let _ = writeln!(
                    out,
                    "        if ({acc}.isPresent()) {{ _form.put({wire}, {value}); }}"
                );
            } else {
                let value = self.form_value("_v", inner);
                let _ = writeln!(out, "        if ({acc}.isPresent()) {{");
                let _ = writeln!(
                    out,
                    "            {} _v = {acc}.get();",
                    self.java_type(inner)
                );
                let _ = writeln!(out, "            _form.put({wire}, {value});");
                out.push_str("        }\n");
            }
        } else if let ir::Type::Nullable(inner) = &ty {
            let value = self.form_value(&acc, inner);
            let _ = writeln!(
                out,
                "        if ({acc} != null) {{ _form.put({wire}, {value}); }}"
            );
        } else {
            let value = self.form_value(&acc, &ty);
            let _ = writeln!(out, "        _form.put({wire}, {value});");
        }
    }

    /// Resolve a top-level alias chain to the type it stands for.
    fn resolve_alias(&self, ty: &ir::Type) -> ir::Type {
        if let ir::Type::Decl(id) = ty
            && let ir::DeclKind::Alias(target) = &self.program.decl(*id).kind
        {
            return self.resolve_alias(&target.clone());
        }
        ty.clone()
    }

    /// Fetch through the session and decode per the response shape. Blocking and
    /// exception-based: a failed request or decode throws (the runtime's
    /// `BoxApiException`), so no error is threaded through the return type.
    fn fetch_and_decode(&self, out: &mut String, op: &ir::Operation) {
        match &op.response {
            ir::ResponseShape::None => {
                out.push_str("        session.fetch(_req);\n");
            }
            ir::ResponseShape::Json(ty) => {
                out.push_str("        Object _tree = ");
                let _ = writeln!(
                    out,
                    "{JSON}.parse(new String({RUNTIME}.responseBytes(session.fetch(_req)), {UTF_8}));"
                );
                let _ = writeln!(
                    out,
                    "        return {};",
                    self.decode_value(unwrap_optionality(ty), "_tree", 0)
                );
            }
            ir::ResponseShape::Binary => {
                let _ = writeln!(
                    out,
                    "        return {RUNTIME}.responseStream(session.fetch(_req));"
                );
            }
            ir::ResponseShape::Text => {
                out.push_str(&format!(
                    "        return new String({RUNTIME}.responseBytes(session.fetch(_req)), {UTF_8});\n"
                ));
            }
            ir::ResponseShape::Redirect => {
                let _ = writeln!(
                    out,
                    "        return {RUNTIME}.responseHeader(session.fetch(_req), \"Location\");"
                );
            }
        }
    }

    // --- pagination (FR-7.3) -----------------------------------------------

    /// Resolve everything needed to synthesize a paginator for `op`, or `None`
    /// when the cursor shape is not one we generate (the plain method still
    /// ships — a documented fallback, VR-6, never wrong code). Mirrors the Rust
    /// (D-154) and TypeScript (D-165) pagination slices.
    fn pagination_plan(
        &self,
        op: &ir::Operation,
        manager: &ManagerPlan,
        method: &str,
        paged: &PagedOperation,
    ) -> Option<PaginationPlan> {
        // The response envelope (a struct) carries `entries` + the cursor field.
        let ir::ResponseShape::Json(response_ty) = &op.response else {
            return None;
        };
        let ir::DeclKind::Struct(envelope) = &self.program.decl(decl_of(response_ty)?).kind else {
            return None;
        };
        let components = struct_components(envelope);
        let component = |wire: &str| {
            components
                .iter()
                .find(|(_, f)| f.wire_name == wire)
                .map(|(ident, f)| (ident.clone(), (*f).clone()))
        };
        let (entries_ident, entries_field) = component(&paged.entries_wire)?;
        let (cursor_ident, cursor_field) = component(&paged.cursor_wire)?;

        // The cursor query parameter (detection guaranteed it is optional).
        let cursor_param = op
            .params
            .iter()
            .find(|p| p.location == ir::ParamLocation::Query && p.wire_name == paged.param_wire)?;
        let param_ident = component_ident(cursor_param.name.as_str());

        let entries_expr =
            self.entries_expr(&entries_field.ty, &format!("_page.{entries_ident}()"));

        // Marker: the request marker must be a string, the response cursor a
        // string or int (converted). Offset: the request offset must be an int
        // and we advance by page length (the response cursor is never read).
        // Anything else is a shape we don't synthesize — skip the paginator.
        let (cursor_decl, cursor_set, cursor_advance) = match paged.style {
            PageStyle::Marker => {
                if !matches!(unwrap_optionality(&cursor_param.ty), ir::Type::String) {
                    return None;
                }
                let acc = format!("_page.{cursor_ident}()");
                let next = self.cursor_expr(&cursor_field.ty, &acc);
                let (_, inner) = self.optionality_layer(&cursor_field.ty);
                let inner = self.resolve_alias(&inner);
                // The response cursor is a string (threaded directly) or an int
                // (stringified). Any other shape we don't synthesize — skip the
                // paginator (`if`/`matches!`, not a wildcard `match` — NF-1).
                let advance = if matches!(inner, ir::Type::String) {
                    format!(
                        "                    String _next = {next};\n\
                         \x20                   if (_next != null && !_next.isEmpty()) {{\n\
                         \x20                       _cursor = _next;\n\
                         \x20                   }} else {{\n\
                         \x20                       _done = true;\n\
                         \x20                   }}\n"
                    )
                } else if matches!(inner, ir::Type::Int64) {
                    format!(
                        "                    Long _next = {next};\n\
                         \x20                   if (_next != null) {{\n\
                         \x20                       _cursor = _next.toString();\n\
                         \x20                   }} else {{\n\
                         \x20                       _done = true;\n\
                         \x20                   }}\n"
                    )
                } else {
                    return None;
                };
                (
                    "            private String _cursor = _opts.".to_string()
                        + &param_ident
                        + ";\n",
                    format!("                    _opts.{param_ident} = _cursor;\n"),
                    advance,
                )
            }
            PageStyle::Offset => {
                if !matches!(unwrap_optionality(&cursor_param.ty), ir::Type::Int64) {
                    return None;
                }
                (
                    format!(
                        "            private long _cursor = _opts.{param_ident} != null ? _opts.{param_ident} : 0L;\n"
                    ),
                    format!("                    _opts.{param_ident} = _cursor;\n"),
                    "                    if (_items.isEmpty()) {\n\
                     \x20                       _done = true;\n\
                     \x20                   } else {\n\
                     \x20                       _cursor += _items.size();\n\
                     \x20                   }\n"
                        .to_string(),
                )
            }
        };

        // Stored constructor fields = the operation's required params (path
        // params included), then the body — everything the plain method needs
        // besides the (threaded) options.
        let mut stored: Vec<(String, String)> = op
            .params
            .iter()
            .filter(|p| !matches!(p.ty, ir::Type::Optional(_)))
            .map(|p| (component_ident(p.name.as_str()), self.java_type(&p.ty)))
            .collect();
        if let Some(body) = &op.request {
            stored.push((
                "body".to_string(),
                self.java_type(unwrap_optionality(&body.ty)),
            ));
        }
        let forward: Vec<String> = stored.iter().map(|(ident, _)| ident.clone()).collect();

        // Every optional parameter (the cursor included) — the fields the
        // paginator copies into its private working options, so the caller's
        // options object is never mutated and each pass is independent.
        let optional_fields: Vec<String> = op
            .params
            .iter()
            .filter(|p| matches!(p.ty, ir::Type::Optional(_)))
            .map(|p| component_ident(p.name.as_str()))
            .collect();

        let prefix = manager
            .class
            .strip_suffix("Manager")
            .unwrap_or(&manager.class);
        Some(PaginationPlan {
            class: format!("{prefix}{}Paginator", pascal(method)),
            manager_class: manager.class.clone(),
            paginate: format!("{method}Paginate"),
            method: method.to_string(),
            options_ty: format!("{}.{}Options", manager.class, pascal(method)),
            envelope: self.java_type(unwrap_optionality(response_ty)),
            element: self.java_type(&paged.element),
            stored,
            forward,
            optional_fields,
            cursor_decl,
            cursor_set,
            cursor_advance,
            entries_expr,
        })
    }

    /// The `<method>Paginate` constructor on the manager: same required args as
    /// the plain method (path params, then the body), then the options, handing
    /// them to the paginator over the shared session.
    fn paginate_method(&self, op: &ir::Operation, method: &str, plan: &PaginationPlan) -> String {
        let mut sig: Vec<String> = op
            .params
            .iter()
            .filter(|p| !matches!(p.ty, ir::Type::Optional(_)))
            .map(|p| {
                format!(
                    "{} {}",
                    self.java_type(&p.ty),
                    component_ident(p.name.as_str())
                )
            })
            .collect();
        if let Some(body) = &op.request {
            sig.push(format!(
                "{} body",
                self.java_type(unwrap_optionality(&body.ty))
            ));
        }
        sig.push(format!("{}Options options", pascal(method)));

        let mut args = vec!["session".to_string()];
        args.extend(plan.forward.iter().cloned());
        args.push("options".to_string());

        let mut out = String::new();
        let _ = writeln!(
            out,
            "    /** Iterate every {} across pages, threading the cursor (FR-7.3). */",
            plan.element
        );
        let _ = writeln!(
            out,
            "    public {} {}({}) {{",
            plan.class,
            plan.paginate,
            sig.join(", ")
        );
        let _ = writeln!(
            out,
            "        return new {}({});",
            plan.class,
            args.join(", ")
        );
        out.push_str("    }\n");
        out
    }

    /// The top-level paginator class: a re-iterable `Iterable` whose iterator
    /// drains a page buffer, then fetches the next page through the plain method
    /// and advances the cursor. Each `iterator()` is an independent pass over a
    /// private copy of the request options, so the caller's options object is
    /// never mutated — the idiomatic Java analogue of Rust's `Paginator::next`
    /// (which owns a cloned options) and TS's `async *` generator (a spread copy).
    fn paginator_file(&self, plan: &PaginationPlan, build: &BuildInfo) -> String {
        let PaginationPlan {
            class,
            manager_class,
            method,
            options_ty,
            envelope,
            element,
            stored,
            forward,
            optional_fields,
            cursor_decl,
            cursor_set,
            cursor_advance,
            entries_expr,
            ..
        } = plan;

        let mut out = header(build);
        let _ = writeln!(out, "package {MANAGERS_PKG};\n");
        let _ = writeln!(
            out,
            "/**\n * Paginator over {{@link {manager_class}#{method}}}, iterating every\n\
             \x20* {{@code {element}}} across pages (FR-7.3). Threads the response cursor back\n\
             \x20* into the request so callers can write {{@code for (var item : paginator)}}.\n\
             \x20* Re-iterable: each {{@code iterator()}} is an independent pass over a private\n\
             \x20* copy of the options, so the caller's options are never mutated.\n */"
        );
        let _ = writeln!(
            out,
            "public final class {class} implements java.lang.Iterable<{element}> {{"
        );
        let _ = writeln!(out, "    private final {SESSION} session;");
        for (ident, ty) in stored {
            let _ = writeln!(out, "    private final {ty} {ident};");
        }
        let _ = writeln!(out, "    private final {options_ty} options;\n");

        // Constructor (package-private — callers use `<method>Paginate`).
        let mut params = vec![format!("{SESSION} session")];
        params.extend(stored.iter().map(|(ident, ty)| format!("{ty} {ident}")));
        params.push(format!("{options_ty} options"));
        let _ = writeln!(out, "    {class}({}) {{", params.join(", "));
        out.push_str("        this.session = session;\n");
        for (ident, _) in stored {
            let _ = writeln!(out, "        this.{ident} = {ident};");
        }
        out.push_str("        this.options = options;\n    }\n\n");

        // A fresh private copy of the caller's options per pass (never mutate the
        // caller's object), so every `iterator()` re-seeds the cursor from the
        // originals and re-iteration is independent.
        let _ = writeln!(
            out,
            "    private {options_ty} _freshOptions() {{\n\
             \x20       {options_ty} _o = new {options_ty}();\n\
             \x20       if (options != null) {{"
        );
        for field in optional_fields {
            let _ = writeln!(out, "            _o.{field} = options.{field};");
        }
        out.push_str("        }\n        return _o;\n    }\n\n");

        // iterator()
        let _ = writeln!(out, "    @Override");
        let _ = writeln!(
            out,
            "    public java.util.Iterator<{element}> iterator() {{"
        );
        let _ = writeln!(out, "        return new java.util.Iterator<{element}>() {{");
        let _ = writeln!(
            out,
            "            private final {manager_class} _manager = new {manager_class}(session);"
        );
        let _ = writeln!(
            out,
            "            private final {options_ty} _opts = _freshOptions();"
        );
        out.push_str(cursor_decl);
        let _ = writeln!(
            out,
            "            private java.util.Iterator<{element}> _buffer = java.util.Collections.emptyIterator();"
        );
        out.push_str("            private boolean _done = false;\n\n");

        out.push_str("            private void _advance() {\n");
        out.push_str("                while (!_buffer.hasNext() && !_done) {\n");
        out.push_str(cursor_set);
        let _ = writeln!(
            out,
            "                    {envelope} _page = _manager.{method}({});",
            {
                let mut a = forward.clone();
                a.push("_opts".to_string());
                a.join(", ")
            }
        );
        let _ = writeln!(
            out,
            "                    java.util.List<{element}> _items = {entries_expr};"
        );
        out.push_str("                    _buffer = _items.iterator();\n");
        out.push_str(cursor_advance);
        out.push_str("                }\n            }\n\n");

        out.push_str(
            "            @Override\n\
             \x20           public boolean hasNext() {\n\
             \x20               _advance();\n\
             \x20               return _buffer.hasNext();\n\
             \x20           }\n\n",
        );
        let _ = writeln!(out, "            @Override");
        let _ = writeln!(out, "            public {element} next() {{");
        out.push_str(
            "                _advance();\n\
             \x20               if (!_buffer.hasNext()) {\n\
             \x20                   throw new java.util.NoSuchElementException();\n\
             \x20               }\n\
             \x20               return _buffer.next();\n\
             \x20           }\n\
             \x20       };\n\
             \x20   }\n\
             }\n",
        );
        out
    }

    /// A Java expression yielding the page's element `List` (empty when the
    /// `entries` field is absent or null), peeling the field's optionality layer.
    fn entries_expr(&self, ty: &ir::Type, acc: &str) -> String {
        match self.optionality_layer(ty).0 {
            OptLayer::Plain => acc.to_string(),
            OptLayer::Nullable => format!("{acc} != null ? {acc} : java.util.List.of()"),
            OptLayer::Optional => format!("{acc}.orElse(java.util.List.of())"),
            // A `Tristate` PRESENT value is non-null by construction, but guard
            // it anyway so absent, null, and present-with-value all coalesce to
            // an empty list per the documented contract.
            OptLayer::Tristate => format!(
                "{acc}.isPresent() && {acc}.value() != null ? {acc}.value() : java.util.List.of()"
            ),
        }
    }

    /// A Java expression yielding the response cursor value (or `null` when
    /// absent), peeling the field's optionality layer.
    fn cursor_expr(&self, ty: &ir::Type, acc: &str) -> String {
        match self.optionality_layer(ty).0 {
            OptLayer::Plain | OptLayer::Nullable => acc.to_string(),
            OptLayer::Optional => format!("{acc}.orElse(null)"),
            OptLayer::Tristate => format!("{acc}.isPresent() ? {acc}.value() : null"),
        }
    }

    /// Classify a field type's optionality wrapper (matching the model layer's
    /// `component_type`, D-110) and return the inner base type. Top-level
    /// aliases resolve through first, as the model does.
    fn optionality_layer(&self, ty: &ir::Type) -> (OptLayer, ir::Type) {
        // `if let` chains, not a `match`, so there is no wildcard over the many
        // `ir::Type` variants (NF-1).
        let ty = self.resolve_alias(ty);
        if let ir::Type::Optional(inner) = &ty {
            if let ir::Type::Nullable(nullable) = &**inner {
                return (OptLayer::Tristate, (**nullable).clone());
            }
            return (OptLayer::Optional, (**inner).clone());
        }
        if let ir::Type::Nullable(inner) = &ty {
            return (OptLayer::Nullable, (**inner).clone());
        }
        (OptLayer::Plain, ty)
    }

    /// A nested options class bundling the operation's optional parameters
    /// (public nullable fields), or `None` when it has none.
    fn options_class(&self, op: &ir::Operation, method: &str) -> Option<String> {
        let optional: Vec<&ir::Param> = op
            .params
            .iter()
            .filter(|p| matches!(p.ty, ir::Type::Optional(_)))
            .collect();
        if optional.is_empty() {
            return None;
        }
        let mut out = String::new();
        let _ = writeln!(
            out,
            "    /** Optional parameters for {method}. */\n    public static final class {}Options {{",
            pascal(method)
        );
        for param in &optional {
            let _ = writeln!(
                out,
                "        /** The {} parameter. */\n        public {} {};",
                param.wire_name,
                self.java_type(unwrap_optionality(&param.ty)),
                component_ident(param.name.as_str())
            );
        }
        out.push_str("    }\n");
        Some(out)
    }

    /// The method's return type: the response's value type directly (fallibility
    /// is a thrown exception, not a wrapper).
    fn return_type(&self, op: &ir::Operation) -> String {
        match &op.response {
            ir::ResponseShape::None => "void".to_string(),
            ir::ResponseShape::Json(ty) => self.java_type(unwrap_optionality(ty)),
            ir::ResponseShape::Binary => STREAM.to_string(),
            ir::ResponseShape::Text | ir::ResponseShape::Redirect => "String".to_string(),
        }
    }

    // --- type + value lowering (FQN model references, no imports) -----------

    /// A parameter / body / return Java type. Mirrors the model layer's `bare`
    /// mapping, but fully-qualifies model declarations (managers live in a
    /// different package).
    fn java_type(&self, ty: &ir::Type) -> String {
        match ty {
            ir::Type::Bool => "Boolean".to_string(),
            ir::Type::Int64 => "Long".to_string(),
            ir::Type::Float64 => "Double".to_string(),
            ir::Type::String => "String".to_string(),
            ir::Type::Date => "java.time.LocalDate".to_string(),
            ir::Type::DateTime => "java.time.OffsetDateTime".to_string(),
            ir::Type::Binary => "byte[]".to_string(),
            ir::Type::JsonValue => "Object".to_string(),
            ir::Type::List(inner) => format!("java.util.List<{}>", self.java_type(inner)),
            ir::Type::Map(inner) => format!("java.util.Map<String, {}>", self.java_type(inner)),
            ir::Type::Nullable(inner) | ir::Type::Optional(inner) => self.java_type(inner),
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(target) => self.java_type(target),
                ir::DeclKind::Struct(_) | ir::DeclKind::Union(_) | ir::DeclKind::Enum(_) => {
                    self.model_fqn(*id)
                }
            },
        }
    }

    /// The fully-qualified model type name for a declaration.
    fn model_fqn(&self, id: ir::DeclId) -> String {
        let module = &self.program.decl(id).module;
        format!("{MODEL_PKG}.{}.{}", self.packages[module], self.names[&id])
    }

    /// The string form of a (non-optional) value for a query/header/path slot.
    /// A list of scalars comma-joins (`?fields=a,b`); a scalar stringifies to one
    /// token; anything complex (a struct, a map, a list of structs) JSON-encodes
    /// — the Box `mdfilters` convention.
    fn string_value(&self, expr: &str, ty: &ir::Type) -> String {
        // `if let`/`if`, not a `match`, so there is no wildcard over `ir::Type`.
        if let ir::Type::List(inner) = ty {
            if matches!(**inner, ir::Type::String) {
                return format!("String.join(\",\", {expr})");
            }
            if self.is_scalar(inner) {
                return format!(
                    "{INTERNAL}.join({expr}, _e -> {})",
                    self.scalar_string("_e", inner)
                );
            }
        }
        if self.is_scalar(ty) {
            self.scalar_string(expr, ty)
        } else {
            format!("{JSON}.write({})", self.encode_value(ty, expr, 0))
        }
    }

    /// The string form of a single scalar value (also the per-element form for a
    /// comma-joined list). Enums stringify to their raw wire value — no JSON
    /// quotes — so `?direction=ASC`, not `?direction=%22ASC%22`.
    fn scalar_string(&self, expr: &str, ty: &ir::Type) -> String {
        match ty {
            ir::Type::String => expr.to_string(),
            ir::Type::Bool | ir::Type::Int64 | ir::Type::Float64 => {
                format!("String.valueOf({expr})")
            }
            ir::Type::Date | ir::Type::DateTime => format!("{expr}.toString()"),
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(target) => self.scalar_string(expr, target),
                // Open enum: the raw `String` newtype; closed enum: its wire
                // spelling. Both are exposed by the model's `toJson`/accessor.
                ir::DeclKind::Enum(e) if matches!(e.extensibility, ir::Extensibility::Open) => {
                    format!("{expr}.value()")
                }
                ir::DeclKind::Enum(_) => format!("{expr}.toJson()"),
                ir::DeclKind::Struct(_) | ir::DeclKind::Union(_) => {
                    format!("{JSON}.write({})", self.encode_value(ty, expr, 0))
                }
            },
            // Complex values JSON-encode (enumerated, never wildcarded — NF-1).
            ir::Type::Binary
            | ir::Type::List(_)
            | ir::Type::Map(_)
            | ir::Type::Optional(_)
            | ir::Type::Nullable(_)
            | ir::Type::JsonValue => format!("{JSON}.write({})", self.encode_value(ty, expr, 0)),
        }
    }

    /// Whether a type stringifies to one token (scalars + enums); complex types
    /// (structs, unions, maps, lists, free-form JSON, binary) JSON-encode.
    fn is_scalar(&self, ty: &ir::Type) -> bool {
        match ty {
            ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::Date
            | ir::Type::DateTime => true,
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(target) => self.is_scalar(target),
                ir::DeclKind::Enum(_) => true,
                ir::DeclKind::Struct(_) | ir::DeclKind::Union(_) => false,
            },
            ir::Type::Binary
            | ir::Type::List(_)
            | ir::Type::Map(_)
            | ir::Type::Optional(_)
            | ir::Type::Nullable(_)
            | ir::Type::JsonValue => false,
        }
    }

    /// The string form of a form-body field value (mirrors `string_value`;
    /// structs/complex values JSON-encode).
    fn form_value(&self, expr: &str, ty: &ir::Type) -> String {
        self.string_value(expr, ty)
    }

    /// Encode a value to a JSON-tree `Object` for a request body — the model
    /// layer's `encode_bare`, standalone (encode only ever calls `.toJson()`,
    /// which needs no type qualification).
    fn encode_value(&self, ty: &ir::Type, expr: &str, depth: usize) -> String {
        if is_json_writable(self.program, ty) {
            return expr.to_string();
        }
        match ty {
            ir::Type::Date | ir::Type::DateTime => {
                format!("({expr} == null ? null : {expr}.toString())")
            }
            ir::Type::Binary => {
                format!(
                    "({expr} == null ? null : java.util.Base64.getEncoder().encodeToString({expr}))"
                )
            }
            ir::Type::List(inner) => {
                let v = format!("_x{depth}");
                format!(
                    "{JSON}.encodeList({expr}, {v} -> {})",
                    self.encode_value(inner, &v, depth + 1)
                )
            }
            ir::Type::Map(inner) => {
                let v = format!("_x{depth}");
                format!(
                    "{JSON}.encodeMap({expr}, {v} -> {})",
                    self.encode_value(inner, &v, depth + 1)
                )
            }
            ir::Type::Nullable(inner) | ir::Type::Optional(inner) => {
                self.encode_value(inner, expr, depth)
            }
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(target) => self.encode_value(target, expr, depth),
                ir::DeclKind::Struct(_) | ir::DeclKind::Union(_) | ir::DeclKind::Enum(_) => {
                    format!("({expr} == null ? null : {expr}.toJson())")
                }
            },
            ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::JsonValue => expr.to_string(),
        }
    }

    /// Decode a value from a parsed JSON-tree `Object` for a response — the
    /// model layer's `decode_bare`, with model references fully-qualified.
    fn decode_value(&self, ty: &ir::Type, expr: &str, depth: usize) -> String {
        match ty {
            ir::Type::Bool => format!("{JSON}.asBoolean({expr})"),
            ir::Type::Int64 => format!("{JSON}.asLong({expr})"),
            ir::Type::Float64 => format!("{JSON}.asDouble({expr})"),
            ir::Type::String => format!("{JSON}.asString({expr})"),
            ir::Type::JsonValue => expr.to_string(),
            ir::Type::Date => {
                format!(
                    "({expr} == null ? null : java.time.LocalDate.parse({JSON}.asString({expr})))"
                )
            }
            ir::Type::DateTime => {
                format!(
                    "({expr} == null ? null : java.time.OffsetDateTime.parse({JSON}.asString({expr})))"
                )
            }
            ir::Type::Binary => {
                format!(
                    "({expr} == null ? null : java.util.Base64.getDecoder().decode({JSON}.asString({expr})))"
                )
            }
            ir::Type::List(inner) => {
                let v = format!("_x{depth}");
                format!(
                    "{JSON}.decodeList({expr}, {v} -> {})",
                    self.decode_value(inner, &v, depth + 1)
                )
            }
            ir::Type::Map(inner) => {
                let v = format!("_x{depth}");
                format!(
                    "{JSON}.decodeMap({expr}, {v} -> {})",
                    self.decode_value(inner, &v, depth + 1)
                )
            }
            ir::Type::Nullable(inner) | ir::Type::Optional(inner) => {
                self.decode_value(inner, expr, depth)
            }
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(target) => self.decode_value(target, expr, depth),
                ir::DeclKind::Struct(_) | ir::DeclKind::Union(_) | ir::DeclKind::Enum(_) => {
                    format!(
                        "({expr} == null ? null : {}.fromJson({expr}))",
                        self.model_fqn(*id)
                    )
                }
            },
        }
    }
}

/// Whether a value of this type is already a JSON-tree `Object` (see the model
/// layer's `is_json_writable`).
fn is_json_writable(program: &ir::Program, ty: &ir::Type) -> bool {
    match ty {
        ir::Type::Bool
        | ir::Type::Int64
        | ir::Type::Float64
        | ir::Type::String
        | ir::Type::JsonValue => true,
        ir::Type::List(inner)
        | ir::Type::Map(inner)
        | ir::Type::Nullable(inner)
        | ir::Type::Optional(inner) => is_json_writable(program, inner),
        ir::Type::Date | ir::Type::DateTime | ir::Type::Binary => false,
        ir::Type::Decl(id) => match &program.decl(*id).kind {
            ir::DeclKind::Alias(target) => is_json_writable(program, target),
            ir::DeclKind::Struct(_) | ir::DeclKind::Union(_) | ir::DeclKind::Enum(_) => false,
        },
    }
}

/// Strip the outer optionality constructors (`Optional`/`Nullable`).
fn unwrap_optionality(ty: &ir::Type) -> &ir::Type {
    match ty {
        ir::Type::Optional(inner) | ir::Type::Nullable(inner) => unwrap_optionality(inner),
        ir::Type::Bool
        | ir::Type::Int64
        | ir::Type::Float64
        | ir::Type::String
        | ir::Type::Date
        | ir::Type::DateTime
        | ir::Type::Binary
        | ir::Type::List(_)
        | ir::Type::Map(_)
        | ir::Type::Decl(_)
        | ir::Type::JsonValue => ty,
    }
}

/// The declaration a type resolves to, peeling optionality; `None` for a
/// non-declaration type.
fn decl_of(ty: &ir::Type) -> Option<ir::DeclId> {
    match unwrap_optionality(ty) {
        ir::Type::Decl(id) => Some(*id),
        ir::Type::Bool
        | ir::Type::Int64
        | ir::Type::Float64
        | ir::Type::String
        | ir::Type::Date
        | ir::Type::DateTime
        | ir::Type::Binary
        | ir::Type::Optional(_)
        | ir::Type::Nullable(_)
        | ir::Type::List(_)
        | ir::Type::Map(_)
        | ir::Type::JsonValue => None,
    }
}

/// camelCase, suffix-escaped if it collides with a Java keyword.
fn keyword_safe(name: &str) -> String {
    if JAVA_KEYWORDS.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// The uppercase HTTP method token.
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

/// The base-URL class name the contract's `baseUrl(name)` expects (D-106).
fn base_class(base: ir::BaseUrl) -> &'static str {
    match base {
        ir::BaseUrl::Api => "api",
        ir::BaseUrl::ApiRoot => "api_root",
        ir::BaseUrl::Upload => "upload",
        ir::BaseUrl::UploadSession => "upload_session",
        ir::BaseUrl::OAuthAuthorize => "oauth_authorize",
        ir::BaseUrl::Download => "download",
    }
}

/// A human-readable wire path for the method doc comment (structure, not a call).
fn wire_path(op: &ir::Operation) -> String {
    let mut out = String::new();
    for segment in &op.path {
        out.push('/');
        match segment {
            ir::PathSegment::Literal(text) => out.push_str(text),
            ir::PathSegment::Parameter(name) => {
                let _ = write!(out, "{{{}}}", name.as_str());
            }
            ir::PathSegment::Composite(parts) => {
                for part in parts {
                    match part {
                        ir::PathPart::Literal(text) => out.push_str(text),
                        ir::PathPart::Parameter(name) => {
                            let _ = write!(out, "{{{}}}", name.as_str());
                        }
                    }
                }
            }
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// The generated do-not-edit header.
fn header(build: &BuildInfo) -> String {
    format!(
        "// Code generated by box-gantry {} (spec {}). DO NOT EDIT.\n",
        build.engine, build.spec_fingerprint
    )
}

/// The `Internal` helper: percent-encoding, form encoding, and a scalar-list
/// joiner — the generation-side utilities the managers call.
fn internal_file(build: &BuildInfo) -> String {
    format!(
        "{}package {INTERNAL_PKG};\n\
         \n\
         import java.nio.charset.StandardCharsets;\n\
         import java.util.List;\n\
         import java.util.Map;\n\
         import java.util.function.Function;\n\
         \n\
         /** Generation-side request helpers (encoding, joining). */\n\
         public final class Internal {{\n\
         \x20   private Internal() {{}}\n\
         \n\
         \x20   /** Percent-encode a path segment, keeping RFC 3986 unreserved chars. */\n\
         \x20   public static String pathEscape(String value) {{\n\
         \x20       StringBuilder out = new StringBuilder();\n\
         \x20       byte[] bytes = value.getBytes(StandardCharsets.UTF_8);\n\
         \x20       for (byte b : bytes) {{\n\
         \x20           int c = b & 0xff;\n\
         \x20           if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')\n\
         \x20                   || c == '-' || c == '.' || c == '_' || c == '~') {{\n\
         \x20               out.append((char) c);\n\
         \x20           }} else {{\n\
         \x20               out.append('%').append(String.format(\"%02X\", c));\n\
         \x20           }}\n\
         \x20       }}\n\
         \x20       return out.toString();\n\
         \x20   }}\n\
         \n\
         \x20   /** Encode ordered key/value pairs as application/x-www-form-urlencoded. */\n\
         \x20   public static String formEncode(Map<String, String> pairs) {{\n\
         \x20       StringBuilder out = new StringBuilder();\n\
         \x20       for (Map.Entry<String, String> entry : pairs.entrySet()) {{\n\
         \x20           if (out.length() > 0) {{\n\
         \x20               out.append('&');\n\
         \x20           }}\n\
         \x20           out.append(pathEscape(entry.getKey())).append('=').append(pathEscape(entry.getValue()));\n\
         \x20       }}\n\
         \x20       return out.toString();\n\
         \x20   }}\n\
         \n\
         \x20   /** Comma-join a list, stringifying each element. */\n\
         \x20   public static <T> String join(List<T> items, Function<T, String> render) {{\n\
         \x20       StringBuilder out = new StringBuilder();\n\
         \x20       for (int i = 0; i < items.size(); i++) {{\n\
         \x20           if (i > 0) {{\n\
         \x20               out.append(',');\n\
         \x20           }}\n\
         \x20           out.append(render.apply(items.get(i)));\n\
         \x20       }}\n\
         \x20       return out.toString();\n\
         \x20   }}\n\
         }}\n",
        header(build)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gantry_ir::{
        BaseUrl, Decl, DeclKind, Field, HttpMethod, Identifier, ModulePath, Operation, Param,
        ParamLocation, PathSegment, ResponseShape, StructDecl, Type,
    };

    fn ident(s: &str) -> Identifier {
        Identifier::new(s).unwrap()
    }

    /// A one-manager program: a `File` struct and `GET /files/{file_id}` → JSON.
    fn program() -> ir::Program {
        let mut p = ir::Program::default();
        let file = p.add(Decl {
            name: ident("File"),
            module: ModulePath(vec![ident("schemas")]),
            api_version: None,
            kind: DeclKind::Struct(StructDecl {
                fields: vec![Field {
                    name: ident("id"),
                    wire_name: "id".into(),
                    ty: Type::String,
                }],
            }),
        });
        p.operations.push(Operation {
            name: ident("get_by_id"),
            variation: None,
            manager: ident("files"),
            api_version: None,
            method: HttpMethod::Get,
            base_url: BaseUrl::Api,
            path: vec![
                PathSegment::Literal("files".into()),
                PathSegment::Parameter(ident("file_id")),
            ],
            params: vec![Param {
                name: ident("file_id"),
                wire_name: "file_id".into(),
                location: ParamLocation::Path,
                ty: Type::String,
            }],
            request: None,
            response: ResponseShape::Json(Type::Decl(file)),
            deprecated: false,
        });
        p
    }

    fn generated(program: &ir::Program) -> Vec<GeneratedFile> {
        let analysis = gantry_sema::analyze(program).expect("well-formed program");
        generate_managers(&analysis, &BuildInfo::new("testfp"))
    }

    fn file_ending(files: &[GeneratedFile], suffix: &str) -> String {
        files
            .iter()
            .find(|f| f.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no generated file ends with {suffix}"))
            .content
            .clone()
    }

    #[test]
    fn manager_method_is_blocking_and_routes_through_the_contract() {
        let files = generated(&program());
        let out = file_ending(&files, "managers/FilesManager.java");
        assert!(out.contains("public final class FilesManager {"), "{out}");
        assert!(
            out.contains("private final com.box.sdk.runtime.Runtime.Session session;"),
            "{out}"
        );
        // A synchronous method (no Future), returning the model type directly.
        assert!(
            out.contains("public com.box.sdk.model.schemas.File getById(String fileId) {"),
            "{out}"
        );
        // The URL is built from the base-URL class + structured path (escaped).
        assert!(
            out.contains("StringBuilder _url = new StringBuilder(session.baseUrl(\"api\"));"),
            "{out}"
        );
        assert!(out.contains("_url.append(\"/files\");"), "{out}");
        assert!(
            out.contains(
                "_url.append(\"/\").append(com.box.sdk.internal.Internal.pathEscape(fileId));"
            ),
            "{out}"
        );
        // The network is reached only through the runtime contract.
        assert!(
            out.contains("session.newRequest(\"GET\", _url.toString())"),
            "{out}"
        );
        assert!(
            out.contains("com.box.sdk.runtime.Runtime.responseBytes(session.fetch(_req))"),
            "{out}"
        );
        assert!(
            out.contains("com.box.sdk.model.schemas.File.fromJson(_tree)"),
            "{out}"
        );
    }

    #[test]
    fn client_wires_one_field_per_manager_over_a_shared_session() {
        let files = generated(&program());
        let out = file_ending(&files, "com/box/sdk/Client.java");
        assert!(
            out.contains("public final com.box.sdk.managers.FilesManager files;"),
            "{out}"
        );
        assert!(
            out.contains(
                "com.box.sdk.runtime.Runtime.Session session = new com.box.sdk.runtime.Runtime.Session(auth);"
            ),
            "{out}"
        );
        assert!(
            out.contains("this.files = new com.box.sdk.managers.FilesManager(session);"),
            "{out}"
        );
    }

    #[test]
    fn optional_params_bundle_into_a_nested_options_class() {
        let mut p = program();
        // Add an optional query param to the operation.
        p.operations[0].params.push(Param {
            name: ident("fields"),
            wire_name: "fields".into(),
            location: ParamLocation::Query,
            ty: Type::Optional(Box::new(Type::List(Box::new(Type::String)))),
        });
        let out = file_ending(&generated(&p), "managers/FilesManager.java");
        // The optional param becomes an options object argument + a nested class.
        assert!(out.contains("GetByIdOptions options)"), "{out}");
        assert!(
            out.contains("public static final class GetByIdOptions {"),
            "{out}"
        );
        assert!(
            out.contains("public java.util.List<String> fields;"),
            "{out}"
        );
        // It's applied only when set, under its wire name.
        assert!(
            out.contains("if (options != null && options.fields != null) {"),
            "{out}"
        );
        assert!(
            out.contains("_req = com.box.sdk.runtime.Runtime.withQuery(_req, \"fields\", String.join(\",\", _v));"),
            "{out}"
        );
    }

    /// How the envelope wraps its `entries` list, exercising each optionality
    /// layer the model lowers (D-110).
    enum Entries {
        Plain,
        Tristate,
    }

    /// A marker-paginated program: `GET /items?marker=…` → `{ entries: [Item],
    /// next_marker: … }`. The building block for the pagination tests.
    fn paged_program(entries: Entries, cursor_ty: Type) -> ir::Program {
        let mut p = ir::Program::default();
        let item = p.add(Decl {
            name: ident("Item"),
            module: ModulePath(vec![ident("schemas")]),
            api_version: None,
            kind: DeclKind::Struct(StructDecl {
                fields: vec![Field {
                    name: ident("id"),
                    wire_name: "id".into(),
                    ty: Type::String,
                }],
            }),
        });
        let list = Type::List(Box::new(Type::Decl(item)));
        let entries_ty = match entries {
            Entries::Plain => list,
            // Optional<Nullable<List<T>>> lowers to the tri-state wrapper.
            Entries::Tristate => Type::Optional(Box::new(Type::Nullable(Box::new(list)))),
        };
        let envelope = p.add(Decl {
            name: ident("Items"),
            module: ModulePath(vec![ident("schemas")]),
            api_version: None,
            kind: DeclKind::Struct(StructDecl {
                fields: vec![
                    Field {
                        name: ident("entries"),
                        wire_name: "entries".into(),
                        ty: entries_ty,
                    },
                    Field {
                        name: ident("next_marker"),
                        wire_name: "next_marker".into(),
                        ty: cursor_ty,
                    },
                ],
            }),
        });
        p.operations.push(Operation {
            name: ident("get_items"),
            variation: None,
            manager: ident("items"),
            api_version: None,
            method: HttpMethod::Get,
            base_url: BaseUrl::Api,
            path: vec![PathSegment::Literal("items".into())],
            params: vec![Param {
                name: ident("marker"),
                wire_name: "marker".into(),
                location: ParamLocation::Query,
                ty: Type::Optional(Box::new(Type::String)),
            }],
            request: None,
            response: ResponseShape::Json(Type::Decl(envelope)),
            deprecated: false,
        });
        p
    }

    #[test]
    fn paged_operation_gets_an_iterable_paginator_and_a_paginate_constructor() {
        let files = generated(&paged_program(
            Entries::Plain,
            Type::Optional(Box::new(Type::String)),
        ));
        // The paginator is a re-iterable `Iterable` over the element type.
        let pag = file_ending(&files, "managers/ItemsGetItemsPaginator.java");
        assert!(
            pag.contains(
                "public final class ItemsGetItemsPaginator implements java.lang.Iterable<com.box.sdk.model.schemas.Item> {"
            ),
            "{pag}"
        );
        assert!(
            pag.contains("public java.util.Iterator<com.box.sdk.model.schemas.Item> iterator() {"),
            "{pag}"
        );
        // Each pass works on a private copy of the caller's options (never
        // mutating the caller's object), seeded per `iterator()` call.
        assert!(
            pag.contains("private ItemsManager.GetItemsOptions _freshOptions() {"),
            "{pag}"
        );
        assert!(pag.contains("_o.marker = options.marker;"), "{pag}");
        assert!(
            pag.contains("private final ItemsManager.GetItemsOptions _opts = _freshOptions();"),
            "{pag}"
        );
        // Marker style: the cursor threads through the options, terminating on an
        // absent/empty next_marker.
        assert!(
            pag.contains("private String _cursor = _opts.marker;"),
            "{pag}"
        );
        assert!(pag.contains("_opts.marker = _cursor;"), "{pag}");
        assert!(
            pag.contains("if (_next != null && !_next.isEmpty()) {"),
            "{pag}"
        );
        // It drives the plain method (URL/param/body logic is never duplicated).
        assert!(
            pag.contains("com.box.sdk.model.schemas.Items _page = _manager.getItems(_opts);"),
            "{pag}"
        );
        // The manager exposes a `<method>Paginate` constructor returning it.
        let mgr = file_ending(&files, "managers/ItemsManager.java");
        assert!(
            mgr.contains(
                "public ItemsGetItemsPaginator getItemsPaginate(GetItemsOptions options) {"
            ),
            "{mgr}"
        );
        assert!(
            mgr.contains("return new ItemsGetItemsPaginator(session, options);"),
            "{mgr}"
        );
    }

    #[test]
    fn tristate_entries_coalesce_absent_and_null_to_an_empty_list() {
        // entries: Optional<Nullable<List<Item>>> → Tristate<List<Item>>. The
        // extraction must treat both absent and explicit-null as an empty list.
        let files = generated(&paged_program(
            Entries::Tristate,
            Type::Optional(Box::new(Type::String)),
        ));
        let pag = file_ending(&files, "managers/ItemsGetItemsPaginator.java");
        assert!(
            pag.contains(
                "java.util.List<com.box.sdk.model.schemas.Item> _items = _page.entries().isPresent() && _page.entries().value() != null ? _page.entries().value() : java.util.List.of();"
            ),
            "{pag}"
        );
    }

    #[test]
    fn an_unsupported_cursor_shape_skips_only_the_paginator() {
        // A `next_marker` that is neither string nor int is a shape we don't
        // synthesize — the paginator is skipped, but the plain method still
        // ships (VR-6, never wrong code).
        let files = generated(&paged_program(
            Entries::Plain,
            Type::Optional(Box::new(Type::Bool)),
        ));
        assert!(
            !files.iter().any(|f| f.path.ends_with("Paginator.java")),
            "no paginator should be emitted for an unsupported cursor shape"
        );
        let mgr = file_ending(&files, "managers/ItemsManager.java");
        assert!(
            mgr.contains("public com.box.sdk.model.schemas.Items getItems("),
            "{mgr}"
        );
        assert!(!mgr.contains("getItemsPaginate"), "{mgr}");
    }
}
