//! **M3.5 Apex spike — throwaway by design** (PLAN.md, D-103).
//!
//! A deliberately rough Apex lowering whose *only* deliverable is the
//! list of IR changes the extreme target forces (assessment §8). The
//! generated source is never shipped, never compiled by a Salesforce
//! toolchain, and never becomes the real backend (that's M4). What makes
//! the spike load-bearing anyway:
//!
//! - every `match` over IR nodes is exhaustive, so **the compiler proves
//!   the IR is total for Apex**: any node kind this lowering can't
//!   express fails the build, not the output;
//! - it consumes only the capability manifest's axes (flat namespace +
//!   identifier limit, no user generics, buffered streaming) — never the
//!   language name (FR-4.2);
//! - its tests run against the full real spec set in CI, so IR changes
//!   that break Apex-expressibility surface immediately.
//!
//! Findings are recorded in `DECISIONS.md` (D-108) and the spike is
//! retired when the real Apex backend lands.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use gantry_ir as ir;
use gantry_manifest::{CapabilityManifest, ModuleSystem};
use gantry_sema::Analysis;

/// Apex reserved words that appear as Box wire names (e.g. `limit` query
/// params, `group` fields). The IR's wire-name/identifier split (FR-2)
/// means mangling the Apex name never touches serialization.
const RESERVED: &[&str] = &[
    "limit", "group", "date", "time", "currency", "list", "map", "set", "object", "decimal",
    "integer", "long", "double", "boolean", "end", "update", "delete", "insert", "trigger",
];

/// One lowered manager: the Apex source and every identifier it minted
/// (so tests can assert the manifest's identifier limit holds).
pub struct LoweredManager {
    pub source: String,
    pub identifiers: BTreeSet<String>,
    /// Operations that got a per-type page class (TR-Apex.2: no user
    /// generics, so no shared `Page<T>`).
    pub paged_operations: usize,
    /// Discriminated unions that got `JSON.deserializeUntyped` dispatch.
    pub dispatch_unions: usize,
}

/// Lower one manager and everything it transitively references.
pub fn lower_manager(
    analysis: &Analysis<'_>,
    manifest: &CapabilityManifest,
    manager: &str,
) -> LoweredManager {
    let ModuleSystem::Flat { identifier_limit } = manifest.modules else {
        panic!("this spike exists for the flat-namespace axis");
    };
    let mut lowering = Spike {
        analysis,
        identifier_limit: identifier_limit as usize,
        identifiers: BTreeSet::new(),
        source: String::new(),
        paged_operations: 0,
        dispatch_unions: 0,
    };
    lowering.manager(manager);
    LoweredManager {
        source: lowering.source,
        identifiers: lowering.identifiers,
        paged_operations: lowering.paged_operations,
        dispatch_unions: lowering.dispatch_unions,
    }
}

struct Spike<'a> {
    analysis: &'a Analysis<'a>,
    identifier_limit: usize,
    identifiers: BTreeSet<String>,
    source: String,
    paged_operations: usize,
    dispatch_unions: usize,
}

impl Spike<'_> {
    fn manager(&mut self, manager: &str) {
        let program = self.analysis.program;
        let Some(op_indices) = self.analysis.managers.get(manager) else {
            panic!("manager {manager:?} not in the analysis");
        };

        // Everything the manager's operations reach, in program order —
        // deterministic (FR-6.2 applies to spikes too).
        let reachable = self.reachable_decls(op_indices);

        let class_name = self.mint(&format!("Box{}Manager", pascal(manager)));
        let _ = writeln!(self.source, "public with sharing class {class_name} {{");

        // Inner classes for every reachable declaration: the
        // flat-namespace lowering (TR-Apex.1) — outer class as grouping.
        for id in &reachable {
            self.decl(program.decl(*id));
        }

        for index in op_indices {
            self.operation(&program.operations[*index]);
        }
        let _ = writeln!(self.source, "}}");
    }

    fn reachable_decls(&self, op_indices: &[usize]) -> Vec<ir::DeclId> {
        let program = self.analysis.program;
        let mut seen: BTreeSet<u32> = BTreeSet::new();
        let mut stack: Vec<ir::DeclId> = Vec::new();
        for index in op_indices {
            let op = &program.operations[*index];
            for param in &op.params {
                collect_type_refs(&param.ty, &mut stack);
            }
            if let Some(body) = &op.request {
                collect_type_refs(&body.ty, &mut stack);
            }
            match &op.response {
                ir::ResponseShape::Json(ty) => collect_type_refs(ty, &mut stack),
                ir::ResponseShape::None
                | ir::ResponseShape::Binary
                | ir::ResponseShape::Text
                | ir::ResponseShape::Redirect => {}
            }
        }
        while let Some(id) = stack.pop() {
            if !seen.insert(id.0) {
                continue;
            }
            match &program.decl(id).kind {
                ir::DeclKind::Struct(s) => {
                    for field in &s.fields {
                        collect_type_refs(&field.ty, &mut stack);
                    }
                }
                ir::DeclKind::Union(u) => {
                    for variant in &u.variants {
                        collect_type_refs(&variant.ty, &mut stack);
                    }
                }
                ir::DeclKind::Enum(_) => {}
                ir::DeclKind::Alias(ty) => collect_type_refs(ty, &mut stack),
            }
        }
        seen.into_iter().map(ir::DeclId).collect()
    }

    fn decl(&mut self, decl: &ir::Decl) {
        let name = self.apex_decl_name(decl);
        match &decl.kind {
            ir::DeclKind::Struct(s) => {
                let _ = writeln!(self.source, "  public class {name} {{");
                for field in &s.fields {
                    let field_name = self.apex_field_name(field);
                    let ty = self.apex_type(&field.ty);
                    // Wire name survives via serialization maps, not the
                    // identifier (FR-2's wire_name/identifier split).
                    let _ = writeln!(
                        self.source,
                        "    public {ty} {field_name}; // wire: {wire}",
                        wire = field.wire_name
                    );
                }
                let _ = writeln!(self.source, "  }}");
            }
            ir::DeclKind::Enum(e) => {
                // Open enum (D-105): a class holding the raw value, so
                // unknown values round-trip — a real Apex enum cannot.
                let _ = writeln!(self.source, "  public class {name} {{");
                let _ = writeln!(self.source, "    public String value;");
                for value in &e.values {
                    let constant = self.mint(&constant_name(value));
                    let _ = writeln!(
                        self.source,
                        "    public static final String {constant} = '{value}';"
                    );
                }
                let _ = writeln!(self.source, "  }}");
            }
            ir::DeclKind::Union(u) => {
                let _ = writeln!(self.source, "  public class {name} {{");
                if let Some(discriminator) = &u.discriminator {
                    // TR-Apex.4: generated deserializeUntyped dispatch.
                    self.dispatch_unions += 1;
                    let _ = writeln!(
                        self.source,
                        "    public static Object parse(Map<String, Object> untyped) {{"
                    );
                    let _ = writeln!(
                        self.source,
                        "      String tag = (String) untyped.get('{discriminator}');"
                    );
                    for variant in &u.variants {
                        if let Some(value) = &variant.discriminator_value {
                            let ty = self.apex_type(&variant.ty);
                            let _ = writeln!(
                                self.source,
                                "      if (tag == '{value}') return ({ty}) JSON.deserialize(JSON.serialize(untyped), {ty}.class);"
                            );
                        }
                    }
                    // Open union (D-105): unknown tags round-trip raw.
                    let _ = writeln!(self.source, "      return untyped;");
                    let _ = writeln!(self.source, "    }}");
                } else {
                    // A structural union erases to Object in Apex — the
                    // caller inspects the shape (manifest-accepted loss).
                    let _ = writeln!(self.source, "    public Object value;");
                }
                let _ = writeln!(self.source, "  }}");
            }
            ir::DeclKind::Alias(_) => {
                // Apex has no type aliases; references resolve through
                // apex_type instead, so nothing is emitted.
            }
        }
    }

    fn operation(&mut self, op: &ir::Operation) {
        let method_name = {
            let mut name = camel(op.name.as_str());
            if let Some(variation) = &op.variation {
                name.push_str(&pascal(variation.as_str()));
            }
            self.mint(&name)
        };
        let return_ty = match &op.response {
            ir::ResponseShape::None => "void".to_string(),
            ir::ResponseShape::Json(ty) => self.apex_type(ty),
            // Buffered platform (manifest Streaming::Buffered): bytes are
            // a Blob in heap, never a stream.
            ir::ResponseShape::Binary => "Blob".to_string(),
            ir::ResponseShape::Text | ir::ResponseShape::Redirect => "String".to_string(),
        };
        let mut args: Vec<String> = Vec::new();
        for param in &op.params {
            let arg_name = self.mint(&safe_word(&camel(param.wire_name.as_str())));
            args.push(format!("{} {arg_name}", self.apex_type(&param.ty)));
        }
        if let Some(body) = &op.request {
            let ty = match body.media {
                ir::RequestMedia::Json
                | ir::RequestMedia::JsonPatch
                | ir::RequestMedia::UrlEncoded
                | ir::RequestMedia::Multipart => self.apex_type(&body.ty),
                ir::RequestMedia::OctetStream => "Blob".to_string(),
            };
            args.push(format!("{ty} requestBody"));
        }
        let _ = writeln!(
            self.source,
            "  public {return_ty} {method_name}({}) {{ /* callout via runtime contract */ }}",
            args.join(", ")
        );

        // TR-Apex.2 + TR-Apex.3: paged surfaces are per-type,
        // transaction-bounded — no shared Page<T> without user generics.
        let is_paged = op.params.iter().any(|p| {
            p.location == ir::ParamLocation::Query
                && (p.wire_name == "marker" || p.wire_name == "offset")
        });
        if is_paged {
            self.paged_operations += 1;
            let page_class = self.mint(&format!("{}Page", pascal(op.name.as_str())));
            let _ = writeln!(
                self.source,
                "  public class {page_class} {{ public {return_ty} items; public String nextMarker; }}"
            );
        }
    }

    /// The flat-namespace decl name (TR-Apex.1): version-prefixed for
    /// versioned modules, deterministically abbreviated to the manifest's
    /// identifier limit.
    fn apex_decl_name(&mut self, decl: &ir::Decl) -> String {
        let mut name = String::new();
        // The module path collapses into the name — the IR keeps the rich
        // module concept; Apex flattens it here (assessment §8).
        for segment in decl.module.0.iter().skip(1) {
            name.push_str(&pascal(segment.as_str()));
        }
        name.push_str(decl.name.as_str());
        self.mint(&name)
    }

    fn apex_field_name(&mut self, field: &ir::Field) -> String {
        safe_word(field.name.as_str())
    }

    fn apex_type(&mut self, ty: &ir::Type) -> String {
        match ty {
            // Every Apex reference is nullable: both tri-state wrappers
            // (D-110) erase at the type level; absent-vs-null lives in
            // the serializer, not the type.
            ir::Type::Optional(inner) | ir::Type::Nullable(inner) => self.apex_type(inner),
            ir::Type::List(inner) => format!("List<{}>", self.apex_type(inner)),
            ir::Type::Map(inner) => format!("Map<String, {}>", self.apex_type(inner)),
            ir::Type::Bool => "Boolean".to_string(),
            ir::Type::Int64 => "Long".to_string(),
            ir::Type::Float64 => "Double".to_string(),
            ir::Type::String => "String".to_string(),
            ir::Type::Date => "Date".to_string(),
            ir::Type::DateTime => "Datetime".to_string(),
            ir::Type::Binary => "Blob".to_string(),
            ir::Type::JsonValue => "Object".to_string(),
            ir::Type::Decl(id) => {
                let decl = self.analysis.program.decl(*id);
                if let ir::DeclKind::Alias(inner) = &decl.kind {
                    // No Apex aliases: resolve through.
                    let inner = inner.clone();
                    self.apex_type(&inner)
                } else {
                    self.apex_decl_name(decl)
                }
            }
        }
    }

    /// Register an identifier, abbreviating deterministically when it
    /// exceeds the manifest's limit: prefix + `_` + 7-hex FNV of the full
    /// name. Same input → same output on every run.
    fn mint(&mut self, name: &str) -> String {
        let minted = if name.len() <= self.identifier_limit {
            name.to_string()
        } else {
            let hash = fnv64(name.as_bytes());
            let keep = self.identifier_limit - 8;
            format!("{}_{:07x}", &name[..keep], hash & 0xFFF_FFFF)
        };
        self.identifiers.insert(minted.clone());
        minted
    }
}

fn collect_type_refs(ty: &ir::Type, stack: &mut Vec<ir::DeclId>) {
    match ty {
        ir::Type::Optional(inner)
        | ir::Type::Nullable(inner)
        | ir::Type::List(inner)
        | ir::Type::Map(inner) => {
            collect_type_refs(inner, stack);
        }
        ir::Type::Decl(id) => stack.push(*id),
        ir::Type::Bool
        | ir::Type::Int64
        | ir::Type::Float64
        | ir::Type::String
        | ir::Type::Date
        | ir::Type::DateTime
        | ir::Type::Binary
        | ir::Type::JsonValue => {}
    }
}

fn pascal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for part in text.split(['_', '-']) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

fn camel(text: &str) -> String {
    let p = pascal(text);
    let mut chars = p.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => p,
    }
}

/// Apex reserved words get a trailing `X`; the wire name is untouched.
fn safe_word(name: &str) -> String {
    if RESERVED.contains(&name.to_ascii_lowercase().as_str()) {
        format!("{name}X")
    } else {
        name.to_string()
    }
}

fn constant_name(value: &str) -> String {
    let upper: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if upper.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("V_{upper}")
    } else {
        upper
    }
}

/// FNV-1a, hand-rolled: deterministic, dependency-free.
fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}
