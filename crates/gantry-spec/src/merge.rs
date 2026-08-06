//! Merge the versioned schema modules into one `schemas` namespace (D-190).
//!
//! The lowering gives each versioned document its own module
//! (`schemas::v2025_0`, FR-7.5) so nothing collides with the base spec. That is
//! correct but leaks into the public surface as two namespaces for what is
//! usually one type (`models.schemas_v2025_0.X` vs `models.schemas.X`) — poor
//! DevX. This pass collapses every schema declaration into a single `schemas`
//! module and, when a name appears in more than one version, produces **one
//! merged superset type**:
//!
//! - **Struct** — the union of all versions' fields. A field carries its shared
//!   type when the versions agree (equal, or references to structurally
//!   equivalent declarations — the same discriminator enum under a different
//!   name); it becomes optional unless it is present and required in *every*
//!   version. So a field only some versions declare round-trips as absent
//!   elsewhere, and the type's contract is the looser union.
//! - **Enum** — the union of all values (base order first), open if any is.
//! - **Union** — the union of variants by discriminator value.
//! - **Alias** — kept when all versions agree.
//!
//! A genuine conflict — the same wire field with non-equivalent types, or one
//! name bound to different declaration kinds — is a loud error (NF-1), never a
//! silent pick.
//!
//! Declaration references are positional [`ir::DeclId`]s, so the pass rebuilds
//! the `decls` table and rewrites every reference (in declarations *and*
//! operations) through one old→new map.

use std::collections::HashSet;

use gantry_ir as ir;
use indexmap::IndexMap;

use crate::error::IngestError;

/// Collapse `schemas::vNrM` declarations into `schemas`, merging same-named
/// types across versions into one superset type (D-190).
pub(crate) fn merge_versioned_schemas(program: ir::Program) -> Result<ir::Program, IngestError> {
    let ir::Program { decls, operations } = program;

    // Group declaration indices by name, in first-occurrence order. The base
    // document is lowered first, so a group's first member is always the base
    // definition — the representative whose name, field order, and identifiers
    // the merge keeps.
    let mut groups: IndexMap<String, Vec<usize>> = IndexMap::new();
    for (index, decl) in decls.iter().enumerate() {
        groups
            .entry(decl.name.as_str().to_string())
            .or_default()
            .push(index);
    }

    // New ids are assigned in group (first-occurrence) order; every old index
    // maps to its group's new id, so references collapse onto the merged decl.
    let mut old_to_new = vec![ir::DeclId(0); decls.len()];
    for (new_id, members) in groups.values().enumerate() {
        let new_id = ir::DeclId(u32::try_from(new_id).expect("decl count fits u32"));
        for &old in members {
            old_to_new[old] = new_id;
        }
    }

    // Build one merged declaration per group, then remap its references.
    let mut merged: Vec<ir::Decl> = Vec::with_capacity(groups.len());
    for members in groups.values() {
        let mut decl = merge_group(&decls, members)?;
        remap_decl(&mut decl, &old_to_new);
        // The namespace is unified: everything lands in `schemas`.
        decl.module = ir::ModulePath(vec![schemas_root(&decl.module)]);
        merged.push(decl);
    }

    let mut operations = operations;
    for op in &mut operations {
        remap_operation(op, &old_to_new);
    }

    Ok(ir::Program {
        decls: merged,
        operations,
    })
}

/// The `schemas` root identifier of a (possibly versioned) module path.
fn schemas_root(module: &ir::ModulePath) -> ir::Identifier {
    module
        .0
        .first()
        .cloned()
        .expect("every schema declaration has a module root")
}

/// Merge a group of same-named declarations into one. A single-member group is
/// returned as-is (the common case); a multi-member group is superset-merged.
fn merge_group(decls: &[ir::Decl], members: &[usize]) -> Result<ir::Decl, IngestError> {
    let base = decls[members[0]].clone();
    if members.len() == 1 {
        return Ok(base);
    }
    let name = base.name.as_str().to_string();
    let rest = &members[1..];

    let kind = match &base.kind {
        ir::DeclKind::Struct(base_struct) => {
            ir::DeclKind::Struct(merge_structs(&name, decls, base_struct, rest)?)
        }
        ir::DeclKind::Enum(base_enum) => {
            ir::DeclKind::Enum(merge_enums(&name, decls, base_enum, rest)?)
        }
        ir::DeclKind::Union(base_union) => {
            ir::DeclKind::Union(merge_unions(&name, decls, base_union, rest)?)
        }
        ir::DeclKind::Alias(base_ty) => {
            for &m in rest {
                let ir::DeclKind::Alias(other) = &decls[m].kind else {
                    return Err(conflict(
                        &name,
                        "alias in one version, non-alias in another",
                    ));
                };
                if !types_equivalent(base_ty, other, decls, &mut HashSet::new()) {
                    return Err(conflict(&name, "aliases resolve to non-equivalent types"));
                }
            }
            ir::DeclKind::Alias(base_ty.clone())
        }
    };

    Ok(ir::Decl { kind, ..base })
}

/// Superset-merge struct fields: the union by wire name, each field optional
/// unless present and required in every version.
fn merge_structs(
    name: &str,
    decls: &[ir::Decl],
    base: &ir::StructDecl,
    rest: &[usize],
) -> Result<ir::StructDecl, IngestError> {
    let others: Vec<&ir::StructDecl> = rest
        .iter()
        .map(|&m| match &decls[m].kind {
            ir::DeclKind::Struct(s) => Ok(s),
            ir::DeclKind::Union(_) | ir::DeclKind::Enum(_) | ir::DeclKind::Alias(_) => Err(
                conflict(name, "struct in one version, non-struct in another"),
            ),
        })
        .collect::<Result<_, _>>()?;

    // Field order: base fields first, then fields new to later versions in the
    // order they first appear. A field is required only when every version has
    // it and none makes it optional.
    let mut order: Vec<String> = base.fields.iter().map(|f| f.wire_name.clone()).collect();
    for other in &others {
        for f in &other.fields {
            if !order.contains(&f.wire_name) {
                order.push(f.wire_name.clone());
            }
        }
    }

    let all: Vec<&ir::StructDecl> = std::iter::once(base)
        .chain(others.iter().copied())
        .collect();
    let mut fields = Vec::with_capacity(order.len());
    for wire in &order {
        // The prototype field (identifier + core type) comes from the first
        // version that declares it — the base when it has the field.
        let proto = all
            .iter()
            .find_map(|s| s.fields.iter().find(|f| &f.wire_name == wire))
            .expect("wire name came from some version's fields");
        let (_, proto_core) = split_optional(&proto.ty);

        // Every version that declares the field must agree on its core type.
        let mut required_everywhere = true;
        for s in &all {
            match s.fields.iter().find(|f| &f.wire_name == wire) {
                Some(f) => {
                    let (required, core) = split_optional(&f.ty);
                    if !types_equivalent(proto_core, core, decls, &mut HashSet::new()) {
                        return Err(conflict(
                            name,
                            &format!("field {wire:?} has non-equivalent types across versions"),
                        ));
                    }
                    required_everywhere &= required;
                }
                None => required_everywhere = false,
            }
        }

        let ty = if required_everywhere {
            proto_core.clone()
        } else {
            ir::Type::Optional(Box::new(proto_core.clone()))
        };
        fields.push(ir::Field {
            name: proto.name.clone(),
            wire_name: wire.clone(),
            ty,
        });
    }

    // A struct is open (D-196) if any version says so — matching the
    // superset philosophy the field loop above uses. Versions that
    // disagree on the *shape* of the extra data are a real conflict, same
    // as a field whose type disagrees across versions.
    let mut extra: Option<&ir::Type> = None;
    for s in &all {
        if let Some(t) = &s.extra {
            match extra {
                None => extra = Some(t),
                Some(prev) if types_equivalent(prev, t, decls, &mut HashSet::new()) => {}
                Some(_) => {
                    return Err(conflict(
                        name,
                        "additionalProperties has non-equivalent types across versions",
                    ));
                }
            }
        }
    }

    Ok(ir::StructDecl {
        fields,
        extra: extra.cloned(),
    })
}

/// Union enum values (base order first) and loosen extensibility.
fn merge_enums(
    name: &str,
    decls: &[ir::Decl],
    base: &ir::EnumDecl,
    rest: &[usize],
) -> Result<ir::EnumDecl, IngestError> {
    let mut values = base.values.clone();
    let mut open = matches!(base.extensibility, ir::Extensibility::Open);
    for &m in rest {
        let ir::DeclKind::Enum(other) = &decls[m].kind else {
            return Err(conflict(name, "enum in one version, non-enum in another"));
        };
        for v in &other.values {
            if !values.contains(v) {
                values.push(v.clone());
            }
        }
        open |= matches!(other.extensibility, ir::Extensibility::Open);
    }
    Ok(ir::EnumDecl {
        values,
        extensibility: if open {
            ir::Extensibility::Open
        } else {
            ir::Extensibility::Closed
        },
    })
}

/// Union union variants by discriminator value; loosen extensibility. A shared
/// discriminator value whose variant types disagree is a conflict.
fn merge_unions(
    name: &str,
    decls: &[ir::Decl],
    base: &ir::UnionDecl,
    rest: &[usize],
) -> Result<ir::UnionDecl, IngestError> {
    let mut variants = base.variants.clone();
    let mut open = matches!(base.extensibility, ir::Extensibility::Open);
    for &m in rest {
        let ir::DeclKind::Union(other) = &decls[m].kind else {
            return Err(conflict(name, "union in one version, non-union in another"));
        };
        if other.discriminator != base.discriminator {
            return Err(conflict(name, "unions disagree on the discriminator field"));
        }
        for variant in &other.variants {
            // Discriminated variants key on their value. A structural union
            // carries none (`lower_union` clears them all), so `None` is not a
            // key — match those on shape, or every variant would conflate with
            // the first one and quietly drop the rest (NF-1: never a silent pick).
            let existing = match &variant.discriminator_value {
                Some(value) => variants
                    .iter()
                    .find(|v| v.discriminator_value.as_ref() == Some(value)),
                None => variants.iter().find(|v| {
                    v.discriminator_value.is_none()
                        && types_equivalent(&v.ty, &variant.ty, decls, &mut HashSet::new())
                }),
            };
            match existing {
                Some(existing) => {
                    if !types_equivalent(&existing.ty, &variant.ty, decls, &mut HashSet::new()) {
                        return Err(conflict(
                            name,
                            "a union variant has non-equivalent types across versions",
                        ));
                    }
                }
                None => variants.push(variant.clone()),
            }
        }
        open |= matches!(other.extensibility, ir::Extensibility::Open);
    }
    Ok(ir::UnionDecl {
        discriminator: base.discriminator.clone(),
        variants,
        extensibility: if open {
            ir::Extensibility::Open
        } else {
            ir::Extensibility::Closed
        },
    })
}

/// Peel the outer optionality layer: `(is_required, core)` where a required
/// field is one not wrapped in [`ir::Type::Optional`]. The core keeps any inner
/// `Nullable` (canonical nesting is `Optional<Nullable<T>>`).
fn split_optional(ty: &ir::Type) -> (bool, &ir::Type) {
    // `if let`, not a `_` match arm — an absent field is optional; anything else
    // is required and carries its own type as the core (NF-1: no wildcard).
    if let ir::Type::Optional(inner) = ty {
        (false, inner)
    } else {
        (true, ty)
    }
}

/// Structural (bisimulation) equivalence of two types, ignoring declaration
/// *names*: two references are equivalent when the declarations they point at
/// have equivalent structure. The `seen` set records in-progress declaration
/// pairs so recursive types terminate (a revisited pair is assumed equivalent).
fn types_equivalent(
    a: &ir::Type,
    b: &ir::Type,
    decls: &[ir::Decl],
    seen: &mut HashSet<(u32, u32)>,
) -> bool {
    use ir::Type::{
        Binary, Bool, Date, DateTime, Decl, Float64, Int64, JsonValue, List, Map, Nullable,
        Optional, String as TStr,
    };
    match (a, b) {
        (Bool, Bool)
        | (Int64, Int64)
        | (Float64, Float64)
        | (TStr, TStr)
        | (Date, Date)
        | (DateTime, DateTime)
        | (Binary, Binary)
        | (JsonValue, JsonValue) => true,
        (Optional(x), Optional(y))
        | (Nullable(x), Nullable(y))
        | (List(x), List(y))
        | (Map(x), Map(y)) => types_equivalent(x, y, decls, seen),
        (Decl(x), Decl(y)) => decls_equivalent(*x, *y, decls, seen),
        _ => false,
    }
}

/// Structural equivalence of two declarations (see [`types_equivalent`]).
fn decls_equivalent(
    x: ir::DeclId,
    y: ir::DeclId,
    decls: &[ir::Decl],
    seen: &mut HashSet<(u32, u32)>,
) -> bool {
    if x == y {
        return true;
    }
    let key = (x.0.min(y.0), x.0.max(y.0));
    if !seen.insert(key) {
        // Already comparing this pair further up the stack: assume equivalent
        // (co-inductive), so a cycle through the declaration graph terminates.
        return true;
    }
    let result = match (&decls[x.0 as usize].kind, &decls[y.0 as usize].kind) {
        (ir::DeclKind::Struct(a), ir::DeclKind::Struct(b)) => {
            a.fields.len() == b.fields.len()
                && a.fields.iter().all(|fa| {
                    b.fields
                        .iter()
                        .find(|fb| fb.wire_name == fa.wire_name)
                        .is_some_and(|fb| types_equivalent(&fa.ty, &fb.ty, decls, seen))
                })
        }
        (ir::DeclKind::Enum(a), ir::DeclKind::Enum(b)) => {
            a.extensibility == b.extensibility
                && a.values.len() == b.values.len()
                && a.values.iter().all(|v| b.values.contains(v))
        }
        (ir::DeclKind::Union(a), ir::DeclKind::Union(b)) => {
            a.discriminator == b.discriminator
                && a.extensibility == b.extensibility
                && a.variants.len() == b.variants.len()
                && a.variants.iter().all(|va| {
                    b.variants
                        .iter()
                        .find(|vb| vb.discriminator_value == va.discriminator_value)
                        .is_some_and(|vb| types_equivalent(&va.ty, &vb.ty, decls, seen))
                })
        }
        (ir::DeclKind::Alias(a), ir::DeclKind::Alias(b)) => types_equivalent(a, b, decls, seen),
        _ => false,
    };
    // The pair only needed pinning while its own subtree was in flight.
    seen.remove(&key);
    result
}

fn conflict(name: &str, detail: &str) -> IngestError {
    IngestError::SchemaVersionConflict {
        name: name.to_string(),
        detail: detail.to_string(),
    }
}

/// Rewrite every declaration reference in a declaration through `map`.
fn remap_decl(decl: &mut ir::Decl, map: &[ir::DeclId]) {
    match &mut decl.kind {
        ir::DeclKind::Struct(s) => {
            for f in &mut s.fields {
                remap_type(&mut f.ty, map);
            }
        }
        ir::DeclKind::Union(u) => {
            for v in &mut u.variants {
                remap_type(&mut v.ty, map);
            }
        }
        ir::DeclKind::Enum(_) => {}
        ir::DeclKind::Alias(ty) => remap_type(ty, map),
    }
}

fn remap_operation(op: &mut ir::Operation, map: &[ir::DeclId]) {
    for param in &mut op.params {
        remap_type(&mut param.ty, map);
    }
    if let Some(body) = &mut op.request {
        remap_type(&mut body.ty, map);
    }
    if let ir::ResponseShape::Json(ty) = &mut op.response {
        remap_type(ty, map);
    }
}

fn remap_type(ty: &mut ir::Type, map: &[ir::DeclId]) {
    match ty {
        ir::Type::Optional(inner)
        | ir::Type::Nullable(inner)
        | ir::Type::List(inner)
        | ir::Type::Map(inner) => remap_type(inner, map),
        ir::Type::Decl(id) => *id = map[id.0 as usize],
        // Scalars hold no declaration reference (NF-1: no wildcard).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> ir::Identifier {
        ir::Identifier::new(name).expect("test identifier")
    }

    /// A structural (non-discriminated) union: `lower_union` clears every
    /// variant's discriminator value, so `None` is not a key to match on.
    fn structural_union(variants: &[ir::Type]) -> ir::DeclKind {
        ir::DeclKind::Union(ir::UnionDecl {
            discriminator: None,
            variants: variants
                .iter()
                .map(|ty| ir::UnionVariant {
                    discriminator_value: None,
                    ty: ty.clone(),
                })
                .collect(),
            extensibility: ir::Extensibility::Open,
        })
    }

    fn union_decl(variants: &[ir::Type]) -> ir::Decl {
        ir::Decl {
            name: ident("Target"),
            module: ir::ModulePath(vec![ident("schemas")]),
            api_version: None,
            kind: structural_union(variants),
        }
    }

    fn merged_variants(base: &[ir::Type], other: &[ir::Type]) -> Vec<ir::Type> {
        let decls = vec![union_decl(base), union_decl(other)];
        let ir::DeclKind::Union(base_union) = &decls[0].kind else {
            unreachable!("constructed as a union")
        };
        let merged =
            merge_unions("Target", &decls, base_union, &[1]).expect("the versions must merge");
        merged.variants.into_iter().map(|v| v.ty).collect()
    }

    #[test]
    fn structural_union_variants_merge_by_shape_not_by_absent_discriminator() {
        // Every variant is `None`-keyed, so keying on the discriminator would
        // conflate them all with the first and silently drop the rest.
        let merged = merged_variants(
            &[ir::Type::String, ir::Type::Int64],
            &[ir::Type::Int64, ir::Type::Bool],
        );
        assert_eq!(
            merged,
            [ir::Type::String, ir::Type::Int64, ir::Type::Bool],
            "the shared variant dedupes and the new one is kept"
        );
    }

    #[test]
    fn a_structural_union_that_only_repeats_variants_gains_none() {
        let merged = merged_variants(&[ir::Type::String, ir::Type::Bool], &[ir::Type::Bool]);
        assert_eq!(merged, [ir::Type::String, ir::Type::Bool]);
    }
}
