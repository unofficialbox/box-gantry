//! Spec-diff / breaking-change report (FR-9).
//!
//! Diffs two verified [`ir::Program`]s — the SDK generated from the old
//! spec set versus the new one — and classifies every difference as
//! **breaking** or **compatible**, then recommends the SDK version bump
//! (semver: breaking → major, compatible-only → minor, none → no bump).
//! It runs on the IR, not the raw OpenAPI, so it reports differences the
//! generated SDK actually exposes — a renamed field the engine normalizes
//! away is not a diff, a removed operation is.
//!
//! Cross-program type identity is by **structural signature** (a decl
//! reference renders to its qualified name, not its arena id), so the two
//! programs' independent `DeclId` spaces compare correctly.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use gantry_ir as ir;

/// Whether a change can break a consumer of the generated SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Source-incompatible for some caller: a removal, a type change, or a
    /// newly required input. Forces a major SDK bump.
    Breaking,
    /// Additive or advisory: safe for existing callers. A minor bump.
    Compatible,
}

/// The nature of a single change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

/// One classified difference between the two programs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub severity: Severity,
    /// The kind of surface that changed: `"operation"` or `"schema"`.
    pub category: &'static str,
    /// The stable key of the changed item (qualified, deterministic).
    pub name: String,
    /// A human description of what changed (empty for pure add/remove).
    pub detail: String,
}

/// The recommended SDK version bump implied by a diff (FR-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionBump {
    /// At least one breaking change.
    Major,
    /// Compatible changes only.
    Minor,
    /// No differences the SDK exposes.
    None,
}

impl VersionBump {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionBump::Major => "major",
            VersionBump::Minor => "minor",
            VersionBump::None => "none",
        }
    }
}

/// The full diff: an ordered, deterministic list of changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecDiff {
    pub changes: Vec<Change>,
}

impl SpecDiff {
    pub fn breaking(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| c.severity == Severity::Breaking)
            .count()
    }

    pub fn compatible(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| c.severity == Severity::Compatible)
            .count()
    }

    /// The recommended version bump (FR-9 feeds the SDK version).
    pub fn bump(&self) -> VersionBump {
        if self.breaking() > 0 {
            VersionBump::Major
        } else if self.changes.is_empty() {
            VersionBump::None
        } else {
            VersionBump::Minor
        }
    }

    /// A deterministic, human-readable report.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "spec-diff: {} change(s) — {} breaking, {} compatible; recommended bump: {}",
            self.changes.len(),
            self.breaking(),
            self.compatible(),
            self.bump().as_str(),
        );
        for change in &self.changes {
            let mark = match change.severity {
                Severity::Breaking => "BREAK",
                Severity::Compatible => "  ok ",
            };
            let verb = match change.kind {
                ChangeKind::Added => "added",
                ChangeKind::Removed => "removed",
                ChangeKind::Changed => "changed",
            };
            let _ = write!(out, "  {mark} {verb} {} {}", change.category, change.name);
            if !change.detail.is_empty() {
                let _ = write!(out, " — {}", change.detail);
            }
            out.push('\n');
        }
        out
    }
}

/// Diff two verified programs (old → new).
pub fn diff(old: &ir::Program, new: &ir::Program) -> SpecDiff {
    let mut changes = Vec::new();
    diff_operations(old, new, &mut changes);
    diff_decls(old, new, &mut changes);
    // Deterministic: category, then key, then kind.
    changes.sort_by(|a, b| {
        a.category
            .cmp(b.category)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| (a.kind as u8).cmp(&(b.kind as u8)))
    });
    SpecDiff { changes }
}

fn diff_operations(old: &ir::Program, new: &ir::Program, changes: &mut Vec<Change>) {
    let old_ops = index_by(&old.operations, operation_key);
    let new_ops = index_by(&new.operations, operation_key);
    for key in union_keys(&old_ops, &new_ops) {
        match (old_ops.get(&key), new_ops.get(&key)) {
            (None, Some(_)) => changes.push(Change {
                kind: ChangeKind::Added,
                severity: Severity::Compatible,
                category: "operation",
                name: key,
                detail: String::new(),
            }),
            (Some(_), None) => changes.push(Change {
                kind: ChangeKind::Removed,
                severity: Severity::Breaking,
                category: "operation",
                name: key,
                detail: String::new(),
            }),
            (Some(before), Some(after)) => {
                if let Some(change) = diff_operation(old, new, &key, before, after) {
                    changes.push(change);
                }
            }
            (None, None) => unreachable!("key came from the union"),
        }
    }
}

/// Compare one operation present in both programs; return a single
/// `Changed` summarizing every difference, at the worst severity.
fn diff_operation(
    old: &ir::Program,
    new: &ir::Program,
    key: &str,
    before: &ir::Operation,
    after: &ir::Operation,
) -> Option<Change> {
    let mut notes: Vec<(Severity, String)> = Vec::new();

    // The request line: a method, host-class, or path change moves the URL.
    if before.method != after.method {
        notes.push((
            Severity::Breaking,
            format!("method {:?} → {:?}", before.method, after.method),
        ));
    }
    if before.base_url != after.base_url {
        notes.push((
            Severity::Breaking,
            format!("base URL {:?} → {:?}", before.base_url, after.base_url),
        ));
    }
    if path_sig(&before.path) != path_sig(&after.path) {
        notes.push((
            Severity::Breaking,
            format!(
                "path {} → {}",
                path_sig(&before.path),
                path_sig(&after.path)
            ),
        ));
    }

    diff_params(old, new, before, after, &mut notes);

    // Request body.
    let (rb, ra) = (
        before.request.as_ref().map(|r| request_sig(old, r)),
        after.request.as_ref().map(|r| request_sig(new, r)),
    );
    if rb != ra {
        notes.push((
            Severity::Breaking,
            format!(
                "request body {} → {}",
                rb.as_deref().unwrap_or("none"),
                ra.as_deref().unwrap_or("none"),
            ),
        ));
    }

    // Response shape.
    let (respb, respa) = (
        response_sig(old, &before.response),
        response_sig(new, &after.response),
    );
    if respb != respa {
        notes.push((Severity::Breaking, format!("response {respb} → {respa}")));
    }

    // Deprecation is advisory, never breaking.
    if !before.deprecated && after.deprecated {
        notes.push((Severity::Compatible, "now deprecated".to_string()));
    } else if before.deprecated && !after.deprecated {
        notes.push((Severity::Compatible, "no longer deprecated".to_string()));
    }

    if notes.is_empty() {
        return None;
    }
    let severity = if notes.iter().any(|(s, _)| *s == Severity::Breaking) {
        Severity::Breaking
    } else {
        Severity::Compatible
    };
    let detail = notes
        .into_iter()
        .map(|(_, note)| note)
        .collect::<Vec<_>>()
        .join("; ");
    Some(Change {
        kind: ChangeKind::Changed,
        severity,
        category: "operation",
        name: key.to_string(),
        detail,
    })
}

fn diff_params(
    old: &ir::Program,
    new: &ir::Program,
    before: &ir::Operation,
    after: &ir::Operation,
    notes: &mut Vec<(Severity, String)>,
) {
    let old_params = index_by(&before.params, param_key);
    let new_params = index_by(&after.params, param_key);
    for key in union_keys(&old_params, &new_params) {
        match (old_params.get(&key), new_params.get(&key)) {
            (None, Some(param)) => {
                // A new required (non-optional) input breaks existing callers.
                let severity = if is_optional(&param.ty) {
                    Severity::Compatible
                } else {
                    Severity::Breaking
                };
                notes.push((severity, format!("+param {key}")));
            }
            (Some(_), None) => notes.push((Severity::Breaking, format!("-param {key}"))),
            (Some(b), Some(a)) => {
                if type_sig(old, &b.ty) != type_sig(new, &a.ty) {
                    notes.push((
                        Severity::Breaking,
                        format!(
                            "param {key} type {} → {}",
                            type_sig(old, &b.ty),
                            type_sig(new, &a.ty)
                        ),
                    ));
                }
            }
            (None, None) => unreachable!(),
        }
    }
}

fn diff_decls(old: &ir::Program, new: &ir::Program, changes: &mut Vec<Change>) {
    let old_decls = index_by(&old.decls, decl_key);
    let new_decls = index_by(&new.decls, decl_key);
    for key in union_keys(&old_decls, &new_decls) {
        match (old_decls.get(&key), new_decls.get(&key)) {
            (None, Some(_)) => changes.push(Change {
                kind: ChangeKind::Added,
                severity: Severity::Compatible,
                category: "schema",
                name: key,
                detail: String::new(),
            }),
            (Some(_), None) => changes.push(Change {
                kind: ChangeKind::Removed,
                severity: Severity::Breaking,
                category: "schema",
                name: key,
                detail: String::new(),
            }),
            (Some(before), Some(after)) => {
                if let Some(change) = diff_decl(old, new, &key, before, after) {
                    changes.push(change);
                }
            }
            (None, None) => unreachable!(),
        }
    }
}

fn diff_decl(
    old: &ir::Program,
    new: &ir::Program,
    key: &str,
    before: &ir::Decl,
    after: &ir::Decl,
) -> Option<Change> {
    let mut notes: Vec<(Severity, String)> = Vec::new();
    match (&before.kind, &after.kind) {
        (ir::DeclKind::Struct(b), ir::DeclKind::Struct(a)) => {
            diff_fields(old, new, b, a, &mut notes)
        }
        (ir::DeclKind::Enum(b), ir::DeclKind::Enum(a)) => {
            diff_values(&b.values, &a.values, &mut notes)
        }
        (ir::DeclKind::Union(b), ir::DeclKind::Union(a)) => {
            diff_variants(old, new, b, a, &mut notes)
        }
        (ir::DeclKind::Alias(b), ir::DeclKind::Alias(a)) => {
            if type_sig(old, b) != type_sig(new, a) {
                notes.push((
                    Severity::Breaking,
                    format!("alias {} → {}", type_sig(old, b), type_sig(new, a)),
                ));
            }
        }
        // A shape change (struct → union, etc.) is always breaking.
        (b, a) => notes.push((
            Severity::Breaking,
            format!("kind {} → {}", kind_name(b), kind_name(a)),
        )),
    }
    finish(key, "schema", notes)
}

fn diff_fields(
    old: &ir::Program,
    new: &ir::Program,
    before: &ir::StructDecl,
    after: &ir::StructDecl,
    notes: &mut Vec<(Severity, String)>,
) {
    let old_fields = index_by(&before.fields, |f| f.wire_name.clone());
    let new_fields = index_by(&after.fields, |f| f.wire_name.clone());
    for key in union_keys(&old_fields, &new_fields) {
        match (old_fields.get(&key), new_fields.get(&key)) {
            // A new field is additive for a reader; the SDK still compiles.
            (None, Some(_)) => notes.push((Severity::Compatible, format!("+field {key}"))),
            (Some(_), None) => notes.push((Severity::Breaking, format!("-field {key}"))),
            (Some(b), Some(a)) => {
                if type_sig(old, &b.ty) != type_sig(new, &a.ty) {
                    notes.push((
                        Severity::Breaking,
                        format!(
                            "field {key} type {} → {}",
                            type_sig(old, &b.ty),
                            type_sig(new, &a.ty)
                        ),
                    ));
                }
            }
            (None, None) => unreachable!(),
        }
    }
}

fn diff_values(before: &[String], after: &[String], notes: &mut Vec<(Severity, String)>) {
    let old: BTreeMap<&str, ()> = before.iter().map(|v| (v.as_str(), ())).collect();
    let new: BTreeMap<&str, ()> = after.iter().map(|v| (v.as_str(), ())).collect();
    for value in before {
        if !new.contains_key(value.as_str()) {
            notes.push((Severity::Breaking, format!("-value {value:?}")));
        }
    }
    for value in after {
        if !old.contains_key(value.as_str()) {
            notes.push((Severity::Compatible, format!("+value {value:?}")));
        }
    }
}

fn diff_variants(
    old: &ir::Program,
    new: &ir::Program,
    before: &ir::UnionDecl,
    after: &ir::UnionDecl,
    notes: &mut Vec<(Severity, String)>,
) {
    let old_v = index_by(&before.variants, |v| variant_key(old, v));
    let new_v = index_by(&after.variants, |v| variant_key(new, v));
    for key in union_keys(&old_v, &new_v) {
        match (old_v.get(&key), new_v.get(&key)) {
            (None, Some(_)) => notes.push((Severity::Compatible, format!("+variant {key}"))),
            (Some(_), None) => notes.push((Severity::Breaking, format!("-variant {key}"))),
            (Some(b), Some(a)) => {
                if type_sig(old, &b.ty) != type_sig(new, &a.ty) {
                    notes.push((
                        Severity::Breaking,
                        format!(
                            "variant {key} type {} → {}",
                            type_sig(old, &b.ty),
                            type_sig(new, &a.ty)
                        ),
                    ));
                }
            }
            (None, None) => unreachable!(),
        }
    }
}

fn finish(key: &str, category: &'static str, notes: Vec<(Severity, String)>) -> Option<Change> {
    if notes.is_empty() {
        return None;
    }
    let severity = if notes.iter().any(|(s, _)| *s == Severity::Breaking) {
        Severity::Breaking
    } else {
        Severity::Compatible
    };
    let detail = notes
        .into_iter()
        .map(|(_, note)| note)
        .collect::<Vec<_>>()
        .join("; ");
    Some(Change {
        kind: ChangeKind::Changed,
        severity,
        category,
        name: key.to_string(),
        detail,
    })
}

// --- keys and signatures -------------------------------------------------

fn index_by<T, F: Fn(&T) -> String>(items: &[T], key: F) -> BTreeMap<String, &T> {
    items.iter().map(|item| (key(item), item)).collect()
}

fn union_keys<T>(a: &BTreeMap<String, T>, b: &BTreeMap<String, T>) -> Vec<String> {
    let mut keys: Vec<String> = a.keys().chain(b.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    keys
}

/// A stable operation key: manager, name, variation, and API version — the
/// same identity the generated method carries.
fn operation_key(op: &ir::Operation) -> String {
    let mut key = format!("{}.{}", op.manager.as_str(), op.name.as_str());
    if let Some(variation) = &op.variation {
        let _ = write!(key, "#{}", variation.as_str());
    }
    if let Some(version) = &op.api_version {
        let _ = write!(key, "@{}", version.0);
    }
    key
}

fn param_key(param: &ir::Param) -> String {
    format!("{:?}:{}", param.location, param.wire_name)
}

fn decl_key(decl: &ir::Decl) -> String {
    let mut key = decl
        .module
        .0
        .iter()
        .map(|seg| seg.as_str())
        .collect::<Vec<_>>()
        .join("::");
    if !key.is_empty() {
        key.push_str("::");
    }
    key.push_str(decl.name.as_str());
    if let Some(version) = &decl.api_version {
        let _ = write!(key, "@{}", version.0);
    }
    key
}

fn variant_key(program: &ir::Program, variant: &ir::UnionVariant) -> String {
    match &variant.discriminator_value {
        Some(value) => value.clone(),
        // Undiscriminated variants are identified by their type.
        None => format!("<{}>", type_sig(program, &variant.ty)),
    }
}

fn is_optional(ty: &ir::Type) -> bool {
    matches!(ty, ir::Type::Optional(_))
}

fn kind_name(kind: &ir::DeclKind) -> &'static str {
    match kind {
        ir::DeclKind::Struct(_) => "struct",
        ir::DeclKind::Union(_) => "union",
        ir::DeclKind::Enum(_) => "enum",
        ir::DeclKind::Alias(_) => "alias",
    }
}

fn path_sig(path: &[ir::PathSegment]) -> String {
    let mut out = String::new();
    for segment in path {
        out.push('/');
        match segment {
            ir::PathSegment::Literal(text) => out.push_str(text),
            ir::PathSegment::Parameter(name) => {
                let _ = write!(out, "{{{}}}", name.as_str());
            }
            ir::PathSegment::Composite(parts) => {
                for part in parts {
                    match part {
                        ir::PathPart::Literal(text) => out.push_str(text),
                        ir::PathPart::Parameter(name) => {
                            let _ = write!(out, "{{{}}}", name.as_str());
                        }
                    }
                }
            }
        }
    }
    out
}

fn request_sig(program: &ir::Program, body: &ir::RequestBody) -> String {
    format!("{:?}({})", body.media, type_sig(program, &body.ty))
}

fn response_sig(program: &ir::Program, shape: &ir::ResponseShape) -> String {
    match shape {
        ir::ResponseShape::None => "none".to_string(),
        ir::ResponseShape::Json(ty) => format!("json({})", type_sig(program, ty)),
        ir::ResponseShape::Binary => "binary".to_string(),
        ir::ResponseShape::Text => "text".to_string(),
        ir::ResponseShape::Redirect => "redirect".to_string(),
    }
}

/// Render a type to a canonical, cross-program signature. A decl reference
/// resolves to its qualified name (never its arena id), so the two
/// programs' independent id spaces compare structurally.
fn type_sig(program: &ir::Program, ty: &ir::Type) -> String {
    match ty {
        ir::Type::Bool => "bool".to_string(),
        ir::Type::Int64 => "int64".to_string(),
        ir::Type::Float64 => "float64".to_string(),
        ir::Type::String => "string".to_string(),
        ir::Type::Date => "date".to_string(),
        ir::Type::DateTime => "datetime".to_string(),
        ir::Type::Binary => "binary".to_string(),
        ir::Type::Optional(inner) => format!("optional<{}>", type_sig(program, inner)),
        ir::Type::Nullable(inner) => format!("nullable<{}>", type_sig(program, inner)),
        ir::Type::List(inner) => format!("list<{}>", type_sig(program, inner)),
        ir::Type::Map(inner) => format!("map<{}>", type_sig(program, inner)),
        ir::Type::Decl(id) => decl_key(program.decl(*id)),
        ir::Type::JsonValue => "json".to_string(),
    }
}
