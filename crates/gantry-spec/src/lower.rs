//! Lowering: named schemas → the typed IR (FR-1.3 meets FR-2).
//!
//! Every convention the Box specs rely on is decided *here*, once,
//! structurally (D-105):
//!
//! - `allOf` is composition: parts are flattened into one struct, later
//!   parts overriding earlier ones per property.
//! - `oneOf`/`anyOf` become unions. Box specs declare no OpenAPI
//!   `discriminator`; when every variant carries a single-valued `type`
//!   property (directly or through its `allOf` chain) with distinct
//!   values, the union is discriminated on `"type"`. Otherwise it is
//!   structural (no discriminator).
//! - String enums are **open** by default (G-11/D-012 lineage): unknown
//!   values are retained and round-tripped.
//! - Inline anonymous objects, enums, and unions get synthesized named
//!   declarations (`Owner` + PascalCase property), disambiguated
//!   deterministically.
//! - A schema that says nothing (`{}`) is `Type::JsonValue` — an explicit
//!   hole, counted in the lowering stats, never a silent fallback.
//!
//! Anything else is a loud [`IngestError`] naming the file and the
//! schema location (FR-1.4, NF-1, NF-3).

use std::collections::{HashMap, HashSet};

use gantry_ir as ir;
use indexmap::IndexMap;

use crate::error::IngestError;
use crate::ingest::{Document, SpecSet};
use crate::raw::{RawAdditionalProperties, RawOperation, RawParameter, RawSchema};

/// Bound on `allOf`/`$ref` chain walks; a real chain is 2–3 deep, so
/// hitting this means a reference cycle (loud error, not a hang).
const MAX_CHAIN_DEPTH: usize = 32;

/// The wire field Box unions discriminate on, by convention.
const DISCRIMINATOR_FIELD: &str = "type";

/// The result of lowering a whole [`SpecSet`].
#[derive(Debug)]
pub struct Lowering {
    pub program: ir::Program,
    pub stats: LoweringStats,
}

/// What the lowering produced — reported by `gantry check` so growth and
/// holes stay visible on every run (VR-6 lineage).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LoweringStats {
    pub structs: usize,
    pub unions: usize,
    /// Unions whose variants all carry a distinct `type` constant.
    pub discriminated_unions: usize,
    pub enums: usize,
    pub aliases: usize,
    /// Declarations synthesized for inline anonymous shapes.
    pub synthesized: usize,
    /// `Type::JsonValue` / free-form map sites: places the spec left a
    /// value unshaped. Explicit holes, watched so they only change
    /// deliberately.
    pub json_value_sites: usize,
    pub operations: usize,
    /// Operations whose success carries no body (e.g. 204).
    pub empty_responses: usize,
    /// Operations returning raw bytes (downloads, thumbnails).
    pub binary_responses: usize,
    /// Operations returning non-JSON text (the authorize page).
    pub text_responses: usize,
    /// Operations whose success is a redirect.
    pub redirect_responses: usize,
}

/// Lower every document of the set into one typed [`ir::Program`].
///
/// Declarations keep spec order per document, lowered into per-version modules
/// so nothing collides mid-lowering; the version merge (D-190) then collapses
/// them into one `schemas` namespace, superset-merging same-named types.
pub fn lower(set: &SpecSet) -> Result<Lowering, IngestError> {
    let mut arena: Vec<Option<ir::Decl>> = Vec::new();
    let mut operations: Vec<ir::Operation> = Vec::new();
    let mut stats = LoweringStats::default();
    for (index, doc) in set.documents.iter().enumerate() {
        let module = module_for(doc, index == 0)?;
        DocLowerer {
            doc,
            module,
            arena: &mut arena,
            operations: &mut operations,
            stats: &mut stats,
            ids: IndexMap::new(),
            used_names: HashSet::new(),
            method_names: HashMap::new(),
            idless_body_seeds: HashSet::new(),
            shapes: HashMap::new(),
        }
        .lower_document()?;
    }
    let decls: Vec<ir::Decl> = arena
        .into_iter()
        .map(|slot| slot.expect("every predeclared schema is filled by lower_document"))
        .collect();
    // Collapse the per-version schema modules into one `schemas` namespace,
    // merging same-named types across versions into one superset type (D-190).
    let program = crate::merge::merge_versioned_schemas(ir::Program { decls, operations })?;
    // The kind breakdown is computed from the *final* decls, not counted
    // during lowering: structural dedupe (D-127) discards inline copies after
    // they are built, so a build-time counter would over-report. (`aliases`
    // are never synthesized, so they are counted at their source.) The version
    // merge runs first, so a merged superset counts once.
    for decl in &program.decls {
        match &decl.kind {
            ir::DeclKind::Struct(_) => stats.structs += 1,
            ir::DeclKind::Enum(_) => stats.enums += 1,
            ir::DeclKind::Union(u) => {
                stats.unions += 1;
                if u.discriminator.is_some() {
                    stats.discriminated_unions += 1;
                }
            }
            ir::DeclKind::Alias(_) => {}
        }
    }
    Ok(Lowering { program, stats })
}

/// `schemas` for the base document, `schemas::vNrM` for versioned ones.
fn module_for(doc: &Document, is_base: bool) -> Result<ir::ModulePath, IngestError> {
    let root = identifier(doc, "components.schemas", "schemas")?;
    if is_base {
        return Ok(ir::ModulePath(vec![root]));
    }
    let segment = format!("v{}", doc.api_version.replace('.', "_"));
    let version = identifier(doc, "components.schemas", &segment)?;
    Ok(ir::ModulePath(vec![root, version]))
}

fn identifier(doc: &Document, location: &str, name: &str) -> Result<ir::Identifier, IngestError> {
    ir::Identifier::new(name).map_err(|err| IngestError::UnsupportedSchema {
        file: doc.file.clone(),
        location: location.to_string(),
        detail: err.to_string(),
    })
}

struct DocLowerer<'a> {
    doc: &'a Document,
    module: ir::ModulePath,
    arena: &'a mut Vec<Option<ir::Decl>>,
    operations: &'a mut Vec<ir::Operation>,
    stats: &'a mut LoweringStats,
    /// Named schema → predeclared id (so references resolve even through
    /// cycles).
    ids: IndexMap<String, ir::DeclId>,
    /// Every declaration name taken in this document's module, for
    /// deterministic synthesized-name disambiguation.
    used_names: HashSet<String>,
    /// Method names already taken, keyed by `manager\0variation` — the pretty
    /// short name (D-126) can collapse distinct operations (e.g. one vs two
    /// `{id}` path params), so a collision falls back to a fuller name.
    method_names: HashMap<String, HashSet<String>>,
    /// The terse request-body names claimed by **id-less** operations (D-189),
    /// gathered in a pre-pass. An id-addressed body only keeps its `{id}`
    /// selector (`FileIdContentCreateRequest`) when its terse name is in here;
    /// otherwise it takes the clean name (`FileUpdateRequest`). Order-independent.
    idless_body_seeds: HashSet<String>,
    /// Structural dedupe (D-127): the canonical decl id for each *synthesized*
    /// shape, keyed on the `DeclKind`'s structure. An inline shape identical
    /// to one already synthesized reuses it instead of minting a copy, so the
    /// hundreds of repeated `{id}`/`{id,type}` inline refs collapse to one
    /// type each. Bottom-up: identical subtrees dedupe first, so their
    /// parents then match too.
    shapes: HashMap<String, ir::DeclId>,
}

impl<'a> DocLowerer<'a> {
    fn lower_document(mut self) -> Result<(), IngestError> {
        let doc = self.doc;
        // Predeclare every named schema so `$ref`s resolve to stable ids.
        for name in doc.schemas.keys() {
            let id = self.next_id();
            self.arena.push(None);
            self.ids.insert(name.clone(), id);
            self.used_names.insert(name.clone());
        }
        for (name, raw) in &doc.schemas {
            let location = format!("components.schemas.{name}");
            let kind = self.lower_named(&location, name, raw)?;
            let decl = self.decl(&location, name, kind)?;
            let id = self.ids[name];
            self.arena[id.0 as usize] = Some(decl);
        }
        // Pre-pass (D-189): record the terse request-body name of every id-less
        // operation, so an id-addressed body only earns an `…Id…` disambiguator
        // when an id-less sibling truly shares its name — regardless of the order
        // the two appear in the spec.
        for item in doc.paths.values() {
            for (method, op) in item.operations() {
                if op.request_body.is_none() {
                    continue;
                }
                let Some(tag) = op.box_tag.as_deref() else {
                    continue;
                };
                let base_id =
                    base_operation_id(op.operation_id.as_deref().unwrap_or(""), &doc.api_version);
                let tokens = short_op_tokens(base_id, tag);
                if !tokens.iter().any(|t| t.eq_ignore_ascii_case("id")) {
                    self.idless_body_seeds
                        .insert(body_seed_for(tag, method, &tokens, false));
                }
            }
        }
        for (path_key, item) in &doc.paths {
            for (method, op) in item.operations() {
                let lowered = self.lower_operation(path_key, method, op)?;
                self.operations.push(lowered);
            }
        }
        Ok(())
    }

    fn next_id(&self) -> ir::DeclId {
        ir::DeclId(u32::try_from(self.arena.len()).expect("declaration arena overflow"))
    }

    fn decl(
        &self,
        location: &str,
        name: &str,
        kind: ir::DeclKind,
    ) -> Result<ir::Decl, IngestError> {
        // FR-1.2: declaration names normalize at ingestion. Newer Box
        // documents name variants `User--Mini`; the separators collapse
        // to the base-document convention (`UserMini`). Colliding results
        // are caught by sema's duplicate-name check, loudly.
        let normalized = gantry_ir::naming::pascal(&clean_name(name));
        Ok(ir::Decl {
            name: identifier(self.doc, location, &normalized)?,
            module: self.module.clone(),
            api_version: Some(ir::ApiVersion(self.doc.api_version.clone())),
            kind,
        })
    }

    /// Classify a *named* top-level schema.
    fn lower_named(
        &mut self,
        location: &str,
        name: &str,
        raw: &'a RawSchema,
    ) -> Result<ir::DeclKind, IngestError> {
        if let Some(reference) = &raw.reference {
            let id = self.resolve_ref(location, reference)?;
            self.stats.aliases += 1;
            return Ok(ir::DeclKind::Alias(ir::Type::Decl(id)));
        }
        // A named schema seeds its inline children from its own normalized
        // name (its decl name), so a field's inline type reads like
        // `FileFullType`, not the whole ancestry.
        let leaf = pascal(&clean_name(name));
        if !raw.one_of.is_empty() || !raw.any_of.is_empty() {
            return self
                .lower_union(location, &leaf, &leaf, raw)
                .map(ir::DeclKind::Union);
        }
        if let Some(values) = &raw.enumeration {
            return self.lower_enum(location, values).map(ir::DeclKind::Enum);
        }
        // `allOf` splits into structural parts and annotation-only parts
        // (description/example/nullable). One structural part and nothing
        // of the schema's own is the reference-wrapper idiom, not
        // composition — including wrappers around unions and enums.
        if !raw.all_of.is_empty() && raw.properties.is_empty() && raw.required.is_empty() {
            let structural: Vec<&RawSchema> = raw
                .all_of
                .iter()
                .filter(|part| !is_annotation_only(part))
                .collect();
            if let [part] = structural.as_slice() {
                return self.lower_named(&format!("{location}.allOf"), name, part);
            }
            if structural.is_empty() {
                // Only annotations: the spec says nothing about the shape.
                self.stats.json_value_sites += 1;
                self.stats.aliases += 1;
                return Ok(ir::DeclKind::Alias(ir::Type::JsonValue));
            }
        }
        if !raw.all_of.is_empty() || is_object(raw) {
            return self
                .lower_struct(location, &leaf, raw)
                .map(ir::DeclKind::Struct);
        }
        // A named primitive (or a schema that says nothing).
        let ty = self.lower_type(location, &leaf, &leaf, raw)?;
        self.stats.aliases += 1;
        Ok(ir::DeclKind::Alias(ty))
    }

    /// Lower a schema in *type position* (property, array item, map value).
    ///
    /// Synthesized names for inline anonymous shapes use **immediate
    /// context**, not the full ancestry (which produced 100+ char monsters —
    /// the box-node-sdk failure mode). `name` is the exact name to give a
    /// synthesized type here (its parent's leaf + this leaf, e.g.
    /// `StaticConfigClassification`); `leaf` is the short prefix threaded to
    /// this type's *children* (just this leaf, so depth adds one segment per
    /// level instead of accumulating the whole path). An array item or map
    /// value shares its container's `(name, leaf)` — it is the same position.
    fn lower_type(
        &mut self,
        location: &str,
        name: &str,
        leaf: &str,
        raw: &'a RawSchema,
    ) -> Result<ir::Type, IngestError> {
        if let Some(reference) = &raw.reference {
            return Ok(ir::Type::Decl(self.resolve_ref(location, reference)?));
        }
        if !raw.one_of.is_empty() || !raw.any_of.is_empty() {
            let union = self.lower_union(location, name, leaf, raw)?;
            let id = self.synthesize(location, name, ir::DeclKind::Union(union))?;
            return Ok(ir::Type::Decl(id));
        }
        if let Some(values) = &raw.enumeration {
            // Numeric "enums" (allowed-value lists) lower to their base
            // type; only string enums become declarations.
            if values.iter().all(serde_json::Value::is_number) {
                return self.primitive(location, raw);
            }
            let has_null = values.iter().any(serde_json::Value::is_null);
            let decl = self.lower_enum(location, values)?;
            let id = self.synthesize(location, name, ir::DeclKind::Enum(decl))?;
            // A `null` entry in the value list is the spec's way of
            // saying the field may be explicitly null (D-105, D-110).
            let ty = ir::Type::Decl(id);
            return Ok(if has_null { nullable(ty) } else { ty });
        }
        // The reference-wrapper idiom in type position: one structural
        // `allOf` part plus annotations is that part's type. (Annotation
        // `nullable` is read by `effective_nullable` at the field site.)
        if !raw.all_of.is_empty() && raw.properties.is_empty() && raw.required.is_empty() {
            let structural: Vec<&'a RawSchema> = raw
                .all_of
                .iter()
                .filter(|part| !is_annotation_only(part))
                .collect();
            if let [part] = structural.as_slice() {
                return self.lower_type(&format!("{location}.allOf"), name, leaf, part);
            }
            if structural.is_empty() {
                self.stats.json_value_sites += 1;
                return Ok(ir::Type::JsonValue);
            }
        }
        if !raw.all_of.is_empty() {
            let decl = self.lower_struct(location, leaf, raw)?;
            let id = self.synthesize(location, name, ir::DeclKind::Struct(decl))?;
            return Ok(ir::Type::Decl(id));
        }
        match raw.schema_type.as_deref() {
            Some("array") => {
                let Some(items) = &raw.items else {
                    return Err(self.unsupported(location, "array schema has no `items`"));
                };
                let inner = self.lower_type(&format!("{location}.items"), name, leaf, items)?;
                Ok(ir::Type::List(Box::new(inner)))
            }
            Some("object") if !raw.properties.is_empty() => {
                let decl = self.lower_struct(location, leaf, raw)?;
                let id = self.synthesize(location, name, ir::DeclKind::Struct(decl))?;
                Ok(ir::Type::Decl(id))
            }
            Some("object") => match raw.additional_properties.as_ref() {
                Some(RawAdditionalProperties::Schema(value)) => {
                    let inner = self.lower_type(
                        &format!("{location}.additionalProperties"),
                        name,
                        leaf,
                        value,
                    )?;
                    Ok(ir::Type::Map(Box::new(inner)))
                }
                Some(RawAdditionalProperties::Bool(_)) | None => {
                    // A free-form object: string keys, unshaped values.
                    self.stats.json_value_sites += 1;
                    Ok(ir::Type::Map(Box::new(ir::Type::JsonValue)))
                }
            },
            Some("string" | "integer" | "number" | "boolean") => self.primitive(location, raw),
            Some(other) => {
                Err(self.unsupported(location, &format!("unknown schema type {other:?}")))
            }
            None if !raw.properties.is_empty() => {
                let decl = self.lower_struct(location, leaf, raw)?;
                let id = self.synthesize(location, name, ir::DeclKind::Struct(decl))?;
                Ok(ir::Type::Decl(id))
            }
            None => {
                // The spec says nothing about this value's shape.
                self.stats.json_value_sites += 1;
                Ok(ir::Type::JsonValue)
            }
        }
    }

    fn primitive(&self, location: &str, raw: &RawSchema) -> Result<ir::Type, IngestError> {
        match raw.schema_type.as_deref() {
            Some("string") => Ok(match raw.format.as_deref() {
                Some("date") => ir::Type::Date,
                Some("date-time") => ir::Type::DateTime,
                Some("binary") => ir::Type::Binary,
                // Every other format the specs use (url, token, digest…)
                // is documentation, not structure.
                _ => ir::Type::String,
            }),
            Some("integer") => Ok(ir::Type::Int64),
            Some("number") => Ok(ir::Type::Float64),
            Some("boolean") => Ok(ir::Type::Bool),
            Some(other) => {
                Err(self.unsupported(location, &format!("unknown primitive type {other:?}")))
            }
            None => Err(self.unsupported(location, "numeric enum without a declared type")),
        }
    }

    /// Flatten `allOf` composition + own properties into one struct.
    ///
    /// `leaf` is this struct's own short name; each inline field type is
    /// named `leaf + FieldName` and threads `FieldName` as the leaf for its
    /// own children (immediate-context naming — see `lower_type`).
    fn lower_struct(
        &mut self,
        location: &str,
        leaf: &str,
        raw: &'a RawSchema,
    ) -> Result<ir::StructDecl, IngestError> {
        let mut properties: IndexMap<String, &'a RawSchema> = IndexMap::new();
        let mut required: HashSet<String> = HashSet::new();
        self.collect_parts(location, raw, &mut properties, &mut required, 0)?;

        let mut fields = Vec::with_capacity(properties.len());
        for (wire_name, prop) in properties {
            let prop_location = format!("{location}.properties.{wire_name}");
            // FR-1.2: the field identifier is the wire name normalized
            // (Box metadata keys are `$`-prefixed: `$id`, `$template`…).
            // The wire name itself is untouched — serialization never
            // depends on the identifier.
            let name = identifier(self.doc, &prop_location, &clean_name(&wire_name))?;
            let child_leaf = pascal(&clean_name(&wire_name));
            let child_name = synth_name(leaf, &wire_name);
            let mut ty = self.lower_type(&prop_location, &child_name, &child_leaf, prop)?;
            // The tri-state is structural (FR-2.3, D-110): `nullable`
            // means the wire value may be an explicit `null` (Box uses it
            // to clear fields); not-required means the key may be absent.
            // Canonical nesting: Optional<Nullable<T>>.
            if effective_nullable(prop) {
                ty = nullable(ty);
            }
            if !required.contains(&wire_name) {
                ty = ir::Type::Optional(Box::new(ty));
            }
            fields.push(ir::Field {
                name,
                wire_name,
                ty,
            });
        }
        Ok(ir::StructDecl { fields })
    }

    /// Walk a schema's `allOf` chain (through `$ref`s), collecting
    /// properties and required-ness. Later parts override earlier ones per
    /// property, deterministically.
    fn collect_parts(
        &self,
        location: &str,
        raw: &'a RawSchema,
        properties: &mut IndexMap<String, &'a RawSchema>,
        required: &mut HashSet<String>,
        depth: usize,
    ) -> Result<(), IngestError> {
        if depth > MAX_CHAIN_DEPTH {
            return Err(self.unsupported(
                location,
                "allOf/$ref chain exceeds the depth bound — reference cycle?",
            ));
        }
        for (index, part) in raw.all_of.iter().enumerate() {
            if is_annotation_only(part) {
                continue;
            }
            let part_location = format!("{location}.allOf[{index}]");
            let target = if let Some(reference) = &part.reference {
                self.resolve_raw(&part_location, reference)?
            } else {
                part
            };
            if !target.one_of.is_empty()
                || !target.any_of.is_empty()
                || target.enumeration.is_some()
            {
                return Err(self.unsupported(
                    &part_location,
                    "allOf part is not an object schema (found a union or enum)",
                ));
            }
            self.collect_parts(&part_location, target, properties, required, depth + 1)?;
        }
        for (key, value) in &raw.properties {
            properties.insert(key.clone(), value);
        }
        for key in &raw.required {
            required.insert(key.clone());
        }
        Ok(())
    }

    /// Lower `oneOf`/`anyOf` into a union, inferring the `type`
    /// discriminator convention where it holds.
    fn lower_union(
        &mut self,
        location: &str,
        name: &str,
        leaf: &str,
        raw: &'a RawSchema,
    ) -> Result<ir::UnionDecl, IngestError> {
        if !raw.one_of.is_empty() && !raw.any_of.is_empty() {
            return Err(self.unsupported(location, "schema has both oneOf and anyOf"));
        }
        let (raw_variants, key) = if raw.one_of.is_empty() {
            (&raw.any_of, "anyOf")
        } else {
            (&raw.one_of, "oneOf")
        };

        let mut variants = Vec::with_capacity(raw_variants.len());
        for (index, variant) in raw_variants.iter().enumerate() {
            let variant_location = format!("{location}.{key}[{index}]");
            let (ty, discriminator_value) = if let Some(reference) = &variant.reference {
                let id = self.resolve_ref(&variant_location, reference)?;
                let target = self.resolve_raw(&variant_location, reference)?;
                (ir::Type::Decl(id), self.type_const(target, 0))
            } else {
                let variant_name = format!("{name}Variant{index}");
                let variant_leaf = format!("{leaf}Variant{index}");
                let ty =
                    self.lower_type(&variant_location, &variant_name, &variant_leaf, variant)?;
                (ty, self.type_const(variant, 0))
            };
            variants.push(ir::UnionVariant {
                discriminator_value,
                ty,
            });
        }

        // The convention holds only if every variant has a distinct value.
        let mut seen = HashSet::new();
        let discriminated = variants.iter().all(|v| {
            v.discriminator_value
                .as_ref()
                .is_some_and(|value| seen.insert(value.clone()))
        });
        let discriminator = if discriminated {
            Some(DISCRIMINATOR_FIELD.to_string())
        } else {
            // Half-inferred values would be misleading: a structural union
            // carries none.
            for variant in &mut variants {
                variant.discriminator_value = None;
            }
            None
        };
        Ok(ir::UnionDecl {
            discriminator,
            variants,
            extensibility: ir::Extensibility::Open,
        })
    }

    /// The single-valued `type` property of a schema, walked through its
    /// `allOf` chain — the Box discriminator convention (D-105).
    fn type_const(&self, raw: &RawSchema, depth: usize) -> Option<String> {
        if depth > MAX_CHAIN_DEPTH {
            return None;
        }
        if let Some(type_prop) = raw.properties.get(DISCRIMINATOR_FIELD)
            && let Some(values) = &type_prop.enumeration
            && let [serde_json::Value::String(value)] = values.as_slice()
        {
            return Some(value.clone());
        }
        for part in &raw.all_of {
            let target = if let Some(reference) = &part.reference {
                let name = reference.strip_prefix("#/components/schemas/")?;
                self.doc.schemas.get(name)?
            } else {
                part
            };
            if let Some(value) = self.type_const(target, depth + 1) {
                return Some(value);
            }
        }
        None
    }

    fn lower_enum(
        &mut self,
        location: &str,
        values: &[serde_json::Value],
    ) -> Result<ir::EnumDecl, IngestError> {
        let mut out = Vec::with_capacity(values.len());
        for value in values {
            if value.is_null() {
                // A null entry encodes nullability, not a value.
                continue;
            }
            match value.as_str() {
                Some(text) => out.push(text.to_string()),
                None => {
                    return Err(
                        self.unsupported(location, &format!("non-string enum value {value}"))
                    );
                }
            }
        }
        Ok(ir::EnumDecl {
            values: out,
            // Box enums are open by convention (G-11, D-105): unknown
            // values are retained and round-tripped.
            extensibility: ir::Extensibility::Open,
        })
    }

    /// Add a synthesized declaration for an inline anonymous shape, with a
    /// deterministic, collision-free name.
    fn synthesize(
        &mut self,
        location: &str,
        base: &str,
        kind: ir::DeclKind,
    ) -> Result<ir::DeclId, IngestError> {
        // Structural dedupe (D-127): an inline shape identical to one already
        // synthesized reuses it. The `DeclKind` Debug form is a faithful
        // structural key — it captures the kind, every field's wire name and
        // type, enum values, and union variants, and the inner `DeclId`s it
        // names are themselves already canonical (children dedupe first). The
        // decl *name* is not part of the key, so differently-named copies of
        // one shape collapse.
        let key = format!("{:?}", kind);
        if let Some(&existing) = self.shapes.get(&key) {
            return Ok(existing);
        }
        let mut name = base.to_string();
        let mut suffix = 2;
        while self.used_names.contains(&name) {
            name = format!("{base}{suffix}");
            suffix += 1;
        }
        self.used_names.insert(name.clone());
        let decl = self.decl(location, &name, kind)?;
        let id = self.next_id();
        self.arena.push(Some(decl));
        self.shapes.insert(key, id);
        self.stats.synthesized += 1;
        Ok(id)
    }

    /// Lower one operation (FR-1.3: base-URL mapping, binary responses,
    /// media classification — all decided here, structurally).
    fn lower_operation(
        &mut self,
        path_key: &str,
        method: &'static str,
        op: &'a RawOperation,
    ) -> Result<ir::Operation, IngestError> {
        let doc = self.doc;
        let location = format!("paths[{path_key:?}].{method}");

        // `#variation` splits into structured data (D-104); the `#` never
        // reaches an identifier.
        let raw_id = op
            .operation_id
            .as_deref()
            .expect("operationId presence is validated at ingestion");
        let (base_id, variation) = match raw_id.split_once('#') {
            Some((base, variation)) => (base, Some(variation)),
            None => (raw_id, None),
        };
        // Versioned documents suffix their operationIds with `_v2025.0`.
        // That is version plumbing, not part of the name: the operation
        // already carries its api_version. Strip the *matching* suffix; a
        // mismatched version marker is a spec inconsistency and fails.
        let base_id = strip_version_suffix(base_id, &doc.api_version);
        if let Some(index) = base_id.rfind("_v")
            && base_id[index + 2..]
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.')
            && base_id[index + 2..].contains('.')
        {
            return Err(self.unsupported(
                &location,
                &format!(
                    "operationId {raw_id:?} carries version marker {:?} but the document \
                     declares version {:?}",
                    &base_id[index..],
                    doc.api_version
                ),
            ));
        }
        let box_tag = op
            .box_tag
            .as_deref()
            .expect("x-box-tag presence is validated at ingestion");
        // Box operationIds carry noise that inflates the method name and every
        // type synthesized under it: opaque id handles leaked from example
        // URLs (`..._6VMVochwUWo_...`) and an echo of the manager tag (the
        // call is already `client.<manager>.<method>`). Strip both once
        // (D-126). The *method name* is then further prettified
        // (`get_files_id`→`GetById`); the *type seed* keeps the fuller token
        // list, since the pretty leaf is deliberately terse and repeats
        // across operations — a `GetById`-seeded `…Body`/`…Response` would
        // collide, the token-list seed stays operation-unique.
        let short_tokens = short_op_tokens(base_id, box_tag);
        let variation = variation
            .map(|v| identifier(doc, &location, &clean_name(v)))
            .transpose()?;
        // Resolve the method name against the ones already taken in this
        // (manager, variation): the pretty form first, the keep-all-ids form
        // on collision, then a numeric suffix. Sema rejects a true duplicate
        // loudly, so this must always converge on a fresh name.
        let scope = format!(
            "{box_tag}\u{0}{}",
            variation.as_ref().map_or("", ir::Identifier::as_str)
        );
        // A collection read leads with the `list` verb instead of `get`, so the
        // single-item read keeps the clean `get` name (`getFileMetadata` for one,
        // `listFileMetadata` for all). Using the verb — not a `List` suffix —
        // lets each backend's casing render the idiomatic form for its language
        // (`ListFileMetadata`, `list_file_metadata`, …). Computed before the
        // `taken` borrow.
        let is_list = method == "get" && self.returns_collection(op);
        let relabel = |snake: String| -> String {
            match is_list {
                true => snake
                    .strip_prefix("get")
                    .map_or(snake.clone(), |rest| format!("list{rest}")),
                false => snake,
            }
        };
        let taken = self.method_names.entry(scope).or_default();
        let method_ident = {
            // A curated name for the few endpoints whose operationId tokens
            // derive an awkward or colliding method (D-194). Otherwise: terse
            // first (no redundant trailing `ById`); fall back to the `…ById`
            // form, then the keep-all-ids form — so `ById` reappears only when a
            // sibling would otherwise collide.
            let mut chosen = match curated_method_name(base_id) {
                Some(curated) => clean_name(curated),
                None => {
                    let terse = clean_name(&relabel(method_name(&short_tokens, false, true)));
                    let pretty = clean_name(&relabel(method_name(&short_tokens, false, false)));
                    let full = clean_name(&relabel(method_name(&short_tokens, true, false)));
                    let mut c = terse;
                    if taken.contains(&c.to_ascii_lowercase()) {
                        c = pretty;
                    }
                    if taken.contains(&c.to_ascii_lowercase()) {
                        c = full;
                    }
                    c
                }
            };
            // A numeric suffix is the last-resort disambiguator. Sema rejects a
            // true duplicate loudly, so this always converges on a fresh name.
            let base = chosen.clone();
            let mut suffix = 2;
            while taken.contains(&chosen.to_ascii_lowercase()) {
                chosen = format!("{base}_{suffix}");
                suffix += 1;
            }
            taken.insert(chosen.to_ascii_lowercase());
            chosen
        };
        let name = identifier(doc, &location, &method_ident)?;
        let manager = identifier(doc, &location, box_tag)?;
        // Seed for synthesized declarations belonging to this operation.
        let owner = {
            let mut owner = pascal(&short_tokens.join("_"));
            if let Some(v) = &variation {
                owner.push_str(&pascal(v.as_str()));
            }
            owner
        };

        let params = self.lower_params(&location, &owner, op)?;
        let path = self.lower_path(&location, path_key, &params)?;
        let request = self.lower_request_body(&location, &owner, op, box_tag, method)?;
        let response = self.lower_response(&location, &owner, op)?;
        let base_url = self.lower_base_url(&location, op)?;

        let method = match method {
            "get" => ir::HttpMethod::Get,
            "put" => ir::HttpMethod::Put,
            "post" => ir::HttpMethod::Post,
            "delete" => ir::HttpMethod::Delete,
            "options" => ir::HttpMethod::Options,
            "head" => ir::HttpMethod::Head,
            "patch" => ir::HttpMethod::Patch,
            "trace" => ir::HttpMethod::Trace,
            other => unreachable!("RawPathItem::operations only yields method keys, got {other:?}"),
        };

        self.stats.operations += 1;
        match &response {
            ir::ResponseShape::None => self.stats.empty_responses += 1,
            ir::ResponseShape::Binary => self.stats.binary_responses += 1,
            ir::ResponseShape::Text => self.stats.text_responses += 1,
            ir::ResponseShape::Redirect => self.stats.redirect_responses += 1,
            ir::ResponseShape::Json(_) => {}
        }

        Ok(ir::Operation {
            name,
            variation,
            manager,
            api_version: Some(ir::ApiVersion(doc.api_version.clone())),
            method,
            base_url,
            path,
            params,
            request,
            response,
            deprecated: op.deprecated,
        })
    }

    fn lower_params(
        &mut self,
        location: &str,
        owner: &str,
        op: &'a RawOperation,
    ) -> Result<Vec<ir::Param>, IngestError> {
        let doc = self.doc;
        let mut params = Vec::with_capacity(op.parameters.len());
        for (index, raw_param) in op.parameters.iter().enumerate() {
            let param_location = format!("{location}.parameters[{index}]");
            let resolved: &'a RawParameter = if let Some(reference) = &raw_param.reference {
                let name = reference
                    .strip_prefix("#/components/parameters/")
                    .ok_or_else(|| self.unresolved(&param_location, reference))?;
                doc.parameters
                    .get(name)
                    .ok_or_else(|| self.unresolved(&param_location, reference))?
            } else {
                raw_param
            };
            let Some(wire_name) = resolved.name.as_deref().filter(|n| !n.is_empty()) else {
                return Err(self.unsupported(&param_location, "parameter has no name"));
            };
            let param_kind = match resolved.location.as_deref() {
                Some("query") => ir::ParamLocation::Query,
                Some("path") => ir::ParamLocation::Path,
                Some("header") => ir::ParamLocation::Header,
                other => {
                    return Err(self.unsupported(
                        &param_location,
                        &format!("unsupported parameter location {other:?}"),
                    ));
                }
            };
            // The `box-version` header is a required, single-value constant
            // equal to the operation's API version — the engine sets it
            // automatically per endpoint (D-191), so it is not a caller-facing
            // parameter and its inline version enum is never synthesized.
            if param_kind == ir::ParamLocation::Header && wire_name == "box-version" {
                continue;
            }
            if param_kind == ir::ParamLocation::Path && !resolved.required {
                return Err(self.unsupported(&param_location, "path parameter not marked required"));
            }
            let Some(schema) = &resolved.schema else {
                return Err(self.unsupported(&param_location, "parameter has no schema"));
            };
            let mut ty = self.lower_type(
                &format!("{param_location}.schema"),
                &synth_name(owner, wire_name),
                &pascal(&clean_name(wire_name)),
                schema,
            )?;
            if !resolved.required {
                ty = ir::Type::Optional(Box::new(ty));
            }
            params.push(ir::Param {
                name: identifier(doc, &param_location, wire_name)?,
                wire_name: wire_name.to_string(),
                location: param_kind,
                ty,
            });
        }
        Ok(params)
    }

    /// Parse the path template into structured segments (FR-2.2). Any
    /// `#variation` fragment on the path key is spec-authoring plumbing,
    /// not part of the request path.
    fn lower_path(
        &self,
        location: &str,
        path_key: &str,
        params: &[ir::Param],
    ) -> Result<Vec<ir::PathSegment>, IngestError> {
        let template = path_key
            .split_once('#')
            .map_or(path_key, |(template, _)| template);
        let mut segments = Vec::new();
        for segment in template.split('/').filter(|s| !s.is_empty()) {
            let parts = self.parse_segment(location, segment, params)?;
            segments.push(match parts.as_slice() {
                [ir::PathPart::Literal(text)] => ir::PathSegment::Literal(text.clone()),
                [ir::PathPart::Parameter(name)] => ir::PathSegment::Parameter(name.clone()),
                _ => ir::PathSegment::Composite(parts),
            });
        }
        Ok(segments)
    }

    /// Scan one segment into literal/parameter parts (the real spec has
    /// mixed segments such as `thumbnail.{extension}`). Every placeholder
    /// must be backed by a declared path parameter.
    fn parse_segment(
        &self,
        location: &str,
        segment: &str,
        params: &[ir::Param],
    ) -> Result<Vec<ir::PathPart>, IngestError> {
        let mut parts = Vec::new();
        let mut rest = segment;
        while let Some(open) = rest.find('{') {
            if open > 0 {
                parts.push(ir::PathPart::Literal(rest[..open].to_string()));
            }
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                return Err(self.unsupported(
                    location,
                    &format!("unbalanced braces in path segment {segment:?}"),
                ));
            };
            let param_name = &after[..close];
            if !params
                .iter()
                .any(|p| p.location == ir::ParamLocation::Path && p.wire_name == param_name)
            {
                return Err(self.unsupported(
                    location,
                    &format!("path parameter {{{param_name}}} has no declaration"),
                ));
            }
            parts.push(ir::PathPart::Parameter(identifier(
                self.doc, location, param_name,
            )?));
            rest = &after[close + 1..];
        }
        if rest.contains('}') {
            return Err(self.unsupported(
                location,
                &format!("unbalanced braces in path segment {segment:?}"),
            ));
        }
        if !rest.is_empty() {
            parts.push(ir::PathPart::Literal(rest.to_string()));
        }
        Ok(parts)
    }

    fn lower_request_body(
        &mut self,
        location: &str,
        owner: &str,
        op: &'a RawOperation,
        box_tag: &str,
        method: &str,
    ) -> Result<Option<ir::RequestBody>, IngestError> {
        let Some(body) = &op.request_body else {
            return Ok(None);
        };
        let body_location = format!("{location}.requestBody");
        let mut content = body.content.iter();
        let Some((media_key, media)) = content.next() else {
            return Err(self.unsupported(&body_location, "requestBody has no content"));
        };
        if content.next().is_some() {
            return Err(
                self.unsupported(&body_location, "requestBody declares multiple media types")
            );
        }
        let media_kind = match media_key.as_str() {
            "application/json" => ir::RequestMedia::Json,
            "application/json-patch+json" => ir::RequestMedia::JsonPatch,
            "application/x-www-form-urlencoded" => ir::RequestMedia::UrlEncoded,
            "multipart/form-data" => ir::RequestMedia::Multipart,
            "application/octet-stream" => ir::RequestMedia::OctetStream,
            other => {
                return Err(self.unsupported(
                    &body_location,
                    &format!("unsupported request media type {other:?}"),
                ));
            }
        };
        // Name request bodies after their operation, not a generic `PostBody`
        // (D-189). The base is the operation's *distinctive* path tokens
        // (`FileUploadSessionsCreateRequest`, `FileUploadSessionCommitRequest`),
        // falling back to the singular manager subject when the operation has no
        // distinctive path (`FolderCreateRequest`). A trailing curated action
        // (`commit`) is the verb; otherwise the HTTP verb supplies it.
        let base_id = base_operation_id(
            op.operation_id.as_deref().unwrap_or(""),
            &self.doc.api_version,
        );
        let all = short_op_tokens(base_id, box_tag);
        let has_id = all.iter().any(|t| t.eq_ignore_ascii_case("id"));
        let terse_seed = body_seed_for(box_tag, method, &all, false);
        // An id-addressed body keeps its `{id}` selector only when an id-less
        // sibling claims the same terse name (`FileContentCreateRequest` +
        // `FileIdContentCreateRequest`); otherwise it takes the clean terse name
        // (`FileUpdateRequest`, no redundant `Id`). The id-less seeds are gathered
        // in a pre-pass (`gather_idless_body_seeds`), so this is order-independent.
        let body_seed = if has_id && self.idless_body_seeds.contains(&terse_seed) {
            body_seed_for(box_tag, method, &all, true)
        } else {
            terse_seed
        };
        let mut ty = match &media.schema {
            Some(schema) => self.lower_type(
                &format!("{body_location}.content[{media_key:?}]"),
                &body_seed,
                owner,
                schema,
            )?,
            None => {
                self.stats.json_value_sites += 1;
                ir::Type::JsonValue
            }
        };
        if !body.required {
            ty = ir::Type::Optional(Box::new(ty));
        }
        Ok(Some(ir::RequestBody {
            media: media_kind,
            ty,
        }))
    }

    /// Classify the success responses (D-106): ascending status order, the
    /// first content-bearing 2xx/3xx decides the shape; every media of
    /// that response must classify identically; a content-free 302 makes
    /// the operation a redirect; otherwise the success is body-less.
    fn lower_response(
        &mut self,
        location: &str,
        owner: &str,
        op: &'a RawOperation,
    ) -> Result<ir::ResponseShape, IngestError> {
        let mut codes: Vec<(&String, &crate::raw::RawResponse)> = op.responses.iter().collect();
        codes.sort_by(|(a, _), (b, _)| a.cmp(b));
        let mut saw_redirect = false;
        for (code, response) in codes {
            if code == "default" {
                continue;
            }
            let Ok(number) = code.parse::<u16>() else {
                return Err(self.unsupported(
                    location,
                    &format!("unparseable response status code {code:?}"),
                ));
            };
            if !(200..400).contains(&number) {
                continue;
            }
            if number == 302 {
                saw_redirect = true;
            }
            if response.content.is_empty() {
                continue;
            }
            let response_location = format!("{location}.responses.{code}");
            let mut shape: Option<ir::ResponseShape> = None;
            for (media_key, media) in &response.content {
                let classified = match media_key.as_str() {
                    "application/json" => {
                        let ty = match &media.schema {
                            Some(schema) => self.lower_type(
                                &format!("{response_location}.content[{media_key:?}]"),
                                &format!("{owner}Response"),
                                owner,
                                schema,
                            )?,
                            None => {
                                self.stats.json_value_sites += 1;
                                ir::Type::JsonValue
                            }
                        };
                        ir::ResponseShape::Json(ty)
                    }
                    "application/octet-stream" => ir::ResponseShape::Binary,
                    key if key.starts_with("image/") => ir::ResponseShape::Binary,
                    "text/html" => ir::ResponseShape::Text,
                    other => {
                        return Err(self.unsupported(
                            &response_location,
                            &format!("unsupported response media type {other:?}"),
                        ));
                    }
                };
                match &shape {
                    None => shape = Some(classified),
                    Some(previous) if *previous == classified => {}
                    Some(previous) => {
                        return Err(self.unsupported(
                            &response_location,
                            &format!(
                                "response mixes content classes ({previous:?} vs {classified:?})"
                            ),
                        ));
                    }
                }
            }
            return Ok(shape.expect("content loop ran at least once"));
        }
        Ok(if saw_redirect {
            ir::ResponseShape::Redirect
        } else {
            ir::ResponseShape::None
        })
    }

    /// The D-106 base-URL mapping (the G-2 quirk): spec `servers` URLs map
    /// to the closed [`ir::BaseUrl`] set; anything else is a loud error.
    fn lower_base_url(
        &self,
        location: &str,
        op: &'a RawOperation,
    ) -> Result<ir::BaseUrl, IngestError> {
        match op.servers.as_slice() {
            [] => Ok(ir::BaseUrl::Api),
            [server] => match server.url.as_str() {
                "https://api.box.com/2.0" => Ok(ir::BaseUrl::Api),
                "https://api.box.com" => Ok(ir::BaseUrl::ApiRoot),
                "https://upload.box.com/api/2.0" => Ok(ir::BaseUrl::Upload),
                "https://{box-upload-server}/api/2.0" => Ok(ir::BaseUrl::UploadSession),
                "https://account.box.com/api/oauth2" => Ok(ir::BaseUrl::OAuthAuthorize),
                "https://dl.boxcloud.com/2.0" => Ok(ir::BaseUrl::Download),
                other => Err(self.unsupported(
                    location,
                    &format!("unknown server URL {other:?} (extend the D-106 mapping)"),
                )),
            },
            _ => Err(self.unsupported(location, "multiple operation-level servers")),
        }
    }

    fn resolve_ref(&self, location: &str, reference: &str) -> Result<ir::DeclId, IngestError> {
        let name = self.ref_name(location, reference)?;
        self.ids
            .get(name)
            .copied()
            .ok_or_else(|| self.unresolved(location, reference))
    }

    /// Whether the operation's success response is a Box **collection** — a
    /// paginated wrapper carrying an `entries` array, or a bare array. A list
    /// read takes a `List` suffix (`getFileMetadataList`) so the single-item
    /// read keeps the clean name (`getFileMetadata`).
    fn returns_collection(&self, op: &'a RawOperation) -> bool {
        let mut codes: Vec<&String> = op.responses.keys().filter(|c| c.starts_with('2')).collect();
        codes.sort();
        codes.into_iter().any(|code| {
            op.responses[code]
                .content
                .values()
                .filter_map(|m| m.schema.as_ref())
                .any(|s| self.schema_is_collection(s, 0))
        })
    }

    fn schema_is_collection(&self, schema: &'a RawSchema, depth: usize) -> bool {
        if depth > 4 {
            return false;
        }
        if schema.schema_type.as_deref() == Some("array")
            || schema.properties.contains_key("entries")
        {
            return true;
        }
        if let Some(reference) = &schema.reference
            && let Ok(target) = self.resolve_raw("", reference)
        {
            return self.schema_is_collection(target, depth + 1);
        }
        schema
            .all_of
            .iter()
            .any(|s| self.schema_is_collection(s, depth + 1))
    }

    fn resolve_raw(&self, location: &str, reference: &str) -> Result<&'a RawSchema, IngestError> {
        let name = self.ref_name(location, reference)?;
        self.doc
            .schemas
            .get(name)
            .ok_or_else(|| self.unresolved(location, reference))
    }

    fn ref_name<'r>(&self, location: &str, reference: &'r str) -> Result<&'r str, IngestError> {
        reference
            .strip_prefix("#/components/schemas/")
            .ok_or_else(|| self.unresolved(location, reference))
    }

    fn unresolved(&self, location: &str, reference: &str) -> IngestError {
        IngestError::UnresolvedRef {
            file: self.doc.file.clone(),
            location: location.to_string(),
            reference: reference.to_string(),
        }
    }

    fn unsupported(&self, location: &str, detail: &str) -> IngestError {
        IngestError::UnsupportedSchema {
            file: self.doc.file.clone(),
            location: location.to_string(),
            detail: detail.to_string(),
        }
    }
}

/// Normalize a raw spec name into identifier-safe characters (FR-1.2):
/// `$` prefixes strip entirely (Box metadata keys: `$id`, `$template`);
/// any other non-identifier character is a word boundary (the
/// `levels:append` custom-method convention → `levels_append`).
fn clean_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| *c != '$')
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The tokens of a Box `operationId` with two kinds of noise removed:
///
/// - **Opaque id handles** leaked from example URLs — a token that mixes an
///   uppercase letter with a digit (`6VMVochwUWo`) is a specific object id,
///   never a word; pure noise in a generated name.
/// - **A manager-tag echo** — the call is already `client.<manager>.<method>`,
///   so repeating the `x-box-tag` tokens is redundant. The first contiguous
///   run matching the tag is dropped (never emptying the token list).
///
/// Strip a versioned document's own `_v<version>` operationId suffix — version
/// plumbing already captured in `api_version`, never part of a name (D-191).
fn strip_version_suffix<'a>(base_id: &'a str, api_version: &str) -> &'a str {
    base_id
        .strip_suffix(&format!("_v{api_version}"))
        .unwrap_or(base_id)
}

/// The operationId reduced to the base that method names *and* type seeds
/// derive from: its `#variation` fragment (D-104) and its own `_v<version>`
/// suffix (D-191) removed, so neither leaks into a generated name.
fn base_operation_id<'a>(operation_id: &'a str, api_version: &str) -> &'a str {
    let base = operation_id
        .split_once('#')
        .map_or(operation_id, |(base, _)| base);
    strip_version_suffix(base, api_version)
}

/// No dictionary or semantic guessing, so the result is deterministic and
/// 1:1 with the spec.
fn short_op_tokens(base_id: &str, box_tag: &str) -> Vec<String> {
    let is_opaque = |t: &str| {
        t.chars().any(|c| c.is_ascii_uppercase()) && t.chars().any(|c| c.is_ascii_digit())
    };
    // Split on `_` and on `:` — the latter is Box's custom-method separator
    // (`levels:append`), so the action verb becomes its own token.
    let mut tokens: Vec<String> = base_id
        .split(['_', ':'])
        .filter(|t| !t.is_empty() && !is_opaque(t))
        .map(str::to_string)
        .collect();

    let tag_tokens: Vec<&str> = box_tag.split('_').filter(|t| !t.is_empty()).collect();
    if !tag_tokens.is_empty()
        && tokens.len() > tag_tokens.len()
        && let Some(pos) = tokens.windows(tag_tokens.len()).position(|w| {
            w.len() == tag_tokens.len()
                && w.iter()
                    .zip(&tag_tokens)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
    {
        tokens.drain(pos..pos + tag_tokens.len());
    }
    if tokens.is_empty() {
        // The whole id was the tag/opaque — fall back to the raw id.
        return base_id
            .split(['_', ':'])
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
    }
    // Legacy "stored as a fixed metadata template" endpoints (D-189): Box keeps
    // file classifications and skills cards under a hardcoded
    // `.../metadata/{enterprise|global}/{template}` path, so the whole path is a
    // storage mechanism the manager tag (`file_classifications`, `skills`)
    // already names — pure noise. Reduce to the verb (+ any trailing action), so
    // it reads `client.fileClassifications.get(id)`, not the pathy
    // `getFileMetadataEnterpriseSecurityClassification…`. (The generic
    // `file_metadata` endpoint takes `{scope}` as a *parameter*, so its tokens
    // carry no `enterprise`/`global` literal and are untouched.)
    if tokens.iter().any(|t| t == "metadata")
        && tokens.iter().any(|t| t == "enterprise" || t == "global")
    {
        const HTTP: [&str; 7] = ["get", "post", "put", "patch", "delete", "options", "head"];
        let verb = tokens
            .first()
            .filter(|t| HTTP.contains(&t.as_str()))
            .cloned();
        let action = tokens
            .last()
            .filter(|t| ACTION_VERBS.contains(&t.as_str()))
            .cloned();
        let reduced: Vec<String> = verb.into_iter().chain(action).collect();
        if !reduced.is_empty() {
            tokens = reduced;
        }
    }
    // Every resource segment that is part of the *path* to an instance is
    // singularized (`createFoldersMetadataById` → `createFolderMetadataById`,
    // `commitFilesUploadSession…` → `…FileUploadSession…`). Two stay plural: the
    // last token (a true trailing collection — `getFolderItems`), and the object
    // of a trailing action verb (`levels:append` → `appendLevels`, since you
    // append *many* levels — it's the object, not a path container).
    let last = tokens.len().saturating_sub(1);
    for i in 0..last {
        if !ACTION_VERBS.contains(&tokens[i + 1].as_str()) {
            tokens[i] = singularize(&tokens[i]);
        }
    }
    tokens
}

/// The request-body type name for an operation (D-189): its distinctive path
/// tokens + a verb, falling back to the singular manager subject when there is
/// no distinctive path (`FolderCreateRequest`). `keep_id` retains the `{id}`
/// selector (`FileIdContentCreateRequest`), used only to disambiguate an
/// id-addressed body from an id-less sibling of the same name.
fn body_seed_for(box_tag: &str, method: &str, op_tokens: &[String], keep_id: bool) -> String {
    let verb = match method {
        "post" => "Create",
        "put" | "patch" => "Update",
        "delete" => "Delete",
        _ => "",
    };
    let tokens: Vec<&str> = op_tokens
        .iter()
        .map(String::as_str)
        .filter(|t| !matches!(*t, "get" | "post" | "put" | "patch" | "delete"))
        .filter(|t| keep_id || !t.eq_ignore_ascii_case("id"))
        .collect();
    if tokens.is_empty() {
        let mut tag_tokens: Vec<String> = box_tag.split('_').map(str::to_string).collect();
        if let Some(last) = tag_tokens.last_mut() {
            *last = singularize(last);
        }
        format!("{}{verb}Request", pascal(&tag_tokens.join("_")))
    } else if ACTION_VERBS.contains(tokens.last().unwrap()) {
        // A trailing curated action (`commit`, `copy`) is the semantic verb. If
        // no resource tokens precede it the subject was the manager tag (stripped
        // as an echo), so prepend it: `copy` under `files` → `FileCopyRequest`,
        // not a bare `CopyRequest` that collides across managers.
        if tokens.len() == 1 {
            let mut tag_tokens: Vec<String> = box_tag.split('_').map(str::to_string).collect();
            if let Some(last) = tag_tokens.last_mut() {
                *last = singularize(last);
            }
            format!(
                "{}{}Request",
                pascal(&tag_tokens.join("_")),
                pascal(tokens[0])
            )
        } else {
            format!("{}Request", pascal(&tokens.join("_")))
        }
    } else {
        format!("{}{verb}Request", pascal(&tokens.join("_")))
    }
}

/// Singularize a resource token that addresses one instance. Box resource names
/// are regular English plurals, so a small rule set covers them; anything
/// already singular (or not matching) passes through unchanged.
fn singularize(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    let cut = |n: usize| token[..token.len() - n].to_string();
    if lower.len() > 3 && lower.ends_with("ies") {
        format!("{}y", cut(3)) // policies → policy, taxonomies → taxonomy
    } else if lower.ends_with("ches") || lower.ends_with("shes") || lower.ends_with("sses") {
        cut(2) // batches → batch, statuses → status
    } else if lower.ends_with('s')
        && !lower.ends_with("ss")
        && !lower.ends_with("us")
        && lower != "s"
    {
        cut(1) // folders → folder, files → file, users → user
    } else {
        token.to_string()
    }
}

/// Box custom-action verbs that appear as the trailing token of an
/// operationId (a `/…/{id}/copy` endpoint or the `:append` custom-method
/// convention). When one trails, it *is* the verb — it leads the method name
/// and the HTTP verb drops — so `post_files_id_copy`→`CopyById`,
/// `post_ai_ask`→`Ask`, `post_metadata_taxonomies_…_levels:append`→
/// `AppendLevels`. Curated from the real spec (D-126), not guessed, since no
/// rule distinguishes a verb-action from a noun-subresource.
const ACTION_VERBS: &[&str] = &[
    "append",
    "apply",
    "ask",
    "authorize",
    "cancel",
    "commit",
    "convert",
    "copy",
    "extract",
    "resend",
    "revoke",
    "start",
    "trim",
];

/// A short, Box-SDK-flavoured method name from the noise-stripped tokens
/// (D-126): a leading HTTP verb maps to a semantic one (`get`→`get`,
/// `post`→`create`, `put`/`patch`→`update`, `delete`→`delete`) unless a
/// curated [`ACTION_VERBS`] token trails (then that action leads instead); a
/// trailing path id becomes `by_id`, and interior ids (parent-path context)
/// drop. So `get_files_id`→`GetById`, `post_folders`→`Create`,
/// `get_folders_id_items`→`GetItems`, `post_files_id_copy`→`CopyById`.
///
/// `keep_all_ids` renders *every* id as `by_id` instead of dropping interior
/// ones — the collision fallback for multi-`{id}` paths (`…_id_id` →
/// `GetByIdById`), so one- and two-id endpoints stay distinct.
/// A curated method name for endpoints whose operationId tokens derive an
/// awkward or colliding name (D-194). Keyed by the version-stripped
/// operationId, so it applies across every spec version.
///
/// - The two `/files/content` uploads both reduce to `createFileContent` (the
///   version endpoint's interior `{file_id}` drops as parent-path context), so
///   one loses the dedup race to a meaningless `CreateFileContent2`. Box's
///   summaries name them "Upload file" / "Upload file version".
/// - `PUT /users/{id}/folders/0` ("Transfer owned folders") leaks Box's literal
///   root-folder id `0` into `updateUserFolder0`.
///
/// Returns a camelCase seed; each backend cases it (`UploadFile`,
/// `upload_file`, …). Curated names still flow through the dedup guard below.
fn curated_method_name(base_id: &str) -> Option<&'static str> {
    match base_id {
        "post_files_content" => Some("uploadFile"),
        "post_files_id_content" => Some("uploadFileVersion"),
        "put_users_id_folders_0" => Some("transferFolders"),
        // "Query files/folders by metadata" — `execute_read` is Box's endpoint
        // plumbing, not user intent.
        "post_metadata_queries_execute_read" => Some("queryByMetadata"),
        _ => None,
    }
}

fn method_name(short_tokens: &[String], keep_all_ids: bool, drop_trailing_id: bool) -> String {
    const HTTP_VERBS: &[&str] = &["get", "post", "put", "patch", "delete", "options", "head"];

    // Strip a leading HTTP verb, remembering its semantic mapping.
    let (http_verb, rest): (Option<&str>, &[String]) = match short_tokens.first() {
        Some(first) if HTTP_VERBS.contains(&first.as_str()) => {
            let mapped = match first.as_str() {
                "post" => "create",
                "put" | "patch" => "update",
                verb => verb,
            };
            (Some(mapped), &short_tokens[1..])
        }
        _ => (None, short_tokens),
    };

    // A trailing curated action verb leads the name (dropping the HTTP verb);
    // otherwise the mapped HTTP verb leads.
    let (verb, body): (Option<&str>, &[String]) = match rest.last() {
        Some(last) if ACTION_VERBS.contains(&last.as_str()) => {
            (Some(last.as_str()), &rest[..rest.len() - 1])
        }
        _ => (http_verb, rest),
    };

    let mut out: Vec<String> = Vec::new();
    if let Some(verb) = verb {
        out.push(verb.to_string());
    }
    for (index, token) in body.iter().enumerate() {
        if token == "id" {
            // A trailing id targets a specific resource (`ById`); an interior
            // id is just parent-path context and drops — unless a collision
            // forced `keep_all_ids`, which keeps each id to stay distinct. The
            // trailing `ById` is itself usually redundant (the id is a required
            // arg), so `drop_trailing_id` omits it too; it's re-added only when a
            // terser name would collide with a sibling (the dedup fallback).
            let is_trailing = index == body.len() - 1;
            if keep_all_ids || (is_trailing && !drop_trailing_id) {
                out.push("by".to_string());
                out.push("id".to_string());
            }
        } else {
            out.push(token.clone());
        }
    }
    if out.is_empty() {
        out.push("call".to_string());
    }
    out.join("_")
}

/// `Owner` + PascalCase(property): `File` + `shared_link` → `FileSharedLink`.
fn synth_name(owner: &str, property: &str) -> String {
    let mut name = String::from(owner);
    name.push_str(&pascal(&clean_name(property)));
    name
}

/// PascalCase from snake/kebab case: `get_files_id` → `GetFilesId`.
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

fn is_object(raw: &RawSchema) -> bool {
    raw.schema_type.as_deref() == Some("object") || !raw.properties.is_empty()
}

/// An `allOf` part that contributes no structure — only annotations such
/// as `description`, `example`, or `nullable` (fields this model doesn't
/// even deserialize, plus `nullable`, which `effective_nullable` reads).
fn is_annotation_only(part: &RawSchema) -> bool {
    part.reference.is_none()
        && part.schema_type.is_none()
        && part.properties.is_empty()
        && part.required.is_empty()
        && part.items.is_none()
        && part.additional_properties.is_none()
        && part.one_of.is_empty()
        && part.all_of.is_empty()
        && part.any_of.is_empty()
        && part.enumeration.is_none()
}

/// Wrap in `Nullable` unless already wrapped (an enum-null marker and a
/// `nullable: true` on the same property must not double-wrap).
fn nullable(ty: ir::Type) -> ir::Type {
    if matches!(ty, ir::Type::Nullable(_)) {
        ty
    } else {
        ir::Type::Nullable(Box::new(ty))
    }
}

/// A property is nullable if it says so directly or through an
/// annotation-only `allOf` part (the wrapper idiom carries `nullable` in
/// the annotation part at 8 real-spec sites).
fn effective_nullable(raw: &RawSchema) -> bool {
    raw.nullable
        || raw
            .all_of
            .iter()
            .any(|part| is_annotation_only(part) && part.nullable)
}
