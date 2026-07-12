//! Pagination detection (FR-7.3, D-013).
//!
//! Box paginates two ways, both discovered structurally from the IR —
//! never from a naming heuristic on the operation alone:
//!
//! - **marker**: a `marker` query parameter, and a JSON response struct
//!   with an `entries` list and a `next_marker` field.
//! - **offset**: an `offset` query parameter, and a JSON response struct
//!   with an `entries` list and an `offset` field.
//!
//! An operation that has the query parameter but whose response lacks the
//! envelope (or vice versa) is simply *not* paginated — it keeps its
//! plain method. That conservative rule means a backend never synthesizes
//! an iterator over a non-envelope, and nothing is silently skipped: the
//! [`detect_pagination`] result is reported in the run summary (VR-6).

use gantry_ir as ir;
use gantry_sema::Analysis;

/// How an operation pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageStyle {
    /// Opaque `next_marker` cursor.
    Marker,
    /// Numeric `offset` + `total_count`.
    Offset,
}

/// A paginated operation, resolved to everything a backend needs to
/// synthesize an iterator: which operation, the page style, the wire
/// names of the request parameter and response fields, and the element
/// type yielded.
#[derive(Debug, Clone)]
pub struct PagedOperation {
    /// Index into `program.operations`.
    pub operation: usize,
    pub style: PageStyle,
    /// Query parameter carrying the cursor (`"marker"` / `"offset"`).
    pub param_wire: String,
    /// Response field holding the page slice (`"entries"`).
    pub entries_wire: String,
    /// Response field holding the next cursor
    /// (`"next_marker"` / `"offset"`).
    pub cursor_wire: String,
    /// The element type inside `entries` — what the iterator yields.
    pub element: ir::Type,
}

/// Detect every paginated operation in the program, in operation order
/// (deterministic — FR-6.2).
pub fn detect_pagination(analysis: &Analysis<'_>) -> Vec<PagedOperation> {
    let program = analysis.program;
    let mut paged = Vec::new();
    for (index, op) in program.operations.iter().enumerate() {
        if let Some(found) = detect_one(program, index, op) {
            paged.push(found);
        }
    }
    paged
}

fn detect_one(program: &ir::Program, index: usize, op: &ir::Operation) -> Option<PagedOperation> {
    // The response must be a JSON struct declaration.
    let ir::ResponseShape::Json(response_ty) = &op.response else {
        return None;
    };
    let decl_id = as_decl(response_ty)?;
    let ir::DeclKind::Struct(envelope) = &program.decl(decl_id).kind else {
        return None;
    };

    // It must carry an `entries` list; the element is what we iterate.
    let entries = envelope.fields.iter().find(|f| f.wire_name == "entries")?;
    let element = list_element(&entries.ty)?.clone();
    let has = |wire: &str| envelope.fields.iter().any(|f| f.wire_name == wire);

    // A cursor query parameter decides the style; it must be optional (a
    // required cursor makes no sense to iterate), and the response must
    // carry the matching cursor field, else it is not really paginated.
    let has_param = |wire: &str| {
        op.params.iter().any(|p| {
            p.location == ir::ParamLocation::Query
                && p.wire_name == wire
                && matches!(p.ty, ir::Type::Optional(_))
        })
    };
    let (style, param_wire, cursor_wire) = if has_param("marker") && has("next_marker") {
        (PageStyle::Marker, "marker", "next_marker")
    } else if has_param("offset") && has("offset") {
        (PageStyle::Offset, "offset", "offset")
    } else {
        return None;
    };

    Some(PagedOperation {
        operation: index,
        style,
        param_wire: param_wire.to_string(),
        entries_wire: "entries".to_string(),
        cursor_wire: cursor_wire.to_string(),
        element,
    })
}

/// Peel optionality; return the declaration id if the type is a `Decl`.
fn as_decl(ty: &ir::Type) -> Option<ir::DeclId> {
    match ty {
        ir::Type::Optional(inner) | ir::Type::Nullable(inner) => as_decl(inner),
        ir::Type::Decl(id) => Some(*id),
        ir::Type::Bool
        | ir::Type::Int64
        | ir::Type::Float64
        | ir::Type::String
        | ir::Type::Date
        | ir::Type::DateTime
        | ir::Type::Binary
        | ir::Type::JsonValue
        | ir::Type::List(_)
        | ir::Type::Map(_) => None,
    }
}

/// Peel optionality; return the element type if the type is a `List`.
fn list_element(ty: &ir::Type) -> Option<&ir::Type> {
    match ty {
        ir::Type::Optional(inner) | ir::Type::Nullable(inner) => list_element(inner),
        ir::Type::List(inner) => Some(inner),
        ir::Type::Bool
        | ir::Type::Int64
        | ir::Type::Float64
        | ir::Type::String
        | ir::Type::Date
        | ir::Type::DateTime
        | ir::Type::Binary
        | ir::Type::JsonValue
        | ir::Type::Map(_)
        | ir::Type::Decl(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> ir::Identifier {
        ir::Identifier::new(name).unwrap()
    }

    /// A program with one element decl, one envelope decl, and one
    /// operation returning the envelope, parameterized by cursor param.
    fn program_with(param_wire: &str, cursor_field: &str) -> ir::Program {
        let module = ir::ModulePath(vec![ident("schemas")]);
        let element = ir::Decl {
            name: ident("Item"),
            module: module.clone(),
            api_version: None,
            kind: ir::DeclKind::Struct(ir::StructDecl { fields: vec![] }),
        };
        let mut envelope_fields = vec![ir::Field {
            name: ident("entries"),
            wire_name: "entries".into(),
            ty: ir::Type::List(Box::new(ir::Type::Decl(ir::DeclId(0)))),
        }];
        envelope_fields.push(ir::Field {
            name: ident("cursor"),
            wire_name: cursor_field.into(),
            ty: ir::Type::String,
        });
        let envelope = ir::Decl {
            name: ident("Items"),
            module,
            api_version: None,
            kind: ir::DeclKind::Struct(ir::StructDecl {
                fields: envelope_fields,
            }),
        };
        let op = ir::Operation {
            name: ident("get_items"),
            variation: None,
            manager: ident("items"),
            api_version: None,
            method: ir::HttpMethod::Get,
            base_url: ir::BaseUrl::Api,
            path: vec![ir::PathSegment::Literal("items".into())],
            params: vec![ir::Param {
                name: ident(param_wire),
                wire_name: param_wire.into(),
                location: ir::ParamLocation::Query,
                ty: ir::Type::Optional(Box::new(ir::Type::String)),
            }],
            request: None,
            response: ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(1))),
            deprecated: false,
        };
        ir::Program {
            decls: vec![element, envelope],
            operations: vec![op],
        }
    }

    #[test]
    fn marker_pagination_detected() {
        let program = program_with("marker", "next_marker");
        let analysis = gantry_sema::analyze(&program).unwrap();
        let paged = detect_pagination(&analysis);
        assert_eq!(paged.len(), 1);
        assert_eq!(paged[0].style, PageStyle::Marker);
        assert_eq!(paged[0].cursor_wire, "next_marker");
        assert_eq!(paged[0].element, ir::Type::Decl(ir::DeclId(0)));
    }

    #[test]
    fn offset_pagination_detected() {
        let program = program_with("offset", "offset");
        let analysis = gantry_sema::analyze(&program).unwrap();
        let paged = detect_pagination(&analysis);
        assert_eq!(paged.len(), 1);
        assert_eq!(paged[0].style, PageStyle::Offset);
    }

    #[test]
    fn param_without_envelope_is_not_paginated() {
        // marker query param, but the response cursor field is unrelated.
        let program = program_with("marker", "unrelated");
        let analysis = gantry_sema::analyze(&program).unwrap();
        assert!(detect_pagination(&analysis).is_empty());
    }
}
