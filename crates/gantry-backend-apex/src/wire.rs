//! Field ↔ wire-name serialization remap (FR-2.2, TR-Apex correctness gap).
//!
//! Apex's `JSON.deserialize(body, T.class)` matches JSON keys to instance
//! variable names (case-insensitively) with **no** wire-name remapping — no
//! `@JsonProperty` equivalent exists. So whenever a Box wire key is altered to
//! form a legal Apex identifier — a reserved word (`limit` → `limit_r`), a
//! `$`-prefixed metadata key (`$parent` → `parent`), a `__` run
//! (`Box__Security__…` → `Box_Security_…`), or a digit-leading key — the Apex
//! field silently fails to populate on read and emits the wrong key on write.
//!
//! The only thing broken is the **key names**; native `JSON.deserialize`
//! converts every value type (dates, blobs, nested structs, lists) correctly
//! once the keys match. So the fix is minimal: rename keys on the *untyped*
//! JSON tree, type-directed, then hand it to native (de)serialization.
//!
//! A struct is **affected** iff it has a field whose Apex name differs from its
//! wire name, or it transitively contains an affected struct. Only affected
//! structs get a generated `normalizeKeys` (wire → Apex, the read path) and
//! `denormalizeKeys` (Apex → wire, the write path); the recursion descends
//! *only* into affected substructures, so free-form `Object`/`Map` values
//! (which carry keys like `$id` as data) are never disturbed. In the real Box
//! spec 119 of 991 classes are affected; the other 872 keep the native path.

use std::collections::HashSet;
use std::fmt::Write as _;

use gantry_ir as ir;

use crate::models::ClassNames;
use crate::safe_word;

/// Direction of a key remap: response bodies come off the wire (rename wire →
/// Apex before typed deserialize); request bodies go to the wire (rename Apex →
/// wire after serialize).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dir {
    Normalize,
    Denormalize,
}

impl Dir {
    fn method(self) -> &'static str {
        match self {
            Dir::Normalize => "normalizeKeys",
            Dir::Denormalize => "denormalizeKeys",
        }
    }
}

/// The wire-remap model: which struct declarations need generated key remapping
/// and the helpers to emit it. Built once per program (deterministic).
pub(crate) struct Wire<'a> {
    program: &'a ir::Program,
    names: &'a ClassNames,
    affected: HashSet<u32>,
}

impl<'a> Wire<'a> {
    /// Compute the affected-struct set by fixpoint: a struct is affected if any
    /// field is name-mismatched or has a type reaching an affected struct.
    pub(crate) fn build(program: &'a ir::Program, names: &'a ClassNames) -> Self {
        let mut wire = Wire {
            program,
            names,
            affected: HashSet::new(),
        };
        loop {
            let mut changed = false;
            for (index, decl) in program.decls.iter().enumerate() {
                let id = index as u32;
                if wire.affected.contains(&id) {
                    continue;
                }
                if let ir::DeclKind::Struct(s) = &decl.kind
                    && s.fields
                        .iter()
                        .any(|f| field_mismatched(f) || wire.type_reaches_affected(&f.ty))
                {
                    wire.affected.insert(id);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        wire
    }

    /// Does this declaration (a struct) need generated remap methods?
    pub(crate) fn is_affected(&self, id: ir::DeclId) -> bool {
        self.affected.contains(&id.0)
    }

    /// Does a value of this type reach an affected struct (so a key remap must
    /// recurse into it)? Peels the tri-state and container wrappers; an alias
    /// resolves through; enums (String) and unions (Object) never recurse.
    pub(crate) fn type_reaches_affected(&self, ty: &ir::Type) -> bool {
        match ty {
            ir::Type::Optional(inner)
            | ir::Type::Nullable(inner)
            | ir::Type::List(inner)
            | ir::Type::Map(inner) => self.type_reaches_affected(inner),
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(inner) => self.type_reaches_affected(inner),
                ir::DeclKind::Struct(_) => self.affected.contains(&id.0),
                // An open enum lowers to `String` and a union to `Object`;
                // neither carries typed keys to remap.
                ir::DeclKind::Enum(_) | ir::DeclKind::Union(_) => false,
            },
            ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::Date
            | ir::Type::DateTime
            | ir::Type::Binary
            | ir::Type::JsonValue => false,
        }
    }

    /// The `normalizeKeys` + `denormalizeKeys` statics for an affected struct.
    /// `normalizeKeys` renames wire → Apex and recurses into affected children;
    /// `denormalizeKeys` is the mirror. Both operate on and return the untyped
    /// `Map<String, Object>` so native (de)serialization does the type work.
    pub(crate) fn remap_methods(&self, class: &str, s: &ir::StructDecl) -> String {
        let mut out = String::new();
        for dir in [Dir::Normalize, Dir::Denormalize] {
            let _ = writeln!(
                out,
                "    /** {} JSON keys for the `{class}` wire shape (see class doc). */",
                match dir {
                    Dir::Normalize => "Wire → Apex:",
                    Dir::Denormalize => "Apex → wire:",
                }
            );
            let _ = writeln!(
                out,
                "    public static Map<String, Object> {}(Map<String, Object> raw) {{",
                dir.method()
            );
            let _ = writeln!(out, "        if (raw == null) return null;");
            for field in &s.fields {
                self.emit_field(&mut out, field, dir);
            }
            let _ = writeln!(out, "        return raw;");
            let _ = writeln!(out, "    }}");
        }
        out
    }

    /// Emit the remap for one field, skipping fields that need neither renaming
    /// nor recursion (the common case — native deserialize already handles it).
    fn emit_field(&self, out: &mut String, field: &ir::Field, dir: Dir) {
        let apex = safe_word(field.name.as_str());
        let wire = &field.wire_name;
        let mismatched = field_mismatched(field);
        let recurse = self.type_reaches_affected(&field.ty);
        if !mismatched && !recurse {
            return;
        }
        // Read under the source key, transform if the type reaches an affected
        // struct, then write under the destination key. When the names match
        // (recurse-only), source == destination so the key is preserved.
        let (from, to) = match dir {
            Dir::Normalize => (wire.as_str(), if mismatched { apex.as_str() } else { wire }),
            Dir::Denormalize => (apex.as_str(), if mismatched { wire } else { apex.as_str() }),
        };
        let _ = writeln!(out, "        if (raw.containsKey('{}')) {{", escape(from));
        let _ = writeln!(
            out,
            "            Object v = raw.remove('{}');",
            escape(from)
        );
        if recurse {
            self.emit_transform(out, "v", &field.ty, dir, 0);
        }
        let _ = writeln!(out, "            raw.put('{}', v);", escape(to));
        let _ = writeln!(out, "        }}");
    }

    /// Emit statements that transform the untyped value in local `var` in place,
    /// recursing through lists/maps/structs to the affected leaves. `depth`
    /// keeps nested-loop locals unique.
    fn emit_transform(&self, out: &mut String, var: &str, ty: &ir::Type, dir: Dir, depth: usize) {
        match ty {
            ir::Type::Optional(inner) | ir::Type::Nullable(inner) => {
                self.emit_transform(out, var, inner, dir, depth);
            }
            ir::Type::List(inner) if !self.type_reaches_affected(inner) => {}
            ir::Type::List(inner) => {
                let (list, idx, elem) = (
                    format!("wLst{depth}"),
                    format!("wIdx{depth}"),
                    format!("wElem{depth}"),
                );
                let _ = writeln!(out, "            if ({var} instanceof List<Object>) {{");
                let _ = writeln!(
                    out,
                    "                List<Object> {list} = (List<Object>) {var};"
                );
                let _ = writeln!(
                    out,
                    "                for (Integer {idx} = 0; {idx} < {list}.size(); {idx}++) {{"
                );
                let _ = writeln!(out, "                    Object {elem} = {list}[{idx}];");
                self.emit_transform(out, &elem, inner, dir, depth + 1);
                let _ = writeln!(out, "                    {list}[{idx}] = {elem};");
                let _ = writeln!(out, "                }}");
                let _ = writeln!(out, "            }}");
            }
            ir::Type::Map(inner) if !self.type_reaches_affected(inner) => {}
            ir::Type::Map(inner) => {
                let (map, key, elem) = (
                    format!("wMap{depth}"),
                    format!("wKey{depth}"),
                    format!("wElem{depth}"),
                );
                let _ = writeln!(
                    out,
                    "            if ({var} instanceof Map<String, Object>) {{"
                );
                let _ = writeln!(
                    out,
                    "                Map<String, Object> {map} = (Map<String, Object>) {var};"
                );
                let _ = writeln!(
                    out,
                    "                for (String {key} : {map}.keySet()) {{"
                );
                let _ = writeln!(out, "                    Object {elem} = {map}.get({key});");
                self.emit_transform(out, &elem, inner, dir, depth + 1);
                let _ = writeln!(out, "                    {map}.put({key}, {elem});");
                let _ = writeln!(out, "                }}");
                let _ = writeln!(out, "            }}");
            }
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(inner) => self.emit_transform(out, var, inner, dir, depth),
                ir::DeclKind::Struct(_) if self.affected.contains(&id.0) => {
                    let class = self.names.get(*id).expect("affected struct has a name");
                    let _ = writeln!(
                        out,
                        "            if ({var} instanceof Map<String, Object>) {var} = {class}.{}((Map<String, Object>) {var});",
                        dir.method()
                    );
                }
                // A clean struct, an enum (String), or a union (Object) has
                // nothing to remap.
                ir::DeclKind::Struct(_) | ir::DeclKind::Enum(_) | ir::DeclKind::Union(_) => {}
            },
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

    /// Emit the response deserialization for a type that reaches an affected
    /// struct: parse untyped, remap keys, then native-deserialize into the typed
    /// shape. `apex_ty` is the Apex return type (e.g. `Items`, `List<Comment>`).
    /// Writes the full `return …;` (may be several statements).
    pub(crate) fn emit_response(&self, out: &mut String, apex_ty: &str, ty: &ir::Type) {
        let _ = writeln!(
            out,
            "        Object parsed = JSON.deserializeUntyped(response.body);"
        );
        self.emit_transform(out, "parsed", ty, Dir::Normalize, 0);
        let _ = writeln!(
            out,
            "        return ({apex_ty}) JSON.deserialize(JSON.serialize(parsed), {apex_ty}.class);"
        );
    }

    /// Emit the request-body assignment for a body type that reaches an affected
    /// struct: serialize the object, remap keys to the wire shape, and hand the
    /// untyped map to the runtime (which serializes it).
    pub(crate) fn emit_request_body(&self, out: &mut String, ty: &ir::Type) {
        let _ = writeln!(
            out,
            "        Object wireBody = JSON.deserializeUntyped(JSON.serialize(body));"
        );
        self.emit_transform(out, "wireBody", ty, Dir::Denormalize, 0);
        let _ = writeln!(out, "        request.body = wireBody;");
    }
}

/// An Apex field name differs from its wire key beyond case (Apex JSON matching
/// is case-insensitive, so a pure case difference round-trips natively).
fn field_mismatched(field: &ir::Field) -> bool {
    !safe_word(field.name.as_str()).eq_ignore_ascii_case(&field.wire_name)
}

/// Escape a string literal for Apex single-quoted strings.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}
