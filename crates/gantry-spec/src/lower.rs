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
use crate::overrides::NameOverrides;
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
    /// `(location, name)` for every declaration this run gave a name to —
    /// both predeclared top-level components and inline-synthesized shapes
    /// — in lowering order, before the cross-version merge. `gantry names`
    /// reads this to enumerate override candidates with their exact
    /// override key; nothing else in the engine depends on it, so it's
    /// deliberately the raw per-document record, not reconciled against
    /// `program`'s post-merge decls.
    pub synthesis_log: Vec<(String, String)>,
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

/// Lower every document of the set into one typed [`ir::Program`], with no
/// name overrides — the common case. Thin wrapper over
/// [`lower_with_overrides`].
pub fn lower(set: &SpecSet) -> Result<Lowering, IngestError> {
    lower_with_overrides(set, &NameOverrides::empty())
}

/// Lower every document of the set into one typed [`ir::Program`], applying
/// `overrides` to synthesized names as they're minted (see
/// [`crate::NameOverrides`]).
///
/// Declarations keep spec order per document, lowered into per-version modules
/// so nothing collides mid-lowering; the version merge (D-190) then collapses
/// them into one `schemas` namespace, superset-merging same-named types.
pub fn lower_with_overrides(
    set: &SpecSet,
    overrides: &NameOverrides,
) -> Result<Lowering, IngestError> {
    let mut arena: Vec<Option<ir::Decl>> = Vec::new();
    let mut operations: Vec<ir::Operation> = Vec::new();
    let mut stats = LoweringStats::default();
    let mut synthesis_log: Vec<(String, String)> = Vec::new();
    let mut consulted_components: HashSet<String> = HashSet::new();
    let mut consulted_locations: HashSet<String> = HashSet::new();
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
            overrides,
            synthesis_log: &mut synthesis_log,
            consulted_components: &mut consulted_components,
            consulted_locations: &mut consulted_locations,
        }
        .lower_document()?;
    }
    // Loud, not silent (NF-1): an override key that never matched anything
    // — a typo, or the spec changed shape since the override was written —
    // looks identical to a working override until someone goes looking for
    // the shorter name and it isn't there. Checked once, after every
    // document has had a chance to consult it.
    if !overrides.is_empty() {
        for key in overrides.components.keys() {
            if !consulted_components.contains(key) {
                return Err(IngestError::UnusedOverride {
                    kind: "component",
                    key: key.clone(),
                });
            }
        }
        for key in overrides.locations.keys() {
            if !consulted_locations.contains(key) {
                return Err(IngestError::UnusedOverride {
                    kind: "location",
                    key: key.clone(),
                });
            }
        }
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
    Ok(Lowering {
        program,
        stats,
        synthesis_log,
    })
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
    /// Human-supplied replacements for names too long to be comfortable,
    /// but not wrong (see [`crate::NameOverrides`]).
    overrides: &'a NameOverrides,
    /// `(location, name)` for every declaration this document gives a name
    /// to, in lowering order — shared across all documents in the run so
    /// `Lowering::synthesis_log` ends up complete. See `Lowering`'s field
    /// doc for why it's pre-merge.
    synthesis_log: &'a mut Vec<(String, String)>,
    /// Which `overrides.components`/`overrides.locations` keys have
    /// actually been consulted, shared across all documents — the other
    /// half of the unused-override check in `lower_with_overrides`.
    consulted_components: &'a mut HashSet<String>,
    consulted_locations: &'a mut HashSet<String>,
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
            // A component-name override reseeds every field synthesized
            // under this schema too: `lower_named` derives its children's
            // naming seed purely from `display_name`, never from `name`
            // itself, so overriding the display name here cascades for
            // free. `name` (the spec's own key) stays untouched in
            // `self.ids` — `$ref`s resolve against it regardless of what
            // this schema is displayed as.
            let display_name = match self.overrides.component(name) {
                Some(overridden) => {
                    self.consulted_components.insert(name.clone());
                    if self.used_names.contains(overridden) {
                        return Err(IngestError::OverrideCollision {
                            kind: "component",
                            key: name.clone(),
                            value: overridden.to_string(),
                        });
                    }
                    self.used_names.insert(overridden.to_string());
                    overridden.to_string()
                }
                None => name.clone(),
            };
            let kind = self.lower_named(&location, &display_name, raw)?;
            let decl = self.decl(&location, &display_name, kind)?;
            self.synthesis_log.push((location, display_name));
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
                let raw_id = op.operation_id.as_deref().unwrap_or("");
                let base_id = base_operation_id(raw_id, &doc.api_version);
                let tokens = short_op_tokens(base_id, tag);
                if !tokens.iter().any(|t| t.eq_ignore_ascii_case("id")) {
                    // Curation wins here too, so the collision set reflects the
                    // seeds bodies actually take (D-194). The `#variation` fold
                    // must match `lower_request_body`, or an id-less sibling's
                    // recorded seed would drift from the one it takes.
                    let variation = variation_seed_tokens(raw_id, &doc.api_version);
                    let seed = curated_body_seed(base_id)
                        .map(str::to_string)
                        .unwrap_or_else(|| body_seed_for(tag, method, &tokens, false, &variation));
                    self.idless_body_seeds.insert(seed);
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
        // The bare names `Request`/`Response` are reserved for the hand-written
        // runtime HTTP envelope types (`runtime::Request` / `runtime::Response`).
        // A synthesized or component declaration that claims one would shadow the
        // runtime type in the generated namespace — a silent collision the caller
        // never intends. A body seed always leads with a verb (`CreateFileRequest`)
        // and a response seed with its owner, so this only trips on a degenerate
        // empty subject/owner or a component schema literally named
        // `Request`/`Response`; fail loudly (naming the file and JSON path) rather
        // than emit a colliding SDK.
        if matches!(normalized.as_str(), "Request" | "Response") {
            return Err(self.unsupported(
                location,
                &format!(
                    "declaration name {normalized:?} is reserved for the runtime HTTP \
                     envelope; this shape needs a distinct name"
                ),
            ));
        }
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
        let one_or_any = match (raw.one_of.is_empty(), raw.any_of.is_empty()) {
            (false, true) => Some(("oneOf", &raw.one_of)),
            (true, false) => Some(("anyOf", &raw.any_of)),
            _ => None,
        };
        // A *named* schema can itself be the nullable-`$ref` idiom (see
        // `lower_type`) — e.g. a component schema defined as nothing but a
        // nullable reference to another component. Same recognition,
        // before `lower_union` ever sees it.
        if let Some((combinator, variants)) = one_or_any
            && let Some((index, real)) = nullable_ref_variant(variants)
        {
            let ty = self.lower_type(
                &format!("{location}.{combinator}[{index}]"),
                name,
                &leaf,
                &leaf,
                real,
            )?;
            self.stats.aliases += 1;
            return Ok(ir::DeclKind::Alias(nullable(ty)));
        }
        if !raw.one_of.is_empty() || !raw.any_of.is_empty() {
            return self
                .lower_union(location, &leaf, &leaf, &leaf, raw)
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
                .lower_struct(location, name, &leaf, &leaf, raw)
                .map(ir::DeclKind::Struct);
        }
        // A named primitive (or a schema that says nothing).
        let ty = self.lower_type(location, &leaf, &leaf, &leaf, raw)?;
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
    /// level instead of accumulating the whole path). `ancestor` is a second,
    /// *accumulating* prefix (never reset per level) that `synthesize` falls
    /// back to — instead of a bare numeral — when two unrelated schemas'
    /// immediate-context names coincide (D-127 collision, not a structural
    /// dupe): real structure the short name dropped, not an invented word.
    /// An array item or map value shares its container's `(name, leaf,
    /// ancestor)` — it is the same position.
    fn lower_type(
        &mut self,
        location: &str,
        name: &str,
        leaf: &str,
        ancestor: &str,
        raw: &'a RawSchema,
    ) -> Result<ir::Type, IngestError> {
        if let Some(reference) = &raw.reference {
            return Ok(ir::Type::Decl(self.resolve_ref(location, reference)?));
        }
        // OpenAPI 3.0 has no way to write `nullable: true` alongside a
        // sibling `$ref` (siblings of `$ref` are ignored), so specs express
        // "nullable reference to X" as `oneOf: [ {$ref: X}, <null-only
        // schema> ]` instead — a two-variant union where nothing distinguishes
        // the variants (no `type` discriminator on either side), so
        // `lower_union` finds it undiscriminated and falls back to a
        // structural union. Every backend collapses a structural union with
        // no shared shape to an opaque JSON blob — silently, since it isn't
        // an unsupported shape, just an unhelpful one. Recognizing the idiom
        // here, before `lower_union` ever sees it, keeps the real referenced
        // type instead of discarding it into `serde_json::Value` (and, as a
        // side effect, stops the null-only variant's synthesized name from
        // colliding with the referenced schema's own name — the source of
        // the `EnterpriseConfigurationSecurity2`-style suffixes).
        let one_or_any = match (raw.one_of.is_empty(), raw.any_of.is_empty()) {
            (false, true) => Some(("oneOf", &raw.one_of)),
            (true, false) => Some(("anyOf", &raw.any_of)),
            _ => None,
        };
        if let Some((combinator, variants)) = one_or_any
            && let Some((index, real)) = nullable_ref_variant(variants)
        {
            let ty = self.lower_type(
                &format!("{location}.{combinator}[{index}]"),
                name,
                leaf,
                ancestor,
                real,
            )?;
            return Ok(nullable(ty));
        }
        if !raw.one_of.is_empty() || !raw.any_of.is_empty() {
            let union = self.lower_union(location, name, leaf, ancestor, raw)?;
            let id = self.synthesize(location, name, ancestor, ir::DeclKind::Union(union))?;
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
            let id = self.synthesize(location, name, ancestor, ir::DeclKind::Enum(decl))?;
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
                return self.lower_type(&format!("{location}.allOf"), name, leaf, ancestor, part);
            }
            if structural.is_empty() {
                self.stats.json_value_sites += 1;
                return Ok(ir::Type::JsonValue);
            }
        }
        if !raw.all_of.is_empty() {
            let decl = self.lower_struct(location, name, leaf, ancestor, raw)?;
            let id = self.synthesize(location, name, ancestor, ir::DeclKind::Struct(decl))?;
            return Ok(ir::Type::Decl(id));
        }
        match raw.schema_type.as_deref() {
            Some("array") => {
                let Some(items) = &raw.items else {
                    return Err(self.unsupported(location, "array schema has no `items`"));
                };
                let inner =
                    self.lower_type(&format!("{location}.items"), name, leaf, ancestor, items)?;
                Ok(ir::Type::List(Box::new(inner)))
            }
            Some("object") if !raw.properties.is_empty() => {
                let decl = self.lower_struct(location, name, leaf, ancestor, raw)?;
                let id = self.synthesize(location, name, ancestor, ir::DeclKind::Struct(decl))?;
                Ok(ir::Type::Decl(id))
            }
            Some("object") => match raw.additional_properties.as_ref() {
                Some(RawAdditionalProperties::Schema(value)) => {
                    let inner = self.lower_type(
                        &format!("{location}.additionalProperties"),
                        name,
                        leaf,
                        ancestor,
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
                let decl = self.lower_struct(location, name, leaf, ancestor, raw)?;
                let id = self.synthesize(location, name, ancestor, ir::DeclKind::Struct(decl))?;
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
    /// own children (immediate-context naming — see `lower_type`). `ancestor`
    /// is the same idea one layer up: unlike `leaf`, it is never reset — each
    /// field appends to it — so it always names the schema's true lineage,
    /// for `synthesize` to fall back on when the short `leaf`-based name
    /// collides with an unrelated schema's.
    fn lower_struct(
        &mut self,
        location: &str,
        name: &str,
        leaf: &str,
        ancestor: &str,
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
            let child_ancestor = synth_name(ancestor, &wire_name);
            let mut ty = self.lower_type(
                &prop_location,
                &child_name,
                &child_leaf,
                &child_ancestor,
                prop,
            )?;
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
        // Named `properties` alongside a non-`false`, non-absent
        // `additionalProperties` is the OpenAPI "typed fields + open
        // extension" idiom (D-196): an open bag of extra keys beyond the
        // named ones. `lower_type`'s properties-empty branch already
        // handles a *pure* additionalProperties map (D-127); this is the
        // same idea, alongside named fields instead of in place of them.
        let extra = match &raw.additional_properties {
            None | Some(RawAdditionalProperties::Bool(false)) => None,
            Some(RawAdditionalProperties::Bool(true)) => {
                self.stats.json_value_sites += 1;
                Some(ir::Type::JsonValue)
            }
            Some(RawAdditionalProperties::Schema(schema)) => Some(self.lower_type(
                &format!("{location}.additionalProperties"),
                name,
                leaf,
                ancestor,
                schema,
            )?),
        };
        Ok(ir::StructDecl { fields, extra })
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
        ancestor: &str,
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
                let variant_ancestor = format!("{ancestor}Variant{index}");
                let ty = self.lower_type(
                    &variant_location,
                    &variant_name,
                    &variant_leaf,
                    &variant_ancestor,
                    variant,
                )?;
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
    ///
    /// `ancestor` is the schema's full, never-reset lineage (see
    /// `lower_struct`) — real structure the short `base` name dropped by
    /// design. When `base` is already taken by an unrelated schema (D-127: a
    /// genuine name coincidence, not a structural dupe — that's caught above),
    /// retry with `ancestor` before falling back to a numeral. `ancestor`
    /// equals `base` exactly at the shallowest synthesis depth (a component's
    /// own direct field, or a top-level request/response body) — there is no
    /// deeper lineage to fall back to, so a numeral is the honest name there.
    fn synthesize(
        &mut self,
        location: &str,
        base: &str,
        ancestor: &str,
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
        // A location override — checked before the base/ancestor/numeral
        // fallback below runs at all — always wins outright, same as
        // `curated_method_name`/`curated_body_seed` win over their derived
        // names. It's checked *after* structural dedupe (D-127) on purpose:
        // a location whose shape is identical to an earlier one never mints
        // its own decl to begin with, so an override on it would be
        // unreachable — the "unused override" check below catches that
        // honestly rather than silently accepting a key that can't apply.
        let name = match self.overrides.location(location) {
            Some(overridden) => {
                self.consulted_locations.insert(location.to_string());
                if self.used_names.contains(overridden) {
                    return Err(IngestError::OverrideCollision {
                        kind: "location",
                        key: location.to_string(),
                        value: overridden.to_string(),
                    });
                }
                overridden.to_string()
            }
            None => {
                let mut name = base.to_string();
                if self.used_names.contains(&name)
                    && ancestor != base
                    && !self.used_names.contains(ancestor)
                {
                    name = ancestor.to_string();
                }
                let mut suffix = 2;
                while self.used_names.contains(&name) {
                    name = format!("{base}{suffix}");
                    suffix += 1;
                }
                name
            }
        };
        self.used_names.insert(name.clone());
        self.synthesis_log
            .push((location.to_string(), name.clone()));
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
            // Collapse consecutive duplicate tokens before seeding a type name:
            // a two-`{id}` path (`.../taxonomies/{id}/{id}`) encodes both
            // selectors as `id`, so the raw seed reads `…IdId…`. The doubled
            // selector carries no meaning in a *type* name (method-name
            // disambiguation keeps its own full token list), so drop the repeat.
            let mut seed_tokens: Vec<&str> = Vec::with_capacity(short_tokens.len());
            for token in &short_tokens {
                if seed_tokens
                    .last()
                    .is_none_or(|prev| !prev.eq_ignore_ascii_case(token))
                {
                    seed_tokens.push(token);
                }
            }
            let mut owner = pascal(&seed_tokens.join("_"));
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
            let parameter_seed = synth_name(owner, wire_name);
            let mut ty = self.lower_type(
                &format!("{param_location}.schema"),
                &parameter_seed,
                &pascal(&clean_name(wire_name)),
                &parameter_seed,
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
        // (D-189). The verb leads, then the operation's *distinctive* path tokens
        // (`CommitFileUploadSessionRequest`), falling back to the singular manager
        // subject when the operation has no distinctive path (`CreateFolderRequest`).
        // A trailing curated action (`commit`) is the verb; otherwise the HTTP verb
        // supplies it.
        let base_id = base_operation_id(
            op.operation_id.as_deref().unwrap_or(""),
            &self.doc.api_version,
        );
        let all = short_op_tokens(base_id, box_tag);
        // A `#variation` fragment (D-104) splits one operationId into distinct
        // operations that share a base but differ in body shape
        // (`put_files_id#add_shared_link` vs `#update_shared_link`). Left alone,
        // both seed `UpdateFileRequest` and the structural dedupe falls back to a
        // meaningless numeric suffix (`UpdateFileRequest2`). Fold the variation
        // into the subject — as `owner` already does — so each body takes a
        // distinct, meaningful name (`UpdateFileAddSharedLinkRequest`).
        let variation = variation_seed_tokens(
            op.operation_id.as_deref().unwrap_or(""),
            &self.doc.api_version,
        );
        // A curated seed for the few bodies whose path tokens derive an awkward or
        // colliding name (D-194). Otherwise: an id-addressed body keeps its `{id}`
        // selector only when an id-less sibling claims the same terse name
        // (`CreateFileContentRequest` + `CreateFileIdContentRequest`); otherwise it
        // takes the clean terse name (`UpdateFileRequest`, no redundant `Id`). The
        // id-less seeds are gathered in a pre-pass (`gather_idless_body_seeds`), so
        // this is order-independent.
        let body_seed = match curated_body_seed(base_id) {
            Some(seed) => seed.to_string(),
            None => {
                let has_id = all.iter().any(|t| t.eq_ignore_ascii_case("id"));
                let terse_seed = body_seed_for(box_tag, method, &all, false, &variation);
                if has_id && self.idless_body_seeds.contains(&terse_seed) {
                    body_seed_for(box_tag, method, &all, true, &variation)
                } else {
                    terse_seed
                }
            }
        };
        let mut ty = match &media.schema {
            Some(schema) => self.lower_type(
                &format!("{body_location}.content[{media_key:?}]"),
                &body_seed,
                owner,
                &body_seed,
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
                                &format!("{owner}Response"),
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

    // Box's spec is internally inconsistent: the `collaboration_allowlist_*`
    // tags and component schemas name the resource `allowlist`, but the matching
    // operationIds and paths still say `whitelist`. Normalize the path
    // vocabulary to the tag's so synthesized type names read the same
    // `CollaborationAllowlist…` as their manager (never `…Whitelist…`) — and so
    // the tag-echo strip below actually matches instead of being defeated by the
    // mismatched word.
    for t in &mut tokens {
        if t.eq_ignore_ascii_case("whitelist") {
            "allowlist".clone_into(t);
        }
    }

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

/// The request-body type name for an operation (D-189): the verb leads, then the
/// distinctive path tokens (`CreateFolderRequest`, `CreateFileIdContentRequest`),
/// falling back to the singular manager subject when there is no distinctive
/// path. The verb-first `{Verb}{Subject}Request` form reads as an imperative and
/// keeps the payload types grouped by action; a trailing curated action
/// (`copy`, `commit`) supplies the verb instead of the HTTP method
/// (`CopyFileRequest`). `keep_id` retains the `{id}` selector
/// (`CreateFileIdContentRequest`), used only to disambiguate an id-addressed body
/// from an id-less sibling of the same name. `variation` carries the
/// `#variation` fragment's words (D-104), folded into the subject so operations
/// sharing a base seed take distinct names rather than a structural dedupe suffix
/// (`UpdateFileAddSharedLinkRequest`, not `UpdateFileRequest2`).
fn body_seed_for(
    box_tag: &str,
    method: &str,
    op_tokens: &[String],
    keep_id: bool,
    variation: &[String],
) -> String {
    let http_verb = match method {
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
    // The subject: resource words plus the `#variation` words (D-104), PascalCase.
    let subject_of = |words: &[&str]| -> String {
        let mut all: Vec<&str> = words.to_vec();
        all.extend(variation.iter().map(String::as_str));
        pascal(&all.join("_"))
    };
    // The singular manager subject, used when the path reduced to nothing (or to
    // just an action verb, whose object was the tag echo, stripped).
    let tag_subject = || -> Vec<String> {
        let mut t: Vec<String> = box_tag.split('_').map(str::to_string).collect();
        if let Some(last) = t.last_mut() {
            *last = singularize(last);
        }
        t
    };
    // The name leads with its verb (verb-noun): the HTTP verb
    // (`Create`/`Update`/`Delete`), or a trailing curated action (`copy`,
    // `commit`) that replaces it — `copy` under `files` → `CopyFileRequest`, not a
    // bare `CopyRequest` that collides across managers.
    let (verb, subject) = if tokens.is_empty() {
        let tag = tag_subject();
        let refs: Vec<&str> = tag.iter().map(String::as_str).collect();
        (http_verb.to_string(), subject_of(&refs))
    } else if ACTION_VERBS.contains(tokens.last().unwrap()) {
        let action = pascal(tokens.last().unwrap());
        if tokens.len() == 1 {
            let tag = tag_subject();
            let refs: Vec<&str> = tag.iter().map(String::as_str).collect();
            (action, subject_of(&refs))
        } else {
            (action, subject_of(&tokens[..tokens.len() - 1]))
        }
    } else {
        (http_verb.to_string(), subject_of(&tokens))
    };
    format!("{verb}{subject}Request")
}

/// The `#variation` fragment (D-104) of an operationId as word tokens, or empty
/// when there is none. Folded into a request-body seed so the distinct
/// operations one base operationId splits into take distinct body names instead
/// of a meaningless structural-dedupe suffix. The fragment's own `_v<version>`
/// suffix is stripped first — it is version plumbing, never part of a name.
fn variation_seed_tokens(operation_id: &str, api_version: &str) -> Vec<String> {
    let Some((_, variation)) = operation_id.split_once('#') else {
        return Vec::new();
    };
    clean_name(strip_version_suffix(variation, api_version))
        .split('_')
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
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
        cut(2) // batches → batch, classes → class, addresses → address
    } else if lower.len() > 4
        && lower.ends_with("uses")
        && !matches!(
            lower.as_bytes()[lower.len() - 5],
            b'a' | b'e' | b'i' | b'o' | b'u'
        )
    {
        // A singular already ending in `-us` pluralizes with `-es`
        // (`status` → `statuses`, `bus` → `buses`). A consonant before `uses`
        // marks that case; a vowel there means a `-use` word (`houses` →
        // `house`), which the `-s` branch below strips correctly.
        cut(2) // statuses → status, buses → bus, viruses → virus
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
/// - The two upload-session creators disambiguate structurally to the plural,
///   position-flavored `createFileUploadSessions` / `createFileByIdUploadSessions`
///   — accurate but awkward. Box's summaries name them "Create upload session" /
///   "Create upload session for existing file"; the singular
///   `createFileUploadSession` / `createFileVersionUploadSession` read the way the
///   chunked-upload orchestrators call them.
/// - The `/group_memberships` CRUD endpoints share the "memberships" tag with
///   the unrelated `/users/{id}/memberships` and `/groups/{id}/memberships`
///   listings (Box's manager spans all three). The tag-echo strip in
///   `short_op_tokens` removes the literal token `memberships` from
///   `post_group_memberships` and friends, leaving only `group` — so they
///   otherwise seed `createGroup`/`getGroup`/`updateGroup`/`deleteGroup`,
///   reading as if they operate on a `Group`, not the join resource. Box's
///   own summaries ("Add user to group", "Get group membership", "Update
///   group membership", "Remove user from group") confirm `GroupMembership`
///   is the real subject.
/// - `POST /query` and `POST /query_insights` default to the generic
///   `create`/`createInsights` (every bare POST seeds the `create` verb),
///   which reads as if a query resource is being persisted. Neither call
///   creates anything — they run a query. Box's summaries ("Query for Box
///   items", "Create insights for Box items" — itself imprecise) and guide
///   titles ("Box query", "Query insights") back `query`/`queryInsights`.
///
/// Returns a camelCase seed; each backend cases it (`UploadFile`,
/// `upload_file`, …). Curated names still flow through the dedup guard below.
fn curated_method_name(base_id: &str) -> Option<&'static str> {
    match base_id {
        "post_files_content" => Some("uploadFile"),
        "post_files_id_content" => Some("uploadFileVersion"),
        "put_users_id_folders_0" => Some("transferFolders"),
        "post_files_upload_sessions" => Some("createFileUploadSession"),
        "post_files_id_upload_sessions" => Some("createFileVersionUploadSession"),
        // "Query files/folders by metadata" — `execute_read` is Box's endpoint
        // plumbing, not user intent.
        "post_metadata_queries_execute_read" => Some("queryByMetadata"),
        "post_group_memberships" => Some("createGroupMembership"),
        "get_group_memberships_id" => Some("getGroupMembership"),
        "put_group_memberships_id" => Some("updateGroupMembership"),
        "delete_group_memberships_id" => Some("deleteGroupMembership"),
        "post_query" => Some("query"),
        "post_query_insights" => Some("queryInsights"),
        _ => None,
    }
}

/// A curated request-body type name for endpoints whose path tokens derive an
/// awkward or colliding seed (D-194). Keyed by the version-stripped operationId,
/// as [`curated_method_name`] is, so it applies across every spec version.
///
/// The two upload-session creators otherwise seed the plural, position-flavored
/// `CreateFileUploadSessionsRequest` / `CreateFileIdUploadSessionsRequest` — the
/// second keeps its `{id}` selector only because the first claims the terse name.
/// The singular `CreateFileUploadSessionRequest` / `CreateFileVersionUploadSessionRequest`
/// read the way the chunked-upload orchestrators construct them and line up with
/// the sibling `CommitFileUploadSessionRequest` and the curated method names.
/// (The commit body reaches its verb-noun name on its own — a trailing `commit`
/// action leads — so only the two creators need curation.)
///
/// `PUT /users/{id}/folders/0` ("Transfer owned folders") otherwise leaks Box's
/// literal root-folder id `0` into `UpdateUserFolder0Request`; the curated
/// `TransferFoldersRequest` (already verb-noun) matches its `transferFolders`
/// method.
///
/// The `/group_memberships` create/update bodies otherwise seed bare
/// `CreateGroupRequest` / `UpdateGroupRequest` (see [`curated_method_name`] for
/// why the `memberships` tag-echo strips down to just `group`) — colliding
/// with the real `Groups` manager's own create/update bodies and forcing a
/// meaningless `…2` suffix on one side. The curated `CreateGroupMembershipRequest`
/// / `UpdateGroupMembershipRequest` match the curated method names and the
/// `GroupMembership` response schema.
///
/// Returns a PascalCase seed; each backend cases it per language. A curated seed
/// bypasses the terse/`keep_id` collision dance in `lower_request_body`.
fn curated_body_seed(base_id: &str) -> Option<&'static str> {
    match base_id {
        "post_files_upload_sessions" => Some("CreateFileUploadSessionRequest"),
        "post_files_id_upload_sessions" => Some("CreateFileVersionUploadSessionRequest"),
        "put_users_id_folders_0" => Some("TransferFoldersRequest"),
        "post_group_memberships" => Some("CreateGroupMembershipRequest"),
        "put_group_memberships_id" => Some("UpdateGroupMembershipRequest"),
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
/// Collapses a duplicate word at the seam: Box occasionally names a schema
/// `…Validation` and then nests a `validation_type` field inside it, which a
/// plain concatenation would double to `…ValidationValidationType`. Same bug
/// family as the `IdId` token collapse in `lower_operation`'s owner seed,
/// generalized to whole words (`gantry_ir::naming::append_without_repeating`).
fn synth_name(owner: &str, property: &str) -> String {
    gantry_ir::naming::append_without_repeating(owner, &pascal(&clean_name(property)))
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

/// The OpenAPI 3.0 "null schema type" placeholder: an object schema that
/// describes nothing but the possibility of `null` — no properties, no
/// extra ones allowed, just `nullable: true`. Specs use it as a `oneOf`
/// sibling to fake a `nullable` `$ref`, since `$ref` can't carry sibling
/// keywords in OpenAPI 3.0.
fn is_null_only_schema(raw: &RawSchema) -> bool {
    raw.reference.is_none()
        && raw.schema_type.as_deref() == Some("object")
        && raw.nullable
        && matches!(
            raw.additional_properties,
            Some(RawAdditionalProperties::Bool(false))
        )
        && raw.properties.is_empty()
        && raw.required.is_empty()
        && raw.items.is_none()
        && raw.one_of.is_empty()
        && raw.all_of.is_empty()
        && raw.any_of.is_empty()
        && raw.enumeration.is_none()
}

/// A two-variant `oneOf`/`anyOf` where exactly one variant is the
/// [`is_null_only_schema`] placeholder is the "nullable `$ref`" idiom
/// (D-195) — the other variant is the real type, returned alongside its
/// index in `variants` so callers can build an accurate JSON path if
/// lowering that variant fails. Any other shape (more than two variants,
/// neither/both variants null-only) isn't this idiom; `None` sends it to
/// the normal union path.
fn nullable_ref_variant(variants: &[RawSchema]) -> Option<(usize, &RawSchema)> {
    let [a, b] = variants else { return None };
    match (is_null_only_schema(a), is_null_only_schema(b)) {
        (true, false) => Some((1, b)),
        (false, true) => Some((0, a)),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::{
        body_seed_for, curated_body_seed, curated_method_name, short_op_tokens,
        variation_seed_tokens,
    };

    /// The chunked-upload orchestrators (every backend) call these exact method
    /// names and construct these exact request-body types; each backend's emit
    /// gate keys off the same literals (D-194). If a curated value drifts, the
    /// gate silently returns `false` and the orchestrator vanishes rather than
    /// failing loudly — so pin the pairs here.
    #[test]
    fn curated_upload_session_names_are_stable() {
        assert_eq!(
            curated_method_name("post_files_upload_sessions"),
            Some("createFileUploadSession")
        );
        assert_eq!(
            curated_method_name("post_files_id_upload_sessions"),
            Some("createFileVersionUploadSession")
        );
        assert_eq!(
            curated_body_seed("post_files_upload_sessions"),
            Some("CreateFileUploadSessionRequest")
        );
        assert_eq!(
            curated_body_seed("post_files_id_upload_sessions"),
            Some("CreateFileVersionUploadSessionRequest")
        );
        // The commit body is not curated — a trailing `commit` action leads, so
        // `body_seed_for` reaches the gate's `CommitFileUploadSessionRequest` on
        // its own. Pin that so a change to the action-verb rule can't silently
        // drop the orchestrator.
        let commit_tokens = [
            "file".into(),
            "upload".into(),
            "session".into(),
            "commit".into(),
        ];
        assert_eq!(
            body_seed_for("chunked_uploads", "post", &commit_tokens, false, &[]),
            "CommitFileUploadSessionRequest"
        );
    }

    /// The `/group_memberships` CRUD endpoints share the `memberships` tag
    /// with the unrelated `/users/{id}/memberships` and `/groups/{id}/memberships`
    /// listings. Left uncurated, the tag-echo strip in `short_op_tokens` removes
    /// the literal token `memberships`, leaving only `group` — so
    /// `post_group_memberships`/`put_group_memberships_id` seed the same bare
    /// `CreateGroupRequest`/`UpdateGroupRequest` the unrelated `Groups` manager's
    /// own create/update bodies already claim, forcing a meaningless `…2`
    /// suffix onto one side. Pin both the reproduction (so a future tokenizer
    /// change can't silently reintroduce the collision unnoticed) and the
    /// curated fix.
    #[test]
    fn group_membership_curation_avoids_the_groups_manager_collision() {
        let uncurated_create = short_op_tokens("post_group_memberships", "memberships");
        assert_eq!(
            body_seed_for("memberships", "post", &uncurated_create, false, &[]),
            "CreateGroupRequest",
            "uncurated, this collides with the Groups manager's own CreateGroupRequest"
        );
        let uncurated_update = short_op_tokens("put_group_memberships_id", "memberships");
        assert_eq!(
            body_seed_for("memberships", "put", &uncurated_update, false, &[]),
            "UpdateGroupRequest",
            "uncurated, this collides with the Groups manager's own UpdateGroupRequest"
        );

        assert_eq!(
            curated_method_name("post_group_memberships"),
            Some("createGroupMembership")
        );
        assert_eq!(
            curated_method_name("get_group_memberships_id"),
            Some("getGroupMembership")
        );
        assert_eq!(
            curated_method_name("put_group_memberships_id"),
            Some("updateGroupMembership")
        );
        assert_eq!(
            curated_method_name("delete_group_memberships_id"),
            Some("deleteGroupMembership")
        );
        assert_eq!(
            curated_body_seed("post_group_memberships"),
            Some("CreateGroupMembershipRequest")
        );
        assert_eq!(
            curated_body_seed("put_group_memberships_id"),
            Some("UpdateGroupMembershipRequest")
        );
    }

    /// Request-body names are verb-noun (D-189): the HTTP verb leads
    /// (`CreateFolderRequest`), and a trailing curated action replaces the HTTP
    /// verb and leads itself, with the tag supplying the subject it echoed
    /// (`copy` under `files` → `CopyFileRequest`, never a bare `CopyRequest`).
    #[test]
    fn body_seed_is_verb_first() {
        let post_folders = ["post".into(), "folder".into()];
        assert_eq!(
            body_seed_for("folders", "post", &post_folders, false, &[]),
            "CreateFolderRequest"
        );

        let post_files_copy = ["post".into(), "copy".into()];
        assert_eq!(
            body_seed_for("files", "post", &post_files_copy, false, &[]),
            "CopyFileRequest"
        );
    }

    /// A `#variation` fragment splits one operationId into distinct operations
    /// that share a base seed. The variation must distinguish their bodies *by
    /// name* — folded into the subject — rather than letting the structural
    /// dedupe fall back to a meaningless `…2` suffix.
    #[test]
    fn variation_folds_into_the_body_seed() {
        let put_files_id = ["put".into(), "file".into(), "id".into()];
        // Base (no variation) keeps the terse subject name.
        assert_eq!(
            body_seed_for("files", "put", &put_files_id, false, &[]),
            "UpdateFileRequest"
        );
        // The shared-link variations that would otherwise collide take distinct,
        // meaningful names instead of `UpdateFileRequest2` / `…3`.
        assert_eq!(
            body_seed_for(
                "files",
                "put",
                &put_files_id,
                false,
                &["add".into(), "shared".into(), "link".into()]
            ),
            "UpdateFileAddSharedLinkRequest"
        );
    }

    /// When the path reduces to just the HTTP verb (the legacy metadata-template
    /// endpoints), the subject comes from the manager-tag fallback; the variation
    /// appends to that subject rather than standing in for it — never a
    /// subjectless `UpdateAddRequest`.
    #[test]
    fn variation_keeps_the_tag_subject_when_the_path_reduces_to_a_verb() {
        let put_only = ["put".into()];
        assert_eq!(
            body_seed_for("classifications", "put", &put_only, false, &["add".into()]),
            "UpdateClassificationAddRequest"
        );
    }

    /// The variation fragment is version-plumbing-free: a `_v<version>` marker on
    /// the fragment is stripped, and a plain operationId yields no tokens.
    #[test]
    fn variation_seed_tokens_strip_version_and_default_empty() {
        assert_eq!(
            variation_seed_tokens("put_files_id#add_shared_link", "2024.0"),
            ["add", "shared", "link"]
        );
        assert_eq!(
            variation_seed_tokens("put_files_id#add_shared_link_v2025.0", "2025.0"),
            ["add", "shared", "link"]
        );
        assert!(variation_seed_tokens("put_files_id", "2024.0").is_empty());
    }
}
