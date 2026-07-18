//! Model generation: IR declarations → Java source files.
//!
//! One Java package per IR module (Java has real packages, so the IR module
//! tree lowers directly — no Apex-style flattening), one `.java` file per
//! declaration (Java's one-public-type-per-file rule). API versions redefine
//! names (e.g. `ClientError` exists in the base document and in `2025.0`), and
//! those modules share no references, so each becomes its own package rather
//! than one flat namespace (the D-147 lineage, applied to Java).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use gantry_ir as ir;
use gantry_ir::naming::{camel, pascal};
use gantry_sema::Analysis;

use crate::{BuildInfo, CORE_PKG, GeneratedFile, MODEL_PKG};

/// One deduped, collision-free Java sub-package name per IR module path — the
/// single source of truth shared by every generator (D-149 review lineage).
/// Flattening a module path with `_` is not injective (`[a_b]` and `[a, b]`
/// collapse), so names are allocated deterministically here.
pub(crate) fn package_names(program: &ir::Program) -> BTreeMap<ir::ModulePath, String> {
    let mut paths: Vec<ir::ModulePath> = Vec::new();
    for decl in &program.decls {
        if !paths.contains(&decl.module) {
            paths.push(decl.module.clone());
        }
    }
    let mut named: Vec<(ir::ModulePath, String)> = paths
        .into_iter()
        .map(|p| (p.clone(), package_name(&p)))
        .collect();
    named.sort_by(|a, b| a.1.cmp(&b.1));
    let mut used: Vec<String> = Vec::new();
    let mut map = BTreeMap::new();
    for (path, name) in named {
        map.insert(path, dedupe(&mut used, name));
    }
    map
}

/// One deduped, collision-free Java type name per (non-alias) declaration,
/// allocated **per package**. Java is one public type per file, so two decls in
/// a module that normalize to the same PascalCase name (`displayName` and
/// `display_name` both → `DisplayName`) would otherwise emit the same `.java`
/// path — one silently overwriting the other, and references resolving to the
/// wrong type. The map is the single source of truth for both filenames and
/// cross-decl references, so they can't disagree. Aliases reserve no name (they
/// emit no type — references resolve through).
pub(crate) fn type_names(program: &ir::Program) -> BTreeMap<ir::DeclId, String> {
    let mut per_module: BTreeMap<ir::ModulePath, Vec<String>> = BTreeMap::new();
    let mut map = BTreeMap::new();
    for (i, decl) in program.decls.iter().enumerate() {
        if matches!(decl.kind, ir::DeclKind::Alias(_)) {
            continue;
        }
        let used = per_module.entry(decl.module.clone()).or_default();
        let name = dedupe(used, type_name(decl.name.as_str()));
        map.insert(ir::DeclId(i as u32), name);
    }
    map
}

/// How a union declaration lowers.
enum UnionPlan {
    /// A transparent `record(Object value)` — no discriminator, or a variant
    /// that isn't a same-package, discriminator-carrying struct.
    Structural,
    /// A `sealed interface` over the variant records (Java's natural `oneOf`
    /// shape, D-164). `permits` holds the variant type names (all same package);
    /// an `open` union additionally permits a nested `Unknown` catch-all so an
    /// unrecognized discriminator round-trips (VR-4), which a `closed` one omits.
    Typed { permits: Vec<String>, open: bool },
}

/// Plan every union's lowering, and the reverse map from a variant decl to the
/// sealed interfaces it must `implements`. Both come from one qualification
/// pass, so `permits` and `implements` can't disagree (Java rejects a mismatch).
fn plan_unions(
    program: &ir::Program,
    names: &BTreeMap<ir::DeclId, String>,
) -> (
    BTreeMap<ir::DeclId, UnionPlan>,
    BTreeMap<ir::DeclId, Vec<String>>,
) {
    let mut plans = BTreeMap::new();
    let mut implemented: BTreeMap<ir::DeclId, Vec<String>> = BTreeMap::new();
    for (i, decl) in program.decls.iter().enumerate() {
        let ir::DeclKind::Union(u) = &decl.kind else {
            continue;
        };
        let union_id = ir::DeclId(i as u32);
        match typed_union_variants(program, &decl.module, u) {
            Some(ids) => {
                let interface = names[&union_id].clone();
                let permits = ids
                    .iter()
                    .map(|id| {
                        implemented.entry(*id).or_default().push(interface.clone());
                        names[id].clone()
                    })
                    .collect();
                let open = matches!(u.extensibility, ir::Extensibility::Open);
                plans.insert(union_id, UnionPlan::Typed { permits, open });
            }
            None => {
                plans.insert(union_id, UnionPlan::Structural);
            }
        }
    }
    // Stable, deduped `implements` lists (a struct can be a variant of several
    // unions).
    for list in implemented.values_mut() {
        list.sort();
        list.dedup();
    }
    (plans, implemented)
}

/// The distinct variant decl ids of a discriminated union that qualifies for the
/// typed sealed-interface form — every variant a **same-package** struct that
/// carries the discriminator field (so a sealed `permits` is legal without a
/// named module, and the tag survives the later serialization slice) — or
/// `None` for the structural fallback.
fn typed_union_variants(
    program: &ir::Program,
    union_module: &ir::ModulePath,
    u: &ir::UnionDecl,
) -> Option<Vec<ir::DeclId>> {
    let discriminator = u.discriminator.as_deref()?;
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    for variant in &u.variants {
        match (&variant.discriminator_value, &variant.ty) {
            (Some(_), ir::Type::Decl(id))
                if program.decl(*id).module == *union_module
                    && decl_carries_field(program, *id, discriminator) =>
            {
                if seen.insert(*id) {
                    ids.push(*id);
                }
            }
            _ => return None,
        }
    }
    if ids.is_empty() { None } else { Some(ids) }
}

/// Whether a decl is a struct with a field serialized under `wire_name` — i.e.
/// it carries its own discriminator, the invariant the typed union relies on.
fn decl_carries_field(program: &ir::Program, id: ir::DeclId, wire_name: &str) -> bool {
    matches!(
        &program.decl(id).kind,
        ir::DeclKind::Struct(s) if s.fields.iter().any(|f| f.wire_name == wire_name)
    )
}

/// Generate one `.java` file per declaration (aliases resolve through, so they
/// emit no file).
pub fn generate_models(analysis: &Analysis<'_>, build: &BuildInfo) -> Vec<GeneratedFile> {
    let program = analysis.program;
    let packages = package_names(program);
    let names = type_names(program);
    let (union_plans, implemented) = plan_unions(program, &names);
    let mut files = Vec::new();
    for (i, decl) in program.decls.iter().enumerate() {
        if let Some(file) = render_decl(
            program,
            &packages,
            &names,
            &union_plans,
            &implemented,
            ir::DeclId(i as u32),
            decl,
            build,
        ) {
            files.push(file);
        }
    }
    files
}

/// A Java sub-package name for an IR module path: every segment sanitized and
/// joined with `_`, so nested paths stay collision-free flat siblings under
/// `com.box.sdk.model` (`[schemas, v2025_0]` → `schemas_v2025_0`).
pub(crate) fn package_name(module: &ir::ModulePath) -> String {
    if module.0.is_empty() {
        return "root".to_string();
    }
    let joined = module
        .0
        .iter()
        .map(|segment| sanitize_lower(segment.as_str()))
        .collect::<Vec<_>>()
        .join("_");
    if JAVA_KEYWORDS.contains(&joined.as_str()) {
        format!("{joined}_")
    } else {
        joined
    }
}

fn sanitize_lower(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    out.make_ascii_lowercase();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, 'm');
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Render one declaration into a complete `.java` file, or `None` for an alias
/// (Java has no type alias — references resolve through to the target type).
#[allow(clippy::too_many_arguments)]
fn render_decl(
    program: &ir::Program,
    packages: &BTreeMap<ir::ModulePath, String>,
    names: &BTreeMap<ir::DeclId, String>,
    union_plans: &BTreeMap<ir::DeclId, UnionPlan>,
    implemented: &BTreeMap<ir::DeclId, Vec<String>>,
    id: ir::DeclId,
    decl: &ir::Decl,
    build: &BuildInfo,
) -> Option<GeneratedFile> {
    if matches!(decl.kind, ir::DeclKind::Alias(_)) {
        return None;
    }
    let package = format!("{MODEL_PKG}.{}", packages[&decl.module]);
    let name = names[&id].clone();

    let mut printer = Printer {
        program,
        packages,
        names,
        union_plans,
        implemented,
        module: &decl.module,
        imports: BTreeSet::new(),
    };
    let body = printer.decl(&name, id, decl);

    let api_version = decl
        .api_version
        .as_ref()
        .map_or("unversioned", |v| v.0.as_str());
    let mut content = format!(
        "// Code generated by box-gantry {} (spec {}) for Box API {api_version}. DO NOT EDIT.\n\
         package {package};\n\n",
        build.engine, build.spec_fingerprint
    );
    if !printer.imports.is_empty() {
        for import in &printer.imports {
            let _ = writeln!(content, "import {import};");
        }
        content.push('\n');
    }
    content.push_str(&body);

    Some(GeneratedFile {
        path: crate::java_path(&package, &name),
        content,
    })
}

struct Printer<'p> {
    program: &'p ir::Program,
    packages: &'p BTreeMap<ir::ModulePath, String>,
    /// The per-package deduped type name for every declaration — the same map
    /// used for filenames, so a reference can't disagree with the file it names.
    names: &'p BTreeMap<ir::DeclId, String>,
    /// How each union lowers (structural fallback vs typed sealed interface).
    union_plans: &'p BTreeMap<ir::DeclId, UnionPlan>,
    /// The sealed interfaces each variant decl must `implements`.
    implemented: &'p BTreeMap<ir::DeclId, Vec<String>>,
    module: &'p ir::ModulePath,
    /// Fully-qualified imports this file needs (library types only; model
    /// cross-package references are inlined as FQNs, so they never collide).
    imports: BTreeSet<String>,
}

impl Printer<'_> {
    fn decl(&mut self, name: &str, id: ir::DeclId, decl: &ir::Decl) -> String {
        match &decl.kind {
            ir::DeclKind::Struct(s) => self.struct_decl(name, id, s),
            ir::DeclKind::Enum(e) => self.enum_decl(name, e),
            ir::DeclKind::Union(u) => self.union_decl(name, id, u),
            // Aliases are filtered out in `render_decl` before reaching here.
            ir::DeclKind::Alias(_) => unreachable!("aliases emit no file"),
        }
    }

    /// A struct lowers to an immutable `record` (the D-164 fit). Each field is a
    /// record component; the tri-state (D-110) picks the component's type shape.
    /// A struct that is a discriminated-union variant `implements` the union's
    /// sealed interface(s) (all in the same package, so no import).
    fn struct_decl(&mut self, name: &str, id: ir::DeclId, s: &ir::StructDecl) -> String {
        let implements = match self.implemented.get(&id) {
            Some(interfaces) => format!(" implements {}", interfaces.join(", ")),
            None => String::new(),
        };
        if s.fields.is_empty() {
            return format!("public record {name}(){implements} {{}}\n");
        }
        // Distinct source names can normalize to the same Java identifier
        // (`displayName` and `display_name` both → `displayName`); a per-record
        // allocator keeps them distinct so the code compiles.
        let mut used: Vec<String> = Vec::new();
        let components: Vec<String> = s
            .fields
            .iter()
            .map(|field| {
                let ident = dedupe(&mut used, component_ident(field.name.as_str()));
                let ty = self.component_type(&field.ty);
                format!("{ty} {ident}")
            })
            .collect();

        let single = format!(
            "public record {name}({}){implements} {{}}",
            components.join(", ")
        );
        if single.len() <= MAX_WIDTH {
            return format!("{single}\n");
        }
        let mut out = format!("public record {name}(\n");
        for (i, component) in components.iter().enumerate() {
            let last = i + 1 == components.len();
            let tail = if last {
                format!("){implements} {{}}")
            } else {
                ",".to_string()
            };
            let _ = writeln!(out, "    {component}{tail}");
        }
        out
    }

    /// Open enums (Box's extensible enums, D-012) lower to a `record` over the
    /// raw `String` with the known values as constants — any unknown value
    /// round-trips untouched. Closed enums lower to a real `enum` that carries
    /// each value's wire spelling for the (later) serialization slice.
    fn enum_decl(&mut self, name: &str, e: &ir::EnumDecl) -> String {
        match e.extensibility {
            ir::Extensibility::Open => self.open_enum(name, e),
            ir::Extensibility::Closed => self.closed_enum(name, e),
        }
    }

    fn open_enum(&self, name: &str, e: &ir::EnumDecl) -> String {
        if e.values.is_empty() {
            return format!("public record {name}(String value) {{}}\n");
        }
        let mut out = format!("public record {name}(String value) {{\n");
        // Values that differ only in case (`ASC` vs `asc`) collapse to the same
        // SCREAMING_SNAKE_CASE constant; disambiguate so both keep a constant.
        let mut used: Vec<String> = Vec::new();
        for value in &e.values {
            let const_name = dedupe(&mut used, constant_ident(value));
            let _ = writeln!(
                out,
                "    public static final {name} {const_name} = new {name}({});",
                java_string(value)
            );
        }
        out.push_str("}\n");
        out
    }

    fn closed_enum(&self, name: &str, e: &ir::EnumDecl) -> String {
        if e.values.is_empty() {
            return format!("public enum {name} {{}}\n");
        }
        let mut out = format!("public enum {name} {{\n");
        // Distinct values can normalize to the same constant identifier
        // (`foo-bar` and `foo_bar`); keep them apart. Each carries its exact
        // wire spelling, so dispatch stays correct once serialization lands.
        let mut used: Vec<String> = Vec::new();
        let constants: Vec<(String, &String)> = e
            .values
            .iter()
            .map(|value| (dedupe(&mut used, constant_ident(value)), value))
            .collect();
        for (i, (const_name, value)) in constants.iter().enumerate() {
            let last = i + 1 == constants.len();
            let tail = if last { ";" } else { "," };
            let _ = writeln!(out, "    {const_name}({}){tail}", java_string(value));
        }
        out.push('\n');
        out.push_str("    /** The wire value this constant serializes to. */\n");
        out.push_str("    public final String wireValue;\n\n");
        let _ = writeln!(out, "    {name}(String wireValue) {{");
        out.push_str("        this.wireValue = wireValue;\n");
        out.push_str("    }\n}\n");
        out
    }

    /// A discriminated union whose variants are all same-package,
    /// discriminator-carrying structs lowers to a `sealed interface` over those
    /// records — Java's natural `oneOf` shape (D-164), mirroring Rust's typed
    /// unions (D-148). Anything else stays a structural `record(Object value)`
    /// newtype (no discriminator, a non-decl variant, or a cross-package one).
    fn union_decl(&self, name: &str, id: ir::DeclId, _u: &ir::UnionDecl) -> String {
        match self.union_plans.get(&id) {
            Some(UnionPlan::Typed { permits, open }) => sealed_union(name, permits, *open),
            Some(UnionPlan::Structural) | None => {
                format!("public record {name}(Object value) {{}}\n")
            }
        }
    }

    /// A struct field's record-component type (the tri-state, D-110). Arms are
    /// enumerated, never wildcarded — a new IR type must break this lowering at
    /// compile time, not fall through silently (NF-1, FR-2.1).
    fn component_type(&mut self, ty: &ir::Type) -> String {
        // Resolve a top-level alias *before* classifying optionality: an alias
        // has no Java type (it resolves through), so a field typed as an alias
        // to `Optional<T>`/`Optional<Nullable<T>>` must keep its wrapper rather
        // than reach `bare`, which strips it. Chained aliases recurse.
        if let ir::Type::Decl(id) = ty
            && let ir::DeclKind::Alias(target) = &self.program.decl(*id).kind
        {
            let target = target.clone();
            return self.component_type(&target);
        }
        match ty {
            ir::Type::Binary => "byte[]".to_string(),
            ir::Type::Optional(inner) => match &**inner {
                // Optional<Nullable<T>> → the three-state wrapper (D-110).
                ir::Type::Nullable(nullable) => {
                    self.imports.insert(format!("{CORE_PKG}.Tristate"));
                    format!("Tristate<{}>", self.bare(nullable))
                }
                // Every other Optional<T> keeps its absence marker, `byte[]`
                // included (a valid generic argument) — so an optional binary
                // stays distinct from a required/nullable one.
                ir::Type::Binary
                | ir::Type::Bool
                | ir::Type::Int64
                | ir::Type::Float64
                | ir::Type::String
                | ir::Type::Date
                | ir::Type::DateTime
                | ir::Type::JsonValue
                | ir::Type::List(_)
                | ir::Type::Map(_)
                | ir::Type::Decl(_)
                | ir::Type::Optional(_) => {
                    self.imports.insert("java.util.Optional".to_string());
                    format!("Optional<{}>", self.bare(inner))
                }
            },
            // A bare nullable value is a nullable Java reference — no wrapper.
            ir::Type::Nullable(inner) => self.bare(inner),
            ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::Date
            | ir::Type::DateTime
            | ir::Type::JsonValue
            | ir::Type::List(_)
            | ir::Type::Map(_)
            | ir::Type::Decl(_) => self.bare(ty),
        }
    }

    /// A bare (boxed, nullable) Java type. Scalars box uniformly (`Boolean`,
    /// `Long`, `Double`) so container elements and nullable fields need no
    /// primitive/reference juggling. Records the imports it needs.
    fn bare(&mut self, ty: &ir::Type) -> String {
        match ty {
            ir::Type::Bool => "Boolean".to_string(),
            ir::Type::Int64 => "Long".to_string(),
            ir::Type::Float64 => "Double".to_string(),
            ir::Type::String => "String".to_string(),
            // Typed date/time: a full-date `LocalDate` (Box's `2020-01-31`) and
            // an RFC 3339 `OffsetDateTime` — the same wire shapes Go's
            // `serialization.Date`/`time.Time` and Rust's chrono types carry.
            ir::Type::Date => {
                self.imports.insert("java.time.LocalDate".to_string());
                "LocalDate".to_string()
            }
            ir::Type::DateTime => {
                self.imports.insert("java.time.OffsetDateTime".to_string());
                "OffsetDateTime".to_string()
            }
            ir::Type::Binary => "byte[]".to_string(),
            // Free-form JSON with no third-party JSON dep is a parsed `Object`
            // graph (the serialization slice pins the concrete shape).
            ir::Type::JsonValue => "Object".to_string(),
            ir::Type::List(inner) => {
                self.imports.insert("java.util.List".to_string());
                format!("List<{}>", self.bare(inner))
            }
            ir::Type::Map(inner) => {
                self.imports.insert("java.util.Map".to_string());
                format!("Map<String, {}>", self.bare(inner))
            }
            ir::Type::Decl(id) => self.decl_type(*id),
            // A `Nullable`/`Optional` nested in a container is just a nullable
            // element — Java references are nullable, so the inner type stands.
            ir::Type::Nullable(inner) | ir::Type::Optional(inner) => self.bare(inner),
        }
    }

    /// The Java type for a declaration reference, resolving through aliases
    /// (Java has no type alias) and qualifying cross-package references.
    fn decl_type(&mut self, id: ir::DeclId) -> String {
        let decl = self.program.decl(id);
        match &decl.kind {
            ir::DeclKind::Alias(ty) => {
                let ty = ty.clone();
                self.bare(&ty)
            }
            ir::DeclKind::Struct(_) | ir::DeclKind::Enum(_) | ir::DeclKind::Union(_) => {
                let name = self.names[&id].clone();
                if decl.module == *self.module {
                    name
                } else {
                    // Cross-package: inline FQN (never an import, so two modules'
                    // like-named types can both be referenced without a clash).
                    format!("{MODEL_PKG}.{}.{name}", self.packages[&decl.module])
                }
            }
        }
    }
}

/// rustfmt-style soft column budget for keeping a record on one line.
const MAX_WIDTH: usize = 100;

/// Emit a `sealed interface` over a discriminated union's variant records. An
/// `open` union also permits (and nests) an `Unknown` catch-all so an
/// unrecognized discriminator round-trips (VR-4); a `closed` one omits it and so
/// rejects unknown tags. The `permits` clause wraps to a continuation line when
/// the declaration would overflow.
fn sealed_union(name: &str, permits: &[String], open: bool) -> String {
    let mut all: Vec<String> = permits.to_vec();
    if open {
        all.push(format!("{name}.Unknown"));
    }
    let permits_clause = format!("permits {}", all.join(", "));
    let body = if open {
        format!(
            " {{\n    \
             /** An unrecognized discriminator, retained verbatim (open union). */\n    \
             record Unknown(Object value) implements {name} {{}}\n}}\n"
        )
    } else {
        " {}\n".to_string()
    };
    // One line when it fits (up to the opening brace); otherwise wrap `permits`.
    let head_len = "public sealed interface  {".len() + name.len() + permits_clause.len();
    if head_len <= MAX_WIDTH {
        format!("public sealed interface {name} {permits_clause}{body}")
    } else {
        format!("public sealed interface {name}\n    {permits_clause}{body}")
    }
}

/// A Java type name from an IR declaration name: PascalCase, guarded against the
/// `java.lang` auto-imports and the library simple-names this backend imports,
/// so a generated type never shadows them.
pub(crate) fn type_name(name: &str) -> String {
    let pascal = pascal(name);
    if RESERVED_TYPES.contains(&pascal.as_str()) {
        format!("{pascal}_")
    } else {
        pascal
    }
}

/// A record-component identifier: camelCase, guarded against Java keywords and
/// `Object`'s method names (a component generates an accessor of that name, so
/// `hashCode`/`toString`/… would clash with the record's own members).
fn component_ident(name: &str) -> String {
    let base = sanitize_ident(&camel(name));
    if JAVA_KEYWORDS.contains(&base.as_str()) || OBJECT_METHODS.contains(&base.as_str()) {
        format!("{base}_")
    } else {
        base
    }
}

/// A `SCREAMING_SNAKE_CASE` constant/enum-constant identifier for an enum value
/// (the Java convention): non-alphanumerics → `_`, uppercased, underscore runs
/// collapsed and edges trimmed, then digit-leading and keyword guarded.
fn constant_ident(value: &str) -> String {
    let mut upper: String = value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    upper.make_ascii_uppercase();
    let mut collapsed = String::with_capacity(upper.len());
    let mut prev_underscore = false;
    for c in upper.chars() {
        if c == '_' {
            if !prev_underscore && !collapsed.is_empty() {
                collapsed.push('_');
            }
            prev_underscore = true;
        } else {
            collapsed.push(c);
            prev_underscore = false;
        }
    }
    let trimmed = collapsed.trim_end_matches('_');
    let base = if trimmed.is_empty() { "EMPTY" } else { trimmed };
    if base.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("V{base}")
    } else if JAVA_KEYWORDS.contains(&base) {
        format!("{base}_")
    } else {
        base.to_string()
    }
}

/// Sanitize an arbitrary name into a valid Java identifier body (no keyword
/// handling): non-alphanumerics → `_`, digit-leading → prefixed.
fn sanitize_ident(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.is_empty() {
        out.push('_');
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, 'n');
    }
    out
}

/// Allocate a collision-free name in a scope: returns `base` if unused, else
/// `base_2`, `base_3`, … Deterministic given a stable iteration order (FR-6.2).
fn dedupe(used: &mut Vec<String>, base: String) -> String {
    let mut candidate = base.clone();
    let mut n = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}_{n}");
        n += 1;
    }
    used.push(candidate.clone());
    candidate
}

/// A Java double-quoted string literal for a wire value: escapes the characters
/// a `.java` source string must, and any non-printable as `\\uXXXX`.
fn java_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `java.lang` auto-imports and the library simple-names this backend imports —
/// a generated type name must not shadow them.
const RESERVED_TYPES: &[&str] = &[
    // java.lang auto-imports (the ones a Box schema might realistically hit).
    "String",
    "Object",
    "Integer",
    "Long",
    "Double",
    "Float",
    "Boolean",
    "Byte",
    "Short",
    "Character",
    "Number",
    "Void",
    "Class",
    "Enum",
    "Record",
    "Math",
    "System",
    "Thread",
    "Runnable",
    "Iterable",
    "Comparable",
    "Cloneable",
    "CharSequence",
    "StringBuilder",
    "Error",
    "Exception",
    "RuntimeException",
    "Throwable",
    // Library simple-names this backend imports (java.util / java.time / core).
    "List",
    "Map",
    "Optional",
    "LocalDate",
    "OffsetDateTime",
    "Tristate",
];

/// `Object`'s public/protected method names — unusable as record components
/// (the component accessor would clash with the inherited method).
const OBJECT_METHODS: &[&str] = &[
    "hashCode",
    "equals",
    "toString",
    "clone",
    "wait",
    "notify",
    "notifyAll",
    "getClass",
    "finalize",
];

/// Java reserved words (keywords + `true`/`false`/`null` literals + the
/// restricted `var`/`record`/`sealed`/… that are unsafe as plain identifiers).
const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "true",
    "false",
    "null",
    "var",
    "record",
    "sealed",
    "permits",
    "yield",
];

#[cfg(test)]
mod tests {
    use super::*;
    use gantry_ir::{
        Decl, DeclKind, EnumDecl, Extensibility, Field, Identifier, ModulePath, StructDecl, Type,
        UnionDecl, UnionVariant,
    };

    fn ident(s: &str) -> Identifier {
        Identifier::new(s).unwrap()
    }

    fn module() -> ModulePath {
        ModulePath(vec![ident("schemas")])
    }

    fn add(program: &mut ir::Program, kind: DeclKind, name: &str) -> ir::DeclId {
        program.add(Decl {
            name: ident(name),
            module: module(),
            api_version: None,
            kind,
        })
    }

    /// Render the single decl at `id` into its `.java` file content.
    fn render(program: &ir::Program, id: ir::DeclId) -> String {
        let build = BuildInfo::new("testfp");
        let packages = package_names(program);
        let names = type_names(program);
        let (plans, implemented) = plan_unions(program, &names);
        render_decl(
            program,
            &packages,
            &names,
            &plans,
            &implemented,
            id,
            program.decl(id),
            &build,
        )
        .expect("decl emits a file")
        .content
    }

    /// The `.java` path a decl renders to (or `None` for an alias).
    fn render_path(program: &ir::Program, id: ir::DeclId) -> Option<String> {
        let build = BuildInfo::new("testfp");
        let packages = package_names(program);
        let names = type_names(program);
        let (plans, implemented) = plan_unions(program, &names);
        render_decl(
            program,
            &packages,
            &names,
            &plans,
            &implemented,
            id,
            program.decl(id),
            &build,
        )
        .map(|f| f.path)
    }

    #[test]
    fn struct_lowers_to_a_record_with_tri_state_and_keyword_fields() {
        let mut p = ir::Program::default();
        let s = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![
                    Field {
                        name: ident("id"),
                        wire_name: "id".into(),
                        ty: Type::String,
                    },
                    Field {
                        name: ident("displayName"),
                        wire_name: "displayName".into(),
                        ty: Type::Optional(Box::new(Type::String)),
                    },
                    Field {
                        name: ident("size"),
                        wire_name: "size".into(),
                        ty: Type::Optional(Box::new(Type::Nullable(Box::new(Type::Int64)))),
                    },
                    Field {
                        name: ident("class"),
                        wire_name: "class".into(),
                        ty: Type::String,
                    },
                ],
            }),
            "Widget",
        );
        let out = render(&p, s);
        assert!(out.contains("package com.box.sdk.model.schemas;"), "{out}");
        assert!(out.contains("public record Widget("), "{out}");
        assert!(out.contains("String id"), "{out}");
        // Optional<T> → java.util.Optional (imported).
        assert!(out.contains("Optional<String> displayName"), "{out}");
        assert!(out.contains("import java.util.Optional;"), "{out}");
        // Optional<Nullable<T>> → the tri-state wrapper (imported from core).
        assert!(out.contains("Tristate<Long> size"), "{out}");
        assert!(out.contains("import com.box.sdk.core.Tristate;"), "{out}");
        // Keyword field → suffixed identifier.
        assert!(out.contains("String class_"), "{out}");
        assert!(out.contains("DO NOT EDIT"), "{out}");
    }

    #[test]
    fn empty_struct_is_an_empty_record() {
        let mut p = ir::Program::default();
        let s = add(
            &mut p,
            DeclKind::Struct(StructDecl { fields: vec![] }),
            "Empty",
        );
        let out = render(&p, s);
        assert!(out.contains("public record Empty() {}"), "{out}");
    }

    #[test]
    fn colliding_field_names_are_disambiguated() {
        let mut p = ir::Program::default();
        let s = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![
                    Field {
                        name: ident("displayName"),
                        wire_name: "displayName".into(),
                        ty: Type::String,
                    },
                    Field {
                        name: ident("display_name"),
                        wire_name: "display_name".into(),
                        ty: Type::String,
                    },
                ],
            }),
            "Widget",
        );
        let out = render(&p, s);
        assert!(out.contains("String displayName"), "{out}");
        assert!(out.contains("String displayName_2"), "{out}");
    }

    #[test]
    fn open_enum_is_a_record_with_deduped_constants() {
        let mut p = ir::Program::default();
        let e = add(
            &mut p,
            DeclKind::Enum(EnumDecl {
                values: vec!["ASC".into(), "asc".into()],
                extensibility: Extensibility::Open,
            }),
            "Direction",
        );
        let out = render(&p, e);
        assert!(
            out.contains("public record Direction(String value) {"),
            "{out}"
        );
        assert!(
            out.contains(r#"public static final Direction ASC = new Direction("ASC");"#),
            "{out}"
        );
        // Case-only duplicates keep distinct constant names.
        assert!(
            out.contains(r#"public static final Direction ASC_2 = new Direction("asc");"#),
            "{out}"
        );
    }

    #[test]
    fn closed_enum_is_a_real_enum_carrying_wire_values() {
        let mut p = ir::Program::default();
        let e = add(
            &mut p,
            DeclKind::Enum(EnumDecl {
                values: vec!["foo-bar".into(), "foo_bar".into()],
                extensibility: Extensibility::Closed,
            }),
            "Kind",
        );
        let out = render(&p, e);
        assert!(out.contains("public enum Kind {"), "{out}");
        // Both variants present, one suffixed — the enum compiles.
        assert!(out.contains(r#"FOO_BAR("foo-bar"),"#), "{out}");
        assert!(out.contains(r#"FOO_BAR_2("foo_bar");"#), "{out}");
        assert!(out.contains("public final String wireValue;"), "{out}");
    }

    #[test]
    fn union_lowers_structurally() {
        let mut p = ir::Program::default();
        let u = add(
            &mut p,
            DeclKind::Union(UnionDecl {
                discriminator: Some("kind".into()),
                variants: vec![UnionVariant {
                    discriminator_value: Some("dog".into()),
                    ty: Type::String,
                }],
                extensibility: Extensibility::Open,
            }),
            "Pet",
        );
        let out = render(&p, u);
        assert!(out.contains("public record Pet(Object value) {}"), "{out}");
    }

    /// `Dog`/`Cat` structs (both carrying the `kind` discriminator) + a union
    /// over them at extensibility `ext`.
    fn union_program(ext: Extensibility) -> (ir::Program, ir::DeclId, ir::DeclId) {
        let mut p = ir::Program::default();
        let dog = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![Field {
                    name: ident("kind"),
                    wire_name: "kind".into(),
                    ty: Type::String,
                }],
            }),
            "Dog",
        );
        let cat = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![Field {
                    name: ident("kind"),
                    wire_name: "kind".into(),
                    ty: Type::String,
                }],
            }),
            "Cat",
        );
        let pet = add(
            &mut p,
            DeclKind::Union(UnionDecl {
                discriminator: Some("kind".into()),
                variants: vec![
                    UnionVariant {
                        discriminator_value: Some("dog".into()),
                        ty: Type::Decl(dog),
                    },
                    UnionVariant {
                        discriminator_value: Some("cat".into()),
                        ty: Type::Decl(cat),
                    },
                ],
                extensibility: ext,
            }),
            "Pet",
        );
        (p, dog, pet)
    }

    #[test]
    fn open_discriminated_union_is_a_sealed_interface_with_unknown() {
        let (p, dog, pet) = union_program(Extensibility::Open);
        let iface = render(&p, pet);
        assert!(
            iface.contains("public sealed interface Pet permits Dog, Cat, Pet.Unknown {"),
            "{iface}"
        );
        assert!(
            iface.contains("record Unknown(Object value) implements Pet {}"),
            "{iface}"
        );
        // The variant record implements the interface (same package, no import).
        let dog_out = render(&p, dog);
        assert!(dog_out.contains("implements Pet"), "{dog_out}");
    }

    #[test]
    fn closed_discriminated_union_omits_the_unknown_catch_all() {
        let (p, _dog, pet) = union_program(Extensibility::Closed);
        let iface = render(&p, pet);
        assert!(
            iface.contains("public sealed interface Pet permits Dog, Cat {}"),
            "{iface}"
        );
        assert!(!iface.contains("Unknown"), "{iface}");
    }

    #[test]
    fn union_with_a_tagless_variant_falls_back_to_structural() {
        // `Fish` lacks the `kind` discriminator field, so the typed form can't
        // carry the tag — the union stays structural and Fish implements nothing.
        let mut p = ir::Program::default();
        let dog = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![Field {
                    name: ident("kind"),
                    wire_name: "kind".into(),
                    ty: Type::String,
                }],
            }),
            "Dog",
        );
        let fish = add(
            &mut p,
            DeclKind::Struct(StructDecl { fields: vec![] }),
            "Fish",
        );
        let pet = add(
            &mut p,
            DeclKind::Union(UnionDecl {
                discriminator: Some("kind".into()),
                variants: vec![
                    UnionVariant {
                        discriminator_value: Some("dog".into()),
                        ty: Type::Decl(dog),
                    },
                    UnionVariant {
                        discriminator_value: Some("fish".into()),
                        ty: Type::Decl(fish),
                    },
                ],
                extensibility: Extensibility::Open,
            }),
            "Pet",
        );
        let out = render(&p, pet);
        assert!(out.contains("public record Pet(Object value) {}"), "{out}");
        let fish_out = render(&p, fish);
        assert!(!fish_out.contains("implements"), "{fish_out}");
    }

    #[test]
    fn alias_emits_no_file_and_resolves_through() {
        let mut p = ir::Program::default();
        // An alias `Id = String`, and a struct field referencing it.
        let alias = add(&mut p, DeclKind::Alias(Type::String), "Id");
        let s = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![Field {
                    name: ident("id"),
                    wire_name: "id".into(),
                    ty: Type::Decl(alias),
                }],
            }),
            "Widget",
        );
        // The alias emits no file.
        assert!(render_path(&p, alias).is_none());
        // The field referencing it resolves through to `String`.
        let out = render(&p, s);
        assert!(out.contains("String id"), "{out}");
    }

    #[test]
    fn alias_to_optional_and_tri_state_keeps_the_wrapper() {
        // An alias whose target is itself optional must not lose the wrapper
        // when a field references it (the tri-state would otherwise collapse).
        let mut p = ir::Program::default();
        let opt = add(
            &mut p,
            DeclKind::Alias(Type::Optional(Box::new(Type::String))),
            "MaybeName",
        );
        let tri = add(
            &mut p,
            DeclKind::Alias(Type::Optional(Box::new(Type::Nullable(Box::new(
                Type::Int64,
            ))))),
            "MaybeSize",
        );
        let s = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![
                    Field {
                        name: ident("name"),
                        wire_name: "name".into(),
                        ty: Type::Decl(opt),
                    },
                    Field {
                        name: ident("size"),
                        wire_name: "size".into(),
                        ty: Type::Decl(tri),
                    },
                ],
            }),
            "Widget",
        );
        let out = render(&p, s);
        assert!(out.contains("Optional<String> name"), "{out}");
        assert!(out.contains("Tristate<Long> size"), "{out}");
    }

    #[test]
    fn optional_binary_keeps_its_absence_marker() {
        let mut p = ir::Program::default();
        let s = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![
                    Field {
                        name: ident("blob"),
                        wire_name: "blob".into(),
                        ty: Type::Optional(Box::new(Type::Binary)),
                    },
                    Field {
                        name: ident("raw"),
                        wire_name: "raw".into(),
                        ty: Type::Binary,
                    },
                ],
            }),
            "Widget",
        );
        let out = render(&p, s);
        // Optional binary keeps its absence marker; a required one stays bare.
        assert!(out.contains("Optional<byte[]> blob"), "{out}");
        assert!(out.contains("byte[] raw"), "{out}");
    }

    #[test]
    fn colliding_decl_names_get_distinct_files_and_references() {
        // Two decls in one module that normalize to the same PascalCase name
        // must not share a `.java` path (Java is one type per file — a shared
        // path would silently overwrite one), and references must stay distinct.
        let mut p = ir::Program::default();
        let a = add(
            &mut p,
            DeclKind::Struct(StructDecl { fields: vec![] }),
            "displayName",
        );
        let b = add(
            &mut p,
            DeclKind::Struct(StructDecl { fields: vec![] }),
            "display_name",
        );
        let s = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![
                    Field {
                        name: ident("first"),
                        wire_name: "first".into(),
                        ty: Type::Decl(a),
                    },
                    Field {
                        name: ident("second"),
                        wire_name: "second".into(),
                        ty: Type::Decl(b),
                    },
                ],
            }),
            "Holder",
        );
        let pa = render_path(&p, a).unwrap();
        let pb = render_path(&p, b).unwrap();
        assert_ne!(pa, pb, "colliding decls share a path: {pa}");
        assert!(pa.ends_with("DisplayName.java"), "{pa}");
        assert!(pb.ends_with("DisplayName_2.java"), "{pb}");
        // The referencing struct names each distinct type.
        let out = render(&p, s);
        assert!(out.contains("DisplayName first"), "{out}");
        assert!(out.contains("DisplayName_2 second"), "{out}");
    }

    #[test]
    fn type_name_guards_reserved_and_library_names() {
        assert_eq!(type_name("string"), "String_");
        assert_eq!(type_name("list"), "List_");
        assert_eq!(type_name("file"), "File");
    }

    #[test]
    fn package_name_sanitizes_and_dedupes() {
        assert_eq!(package_name(&ModulePath(vec![ident("schemas")])), "schemas");
        assert_eq!(
            package_name(&ModulePath(vec![ident("schemas"), ident("v2025-0")])),
            "schemas_v2025_0"
        );
    }
}
