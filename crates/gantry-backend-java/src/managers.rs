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

use std::collections::BTreeMap;
use std::fmt::Write as _;

use gantry_ir as ir;
use gantry_ir::naming::{camel, pascal, snake};
use gantry_sema::Analysis;

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
struct ManagerPlan {
    ops: Vec<usize>,
    /// The manager class name, e.g. `FilesManager`.
    class: String,
    /// The `Client` field / constructor accessor, e.g. `files`.
    field: String,
}

/// Allocate a collision-free class + field name per manager. `Analysis::managers`
/// is a `BTreeMap`, so managers are planned in sorted-key order (deterministic,
/// FR-6.2); the dedup accumulator runs across all managers so two keys that
/// normalize together still get distinct names.
fn plan_managers(analysis: &Analysis<'_>) -> Vec<ManagerPlan> {
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
    };
    let plans = plan_managers(analysis);

    let mut files = Vec::new();
    for plan in &plans {
        files.push(GeneratedFile {
            path: java_path(MANAGERS_PKG, &plan.class),
            content: printer.manager_file(plan, build),
        });
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
}

impl ManagerPrinter<'_> {
    /// One manager's complete `.java` file: the class, its shared session, an
    /// operation method per grouped op, and a nested options class per op that
    /// has optional parameters.
    fn manager_file(&self, plan: &ManagerPlan, build: &BuildInfo) -> String {
        // Method names dedup per manager, so distinct ops that normalize to the
        // same name stay distinct — and the options class reuses the name.
        let mut used: Vec<String> = Vec::new();
        let methods: Vec<(usize, String)> = plan
            .ops
            .iter()
            .map(|&i| {
                let name = dedupe(&mut used, self.method_name(&self.program.operations[i]));
                (i, name)
            })
            .collect();

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
        for (i, name) in &methods {
            out.push_str(&self.operation(&self.program.operations[*i], name));
            out.push('\n');
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
        out
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
                let _ = writeln!(
                    out,
                    "        _req = {RUNTIME}.withStreamBody(_req, {STREAM}.empty(), \"application/octet-stream\");"
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

    /// The method name for an operation: `snake` base + variation + a
    /// `_v<version>` suffix for a non-base API version (so base and versioned
    /// surfaces never collide, FR-7.5), camelCased and keyword-guarded.
    fn method_name(&self, op: &ir::Operation) -> String {
        let mut name = snake(op.name.as_str());
        if let Some(variation) = &op.variation {
            name.push('_');
            name.push_str(&snake(variation.as_str()));
        }
        if op.api_version.as_ref() != self.base_version
            && let Some(version) = &op.api_version
        {
            name.push_str("_v");
            name.push_str(&version.0.replace(['.', '-'], "_"));
        }
        keyword_safe(&camel(&name))
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
}
