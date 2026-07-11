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

use std::collections::HashSet;

use gantry_ir as ir;
use indexmap::IndexMap;

use crate::error::IngestError;
use crate::ingest::{Document, SpecSet};
use crate::raw::{RawAdditionalProperties, RawSchema};

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
}

/// Lower every document of the set into one typed [`ir::Program`].
///
/// Declarations keep spec order per document; versioned documents get
/// their own module (`schemas::v2025_0`, FR-7.5) so nothing collides with
/// the base spec (G-9).
pub fn lower(set: &SpecSet) -> Result<Lowering, IngestError> {
    let mut arena: Vec<Option<ir::Decl>> = Vec::new();
    let mut stats = LoweringStats::default();
    for (index, doc) in set.documents.iter().enumerate() {
        let module = module_for(doc, index == 0)?;
        DocLowerer {
            doc,
            module,
            arena: &mut arena,
            stats: &mut stats,
            ids: IndexMap::new(),
            used_names: HashSet::new(),
        }
        .lower_document()?;
    }
    let decls = arena
        .into_iter()
        .map(|slot| slot.expect("every predeclared schema is filled by lower_document"))
        .collect();
    Ok(Lowering {
        program: ir::Program { decls },
        stats,
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
    stats: &'a mut LoweringStats,
    /// Named schema → predeclared id (so references resolve even through
    /// cycles).
    ids: IndexMap<String, ir::DeclId>,
    /// Every declaration name taken in this document's module, for
    /// deterministic synthesized-name disambiguation.
    used_names: HashSet<String>,
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
        Ok(ir::Decl {
            name: identifier(self.doc, location, name)?,
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
        if !raw.one_of.is_empty() || !raw.any_of.is_empty() {
            return self
                .lower_union(location, name, raw)
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
                .lower_struct(location, name, raw)
                .map(ir::DeclKind::Struct);
        }
        // A named primitive (or a schema that says nothing).
        let ty = self.lower_type(location, name, raw)?;
        self.stats.aliases += 1;
        Ok(ir::DeclKind::Alias(ty))
    }

    /// Lower a schema in *type position* (property, array item, map value).
    /// `owner` seeds synthesized names for inline anonymous shapes.
    fn lower_type(
        &mut self,
        location: &str,
        owner: &str,
        raw: &'a RawSchema,
    ) -> Result<ir::Type, IngestError> {
        if let Some(reference) = &raw.reference {
            return Ok(ir::Type::Decl(self.resolve_ref(location, reference)?));
        }
        if !raw.one_of.is_empty() || !raw.any_of.is_empty() {
            let union = self.lower_union(location, owner, raw)?;
            let id = self.synthesize(location, owner, ir::DeclKind::Union(union))?;
            return Ok(ir::Type::Decl(id));
        }
        if let Some(values) = &raw.enumeration {
            // Numeric "enums" (allowed-value lists) lower to their base
            // type; only string enums become declarations.
            if values.iter().all(serde_json::Value::is_number) {
                return self.primitive(location, raw);
            }
            let decl = self.lower_enum(location, values)?;
            let id = self.synthesize(location, owner, ir::DeclKind::Enum(decl))?;
            return Ok(ir::Type::Decl(id));
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
                return self.lower_type(&format!("{location}.allOf"), owner, part);
            }
            if structural.is_empty() {
                self.stats.json_value_sites += 1;
                return Ok(ir::Type::JsonValue);
            }
        }
        if !raw.all_of.is_empty() {
            let decl = self.lower_struct(location, owner, raw)?;
            let id = self.synthesize(location, owner, ir::DeclKind::Struct(decl))?;
            return Ok(ir::Type::Decl(id));
        }
        match raw.schema_type.as_deref() {
            Some("array") => {
                let Some(items) = &raw.items else {
                    return Err(self.unsupported(location, "array schema has no `items`"));
                };
                let inner = self.lower_type(&format!("{location}.items"), owner, items)?;
                Ok(ir::Type::List(Box::new(inner)))
            }
            Some("object") if !raw.properties.is_empty() => {
                let decl = self.lower_struct(location, owner, raw)?;
                let id = self.synthesize(location, owner, ir::DeclKind::Struct(decl))?;
                Ok(ir::Type::Decl(id))
            }
            Some("object") => match raw.additional_properties.as_ref() {
                Some(RawAdditionalProperties::Schema(value)) => {
                    let inner =
                        self.lower_type(&format!("{location}.additionalProperties"), owner, value)?;
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
                let decl = self.lower_struct(location, owner, raw)?;
                let id = self.synthesize(location, owner, ir::DeclKind::Struct(decl))?;
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
    fn lower_struct(
        &mut self,
        location: &str,
        owner: &str,
        raw: &'a RawSchema,
    ) -> Result<ir::StructDecl, IngestError> {
        let mut properties: IndexMap<String, &'a RawSchema> = IndexMap::new();
        let mut required: HashSet<String> = HashSet::new();
        self.collect_parts(location, raw, &mut properties, &mut required, 0)?;

        let mut fields = Vec::with_capacity(properties.len());
        for (wire_name, prop) in properties {
            let prop_location = format!("{location}.properties.{wire_name}");
            let name = identifier(self.doc, &prop_location, &wire_name)?;
            let mut ty = self.lower_type(&prop_location, &synth_name(owner, &wire_name), prop)?;
            // Optionality is structural (FR-2.3). `nullable` and
            // not-required both collapse to Optional for now; the
            // null-vs-absent tri-state is revisited before serializer work
            // (PLAN.md next steps).
            if !required.contains(&wire_name) || effective_nullable(prop) {
                ty = ir::Type::Optional(Box::new(ty));
            }
            fields.push(ir::Field {
                name,
                wire_name,
                ty,
            });
        }
        self.stats.structs += 1;
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
        owner: &str,
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
                let name = format!("{owner}Variant{index}");
                let ty = self.lower_type(&variant_location, &name, variant)?;
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
            self.stats.discriminated_unions += 1;
            Some(DISCRIMINATOR_FIELD.to_string())
        } else {
            // Half-inferred values would be misleading: a structural union
            // carries none.
            for variant in &mut variants {
                variant.discriminator_value = None;
            }
            None
        };
        self.stats.unions += 1;
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
        self.stats.enums += 1;
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
        self.stats.synthesized += 1;
        Ok(id)
    }

    fn resolve_ref(&self, location: &str, reference: &str) -> Result<ir::DeclId, IngestError> {
        let name = self.ref_name(location, reference)?;
        self.ids
            .get(name)
            .copied()
            .ok_or_else(|| self.unresolved(location, reference))
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

/// `Owner` + PascalCase(property): `File` + `shared_link` → `FileSharedLink`.
fn synth_name(owner: &str, property: &str) -> String {
    let mut name = String::from(owner);
    for part in property.split(['_', '-']) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            name.extend(first.to_uppercase());
            name.push_str(chars.as_str());
        }
    }
    name
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
