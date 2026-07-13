//! Apex model lowering: IR declarations → top-level Apex classes.
//!
//! Apex has one flat namespace, so each schema becomes its own top-level
//! `.cls` with a globally-unique, deterministically-mangled name
//! (TR-Apex.1). The module tree — which Go/Rust keep as packages —
//! collapses into that name here; the rich IR module concept is never
//! shaped by Apex's flatness (assessment §8 risk).
//!
//! - **Struct** → a class of public fields. Every reference type is
//!   nullable in Apex, so both D-110 tri-state wrappers erase at the type
//!   level; absent-vs-null is the serializer's job (a later slice).
//! - **Open enum** → a class holding a `String value` plus `static final
//!   String` constants, so unknown values round-trip (a real Apex `enum`
//!   cannot retain them — D-105/G-11).
//! - **Discriminated union** → a class with a generated
//!   `JSON.deserializeUntyped` dispatch on the tag, retaining unknown tags
//!   (open unions, G-10/TR-Apex.4).
//! - **Structural union** → an `Object value` (the manifest-accepted loss).
//! - **Alias** → nothing; references resolve through it (Apex has no
//!   type aliases).

use std::collections::HashSet;
use std::fmt::Write as _;

use gantry_ir as ir;
use gantry_ir::naming::{constant, pascal};
use gantry_manifest::{CapabilityManifest, ModuleSystem};
use gantry_sema::Analysis;

use crate::{CLASSES_DIR, GeneratedFile, fnv1a_64, safe_word};

/// Generate the Apex model classes for a verified program — one top-level
/// class per schema declaration, mangled to the manifest's identifier
/// limit. Deterministic (FR-6.2): identical inputs → byte-identical output.
pub fn generate_models(
    analysis: &Analysis<'_>,
    manifest: &CapabilityManifest,
) -> Vec<GeneratedFile> {
    let ModuleSystem::Flat { identifier_limit } = manifest.modules else {
        // The Apex backend exists for the flat-namespace axis; a
        // hierarchical manifest here is an engine bug (NF-1), not a config.
        panic!("the Apex backend requires the flat-namespace manifest axis");
    };
    let program = analysis.program;
    let names = ClassNames::build(program, identifier_limit as usize);

    let mut files = Vec::new();
    for (index, decl) in program.decls.iter().enumerate() {
        if let Some(content) = render_decl(program, &names, ir::DeclId(index as u32), decl) {
            let name = names
                .get(ir::DeclId(index as u32))
                .expect("rendered decl has a name");
            files.push(GeneratedFile {
                path: format!("{CLASSES_DIR}/{name}.cls"),
                content,
            });
        }
    }
    files
}

/// The stable, unique top-level class name for every non-alias declaration,
/// indexed by `DeclId`. Built once in program order so a reference always
/// renders to the same name as the class it points at.
pub(crate) struct ClassNames {
    names: Vec<Option<String>>,
}

impl ClassNames {
    pub(crate) fn build(program: &ir::Program, limit: usize) -> Self {
        let mut names = vec![None; program.decls.len()];
        let mut used: HashSet<String> = HashSet::new();
        for (index, decl) in program.decls.iter().enumerate() {
            if matches!(decl.kind, ir::DeclKind::Alias(_)) {
                continue; // aliases resolve through — no class, no name
            }
            // A schema named for a reserved type (`Group`, `Date`, …) can't
            // be a bare class name either — shape + reserved-word escape
            // applies to the class identifier just like a field.
            names[index] = Some(mint_unique(&safe_word(&base_name(decl)), limit, &mut used));
        }
        Self { names }
    }

    pub(crate) fn get(&self, id: ir::DeclId) -> Option<&str> {
        self.names[id.0 as usize].as_deref()
    }

    /// Every minted class name (aliases excluded) — the seed for keeping
    /// manager/client/stub names globally unique in the flat namespace.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().filter_map(Option::as_deref)
    }
}

/// The flat-namespace base name (TR-Apex.1): the module path (minus the
/// leading `schemas` segment) collapses into a Pascal prefix, then the decl
/// name. Versioned modules (`schemas::v2025_0`) get their version in the
/// prefix so `File@2024` and `File@2025` never collide.
fn base_name(decl: &ir::Decl) -> String {
    let mut name = String::new();
    for segment in decl.module.0.iter().skip(1) {
        name.push_str(&pascal(segment.as_str()));
    }
    name.push_str(decl.name.as_str());
    name
}

/// Register a unique identifier, abbreviating deterministically when it
/// exceeds the platform limit: `prefix_<7-hex FNV>`, then a numeric suffix
/// if that still collides. Same inputs (in the same order) → same output.
pub(crate) fn mint_unique(base: &str, limit: usize, used: &mut HashSet<String>) -> String {
    let candidate = if base.len() <= limit {
        base.to_string()
    } else {
        let hash = fnv1a_64(base.as_bytes()) & 0xFFF_FFFF;
        let keep = limit.saturating_sub(8);
        format!("{}_{hash:07x}", &base[..keep])
    };
    if used.insert(candidate.clone()) {
        return candidate;
    }
    // Collision (rare): append the smallest numeric suffix that fits.
    for n in 1u32.. {
        let suffix = format!("_{n}");
        let keep = limit.saturating_sub(suffix.len());
        let disambiguated = format!("{}{suffix}", &candidate[..candidate.len().min(keep)]);
        if used.insert(disambiguated.clone()) {
            return disambiguated;
        }
    }
    unreachable!("the numeric-suffix space is unbounded")
}

/// Render one declaration to Apex source, or `None` for an alias.
fn render_decl(
    program: &ir::Program,
    names: &ClassNames,
    id: ir::DeclId,
    decl: &ir::Decl,
) -> Option<String> {
    let name = names.get(id)?;
    let mut out = format!(
        "// Code generated by box-gantry {}. DO NOT EDIT.\n",
        env!("CARGO_PKG_VERSION")
    );
    match &decl.kind {
        ir::DeclKind::Struct(s) => {
            let _ = writeln!(
                out,
                "/** Model for the `{name}` schema ({count} field(s)). Fields\n \
                 * carry their wire (JSON) name in a trailing comment. */",
                count = s.fields.len()
            );
            let _ = writeln!(out, "public class {name} {{");
            for field in &s.fields {
                let ty = apex_type(program, names, &field.ty);
                let field_name = safe_word(field.name.as_str());
                let _ = writeln!(
                    out,
                    "    public {ty} {field_name}; // wire: {}",
                    field.wire_name
                );
            }
            let _ = writeln!(out, "}}");
        }
        ir::DeclKind::Enum(e) => {
            // Open enum: a constants namespace. The *field* type is `String`
            // (see `apex_type`), so JSON round-trips the value natively —
            // unknown values included; this class just names the known ones.
            let _ = writeln!(
                out,
                "/** Open enum `{name}`: known values as `String` constants.\n \
                 * A field of this type is a `String`, so unknown values from\n \
                 * the API round-trip unchanged. */"
            );
            let _ = writeln!(out, "public class {name} {{");
            // Constant names are the shared PascalCase identifier form, made
            // Apex-safe (a value like `Date`/`Group`/`ASC` is a reserved
            // word and is rejected as a bare identifier), then deduped within
            // the class so two wire values that collapse to the same
            // identifier can't emit a duplicate `static final`. Apex
            // identifiers are **case-insensitive**, so the dedup is keyed on
            // the lowercased name — `ASC` and `asc` both escape to `Asc_r`,
            // which the platform rejects as a duplicate field.
            let mut used: HashSet<String> = HashSet::new();
            for value in &e.values {
                let base = safe_word(&constant(value));
                let mut constant_name = base.clone();
                for n in 2u32.. {
                    if used.insert(constant_name.to_ascii_lowercase()) {
                        break;
                    }
                    constant_name = format!("{base}{n}");
                }
                let _ = writeln!(
                    out,
                    "    public static final String {constant_name} = '{}';",
                    escape(value)
                );
            }
            let _ = writeln!(out, "}}");
        }
        ir::DeclKind::Union(u) => render_union(&mut out, names, name, u),
        ir::DeclKind::Alias(_) => return None,
    }
    Some(out)
}

fn render_union(out: &mut String, names: &ClassNames, name: &str, u: &ir::UnionDecl) {
    match &u.discriminator {
        Some(_) => {
            let _ = writeln!(
                out,
                "/** Discriminated union `{name}`: call `{name}.parse(map)` on\n \
                 * the decoded JSON object to dispatch on its `type` tag; an\n \
                 * unknown tag returns the raw `Map<String, Object>`. */"
            );
        }
        None => {
            let _ = writeln!(
                out,
                "/** Structural union `{name}`: the value is an untyped\n \
                 * `Object`; inspect its shape at the call site. */"
            );
        }
    }
    let _ = writeln!(out, "public class {name} {{");
    match &u.discriminator {
        Some(discriminator) => {
            // TR-Apex.4: generated deserializeUntyped dispatch on the tag.
            let _ = writeln!(
                out,
                "    public static Object parse(Map<String, Object> untyped) {{"
            );
            let _ = writeln!(
                out,
                "        String tag = (String) untyped.get('{}');",
                escape(discriminator)
            );
            for variant in &u.variants {
                if let (Some(value), ir::Type::Decl(id)) =
                    (&variant.discriminator_value, &variant.ty)
                    && let Some(variant_ty) = names.get(*id)
                {
                    let _ = writeln!(
                        out,
                        "        if (tag == '{}') return ({variant_ty}) JSON.deserialize(JSON.serialize(untyped), {variant_ty}.class);",
                        escape(value)
                    );
                }
            }
            // Open union (G-10): an unknown tag round-trips as the raw map.
            let _ = writeln!(out, "        return untyped;");
            let _ = writeln!(out, "    }}");
        }
        None => {
            // A structural union erases to Object — the caller inspects the
            // shape (manifest-accepted loss, recorded for conformance).
            let _ = writeln!(out, "    public Object value;");
        }
    }
    let _ = writeln!(out, "}}");
}

/// Map an IR type to its Apex type expression. Built-in `List`/`Map` are
/// available (the no-generics axis forbids *user-defined* generics, not the
/// platform collections). Both tri-state wrappers erase — every Apex
/// reference is nullable, so absent-vs-null is the serializer's concern.
pub(crate) fn apex_type(program: &ir::Program, names: &ClassNames, ty: &ir::Type) -> String {
    match ty {
        ir::Type::Optional(inner) | ir::Type::Nullable(inner) => apex_type(program, names, inner),
        ir::Type::List(inner) => format!("List<{}>", apex_type(program, names, inner)),
        ir::Type::Map(inner) => format!("Map<String, {}>", apex_type(program, names, inner)),
        ir::Type::Bool => "Boolean".to_string(),
        ir::Type::Int64 => "Long".to_string(),
        ir::Type::Float64 => "Double".to_string(),
        ir::Type::String => "String".to_string(),
        ir::Type::Date => "Date".to_string(),
        ir::Type::DateTime => "Datetime".to_string(),
        // Buffered platform (manifest Streaming::Buffered): bytes are a Blob
        // in heap, never a stream.
        ir::Type::Binary => "Blob".to_string(),
        ir::Type::JsonValue => "Object".to_string(),
        ir::Type::Decl(id) => {
            let decl = program.decl(*id);
            match &decl.kind {
                // No Apex aliases — resolve through to the target type.
                ir::DeclKind::Alias(inner) => apex_type(program, names, inner),
                // An open enum is a `String` on the wire (its class is a
                // constants namespace), so it round-trips natively via
                // JSON.deserialize, unknown values included.
                ir::DeclKind::Enum(_) => "String".to_string(),
                // A union deserializes as its raw untyped map; the field is
                // `Object` and the caller dispatches with `<Union>.parse`.
                // Typing it as the dispatch-only class would make
                // JSON.deserialize mis-map the wire object.
                ir::DeclKind::Union(_) => "Object".to_string(),
                ir::DeclKind::Struct(_) => names
                    .get(*id)
                    .expect("a struct always has a class name")
                    .to_string(),
            }
        }
    }
}

/// Escape a string literal for Apex single-quoted strings.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}
