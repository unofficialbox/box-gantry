//! Field ↔ wire-name serialization remap + explicit-null (absent-vs-null).
//!
//! Two Apex-JSON gaps live here, both fixed by transforming the *untyped* JSON
//! tree around native (de)serialization:
//!
//! **1. Key remap (FR-2.2).** `JSON.deserialize(body, T.class)` matches JSON
//! keys to instance-variable names (case-insensitively) with **no** wire-name
//! remapping — no `@JsonProperty` equivalent. So whenever a Box wire key is
//! altered to form a legal Apex identifier — a reserved word (`limit` →
//! `limit_r`), a `$`-prefixed metadata key, a `__` run, or a digit-leading key
//! — the Apex field silently fails to populate on read and emits the wrong key
//! on write. The fix renames keys on the untyped tree, type-directed, then hands
//! it to native (de)serialization (which converts every value type correctly
//! once the keys match).
//!
//! **2. Explicit null (D-110/D-138).** Box uses an explicit JSON `null` to
//! *clear* a field on update; an *absent* key leaves it unchanged. On a plain
//! Apex object an unset field and a field set to `null` are indistinguishable,
//! and `JSON.serialize(o, true)` suppresses both. So a body struct that (a) is
//! reachable from a request body and (b) has a `Nullable` field gains a
//! `Set<String> fieldsToNull` control field: the caller lists the Apex field
//! names to send as `null`, and the write transform injects `"<wire>": null`
//! for each (then drops the control key). Unset fields still serialize as absent.
//!
//! **Read vs write.** A struct is **read-affected** iff a field's Apex name
//! differs from its wire name, or it transitively contains a read-affected
//! struct; it gets `normalizeKeys` (wire → Apex), used on responses and union
//! parse. It is **write-affected** iff read-affected, *or* it is null-writable,
//! *or* it transitively contains a write-affected struct; it gets
//! `denormalizeKeys` (Apex → wire, plus null injection), used on request bodies.
//! The recursion descends only into the direction-appropriate set, so free-form
//! `Object`/`Map` values (which carry keys like `$id` as data) are never
//! disturbed.

use std::collections::HashSet;
use std::fmt::Write as _;

use gantry_ir as ir;

use crate::models::ClassNames;
use crate::safe_word;

/// Direction of a key remap: response bodies come off the wire (rename wire →
/// Apex before typed deserialize); request bodies go to the wire (rename Apex →
/// wire after serialize, and inject explicit nulls).
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
/// / null injection, and the helpers to emit it. Built once per program.
pub(crate) struct Wire<'a> {
    program: &'a ir::Program,
    names: &'a ClassNames,
    /// Renames on the read path: name-mismatch or transitively contains one.
    read_affected: HashSet<u32>,
    /// The write path: read_affected ∪ null-writable ∪ transitive containers.
    write_affected: HashSet<u32>,
    /// Body-reachable structs with a `Nullable` field — they carry `fieldsToNull`
    /// and inject explicit nulls on the write path.
    null_writable: HashSet<u32>,
}

impl<'a> Wire<'a> {
    /// Compute the read/write affected sets and the null-writable set.
    pub(crate) fn build(program: &'a ir::Program, names: &'a ClassNames) -> Self {
        // Structs reachable from any JSON-ish request body — the only place an
        // explicit null can be sent.
        let mut body_reachable = HashSet::new();
        for op in &program.operations {
            if let Some(body) = &op.request
                && matches!(
                    body.media,
                    ir::RequestMedia::Json
                        | ir::RequestMedia::JsonPatch
                        | ir::RequestMedia::UrlEncoded
                        | ir::RequestMedia::Multipart
                )
            {
                reach_structs(program, &body.ty, &mut body_reachable);
            }
        }
        // Null-writable: body-reachable structs with a wire-nullable field.
        let mut null_writable = HashSet::new();
        for (index, decl) in program.decls.iter().enumerate() {
            let id = index as u32;
            if body_reachable.contains(&id)
                && let ir::DeclKind::Struct(s) = &decl.kind
                && s.fields.iter().any(|f| is_nullable(&f.ty))
            {
                null_writable.insert(id);
            }
        }

        let mut wire = Wire {
            program,
            names,
            read_affected: HashSet::new(),
            write_affected: HashSet::new(),
            null_writable,
        };
        // Read set (fixpoint): a struct is read-affected if a field is
        // name-mismatched or its type reaches a read-affected struct.
        wire.read_affected = wire.close(HashSet::new());
        // Write set (fixpoint): seed with the read set (every rename must also
        // remap on write) plus the null-writable structs (they inject explicit
        // nulls), then close over "contains a write-affected struct".
        let mut seed = wire.read_affected.clone();
        seed.extend(wire.null_writable.iter().copied());
        wire.write_affected = wire.close(seed);
        wire
    }

    /// Close a struct-id set under "name-mismatch or reaches a member of the set"
    /// to a fixpoint. `seed` pre-marks structs that are affected for another
    /// reason (e.g. null-writable).
    fn close(&self, seed: HashSet<u32>) -> HashSet<u32> {
        let mut set = seed;
        loop {
            let mut changed = false;
            for (index, decl) in self.program.decls.iter().enumerate() {
                let id = index as u32;
                if set.contains(&id) {
                    continue;
                }
                if let ir::DeclKind::Struct(s) = &decl.kind
                    && s.fields
                        .iter()
                        .any(|f| field_mismatched(f) || self.type_reaches(&f.ty, &set))
                {
                    set.insert(id);
                    changed = true;
                }
            }
            if !changed {
                break set;
            }
        }
    }

    /// Does this struct need any generated hook (either direction)?
    pub(crate) fn needs_hooks(&self, id: ir::DeclId) -> bool {
        self.read_affected.contains(&id.0) || self.write_affected.contains(&id.0)
    }

    /// Does this struct need `normalizeKeys` (the read path)? Used by union parse.
    pub(crate) fn needs_read_hook(&self, id: ir::DeclId) -> bool {
        self.read_affected.contains(&id.0)
    }

    /// Is this a body-reachable struct with a nullable field (carries
    /// `fieldsToNull` and injects explicit nulls)?
    pub(crate) fn is_null_writable(&self, id: ir::DeclId) -> bool {
        self.null_writable.contains(&id.0)
    }

    /// Does a response value of this type reach a read-affected struct?
    pub(crate) fn type_reaches_read_affected(&self, ty: &ir::Type) -> bool {
        self.type_reaches(ty, &self.read_affected)
    }

    /// Does a request value of this type reach a write-affected struct (a rename
    /// or a null-writable struct), so the body must route through the transform?
    pub(crate) fn type_reaches_write_affected(&self, ty: &ir::Type) -> bool {
        self.type_reaches(ty, &self.write_affected)
    }

    /// Does a value of this type reach a struct in `set`? Peels the tri-state and
    /// container wrappers; an alias resolves through; enums (String) and unions
    /// (Object) never recurse.
    fn type_reaches(&self, ty: &ir::Type, set: &HashSet<u32>) -> bool {
        match ty {
            ir::Type::Optional(inner)
            | ir::Type::Nullable(inner)
            | ir::Type::List(inner)
            | ir::Type::Map(inner) => self.type_reaches(inner, set),
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(inner) => self.type_reaches(inner, set),
                ir::DeclKind::Struct(_) => set.contains(&id.0),
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

    /// The generated hook statics for a struct: `normalizeKeys` if read-affected,
    /// `denormalizeKeys` (with explicit-null injection if null-writable) if
    /// write-affected. Both operate on and return the untyped `Map<String,
    /// Object>` so native (de)serialization does the type work.
    pub(crate) fn hooks(&self, id: ir::DeclId, class: &str, s: &ir::StructDecl) -> String {
        let mut out = String::new();
        if self.read_affected.contains(&id.0) {
            self.emit_method(&mut out, class, s, Dir::Normalize, false);
        }
        if self.write_affected.contains(&id.0) {
            self.emit_method(
                &mut out,
                class,
                s,
                Dir::Denormalize,
                self.null_writable.contains(&id.0),
            );
        }
        out
    }

    /// Emit one direction's remap static. `inject_nulls` appends the explicit-null
    /// pass (denormalize / null-writable only).
    fn emit_method(
        &self,
        out: &mut String,
        class: &str,
        s: &ir::StructDecl,
        dir: Dir,
        inject_nulls: bool,
    ) {
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
            self.emit_field(out, field, dir);
        }
        if inject_nulls {
            self.emit_null_injection(out, s);
        }
        let _ = writeln!(out, "        return raw;");
        let _ = writeln!(out, "    }}");
    }

    /// Emit the explicit-null pass: for each Apex field name the caller listed in
    /// `fieldsToNull`, write `"<wire>": null` (Box clears it); then drop the
    /// control key so it never reaches the wire.
    fn emit_null_injection(&self, out: &mut String, s: &ir::StructDecl) {
        let _ = writeln!(
            out,
            "        // Explicit null (D-138): send `null` for each field the caller listed."
        );
        let _ = writeln!(out, "        Object toNull = raw.remove('fieldsToNull');");
        let _ = writeln!(out, "        if (toNull instanceof List<Object>) {{");
        let _ = writeln!(
            out,
            "            for (Object nf : (List<Object>) toNull) {{"
        );
        let _ = writeln!(out, "                String nfName = String.valueOf(nf);");
        for field in &s.fields {
            if !is_nullable(&field.ty) {
                continue;
            }
            let apex = safe_word(field.name.as_str());
            let _ = writeln!(
                out,
                "                if (nfName == '{}') raw.put('{}', null);",
                escape(&apex),
                escape(&field.wire_name)
            );
        }
        let _ = writeln!(out, "            }}");
        let _ = writeln!(out, "        }}");
    }

    /// Emit the remap for one field, skipping fields that need neither renaming
    /// nor recursion (the common case — native deserialize already handles it).
    fn emit_field(&self, out: &mut String, field: &ir::Field, dir: Dir) {
        let apex = safe_word(field.name.as_str());
        let wire = &field.wire_name;
        let mismatched = field_mismatched(field);
        let recurse = self.type_reaches(&field.ty, self.set_for(dir));
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

    /// The affected set that governs recursion in this direction.
    fn set_for(&self, dir: Dir) -> &HashSet<u32> {
        match dir {
            Dir::Normalize => &self.read_affected,
            Dir::Denormalize => &self.write_affected,
        }
    }

    /// Emit statements that transform the untyped value in local `var` in place,
    /// recursing through lists/maps/structs to the affected leaves. `depth`
    /// keeps nested-loop locals unique.
    fn emit_transform(&self, out: &mut String, var: &str, ty: &ir::Type, dir: Dir, depth: usize) {
        let set = self.set_for(dir);
        match ty {
            ir::Type::Optional(inner) | ir::Type::Nullable(inner) => {
                self.emit_transform(out, var, inner, dir, depth);
            }
            ir::Type::List(inner) if !self.type_reaches(inner, set) => {}
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
            ir::Type::Map(inner) if !self.type_reaches(inner, set) => {}
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
                ir::DeclKind::Struct(_) if set.contains(&id.0) => {
                    let class = self.names.get(*id).expect("affected struct has a name");
                    let _ = writeln!(
                        out,
                        "            if ({var} instanceof Map<String, Object>) {var} = {class}.{}((Map<String, Object>) {var});",
                        dir.method()
                    );
                }
                // A struct outside this direction's set, an enum (String), or a
                // union (Object) has nothing to remap.
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

    /// Emit the response deserialization for a type that reaches a read-affected
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

    /// Emit the request-body assignment for a body type that reaches a
    /// write-affected struct: reduce to non-null keys (unset fields stay absent),
    /// remap keys to the wire shape and inject explicit nulls, then hand the
    /// untyped map to the runtime with null-suppression off (the map now carries
    /// only intended keys — including any intentional nulls).
    pub(crate) fn emit_request_body(&self, out: &mut String, ty: &ir::Type) {
        let _ = writeln!(
            out,
            "        Object wireBody = JSON.deserializeUntyped(JSON.serialize(body, true));"
        );
        self.emit_transform(out, "wireBody", ty, Dir::Denormalize, 0);
        let _ = writeln!(out, "        request.body = wireBody;");
        let _ = writeln!(out, "        request.suppressNulls = false;");
    }
}

/// Collect every struct decl id reachable from `ty` (through containers,
/// tri-state, aliases, and struct fields).
fn reach_structs(program: &ir::Program, ty: &ir::Type, seen: &mut HashSet<u32>) {
    match ty {
        ir::Type::Optional(inner)
        | ir::Type::Nullable(inner)
        | ir::Type::List(inner)
        | ir::Type::Map(inner) => reach_structs(program, inner, seen),
        ir::Type::Decl(id) => match &program.decl(*id).kind {
            ir::DeclKind::Alias(inner) => reach_structs(program, inner, seen),
            ir::DeclKind::Struct(s) => {
                if seen.insert(id.0) {
                    for f in &s.fields {
                        reach_structs(program, &f.ty, seen);
                    }
                }
            }
            ir::DeclKind::Enum(_) | ir::DeclKind::Union(_) => {}
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

/// Does this type carry an explicit-null wire value — a `Nullable`, possibly
/// under an `Optional` (absent-or-null)?
fn is_nullable(ty: &ir::Type) -> bool {
    match ty {
        ir::Type::Nullable(_) => true,
        ir::Type::Optional(inner) => is_nullable(inner),
        ir::Type::List(_)
        | ir::Type::Map(_)
        | ir::Type::Decl(_)
        | ir::Type::Bool
        | ir::Type::Int64
        | ir::Type::Float64
        | ir::Type::String
        | ir::Type::Date
        | ir::Type::DateTime
        | ir::Type::Binary
        | ir::Type::JsonValue => false,
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
