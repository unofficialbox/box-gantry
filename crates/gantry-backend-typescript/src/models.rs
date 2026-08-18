//! Model generation: IR declarations → TypeScript source files.
//!
//! One `.ts` module per IR module (API versions redefine names, and the base
//! and versioned modules share no references, so each is its own module —
//! mirroring D-147). The type system is a near-structural fit for the IR:
//!
//! - struct → `export interface`, with the tri-state mapped straight onto the
//!   type system — absent → `field?: T`, explicit null → `T | null`, so the
//!   absent-vs-null distinction needs no wrapper (TR-TS.2);
//! - open enum → a string-literal union widened with `(string & {})`; closed
//!   enum → the bare literal union (TR-TS.1);
//! - discriminated union → a union of its variant interfaces, each carrying its
//!   own literal discriminator; open unions add a catch-all so an unknown tag is
//!   retained (TR-TS.1);
//! - alias → `export type`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use gantry_ir as ir;
use gantry_ir::naming::pascal;
use gantry_sema::Analysis;

use crate::{BuildInfo, GeneratedFile};

/// One rendered model file: an IR module, optionally narrowed to one
/// manager's bucket within it (D-201). The catch-all (`bucket: None`) keeps
/// the module's own path and name; a bucket lives one directory deeper.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Unit {
    module: String,
    bucket: Option<String>,
}

impl Unit {
    fn path(&self) -> String {
        match &self.bucket {
            None => format!("src/models/{}.ts", self.module),
            Some(bucket) => format!("src/models/{}/{bucket}.ts", self.module),
        }
    }

    /// The relative specifier to import `to` from inside this unit. A bucket
    /// file sits one directory deeper than the catch-all, hence the `../`.
    fn specifier_to(&self, to: &Unit) -> String {
        let prefix = if self.bucket.is_some() { ".." } else { "." };
        match &to.bucket {
            None => format!("{prefix}/{}.js", to.module),
            Some(bucket) => format!("{prefix}/{}/{bucket}.js", to.module),
        }
    }
}

/// Generate `src/models/<module>.ts` per IR module (further split per manager,
/// D-201) plus the `index.ts` barrel.
pub fn generate_models(analysis: &Analysis<'_>, build: &BuildInfo) -> Vec<GeneratedFile> {
    let program = analysis.program;
    let modules = module_names(program);
    // Manager tag → TypeScript module base, the same allocation
    // `managers.rs` uses for `src/managers/<module>.ts` (D-201): a
    // per-manager schema file and its manager file can never name different
    // managers for one tag.
    let manager_modules: BTreeMap<&str, String> = crate::managers::plan_managers(analysis)
        .into_iter()
        .map(|(key, _, name)| (key.as_str(), name.module))
        .collect();
    // Every declaration's home file and TypeScript name, indexed by `DeclId`
    // (== position in `program.decls`) so type references resolve — and,
    // when they cross a file boundary, produce an `import`.
    let decls: Vec<(Unit, String)> = program
        .decls
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let unit = Unit {
                module: modules[&d.module].clone(),
                bucket: analysis.sole_manager(i).map(|m| manager_modules[m].clone()),
            };
            (unit, type_name(d.name.as_str()))
        })
        .collect();

    // Group declaration indices by module, in program order (stable output).
    let mut grouped: Vec<(ir::ModulePath, Vec<usize>)> = Vec::new();
    for (index, decl) in program.decls.iter().enumerate() {
        match grouped.iter_mut().find(|(m, _)| *m == decl.module) {
            Some((_, v)) => v.push(index),
            None => grouped.push((decl.module.clone(), vec![index])),
        }
    }
    let mut named: Vec<(String, Vec<usize>, Option<&ir::ApiVersion>)> = grouped
        .iter()
        .map(|(m, indices)| {
            let version = indices
                .first()
                .and_then(|&i| program.decls[i].api_version.as_ref());
            (modules[m].clone(), indices.clone(), version)
        })
        .collect();
    named.sort_by(|a, b| a.0.cmp(&b.0));

    let mut files = Vec::new();
    for (name, indices, version) in &named {
        let (shared, buckets) = analysis.bucket_decls(indices);
        let reexports: Vec<String> = buckets
            .keys()
            .map(|manager| manager_modules[manager.as_str()].clone())
            .collect();
        let catch_all = Unit {
            module: name.clone(),
            bucket: None,
        };
        files.push(render_file(
            program, &decls, &catch_all, &shared, &reexports, *version, build,
        ));
        for (manager, bucket_indices) in &buckets {
            let unit = Unit {
                module: name.clone(),
                bucket: Some(manager_modules[manager.as_str()].clone()),
            };
            files.push(render_file(
                program,
                &decls,
                &unit,
                bucket_indices,
                &[],
                *version,
                build,
            ));
        }
    }
    // The barrel re-exports each module's catch-all as a namespace — bucket
    // files aren't named here at all; the catch-all's own `export *` reaches
    // them (see render_file). The version merge (D-190) collapses the
    // versioned schema modules into one `schemas`, so a name shared across
    // API versions is a single `models.schemas.ClientError`.
    let mut index = String::from("// Code generated by box-gantry. DO NOT EDIT.\n\n");
    for (name, _, _) in &named {
        let _ = writeln!(index, "export * as {name} from './{name}.js';");
    }
    files.push(GeneratedFile {
        path: "src/models/index.ts".to_string(),
        content: index,
    });
    files
}

/// Render one file: `unit` places it (the catch-all at
/// `src/models/<module>.ts`, a bucket at `src/models/<module>/<bucket>.ts`,
/// D-201); `reexports` (only non-empty for the catch-all) are the buckets
/// split out of this module, so the catch-all can `export *` them and keep
/// `models.<module>.Type` valid for every type regardless of which file
/// declares it.
fn render_file(
    program: &ir::Program,
    decls: &[(Unit, String)],
    unit: &Unit,
    indices: &[usize],
    reexports: &[String],
    version: Option<&ir::ApiVersion>,
    build: &BuildInfo,
) -> GeneratedFile {
    let mut printer = Printer {
        program,
        decls,
        unit,
        imports: BTreeMap::new(),
        body: String::new(),
    };
    for &index in indices {
        printer.decl(&program.decls[index]);
    }

    let api_version = version.map_or("unversioned", |v| v.0.as_str());
    let mut content = format!(
        "// Code generated by box-gantry {} (spec {}) for Box API {api_version}. DO NOT EDIT.\n\n",
        build.engine, build.spec_fingerprint
    );
    // Cross-file type references, as `import type` (erased at build time).
    for (specifier, names) in &printer.imports {
        let list = names.iter().cloned().collect::<Vec<_>>().join(", ");
        let _ = writeln!(content, "import type {{ {list} }} from '{specifier}';");
    }
    if !printer.imports.is_empty() {
        content.push('\n');
    }
    // Re-export every per-manager file, so `models.<module>.<Type>` names
    // every type of the module regardless of which file declares it — the
    // split does not move a single name on the public surface. Mirrors the
    // manager barrel (managers.rs).
    for reexport in reexports {
        let _ = writeln!(content, "export * from './{}/{reexport}.js';", unit.module);
    }
    if !reexports.is_empty() {
        content.push('\n');
    }
    content.push_str(printer.body.trim_end());
    // An empty-body file (a bucket-less catch-all with only re-exports) must
    // not end in a stray blank line, or the generated-file gate's formatter
    // check fails.
    content.truncate(content.trim_end().len());
    content.push('\n');

    GeneratedFile {
        path: unit.path(),
        content,
    }
}

struct Printer<'p> {
    program: &'p ir::Program,
    /// `DeclId` → (home file, TypeScript name).
    decls: &'p [(Unit, String)],
    /// The file currently being rendered (references to it stay bare).
    unit: &'p Unit,
    /// Cross-file references to import: import specifier → the names it
    /// provides.
    imports: BTreeMap<String, BTreeSet<String>>,
    body: String,
}

impl Printer<'_> {
    fn decl(&mut self, decl: &ir::Decl) {
        let name = type_name(decl.name.as_str());
        match &decl.kind {
            ir::DeclKind::Struct(s) => self.struct_decl(&name, s),
            ir::DeclKind::Enum(e) => self.enum_decl(&name, e),
            ir::DeclKind::Union(u) => self.union_decl(&name, u),
            ir::DeclKind::Alias(ty) => {
                let target = self.ts_type(ty);
                let _ = writeln!(self.body, "export type {name} = {target};\n");
            }
        }
    }

    fn struct_decl(&mut self, name: &str, s: &ir::StructDecl) {
        if s.fields.is_empty() && s.extra.is_none() {
            // An empty interface is a lint smell; a type alias to an empty
            // object is the tsc-clean equivalent.
            let _ = writeln!(self.body, "export type {name} = Record<string, never>;\n");
            return;
        }
        if s.fields.is_empty() {
            // A pure open map (D-196, no named fields — `GenericSource`'s
            // real shape): nothing to intersect with, just the map itself.
            let extra_ty = self.ts_type(s.extra.as_ref().expect("checked above"));
            let _ = writeln!(
                self.body,
                "export type {name} = Record<string, {extra_ty}>;\n"
            );
            return;
        }
        // D-196: named fields alongside a non-`false` `additionalProperties`.
        // An `interface` with both named properties *and* an index
        // signature forces every named property's type to be assignable to
        // the index signature's value type — a real constraint tsc
        // enforces, and one an arbitrary typed `additionalProperties`
        // wouldn't generally satisfy. Intersecting a plain object type with
        // `Record<string, T>` instead sidesteps that: each half of the
        // intersection keeps its own member types.
        let extra_ty = s.extra.as_ref().map(|t| self.ts_type(t));
        let (keyword, eq) = match extra_ty {
            Some(_) => ("type", "= "),
            None => ("interface", ""),
        };
        let _ = writeln!(self.body, "export {keyword} {name} {eq}{{");
        for field in &s.fields {
            // Peel the tri-state wrappers: an outer `Optional` makes the key
            // optional (`?:`), a `Nullable` widens the value with `| null`.
            let mut ty = &field.ty;
            let mut optional = false;
            let mut nullable = false;
            loop {
                match ty {
                    ir::Type::Optional(inner) => {
                        optional = true;
                        ty = inner;
                    }
                    ir::Type::Nullable(inner) => {
                        nullable = true;
                        ty = inner;
                    }
                    // A non-wrapper type: the field's base type is reached
                    // (enumerated, never wildcarded — NF-1).
                    ir::Type::Bool
                    | ir::Type::Int64
                    | ir::Type::Float64
                    | ir::Type::String
                    | ir::Type::Date
                    | ir::Type::DateTime
                    | ir::Type::Binary
                    | ir::Type::JsonValue
                    | ir::Type::List(_)
                    | ir::Type::Map(_)
                    | ir::Type::Decl(_) => break,
                }
            }
            let base = self.ts_type(ty);
            let key = field_key(&field.wire_name);
            let opt = if optional { "?" } else { "" };
            let null = if nullable { " | null" } else { "" };
            let _ = writeln!(self.body, "  {key}{opt}: {base}{null};");
        }
        match extra_ty {
            Some(extra_ty) => {
                let _ = writeln!(self.body, "}} & Record<string, {extra_ty}>;\n");
            }
            None => self.body.push_str("}\n\n"),
        }
    }

    fn enum_decl(&mut self, name: &str, e: &ir::EnumDecl) {
        if e.values.is_empty() {
            let _ = writeln!(self.body, "export type {name} = string;\n");
            return;
        }
        let mut variants: Vec<String> = e.values.iter().map(|v| format!("{v:?}")).collect();
        // Open enums retain unknown values: widen with `(string & {})`, which
        // keeps literal autocomplete while accepting any string (TR-TS.1).
        if matches!(e.extensibility, ir::Extensibility::Open) {
            variants.push("(string & {})".to_string());
        }
        let _ = writeln!(
            self.body,
            "export type {name} = {};\n",
            variants.join(" | ")
        );
    }

    fn union_decl(&mut self, name: &str, u: &ir::UnionDecl) {
        match discriminated_variants(self.program, u) {
            Some(variants) => {
                // Safe: `discriminated_variants` only returns `Some` when the
                // union has a discriminator.
                let disc = field_key(u.discriminator.as_deref().unwrap());
                let mut members: Vec<String> = variants
                    .iter()
                    .map(|(id, value)| {
                        // Pin the discriminator to its literal via an
                        // intersection, so the union narrows even though the
                        // variant's own discriminator field is an open
                        // (string-widened) enum (TR-TS.1).
                        format!("({} & {{ {disc}: {value:?} }})", self.decl_ref(*id))
                    })
                    .collect();
                // Open unions keep an unrecognized tag: the discriminator is
                // present but matches no known literal (TR-TS.1). Requiring the
                // key keeps the catch-all from swallowing `{}`; closed unions
                // reject an unknown tag structurally.
                if matches!(u.extensibility, ir::Extensibility::Open) {
                    members.push(format!("{{ {disc}: string; [key: string]: unknown }}"));
                }
                let _ = writeln!(self.body, "export type {name} = {};\n", members.join(" | "));
            }
            // A union without a clean discriminator lowers to `unknown` — the
            // caller inspects it (mirrors the structural fallback in Go/Rust).
            None => {
                let _ = writeln!(self.body, "export type {name} = unknown;\n");
            }
        }
    }

    /// A TypeScript type expression for an IR type, recording any cross-module
    /// declaration reference as an import.
    fn ts_type(&mut self, ty: &ir::Type) -> String {
        match ty {
            ir::Type::Bool => "boolean".into(),
            ir::Type::Int64 => "number".into(),
            ir::Type::Float64 => "number".into(),
            ir::Type::String => "string".into(),
            // Box dates/date-times are ISO-8601 strings on the wire.
            ir::Type::Date => "string".into(),
            ir::Type::DateTime => "string".into(),
            ir::Type::Binary => "Blob".into(),
            ir::Type::JsonValue => "unknown".into(),
            ir::Type::List(inner) => format!("Array<{}>", self.ts_type(inner)),
            ir::Type::Map(inner) => format!("Record<string, {}>", self.ts_type(inner)),
            ir::Type::Decl(id) => self.decl_ref(*id),
            ir::Type::Optional(inner) => format!("{} | undefined", self.ts_type(inner)),
            ir::Type::Nullable(inner) => format!("{} | null", self.ts_type(inner)),
        }
    }

    /// Reference a declaration by its TypeScript name, importing it when it
    /// lives in another file — another IR module, or another manager's
    /// bucket within this one (D-201).
    fn decl_ref(&mut self, id: ir::DeclId) -> String {
        let (home, name) = &self.decls[id.0 as usize];
        if home != self.unit {
            self.imports
                .entry(self.unit.specifier_to(home))
                .or_default()
                .insert(name.clone());
        }
        name.clone()
    }
}

/// The `(declaration id, discriminator value)` pairs a discriminated union
/// lowers to — every variant a tagged, discriminator-carrying struct — or
/// `None` when it lowers to the structural `unknown` fallback (no
/// discriminator, or any variant that isn't a discriminator-carrying
/// declaration). The literal value lets each union member pin its tag so the
/// union narrows.
pub(crate) fn discriminated_variants(
    program: &ir::Program,
    u: &ir::UnionDecl,
) -> Option<Vec<(ir::DeclId, String)>> {
    let discriminator = u.discriminator.as_deref()?;
    u.variants
        .iter()
        .map(|v| match (&v.discriminator_value, &v.ty) {
            (Some(value), ir::Type::Decl(id))
                if decl_carries_field(program, *id, discriminator) =>
            {
                Some((*id, value.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Whether a declaration is a struct with a field serialized under `wire_name`
/// — the invariant a typed discriminated union relies on.
fn decl_carries_field(program: &ir::Program, id: ir::DeclId, wire_name: &str) -> bool {
    matches!(
        &program.decl(id).kind,
        ir::DeclKind::Struct(s) if s.fields.iter().any(|f| f.wire_name == wire_name)
    )
}

/// An interface member key: the wire name verbatim when it is a valid
/// identifier (serialization is identity, TR-TS.2), quoted otherwise.
fn field_key(wire: &str) -> String {
    let mut chars = wire.chars();
    let valid = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if valid {
        wire.to_string()
    } else {
        format!("{wire:?}")
    }
}

/// A TypeScript type name for an IR declaration: PascalCase, kept clear of the
/// ambient/global type names it would otherwise shadow.
pub(crate) fn type_name(name: &str) -> String {
    let pascal = pascal(name);
    if GLOBAL_TYPES.contains(&pascal.as_str()) {
        format!("{pascal}_")
    } else {
        pascal
    }
}

/// A collision-free lowercase module name per IR module path — flattening a
/// path with `_` is not injective (`[a_b]` and `[a, b]` collapse), so names are
/// allocated deterministically and shared, never recomputed (mirrors D-149).
pub(crate) fn module_names(program: &ir::Program) -> BTreeMap<ir::ModulePath, String> {
    let mut paths: Vec<ir::ModulePath> = Vec::new();
    for decl in &program.decls {
        if !paths.contains(&decl.module) {
            paths.push(decl.module.clone());
        }
    }
    let mut named: Vec<(ir::ModulePath, String)> = paths
        .into_iter()
        .map(|p| (p.clone(), module_name(&p)))
        .collect();
    named.sort_by(|a, b| a.1.cmp(&b.1));
    let mut used: Vec<String> = Vec::new();
    let mut map = BTreeMap::new();
    for (path, name) in named {
        map.insert(path, dedupe(&mut used, name));
    }
    map
}

fn module_name(module: &ir::ModulePath) -> String {
    if module.0.is_empty() {
        return "root".to_string();
    }
    module
        .0
        .iter()
        .map(|segment| {
            let mut out: String = segment
                .as_str()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            out.make_ascii_lowercase();
            if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                out.insert(0, 'm');
            }
            out
        })
        .collect::<Vec<_>>()
        .join("_")
}

fn dedupe(used: &mut Vec<String>, base: String) -> String {
    if !used.contains(&base) {
        used.push(base.clone());
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}_{n}");
        if !used.contains(&candidate) {
            used.push(candidate.clone());
            return candidate;
        }
        n += 1;
    }
}

/// Ambient/global type names an exported declaration must not shadow (an
/// `interface Date {}` would merge with the global `Date`).
const GLOBAL_TYPES: &[&str] = &[
    "Array", "Blob", "Boolean", "Date", "Error", "Function", "Map", "Number", "Object", "Promise",
    "Record", "Set", "String", "Symbol",
];
