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

/// One sealed union's shape, resolved for the generated behavioral tests
/// (FR-7.8, VR-4) — reusing the same `plan_unions` allocation the codec does,
/// so a test can never reference a union that lowered to the structural newtype.
pub(crate) struct UnionTestRow {
    /// The sealed interface's fully-qualified name.
    pub(crate) union_fqn: String,
    /// The discriminator wire name.
    pub(crate) discriminator: String,
    /// Whether the union is open (unknown tag → `Unknown`) or closed (rejects).
    pub(crate) open: bool,
    /// A representative variant whose minimal `{"disc":"value"}` JSON round-trips
    /// (its only non-optional field is the discriminator): `(tag, variant_fqn)`.
    /// `None` when every variant carries another required field.
    pub(crate) known: Option<(String, String)>,
}

/// Every typed (sealed-interface) union, resolved for the round-trip tests.
pub(crate) fn union_test_rows(program: &ir::Program) -> Vec<UnionTestRow> {
    let packages = package_names(program);
    let names = type_names(program);
    let (plans, _) = plan_unions(program, &names);
    let fqn = |id: ir::DeclId| {
        format!(
            "{MODEL_PKG}.{}.{}",
            packages[&program.decl(id).module],
            names[&id]
        )
    };
    let mut rows = Vec::new();
    for (i, decl) in program.decls.iter().enumerate() {
        let id = ir::DeclId(i as u32);
        let ir::DeclKind::Union(u) = &decl.kind else {
            continue;
        };
        let Some(UnionPlan::Typed { open, .. }) = plans.get(&id) else {
            continue;
        };
        let Some(discriminator) = u.discriminator.clone() else {
            continue;
        };
        // A "safe" variant carries no required field beyond the discriminator, so
        // `{"disc":"value"}` deserializes (matching the Rust rule, D-156).
        let known = u.variants.iter().find_map(|v| {
            let (Some(tag), ir::Type::Decl(vid)) = (&v.discriminator_value, &v.ty) else {
                return None;
            };
            let ir::DeclKind::Struct(s) = &program.decl(*vid).kind else {
                return None;
            };
            let safe = s
                .fields
                .iter()
                .all(|f| f.wire_name == discriminator || matches!(f.ty, ir::Type::Optional(_)));
            safe.then(|| (tag.clone(), fqn(*vid)))
        });
        rows.push(UnionTestRow {
            union_fqn: fqn(id),
            discriminator,
            open: *open,
            known,
        });
    }
    rows
}

/// A real struct usable to exercise the model codec's tri-state (D-110): every
/// field optional (so `{}` and `{"f":…}` both deserialize) with a `String`
/// tri-state field to assert absent / null / value on. `None` if the spec has
/// none (the test block is then skipped).
pub(crate) struct TristateTarget {
    pub(crate) struct_fqn: String,
    pub(crate) wire_name: String,
    pub(crate) accessor: String,
}

/// Find a struct suitable for the tri-state round-trip test, in declaration
/// order (deterministic, FR-6.2).
pub(crate) fn tristate_test_target(program: &ir::Program) -> Option<TristateTarget> {
    let packages = package_names(program);
    let names = type_names(program);
    let is_string_tristate = |ty: &ir::Type| {
        matches!(ty, ir::Type::Optional(inner)
            if matches!(&**inner, ir::Type::Nullable(n) if matches!(**n, ir::Type::String)))
    };
    for (i, decl) in program.decls.iter().enumerate() {
        let ir::DeclKind::Struct(s) = &decl.kind else {
            continue;
        };
        if s.fields.is_empty()
            || !s
                .fields
                .iter()
                .all(|f| matches!(f.ty, ir::Type::Optional(_)))
        {
            continue;
        }
        let Some(tri) = s.fields.iter().find(|f| is_string_tristate(&f.ty)) else {
            continue;
        };
        let id = ir::DeclId(i as u32);
        let accessor = struct_components(s)
            .into_iter()
            .find(|(_, f)| f.wire_name == tri.wire_name)
            .map(|(ident, _)| ident)
            .expect("the tri-state field is a component of its own struct");
        return Some(TristateTarget {
            struct_fqn: format!("{MODEL_PKG}.{}.{}", packages[&decl.module], names[&id]),
            wire_name: tri.wire_name.clone(),
            accessor,
        });
    }
    None
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
        // A single module import declaration (JEP 511, Java 25+) collapses every
        // `java.base` library import (`java.util.*`, `java.time.*`) into one line;
        // non-`java.base` imports (the SDK's own `Tristate`) stay explicit. A
        // locally declared type shadows an on-demand module import, so a model
        // named like a `java.base` type can't be captured by it.
        let base: Vec<&String> = printer.imports.iter().filter(|i| in_java_base(i)).collect();
        let others: Vec<&String> = printer
            .imports
            .iter()
            .filter(|i| !in_java_base(i))
            .collect();
        if !base.is_empty() {
            content.push_str("import module java.base;\n");
        }
        for import in others {
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
    /// sealed interface(s) (all in the same package, so no import). The body
    /// carries the JSON codec (`toJson`/`fromJson`, D-172).
    fn struct_decl(&mut self, name: &str, id: ir::DeclId, s: &ir::StructDecl) -> String {
        let implements = match self.implemented.get(&id) {
            Some(interfaces) => format!(" implements {}", interfaces.join(", ")),
            None => String::new(),
        };
        let fields = struct_components(s);
        let header = self.struct_header(name, &fields, &implements);
        let body = self.struct_codec(name, &fields);
        format!("{header} {{\n{body}}}\n")
    }

    /// The `public record Name(components)implements` declaration line(s), up to
    /// (but not including) the opening brace — wrapping the components onto
    /// continuation lines when the single-line form would overflow.
    fn struct_header(
        &mut self,
        name: &str,
        fields: &[(String, &ir::Field)],
        implements: &str,
    ) -> String {
        let components: Vec<String> = fields
            .iter()
            .map(|(ident, field)| format!("{} {ident}", self.component_type(&field.ty)))
            .collect();
        let single = format!(
            "public record {name}({}){implements}",
            components.join(", ")
        );
        if single.len() <= MAX_WIDTH {
            return single;
        }
        let mut out = format!("public record {name}(\n");
        for (i, component) in components.iter().enumerate() {
            let last = i + 1 == components.len();
            let tail = if last {
                format!("){implements}")
            } else {
                ",".to_string()
            };
            let _ = writeln!(out, "    {component}{tail}");
        }
        // Trim the trailing newline: the caller appends ` {`.
        out.pop();
        out
    }

    /// The struct's JSON codec (D-172): `toJson` builds an ordered field map
    /// (the tri-state omits an absent field, writes an explicit `null`, or
    /// writes the value — D-110); `fromJson` reconstructs from the parsed tree.
    fn struct_codec(&mut self, name: &str, fields: &[(String, &ir::Field)]) -> String {
        let mut out = String::new();
        out.push_str("    public java.util.Map<String, Object> toJson() {\n");
        out.push_str(
            "        java.util.Map<String, Object> _m = new java.util.LinkedHashMap<>();\n",
        );
        for (ident, field) in fields {
            for line in self.encode_field(ident, field) {
                let _ = writeln!(out, "        {line}");
            }
        }
        out.push_str("        return _m;\n");
        out.push_str("    }\n\n");

        let _ = writeln!(out, "    public static {name} fromJson(Object _json) {{");
        if fields.is_empty() {
            let _ = writeln!(out, "        return new {name}();");
        } else {
            out.push_str(
                "        java.util.Map<String, Object> _m = com.box.sdk.core.Json.asObject(_json);\n",
            );
            let _ = writeln!(out, "        return new {name}(");
            for (i, (_ident, field)) in fields.iter().enumerate() {
                let last = i + 1 == fields.len();
                let tail = if last { "" } else { "," };
                let expr = self.decode_field(field);
                let _ = writeln!(out, "            {expr}{tail}");
            }
            out.push_str("        );\n");
        }
        out.push_str("    }\n");
        out
    }

    /// Open enums (Box's extensible enums, D-012) lower to a `record` over the
    /// raw `String` with the known values as constants — any unknown value
    /// round-trips untouched. Closed enums lower to a real `enum` that carries
    /// each value's wire spelling, so serialization (D-172) dispatches on it.
    fn enum_decl(&mut self, name: &str, e: &ir::EnumDecl) -> String {
        match e.extensibility {
            ir::Extensibility::Open => self.open_enum(name, e),
            ir::Extensibility::Closed => self.closed_enum(name, e),
        }
    }

    /// An open enum is transparent over its raw `String`, so its codec (D-172)
    /// is identity: `toJson` yields the value, `fromJson` wraps whatever came in
    /// (an unknown value round-trips untouched — the D-012 guarantee).
    fn open_enum(&self, name: &str, e: &ir::EnumDecl) -> String {
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
        if !e.values.is_empty() {
            out.push('\n');
        }
        out.push_str("    public String toJson() {\n        return value;\n    }\n\n");
        let _ = writeln!(out, "    public static {name} fromJson(Object _json) {{");
        let _ = writeln!(
            out,
            "        return new {name}(com.box.sdk.core.Json.asString(_json));"
        );
        out.push_str("    }\n}\n");
        out
    }

    /// A closed enum is a real `enum` carrying each value's wire spelling; its
    /// codec (D-172) maps to/from that spelling and **rejects** an unrecognized
    /// value (the closed-vs-open contract, mirroring Rust/TS).
    fn closed_enum(&self, name: &str, e: &ir::EnumDecl) -> String {
        let mut out = format!("public enum {name} {{\n");
        // Distinct values can normalize to the same constant identifier
        // (`foo-bar` and `foo_bar`); keep them apart. Each carries its exact
        // wire spelling, so the codec dispatch stays correct.
        let mut used: Vec<String> = Vec::new();
        let constants: Vec<(String, &String)> = e
            .values
            .iter()
            .map(|value| (dedupe(&mut used, constant_ident(value)), value))
            .collect();
        // An empty enum still needs the leading `;` before its members.
        if constants.is_empty() {
            out.push_str("    ;\n");
        }
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
        out.push_str("    }\n\n");
        out.push_str("    public String toJson() {\n        return wireValue;\n    }\n\n");
        let _ = writeln!(out, "    public static {name} fromJson(Object _json) {{");
        out.push_str("        String _w = com.box.sdk.core.Json.asString(_json);\n");
        let _ = writeln!(out, "        for ({name} _v : values()) {{");
        out.push_str("            if (_v.wireValue.equals(_w)) {\n");
        out.push_str("                return _v;\n");
        out.push_str("            }\n");
        out.push_str("        }\n");
        let _ = writeln!(
            out,
            "        throw new IllegalArgumentException(\"unknown {name} wire value: \" + _w);"
        );
        out.push_str("    }\n}\n");
        out
    }

    /// A discriminated union whose variants are all same-package,
    /// discriminator-carrying structs lowers to a `sealed interface` over those
    /// records — Java's natural `oneOf` shape (D-164), mirroring Rust's typed
    /// unions (D-148). Anything else stays a structural `record(Object value)`
    /// newtype (no discriminator, a non-decl variant, or a cross-package one).
    /// Either way it carries the JSON codec (D-172): the typed form dispatches
    /// `fromJson` by pattern-matching `switch` on the discriminator; the
    /// structural form is a transparent pass-through of the raw value.
    fn union_decl(&mut self, name: &str, id: ir::DeclId, u: &ir::UnionDecl) -> String {
        match self.union_plans.get(&id) {
            Some(UnionPlan::Typed { permits, open }) => {
                let permits = permits.clone();
                let open = *open;
                let discriminator = u
                    .discriminator
                    .clone()
                    .expect("a typed union has a discriminator");
                // (discriminator value → variant type name) dispatch arms, in
                // program order, deduped on the tag so no two `case` labels
                // collide. Every variant qualifies (that's what Typed means), so
                // each is a same-package struct referenced by its short name.
                let mut seen = BTreeSet::new();
                let mut arms: Vec<(String, String)> = Vec::new();
                for variant in &u.variants {
                    if let (Some(tag), ir::Type::Decl(vid)) =
                        (&variant.discriminator_value, &variant.ty)
                        && seen.insert(tag.clone())
                    {
                        arms.push((tag.clone(), self.names[vid].clone()));
                    }
                }
                sealed_union(name, &permits, open, &discriminator, &arms)
            }
            Some(UnionPlan::Structural) | None => structural_union(name),
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

    // --- JSON codec (D-172) -------------------------------------------------
    //
    // The serialization method bodies reference runtime helpers by fully
    // qualified name (`com.box.sdk.core.Json`, `java.util.*`, `java.time.*`),
    // so they never perturb the file's import set — the only imports are the
    // ones the component *types* already pull in. Encode/decode arms are
    // enumerated over `ir::Type`, never wildcarded, so a new IR type breaks the
    // codec at compile time rather than silently mis-serializing (NF-1).

    /// Resolve a top-level alias chain to the type it stands for. Java has no
    /// type alias, so a field typed as an alias serializes as its target.
    fn resolve_alias(&self, ty: &ir::Type) -> ir::Type {
        if let ir::Type::Decl(id) = ty
            && let ir::DeclKind::Alias(target) = &self.program.decl(*id).kind
        {
            return self.resolve_alias(&target.clone());
        }
        ty.clone()
    }

    /// Encode statement(s) that put one field into the `_m` map. The tri-state
    /// (D-110) is the reason this is statements, not one expression: an absent
    /// field is *omitted*, an explicit null writes `null`, a value writes the
    /// value — Box's clear-on-update semantics.
    fn encode_field(&mut self, ident: &str, field: &ir::Field) -> Vec<String> {
        let wire = java_string(&field.wire_name);
        let acc = format!("{ident}()");
        // Optional<Nullable<T>> is the tri-state; Optional<T> a plain optional;
        // Nullable<T> a bare nullable reference. Everything else encodes bare.
        // (`if let` chains, not a `match`, so no wildcard over `ir::Type`.)
        let ty = self.resolve_alias(&field.ty);
        if let ir::Type::Optional(inner) = &ty {
            if let ir::Type::Nullable(n) = &**inner {
                // Tri-state: omit (absent) / explicit null / value.
                let enc = self.encode_bare(n, &format!("{acc}.value()"), 0);
                vec![
                    format!("if ({acc}.isPresent()) {{ _m.put({wire}, {enc}); }}"),
                    format!("else if ({acc}.isNull()) {{ _m.put({wire}, null); }}"),
                ]
            } else {
                // A plain optional: present writes the value, absent omits.
                let enc = self.encode_bare(inner, "_v", 0);
                vec![format!("{acc}.ifPresent(_v -> _m.put({wire}, {enc}));")]
            }
        } else if let ir::Type::Nullable(inner) = &ty {
            // A bare nullable reference writes its value or an explicit `null`.
            let enc = self.encode_bare(inner, &acc, 0);
            vec![format!("_m.put({wire}, {enc});")]
        } else {
            let enc = self.encode_bare(&ty, &acc, 0);
            vec![format!("_m.put({wire}, {enc});")]
        }
    }

    /// A constructor-argument expression that decodes one field from the parsed
    /// `_m` map — the mirror of `encode_field`. The tri-state distinguishes a
    /// missing key (absent) from a present `null` (`ofNull`).
    fn decode_field(&mut self, field: &ir::Field) -> String {
        let wire = java_string(&field.wire_name);
        let get = format!("_m.get({wire})");
        let has = format!("_m.containsKey({wire})");
        let ty = self.resolve_alias(&field.ty);
        if let ir::Type::Optional(inner) = &ty {
            if let ir::Type::Nullable(n) = &**inner {
                // Tri-state: missing key → absent, present null → ofNull.
                let bare = self.bare(n);
                let dec = self.decode_bare(n, &get, 0);
                format!(
                    "!{has} ? com.box.sdk.core.Tristate.<{bare}>absent() \
                     : ({get} == null ? com.box.sdk.core.Tristate.<{bare}>ofNull() \
                     : com.box.sdk.core.Tristate.of({dec}))"
                )
            } else {
                let bare = self.bare(inner);
                let dec = self.decode_bare(inner, &get, 0);
                format!(
                    "(!{has} || {get} == null) ? java.util.Optional.<{bare}>empty() \
                     : java.util.Optional.of({dec})"
                )
            }
        } else if let ir::Type::Nullable(inner) = &ty {
            let dec = self.decode_bare(inner, &get, 0);
            format!("{get} == null ? null : {dec}")
        } else {
            self.decode_bare(&ty, &get, 0)
        }
    }

    /// Encode a bare (post-optionality) value to a JSON-tree `Object`. Types
    /// already representable in the tree (scalars, `JsonValue`, and containers
    /// of them) pass straight through; everything else is transformed, guarding
    /// `null` so a nullable element or field never dereferences.
    fn encode_bare(&mut self, ty: &ir::Type, expr: &str, depth: usize) -> String {
        if self.is_json_writable(ty) {
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
                let enc = self.encode_bare(inner, &v, depth + 1);
                format!("com.box.sdk.core.Json.encodeList({expr}, {v} -> {enc})")
            }
            ir::Type::Map(inner) => {
                let v = format!("_x{depth}");
                let enc = self.encode_bare(inner, &v, depth + 1);
                format!("com.box.sdk.core.Json.encodeMap({expr}, {v} -> {enc})")
            }
            // In-container optionality collapses to a nullable element (`bare`).
            ir::Type::Nullable(inner) | ir::Type::Optional(inner) => {
                self.encode_bare(inner, expr, depth)
            }
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(target) => {
                    let target = target.clone();
                    self.encode_bare(&target, expr, depth)
                }
                // A struct / enum / union carries its own `toJson`.
                ir::DeclKind::Struct(_) | ir::DeclKind::Union(_) | ir::DeclKind::Enum(_) => {
                    format!("({expr} == null ? null : {expr}.toJson())")
                }
            },
            // Directly writable — handled by the `is_json_writable` shortcut.
            ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::JsonValue => expr.to_string(),
        }
    }

    /// Decode a bare value from a JSON-tree `Object` `expr` — the mirror of
    /// `encode_bare`, producing a value of the `bare` Java type. `null` guards
    /// keep a nullable element or absent field from dereferencing.
    fn decode_bare(&mut self, ty: &ir::Type, expr: &str, depth: usize) -> String {
        match ty {
            ir::Type::Bool => format!("com.box.sdk.core.Json.asBoolean({expr})"),
            ir::Type::Int64 => format!("com.box.sdk.core.Json.asLong({expr})"),
            ir::Type::Float64 => format!("com.box.sdk.core.Json.asDouble({expr})"),
            ir::Type::String => format!("com.box.sdk.core.Json.asString({expr})"),
            // Free-form JSON is the parsed tree itself.
            ir::Type::JsonValue => expr.to_string(),
            ir::Type::Date => {
                format!(
                    "({expr} == null ? null : java.time.LocalDate.parse(com.box.sdk.core.Json.asString({expr})))"
                )
            }
            ir::Type::DateTime => {
                format!(
                    "({expr} == null ? null : java.time.OffsetDateTime.parse(com.box.sdk.core.Json.asString({expr})))"
                )
            }
            ir::Type::Binary => {
                format!(
                    "({expr} == null ? null : java.util.Base64.getDecoder().decode(com.box.sdk.core.Json.asString({expr})))"
                )
            }
            ir::Type::List(inner) => {
                let v = format!("_x{depth}");
                let dec = self.decode_bare(inner, &v, depth + 1);
                format!("com.box.sdk.core.Json.decodeList({expr}, {v} -> {dec})")
            }
            ir::Type::Map(inner) => {
                let v = format!("_x{depth}");
                let dec = self.decode_bare(inner, &v, depth + 1);
                format!("com.box.sdk.core.Json.decodeMap({expr}, {v} -> {dec})")
            }
            ir::Type::Nullable(inner) | ir::Type::Optional(inner) => {
                self.decode_bare(inner, expr, depth)
            }
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(target) => {
                    let target = target.clone();
                    self.decode_bare(&target, expr, depth)
                }
                ir::DeclKind::Struct(_) | ir::DeclKind::Union(_) | ir::DeclKind::Enum(_) => {
                    let ref_name = self.decl_type(*id);
                    format!("({expr} == null ? null : {ref_name}.fromJson({expr}))")
                }
            },
        }
    }

    /// Whether a value of this type is already a JSON-tree `Object` (so it can
    /// be `put` or returned as-is on encode): scalars, free-form JSON, and
    /// containers whose elements are themselves directly writable. Dates,
    /// binary, and declaration references need transformation.
    fn is_json_writable(&self, ty: &ir::Type) -> bool {
        match ty {
            ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::JsonValue => true,
            ir::Type::List(inner)
            | ir::Type::Map(inner)
            | ir::Type::Nullable(inner)
            | ir::Type::Optional(inner) => self.is_json_writable(inner),
            ir::Type::Date | ir::Type::DateTime | ir::Type::Binary => false,
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(target) => self.is_json_writable(target),
                ir::DeclKind::Struct(_) | ir::DeclKind::Union(_) | ir::DeclKind::Enum(_) => false,
            },
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
///
/// The body carries the JSON codec (D-172): an `Object toJson()` the variant
/// records implement (covariantly — a struct's `Map<String, Object> toJson()`
/// overrides it), and a `static fromJson` that reads the discriminator and
/// pattern-matches a `switch` to the right variant's `fromJson`. An open union
/// routes an unrecognized (or absent) tag to `Unknown`; a closed one throws.
fn sealed_union(
    name: &str,
    permits: &[String],
    open: bool,
    discriminator: &str,
    arms: &[(String, String)],
) -> String {
    let mut all: Vec<String> = permits.to_vec();
    if open {
        all.push(format!("{name}.Unknown"));
    }
    let permits_clause = format!("permits {}", all.join(", "));
    // One line when it fits (up to the opening brace); otherwise wrap `permits`.
    let head_len = "public sealed interface  {".len() + name.len() + permits_clause.len();
    let head = if head_len <= MAX_WIDTH {
        format!("public sealed interface {name} {permits_clause}")
    } else {
        format!("public sealed interface {name}\n    {permits_clause}")
    };

    let mut out = format!("{head} {{\n");
    out.push_str("    /** Serialize this variant, discriminator and all. */\n");
    out.push_str("    Object toJson();\n\n");
    out.push_str("    /** Dispatch on the discriminator to the matching variant. */\n");
    let _ = writeln!(out, "    static {name} fromJson(Object _json) {{");
    out.push_str(
        "        java.util.Map<String, Object> _m = com.box.sdk.core.Json.asObject(_json);\n",
    );
    let _ = writeln!(
        out,
        "        String _tag = com.box.sdk.core.Json.asString(_m.get({}));",
        java_string(discriminator)
    );
    out.push_str("        return switch (_tag) {\n");
    for (tag, variant) in arms {
        let _ = writeln!(
            out,
            "            case {} -> {variant}.fromJson(_json);",
            java_string(tag)
        );
    }
    if open {
        out.push_str("            case null, default -> new Unknown(_json);\n");
    } else {
        let _ = writeln!(
            out,
            "            case null, default -> throw new IllegalArgumentException(\
             \"unknown {name} discriminator: \" + _tag);"
        );
    }
    out.push_str("        };\n");
    out.push_str("    }\n");
    if open {
        out.push('\n');
        out.push_str("    /** An unrecognized discriminator, retained verbatim (open union). */\n");
        let _ = writeln!(out, "    record Unknown(Object value) implements {name} {{");
        out.push_str("        public Object toJson() {\n");
        out.push_str("            return value;\n");
        out.push_str("        }\n");
        out.push_str("    }\n");
    }
    out.push_str("}\n");
    out
}

/// The structural union fallback: a transparent `record(Object value)` newtype
/// whose codec (D-172) passes the raw parsed value straight through, so an
/// arbitrary `oneOf` shape round-trips untouched.
fn structural_union(name: &str) -> String {
    let mut out = format!("public record {name}(Object value) {{\n");
    out.push_str("    public Object toJson() {\n        return value;\n    }\n\n");
    let _ = writeln!(out, "    public static {name} fromJson(Object _json) {{");
    let _ = writeln!(out, "        return new {name}(_json);");
    out.push_str("    }\n}\n");
    out
}

/// Whether an FQN import belongs to the `java.base` module — the model layer
/// only ever imports `java.util.*` and `java.time.*`, both in `java.base`, so
/// they collapse into a single `import module java.base;` (JEP 511). Anything
/// else (the SDK's own `com.box.sdk.core.Tristate`) stays an explicit import.
fn in_java_base(import: &str) -> bool {
    import.starts_with("java.util.") || import.starts_with("java.time.")
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

/// One collision-free `(ident, field)` per struct field, allocated per record.
/// Distinct source names can normalize to the same Java identifier
/// (`displayName` and `display_name` both → `displayName`); this keeps them
/// apart so the record's accessors, constructor, and codec all agree — and the
/// managers backend reuses it to read a struct's fields (e.g. a form body).
pub(crate) fn struct_components(s: &ir::StructDecl) -> Vec<(String, &ir::Field)> {
    let mut used: Vec<String> = Vec::new();
    s.fields
        .iter()
        .map(|field| {
            (
                dedupe(&mut used, component_ident(field.name.as_str())),
                field,
            )
        })
        .collect()
}

/// A record-component identifier: camelCase, guarded against Java keywords,
/// `Object`'s method names (a component generates an accessor of that name, so
/// `hashCode`/`toString`/… would clash with the record's own members), and the
/// `toJson`/`fromJson` codec members this backend adds to every model type.
pub(crate) fn component_ident(name: &str) -> String {
    let base = sanitize_ident(&camel(name));
    if JAVA_KEYWORDS.contains(&base.as_str())
        || OBJECT_METHODS.contains(&base.as_str())
        || CODEC_METHODS.contains(&base.as_str())
    {
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
pub(crate) fn dedupe(used: &mut Vec<String>, base: String) -> String {
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

/// The serialization members this backend adds to every model type (D-172); a
/// record component of the same name would clash with its accessor.
const CODEC_METHODS: &[&str] = &["toJson", "fromJson"];

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
pub(crate) const JAVA_KEYWORDS: &[&str] = &[
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
        // java.base library types (java.util/java.time) collapse to one module
        // import (JEP 511); no per-type `import java.util.Optional;` line.
        assert!(out.contains("import module java.base;"), "{out}");
        assert!(!out.contains("import java.util."), "{out}");
        // Optional<Nullable<T>> → the tri-state wrapper (imported from core, kept
        // explicit — not part of java.base).
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
        assert!(out.contains("public record Empty() {"), "{out}");
        // An empty record still carries the codec: an empty map out, a bare
        // constructor back (D-172).
        assert!(
            out.contains("public java.util.Map<String, Object> toJson()"),
            "{out}"
        );
        assert!(out.contains("return new Empty();"), "{out}");
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
        assert!(out.contains("public record Pet(Object value) {"), "{out}");
        // The structural fallback's codec passes the raw value straight through.
        assert!(out.contains("public Object toJson() {"), "{out}");
        assert!(out.contains("return new Pet(_json);"), "{out}");
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
            iface.contains("record Unknown(Object value) implements Pet {"),
            "{iface}"
        );
        // The codec (D-172) reads the discriminator and dispatches; an
        // unrecognized (or absent) tag routes to Unknown so it round-trips.
        assert!(
            iface.contains("String _tag = com.box.sdk.core.Json.asString(_m.get(\"kind\"));"),
            "{iface}"
        );
        assert!(
            iface.contains("case \"dog\" -> Dog.fromJson(_json);"),
            "{iface}"
        );
        assert!(
            iface.contains("case null, default -> new Unknown(_json);"),
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
            iface.contains("public sealed interface Pet permits Dog, Cat {"),
            "{iface}"
        );
        assert!(!iface.contains("Unknown"), "{iface}");
        // A closed union rejects an unrecognized tag rather than retaining it.
        assert!(
            iface.contains("case null, default -> throw new IllegalArgumentException("),
            "{iface}"
        );
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
        assert!(out.contains("public record Pet(Object value) {"), "{out}");
        assert!(out.contains("return new Pet(_json);"), "{out}");
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
    fn struct_codec_encodes_tri_state_and_optional() {
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
                        wire_name: "display_name".into(),
                        ty: Type::Optional(Box::new(Type::String)),
                    },
                    Field {
                        name: ident("size"),
                        wire_name: "size".into(),
                        ty: Type::Optional(Box::new(Type::Nullable(Box::new(Type::Int64)))),
                    },
                ],
            }),
            "Widget",
        );
        let out = render(&p, s);
        // Required field: put straight through; read via a typed coercion.
        assert!(out.contains(r#"_m.put("id", id());"#), "{out}");
        assert!(
            out.contains(r#"com.box.sdk.core.Json.asString(_m.get("id"))"#),
            "{out}"
        );
        // Optional: present writes the value (under its *wire* name), absent
        // omits; decode maps a missing/null key to empty.
        assert!(
            out.contains(r#"displayName().ifPresent(_v -> _m.put("display_name", _v));"#),
            "{out}"
        );
        assert!(
            out.contains(r#"java.util.Optional.<String>empty()"#),
            "{out}"
        );
        // Tri-state (D-110): omit / explicit null / value on encode; the three
        // states reconstructed on decode.
        assert!(
            out.contains(r#"if (size().isPresent()) { _m.put("size", size().value()); }"#),
            "{out}"
        );
        assert!(
            out.contains(r#"else if (size().isNull()) { _m.put("size", null); }"#),
            "{out}"
        );
        assert!(
            out.contains("com.box.sdk.core.Tristate.<Long>absent()"),
            "{out}"
        );
        assert!(
            out.contains("com.box.sdk.core.Tristate.<Long>ofNull()"),
            "{out}"
        );
    }

    #[test]
    fn struct_codec_maps_lists_and_nested_decls() {
        let mut p = ir::Program::default();
        let owner = add(
            &mut p,
            DeclKind::Struct(StructDecl { fields: vec![] }),
            "Owner",
        );
        let s = add(
            &mut p,
            DeclKind::Struct(StructDecl {
                fields: vec![
                    Field {
                        name: ident("tags"),
                        wire_name: "tags".into(),
                        ty: Type::List(Box::new(Type::String)),
                    },
                    Field {
                        name: ident("owner"),
                        wire_name: "owner".into(),
                        ty: Type::Decl(owner),
                    },
                ],
            }),
            "Widget",
        );
        let out = render(&p, s);
        // A list of directly-writable elements is `put` as-is; decoded via the
        // typed helper so no unchecked cast leaks.
        assert!(out.contains(r#"_m.put("tags", tags());"#), "{out}");
        assert!(
            out.contains(r#"com.box.sdk.core.Json.decodeList(_m.get("tags"), _x0 -> com.box.sdk.core.Json.asString(_x0))"#),
            "{out}"
        );
        // A nested declaration delegates to its own codec, null-guarded.
        assert!(
            out.contains(r#"_m.put("owner", (owner() == null ? null : owner().toJson()));"#),
            "{out}"
        );
        assert!(
            out.contains(r#"(_m.get("owner") == null ? null : Owner.fromJson(_m.get("owner")))"#),
            "{out}"
        );
    }

    #[test]
    fn enum_codecs_round_trip_wire_values() {
        let mut p = ir::Program::default();
        let open = add(
            &mut p,
            DeclKind::Enum(EnumDecl {
                values: vec!["asc".into()],
                extensibility: Extensibility::Open,
            }),
            "Direction",
        );
        let closed = add(
            &mut p,
            DeclKind::Enum(EnumDecl {
                values: vec!["foo".into()],
                extensibility: Extensibility::Closed,
            }),
            "Kind",
        );
        // Open enum: transparent over the raw string (unknown round-trips).
        let open_out = render(&p, open);
        assert!(
            open_out.contains("public String toJson() {\n        return value;"),
            "{open_out}"
        );
        assert!(
            open_out.contains("return new Direction(com.box.sdk.core.Json.asString(_json));"),
            "{open_out}"
        );
        // Closed enum: maps to/from the wire spelling and rejects the unknown.
        let closed_out = render(&p, closed);
        assert!(
            closed_out.contains("public String toJson() {\n        return wireValue;"),
            "{closed_out}"
        );
        assert!(
            closed_out.contains("if (_v.wireValue.equals(_w)) {"),
            "{closed_out}"
        );
        assert!(
            closed_out.contains(
                r#"throw new IllegalArgumentException("unknown Kind wire value: " + _w);"#
            ),
            "{closed_out}"
        );
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
