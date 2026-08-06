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

use crate::models::{ClassNames, apex_type};
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
    /// Body-reachable structs with an open extra bag (D-196) — they carry a
    /// flatten pass in `denormalizeKeys` (see `build`).
    extra_writable: HashSet<u32>,
    /// Structs that transitively contain an `Object` leaf — a union or
    /// `JsonValue` field — **or** carry an open extra bag (D-196), which is
    /// equally unpopulatable by native typed deserialize (it can't route
    /// "every key I don't otherwise recognize" into a map field). Both get a
    /// generated `deserialize(Object)` builder used on the read path.
    object_bearing: HashSet<u32>,
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
        // Extra-writable: body-reachable structs with an open extra bag
        // (D-196). Native `JSON.serialize` nests it under its own field name
        // instead of merging its keys into the parent object, so
        // `denormalizeKeys` needs a flatten pass for it — same shape as the
        // null-injection pass above, gated the same way.
        let mut extra_writable = HashSet::new();
        for (index, decl) in program.decls.iter().enumerate() {
            let id = index as u32;
            if body_reachable.contains(&id)
                && let ir::DeclKind::Struct(s) = &decl.kind
                && s.extra.is_some()
            {
                extra_writable.insert(id);
            }
        }

        let mut wire = Wire {
            program,
            names,
            extra_writable,
            read_affected: HashSet::new(),
            write_affected: HashSet::new(),
            null_writable,
            object_bearing: HashSet::new(),
        };
        // Read set (fixpoint): a struct is read-affected if a field is
        // name-mismatched or its type reaches a read-affected struct.
        wire.read_affected = wire.close(HashSet::new());
        // Write set (fixpoint): seed with the read set (every rename must also
        // remap on write) plus the null-writable and extra-writable structs
        // (they inject explicit nulls / flatten the extra bag), then close
        // over "contains a write-affected struct".
        let mut seed = wire.read_affected.clone();
        seed.extend(wire.null_writable.iter().copied());
        seed.extend(wire.extra_writable.iter().copied());
        wire.write_affected = wire.close(seed);
        // Object-bearing set (fixpoint): a struct is object-bearing if a field
        // is an `Object` leaf (union / JsonValue) or reaches an object-bearing
        // struct — those can't be populated by native typed deserialize.
        wire.object_bearing = wire.close_object_bearing();
        wire
    }

    /// Close the object-bearing set: a struct joins if a field is an `Object`
    /// leaf or its type reaches a struct already in the set.
    fn close_object_bearing(&self) -> HashSet<u32> {
        let mut set = HashSet::new();
        loop {
            let mut changed = false;
            for (index, decl) in self.program.decls.iter().enumerate() {
                let id = index as u32;
                if set.contains(&id) {
                    continue;
                }
                if let ir::DeclKind::Struct(s) = &decl.kind
                    && (s.extra.is_some()
                        || s.fields
                            .iter()
                            .any(|f| self.type_reaches_object(&f.ty, &set)))
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

    /// Does this struct need `normalizeKeys` (the read path)? Used by union parse.
    pub(crate) fn needs_read_hook(&self, id: ir::DeclId) -> bool {
        self.read_affected.contains(&id.0)
    }

    /// Does this struct need `denormalizeKeys` (the write path)?
    pub(crate) fn needs_write_hook(&self, id: ir::DeclId) -> bool {
        self.write_affected.contains(&id.0)
    }

    /// The explicit-null control field's Apex name for a null-writable struct.
    /// Normally `fieldsToNull`, but disambiguated (case-insensitively) if a real
    /// schema field already sanitizes to that name — the control key must never
    /// collide with a wire field (whose value `denormalizeKeys` would otherwise
    /// consume). Deterministic: same field set → same name.
    pub(crate) fn null_control_name(&self, s: &ir::StructDecl) -> String {
        let taken: HashSet<String> = s
            .fields
            .iter()
            .map(|f| safe_word(f.name.as_str()).to_ascii_lowercase())
            .collect();
        let mut name = "fieldsToNull".to_string();
        let mut n = 2u32;
        while taken.contains(&name.to_ascii_lowercase()) {
            name = format!("fieldsToNull{n}");
            n += 1;
        }
        name
    }

    /// Is this a body-reachable struct with a nullable field (carries
    /// `fieldsToNull` and injects explicit nulls)?
    pub(crate) fn is_null_writable(&self, id: ir::DeclId) -> bool {
        self.null_writable.contains(&id.0)
    }

    /// The open extra bag's Apex field name (D-196). Normally `extra`, but
    /// disambiguated (case-insensitively) if a real schema field already
    /// sanitizes to that name — same collision rule as `null_control_name`,
    /// and for the same reason: it must never consume a named field's value.
    /// Deterministic: same field set → same name.
    pub(crate) fn extra_field_name(&self, s: &ir::StructDecl) -> String {
        let taken: HashSet<String> = s
            .fields
            .iter()
            .map(|f| safe_word(f.name.as_str()).to_ascii_lowercase())
            .collect();
        let mut name = "extra".to_string();
        let mut n = 2u32;
        while taken.contains(&name.to_ascii_lowercase()) {
            name = format!("extra{n}");
            n += 1;
        }
        name
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

    /// Does this struct need a generated `deserialize` builder (it reaches an
    /// `Object` leaf)?
    pub(crate) fn is_object_bearing(&self, id: ir::DeclId) -> bool {
        self.object_bearing.contains(&id.0)
    }

    /// Does a response value of this type reach an `Object` leaf, so it must be
    /// built via the `deserialize` path rather than native `JSON.deserialize`?
    pub(crate) fn type_reaches_object_bearing(&self, ty: &ir::Type) -> bool {
        self.type_reaches_object(ty, &self.object_bearing)
    }

    /// Does a value of this type reach an `Object` leaf (a union or `JsonValue`)
    /// or a struct in `set` (the object-bearing structs, mid-fixpoint)? Peels the
    /// tri-state and container wrappers; an alias resolves through.
    fn type_reaches_object(&self, ty: &ir::Type, set: &HashSet<u32>) -> bool {
        match ty {
            ir::Type::JsonValue => true,
            ir::Type::Optional(inner)
            | ir::Type::Nullable(inner)
            | ir::Type::List(inner)
            | ir::Type::Map(inner) => self.type_reaches_object(inner, set),
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(inner) => self.type_reaches_object(inner, set),
                // A union field erases to `Object` — the leaf that breaks native
                // deserialize.
                ir::DeclKind::Union(_) => true,
                ir::DeclKind::Struct(_) => set.contains(&id.0),
                ir::DeclKind::Enum(_) => false,
            },
            ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::Date
            | ir::Type::DateTime
            | ir::Type::Binary => false,
        }
    }

    /// The generated hook statics for a struct: `normalizeKeys` if read-affected,
    /// `denormalizeKeys` (with explicit-null injection if null-writable, and an
    /// extra-bag flatten pass if extra-writable) if write-affected. Both operate
    /// on and return the untyped `Map<String, Object>` so native
    /// (de)serialization does the type work.
    pub(crate) fn hooks(&self, id: ir::DeclId, class: &str, s: &ir::StructDecl) -> String {
        let mut out = String::new();
        if self.read_affected.contains(&id.0) {
            self.emit_method(&mut out, class, s, Dir::Normalize, false, false);
        }
        if self.write_affected.contains(&id.0) {
            self.emit_method(
                &mut out,
                class,
                s,
                Dir::Denormalize,
                self.null_writable.contains(&id.0),
                self.extra_writable.contains(&id.0),
            );
        }
        out
    }

    /// Apex statements that drive every branch of a struct's generated wire
    /// statics with **populated** inputs, for the coverage suite (tests.rs).
    ///
    /// The manager suite calls each hook indirectly with an empty `{}`/`[]`
    /// body, so every per-field `containsKey` branch, the null-injection loop,
    /// and the `deserialize` reattach arms stay unexecuted — the bulk of the
    /// uncovered lines against the 75% gate. Here each static is called directly
    /// with a map shaped to enter those branches one level deep; the nested
    /// structs' own branches are covered by their own exercises. Returns an
    /// empty vec for a struct that carries no generated statics.
    ///
    /// Safety: `normalizeKeys`/`denormalizeKeys` only remap keys on the untyped
    /// map (no typed deserialize), and `deserialize` removes the object-bearing
    /// keys *before* its typed `JSON.deserialize` — so the shaped values never
    /// reach a typed coercion and cannot throw.
    pub(crate) fn hook_exercises(
        &self,
        id: ir::DeclId,
        class: &str,
        s: &ir::StructDecl,
    ) -> Vec<String> {
        let mut stmts = Vec::new();
        if self.read_affected.contains(&id.0) {
            stmts.push(format!("{class}.normalizeKeys(null);"));
            stmts.push(format!(
                "{class}.normalizeKeys(new Map<String, Object>{{ {} }});",
                self.branch_entries(s, Dir::Normalize)
            ));
        }
        if self.write_affected.contains(&id.0) {
            stmts.push(format!(
                "{class}.denormalizeKeys(new Map<String, Object>{{ {} }});",
                self.denormalize_entries(s, id)
            ));
        }
        if self.object_bearing.contains(&id.0) {
            stmts.push(format!("{class}.deserialize(null);"));
            stmts.push(format!(
                "{class}.deserialize(new Map<String, Object>{{ {} }});",
                self.detach_entries(s)
            ));
        }
        stmts
    }

    /// `'key' => <value>` entries for each field a remap hook branches on
    /// (renamed or type-reaching in this direction) — the key under the hook's
    /// source name, the value shaped to drive the transform one level.
    fn branch_entries(&self, s: &ir::StructDecl, dir: Dir) -> String {
        let set = self.set_for(dir);
        let mut parts = Vec::new();
        for field in &s.fields {
            if !field_mismatched(field) && !self.type_reaches(&field.ty, set) {
                continue;
            }
            let key = match dir {
                Dir::Normalize => escape(&field.wire_name),
                Dir::Denormalize => escape(&safe_word(field.name.as_str())),
            };
            parts.push(format!("'{key}' => {}", self.wire_value(&field.ty, set)));
        }
        parts.join(", ")
    }

    /// Denormalize entries plus, for a null-writable struct, the explicit-null
    /// control key carrying every nullable field's Apex name (so each
    /// `if (nfName == '…')` injection branch executes) and, for an
    /// extra-writable struct, the extra field's own name under a nested map
    /// (so the flatten pass's `instanceof`/`putAll` branch executes).
    fn denormalize_entries(&self, s: &ir::StructDecl, id: ir::DeclId) -> String {
        let mut entries = self.branch_entries(s, Dir::Denormalize);
        let push = |entries: &mut String, entry: String| {
            *entries = if entries.is_empty() {
                entry
            } else {
                format!("{entries}, {entry}")
            };
        };
        if self.null_writable.contains(&id.0) {
            let names: Vec<String> = s
                .fields
                .iter()
                .filter(|f| is_nullable(&f.ty))
                .map(|f| format!("'{}'", escape(&safe_word(f.name.as_str()))))
                .collect();
            push(
                &mut entries,
                format!(
                    "'{}' => new List<Object>{{ {} }}",
                    escape(&self.null_control_name(s)),
                    names.join(", ")
                ),
            );
        }
        if self.extra_writable.contains(&id.0) {
            push(
                &mut entries,
                format!(
                    "'{}' => new Map<String, Object>{{ 'x' => 'y' }}",
                    escape(&self.extra_field_name(s))
                ),
            );
        }
        entries
    }

    /// `'wire' => <value>` entries for each object-bearing field `deserialize`
    /// detaches and reattaches, the value shaped to drive its reattach arm,
    /// plus (for an extra-bearing struct) an unrecognized key whose value is
    /// shaped for `extra_ty` — not a bare `'x'`, which would throw when
    /// `extra_ty` is a strict-typed scalar (e.g. `JSON.deserialize('x',
    /// Integer.class)`) — so the extra-bag computation loop executes cleanly
    /// regardless of the extra bag's value type.
    fn detach_entries(&self, s: &ir::StructDecl) -> String {
        let mut parts = Vec::new();
        for field in &s.fields {
            if !self.type_reaches_object_bearing(&field.ty) {
                continue;
            }
            parts.push(format!(
                "'{}' => {}",
                escape(&field.wire_name),
                self.reattach_value(&field.ty)
            ));
        }
        if let Some(extra_ty) = &s.extra {
            parts.push(format!(
                "'__extra_probe__' => {}",
                self.reattach_value(extra_ty)
            ));
        }
        parts.join(", ")
    }

    /// An untyped Apex value that makes `emit_transform` enter its branches one
    /// level: a reaching struct becomes an empty map (the nested hook runs on
    /// it), a reaching list/map wraps one such value, everything else a scalar
    /// (present key is all a rename-only branch needs).
    fn wire_value(&self, ty: &ir::Type, set: &HashSet<u32>) -> String {
        match ty {
            ir::Type::Optional(inner) | ir::Type::Nullable(inner) => self.wire_value(inner, set),
            ir::Type::List(inner) if self.type_reaches(inner, set) => {
                format!("new List<Object>{{ {} }}", self.wire_value(inner, set))
            }
            ir::Type::Map(inner) if self.type_reaches(inner, set) => {
                format!(
                    "new Map<String, Object>{{ 'k' => {} }}",
                    self.wire_value(inner, set)
                )
            }
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(inner) => self.wire_value(inner, set),
                ir::DeclKind::Struct(_) if set.contains(&id.0) => {
                    "new Map<String, Object>()".to_string()
                }
                ir::DeclKind::Struct(_) | ir::DeclKind::Enum(_) | ir::DeclKind::Union(_) => {
                    "'x'".to_string()
                }
            },
            // A rename-only branch needs only the key present; a non-reaching
            // container or scalar takes any placeholder.
            ir::Type::List(_)
            | ir::Type::Map(_)
            | ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::Date
            | ir::Type::DateTime
            | ir::Type::Binary
            | ir::Type::JsonValue => "'x'".to_string(),
        }
    }

    /// An untyped Apex value that makes `emit_reattach` enter its branches one
    /// level: an `Object` leaf takes any scalar; a nested object-bearing struct
    /// an empty map (its `deserialize` runs on it); a list/map of either wraps
    /// one such value so the element loop executes. Also used to shape the
    /// extra bag's (D-196) probe value, where `ty` is `extra_ty` itself rather
    /// than a field type.
    fn reattach_value(&self, ty: &ir::Type) -> String {
        match ty {
            ir::Type::Optional(inner) | ir::Type::Nullable(inner) => self.reattach_value(inner),
            ir::Type::JsonValue => "'x'".to_string(),
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(inner) => self.reattach_value(inner),
                ir::DeclKind::Union(_) => "'x'".to_string(),
                ir::DeclKind::Struct(_) if self.object_bearing.contains(&id.0) => {
                    "new Map<String, Object>()".to_string()
                }
                ir::DeclKind::Struct(_) | ir::DeclKind::Enum(_) => "null".to_string(),
            },
            ir::Type::List(inner) if self.is_object_leaf(inner) => "new List<Object>()".to_string(),
            ir::Type::List(inner) => {
                format!("new List<Object>{{ {} }}", self.reattach_value(inner))
            }
            ir::Type::Map(inner) if self.is_object_leaf(inner) => {
                "new Map<String, Object>()".to_string()
            }
            ir::Type::Map(inner) => {
                format!(
                    "new Map<String, Object>{{ 'k' => {} }}",
                    self.reattach_value(inner)
                )
            }
            // Scalars never reach the detached set for an object-bearing
            // field's own type — but `extra_ty` (D-196) can itself be a bare
            // scalar, and `null` round-trips cleanly through
            // `emit_reattach`'s scalar-arm cast for any strict Apex type
            // without throwing.
            ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::Date
            | ir::Type::DateTime
            | ir::Type::Binary => "null".to_string(),
        }
    }

    /// Emit one direction's remap static. `inject_nulls` appends the explicit-null
    /// pass (denormalize / null-writable only); `flatten_extra` appends the
    /// extra-bag merge (denormalize / extra-writable only).
    fn emit_method(
        &self,
        out: &mut String,
        class: &str,
        s: &ir::StructDecl,
        dir: Dir,
        inject_nulls: bool,
        flatten_extra: bool,
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
        if flatten_extra {
            self.emit_extra_flatten(out, s);
        }
        if inject_nulls {
            self.emit_null_injection(out, s);
        }
        let _ = writeln!(out, "        return raw;");
        let _ = writeln!(out, "    }}");
    }

    /// Emit the extra-bag flatten pass (D-196): native `JSON.serialize` puts
    /// the extra field's own map under its own key, nested one level too
    /// deep — Box's wire shape has those keys sitting alongside the named
    /// fields, not under an `"extra"` object. Pop the nested map and merge
    /// its entries back into `raw`. A struct whose extra bag was never set
    /// serializes it as absent (native null-suppression), so the removed
    /// value is `null` here, not an empty map — the `instanceof` guards that.
    fn emit_extra_flatten(&self, out: &mut String, s: &ir::StructDecl) {
        let field = self.extra_field_name(s);
        let _ = writeln!(
            out,
            "        // Extra bag (D-196): merge its keys into the parent object."
        );
        let _ = writeln!(
            out,
            "        Object extraRaw = raw.remove('{}');",
            escape(&field)
        );
        let _ = writeln!(
            out,
            "        if (extraRaw instanceof Map<String, Object>) {{"
        );
        let _ = writeln!(
            out,
            "            raw.putAll((Map<String, Object>) extraRaw);"
        );
        let _ = writeln!(out, "        }}");
    }

    /// Emit the explicit-null pass: for each Apex field name the caller listed in
    /// `fieldsToNull`, write `"<wire>": null` (Box clears it); then drop the
    /// control key so it never reaches the wire.
    fn emit_null_injection(&self, out: &mut String, s: &ir::StructDecl) {
        let control = self.null_control_name(s);
        let _ = writeln!(
            out,
            "        // Explicit null (D-138): send `null` for each field the caller listed."
        );
        let _ = writeln!(
            out,
            "        Object toNull = raw.remove('{}');",
            escape(&control)
        );
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

    /// Is this type position an `Object` leaf — a union or `JsonValue` (peeling
    /// only the tri-state / alias wrappers, not a list or map)?
    fn is_object_leaf(&self, ty: &ir::Type) -> bool {
        match ty {
            ir::Type::JsonValue => true,
            ir::Type::Optional(inner) | ir::Type::Nullable(inner) => self.is_object_leaf(inner),
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Union(_) => true,
                ir::DeclKind::Alias(inner) => self.is_object_leaf(inner),
                ir::DeclKind::Struct(_) | ir::DeclKind::Enum(_) => false,
            },
            ir::Type::List(_)
            | ir::Type::Map(_)
            | ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::Date
            | ir::Type::DateTime
            | ir::Type::Binary => false,
        }
    }

    /// The `deserialize(Object)` builder for an object-bearing struct. Native
    /// typed `JSON.deserialize` can't populate an `Object` field (a union or
    /// free-form JSON), so the fields that reach an `Object` leaf are detached
    /// from the untyped map, the natively-typed shell is deserialized (renamed
    /// first, if read-affected), and the detached fields are reattached from the
    /// raw tree — recursing into nested object-bearing structs (D-140).
    ///
    /// An open extra bag (D-196) is the same underlying problem — native
    /// deserialize can't route "every key I don't otherwise recognize" into a
    /// map field either, and worse: `JSON.deserialize` **throws** on a JSON
    /// property with no matching instance variable, so those keys must be
    /// stripped from the map before the typed shell is built, not just left
    /// for the extra field to pick up. The extra keys are computed first (raw
    /// minus every named field's wire key), removed from `raw` so the shell
    /// deserialize never sees them, and reattached last.
    pub(crate) fn emit_deserialize(
        &self,
        id: ir::DeclId,
        class: &str,
        s: &ir::StructDecl,
    ) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "    /** Build `{class}` from the untyped JSON tree. Apex's typed\n     \
             * `JSON.deserialize` can't populate an `Object` field (a union or\n     \
             * free-form JSON) or an open extra bag, so those are detached, the\n     \
             * typed shell is deserialized, then they are reattached from the\n     \
             * raw tree (D-140). */"
        );
        let _ = writeln!(
            out,
            "    public static {class} deserialize(Object rawInput) {{"
        );
        let _ = writeln!(out, "        if (rawInput == null) return null;");
        let _ = writeln!(
            out,
            "        Map<String, Object> raw = ((Map<String, Object>) rawInput).clone();"
        );
        if s.extra.is_some() {
            let _ = writeln!(
                out,
                "        // Extra bag (D-196): every key no named field claims. Computed\n        \
                 // and stripped before the shell deserialize below — an unrecognized\n        \
                 // JSON property makes native `JSON.deserialize` throw."
            );
            let _ = writeln!(out, "        Map<String, Object> extraRaw = raw.clone();");
            for field in &s.fields {
                let _ = writeln!(
                    out,
                    "        extraRaw.remove('{}');",
                    escape(&field.wire_name)
                );
            }
            let _ = writeln!(out, "        for (String extraKey : extraRaw.keySet()) {{");
            let _ = writeln!(out, "            raw.remove(extraKey);");
            let _ = writeln!(out, "        }}");
        }
        let detached: Vec<&ir::Field> = s
            .fields
            .iter()
            .filter(|f| self.type_reaches_object_bearing(&f.ty))
            .collect();
        for field in &detached {
            let _ = writeln!(
                out,
                "        Object d_{} = raw.remove('{}');",
                safe_word(field.name.as_str()),
                escape(&field.wire_name)
            );
        }
        let shell = if self.read_affected.contains(&id.0) {
            format!("{class}.normalizeKeys(raw)")
        } else {
            "raw".to_string()
        };
        let _ = writeln!(
            out,
            "        {class} result = ({class}) JSON.deserialize(JSON.serialize({shell}), {class}.class);"
        );
        for field in &detached {
            let apex = safe_word(field.name.as_str());
            self.emit_reattach(
                &mut out,
                &format!("result.{apex}"),
                &format!("d_{apex}"),
                &field.ty,
                0,
            );
        }
        if let Some(extra_ty) = &s.extra {
            let extra_field = self.extra_field_name(s);
            let map_ty = ir::Type::Map(Box::new(extra_ty.clone()));
            self.emit_reattach(
                &mut out,
                &format!("result.{extra_field}"),
                "extraRaw",
                &map_ty,
                0,
            );
        }
        let _ = writeln!(out, "        return result;");
        let _ = writeln!(out, "    }}");
        out
    }

    /// Emit statements that assign the reattached value of `src` (an untyped
    /// `Object`) into `lval`, per the field type. `depth` keeps nested-loop
    /// locals unique.
    fn emit_reattach(&self, out: &mut String, lval: &str, src: &str, ty: &ir::Type, depth: usize) {
        match ty {
            ir::Type::Optional(inner) | ir::Type::Nullable(inner) => {
                self.emit_reattach(out, lval, src, inner, depth);
            }
            // An `Object` leaf: assign the raw value (the caller dispatches a
            // union with `<Union>.parse`).
            ir::Type::JsonValue => {
                let _ = writeln!(out, "        {lval} = {src};");
            }
            ir::Type::Decl(id) => match &self.program.decl(*id).kind {
                ir::DeclKind::Alias(inner) => self.emit_reattach(out, lval, src, inner, depth),
                ir::DeclKind::Union(_) => {
                    let _ = writeln!(out, "        {lval} = {src};");
                }
                ir::DeclKind::Struct(_) if self.object_bearing.contains(&id.0) => {
                    let child = self
                        .names
                        .get(*id)
                        .expect("object-bearing struct has a name");
                    let _ = writeln!(out, "        {lval} = {child}.deserialize({src});");
                }
                // A clean struct or enum is never in the detached set.
                ir::DeclKind::Struct(_) | ir::DeclKind::Enum(_) => {}
            },
            ir::Type::List(inner) if self.is_object_leaf(inner) => {
                let _ = writeln!(out, "        {lval} = (List<Object>) {src};");
            }
            ir::Type::List(inner) => {
                let ai = apex_type(self.program, self.names, inner);
                let (lst, elem, val) = (
                    format!("dLst{depth}"),
                    format!("dEl{depth}"),
                    format!("dEv{depth}"),
                );
                // A null/absent source stays null; a non-list value fails loudly
                // on the cast rather than silently deserializing as empty.
                let _ = writeln!(out, "        if ({src} == null) {{");
                let _ = writeln!(out, "            {lval} = null;");
                let _ = writeln!(out, "        }} else {{");
                let _ = writeln!(out, "            List<{ai}> {lst} = new List<{ai}>();");
                let _ = writeln!(
                    out,
                    "            for (Object {elem} : (List<Object>) {src}) {{"
                );
                let _ = writeln!(out, "                {ai} {val};");
                self.emit_reattach(out, &val, &elem, inner, depth + 1);
                let _ = writeln!(out, "                {lst}.add({val});");
                let _ = writeln!(out, "            }}");
                let _ = writeln!(out, "            {lval} = {lst};");
                let _ = writeln!(out, "        }}");
            }
            ir::Type::Map(inner) if self.is_object_leaf(inner) => {
                let _ = writeln!(out, "        {lval} = (Map<String, Object>) {src};");
            }
            ir::Type::Map(inner) => {
                let ai = apex_type(self.program, self.names, inner);
                let (map, sub, key, val) = (
                    format!("dMap{depth}"),
                    format!("dSm{depth}"),
                    format!("dK{depth}"),
                    format!("dMv{depth}"),
                );
                // A null/absent source stays null; a non-map value fails loudly
                // on the cast rather than silently deserializing as empty.
                let _ = writeln!(out, "        if ({src} == null) {{");
                let _ = writeln!(out, "            {lval} = null;");
                let _ = writeln!(out, "        }} else {{");
                let _ = writeln!(
                    out,
                    "            Map<String, {ai}> {map} = new Map<String, {ai}>();"
                );
                let _ = writeln!(
                    out,
                    "            Map<String, Object> {sub} = (Map<String, Object>) {src};"
                );
                let _ = writeln!(out, "            for (String {key} : {sub}.keySet()) {{");
                let _ = writeln!(out, "                {ai} {val};");
                self.emit_reattach(out, &val, &format!("{sub}.get({key})"), inner, depth + 1);
                let _ = writeln!(out, "                {map}.put({key}, {val});");
                let _ = writeln!(out, "            }}");
                let _ = writeln!(out, "            {lval} = {map};");
                let _ = writeln!(out, "        }}");
            }
            // A named field's own scalar type never reaches the detached
            // set (it's never an `Object` leaf itself), so this arm was
            // unreachable until the extra bag (D-196) started calling
            // `emit_reattach` on a `Map<extra_ty>` whose value type can be
            // any scalar (a typed `additionalProperties` need not be a
            // struct or union). Round-trip through native (de)serialize —
            // the same trick the struct/union arms use — rather than
            // hand-coding `Object`→scalar coercion for every JSON.
            // deserializeUntyped() representation (Decimal for numbers,
            // ISO strings for Date/DateTime, base64 for Blob, …).
            ir::Type::Bool
            | ir::Type::Int64
            | ir::Type::Float64
            | ir::Type::String
            | ir::Type::Date
            | ir::Type::DateTime
            | ir::Type::Binary => {
                let apex_ty = apex_type(self.program, self.names, ty);
                let _ = writeln!(
                    out,
                    "        {lval} = ({apex_ty}) JSON.deserialize(JSON.serialize({src}), {apex_ty}.class);"
                );
            }
        }
    }

    /// Emit the response deserialization for a type that reaches an object-bearing
    /// struct: parse untyped, then build the typed value through the `deserialize`
    /// builders. `apex_ty` is the Apex return type. Writes the full `return …;`.
    pub(crate) fn emit_response_deserialize(&self, out: &mut String, apex_ty: &str, ty: &ir::Type) {
        let _ = writeln!(
            out,
            "        Object parsed = JSON.deserializeUntyped(response.body);"
        );
        let _ = writeln!(out, "        {apex_ty} deserialized;");
        self.emit_reattach(out, "deserialized", "parsed", ty, 0);
        let _ = writeln!(out, "        return deserialized;");
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
