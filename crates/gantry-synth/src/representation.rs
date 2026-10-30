//! Representation detection (Box's file representations).
//!
//! Box renders a file into other formats — a PDF, its extracted text, page
//! images, thumbnails — on demand. A `GET` of the file with
//! `fields=representations` and an `X-Rep-Hints` header naming the formats
//! returns `representations.entries`, each with an `info.url` to poll, a
//! `status.state` (`success`, `viewable`, `pending`, `none`, `error`), and a
//! `content.url_template` whose `{+asset_path}` names the asset to download.
//! The runtime drives that protocol; the generated method only supplies the
//! item's request.
//!
//! The operation is found structurally — a `GET` with an optional `fields`
//! query list and an optional `x-rep-hints` header, answering a struct with
//! a field whose `entries` carry `content.url_template`, `info.url`, and
//! `status.state` — never from its name. A partial match is simply not one:
//! the plain method remains, and the real-spec test pins exactly which
//! operation is detected, so a spec change that breaks the shape fails loudly
//! there.

use gantry_ir as ir;
use gantry_sema::Analysis;

/// A detected representation source: the item `GET` and the wire names the
/// generated method sets on its request. Params and the field are the IR's
/// own, so each backend renders them with its own naming.
#[derive(Debug, Clone)]
pub struct Representation {
    /// Index into `program.operations` of the item `GET`.
    pub operation: usize,
    /// The optional query list naming the fields to return; the request
    /// asks for [`Representation::field`] alone.
    pub fields_param: ir::Param,
    /// The optional header carrying the representation hints.
    pub hints_param: ir::Param,
    /// The response field holding the representations (its wire name is the
    /// key the runtime reads).
    pub field: ir::Field,
}

/// Detect every representation source in the program, in operation order
/// (deterministic — FR-6.2).
pub fn detect_representations(analysis: &Analysis<'_>) -> Vec<Representation> {
    let program = analysis.program;
    program
        .operations
        .iter()
        .enumerate()
        .filter_map(|(index, op)| detect_one(program, index, op))
        .collect()
}

fn detect_one(
    program: &ir::Program,
    operation: usize,
    op: &ir::Operation,
) -> Option<Representation> {
    // A plain `GET`: no body, and nothing required beyond its path.
    if op.method != ir::HttpMethod::Get
        || op.request.is_some()
        || op.params.iter().any(|p| {
            p.location != ir::ParamLocation::Path && !matches!(p.ty, ir::Type::Optional(_))
        })
    {
        return None;
    }
    let optional = |ty: ir::Type| ir::Type::Optional(Box::new(ty));
    let fields_param = op.params.iter().find(|p| {
        p.location == ir::ParamLocation::Query
            && p.wire_name == "fields"
            && p.ty == optional(ir::Type::List(Box::new(ir::Type::String)))
    })?;
    let hints_param = op.params.iter().find(|p| {
        p.location == ir::ParamLocation::Header
            && p.wire_name.eq_ignore_ascii_case("x-rep-hints")
            && p.ty == optional(ir::Type::String)
    })?;
    let ir::ResponseShape::Json(ir::Type::Decl(item)) = &op.response else {
        return None;
    };
    let field = struct_of(program, *item)?
        .fields
        .iter()
        .find(|f| is_representations(program, &f.ty))?;
    Some(Representation {
        operation,
        fields_param: fields_param.clone(),
        hints_param: hints_param.clone(),
        field: field.clone(),
    })
}

/// Whether `ty` is an optional struct of `entries`, each with
/// `content.url_template`, `info.url`, and `status.state`.
fn is_representations(program: &ir::Program, ty: &ir::Type) -> bool {
    let Some(set) = optional_struct(program, ty) else {
        return false;
    };
    let Some(entries) = field(set, "entries") else {
        return false;
    };
    let ir::Type::Optional(list) = &entries.ty else {
        return false;
    };
    let ir::Type::List(element) = list.as_ref() else {
        return false;
    };
    let ir::Type::Decl(entry) = element.as_ref() else {
        return false;
    };
    let Some(entry) = struct_of(program, *entry) else {
        return false;
    };
    let nested = |outer: &str, inner: &str| {
        field(entry, outer)
            .and_then(|f| optional_struct(program, &f.ty))
            .and_then(|s| field(s, inner))
            .is_some()
    };
    nested("content", "url_template") && nested("info", "url") && nested("status", "state")
}

fn optional_struct<'p>(program: &'p ir::Program, ty: &ir::Type) -> Option<&'p ir::StructDecl> {
    let ir::Type::Optional(inner) = ty else {
        return None;
    };
    let ir::Type::Decl(id) = inner.as_ref() else {
        return None;
    };
    struct_of(program, *id)
}

fn struct_of(program: &ir::Program, id: ir::DeclId) -> Option<&ir::StructDecl> {
    let ir::DeclKind::Struct(decl) = &program.decl(id).kind else {
        return None;
    };
    Some(decl)
}

fn field<'a>(decl: &'a ir::StructDecl, wire: &str) -> Option<&'a ir::Field> {
    decl.fields.iter().find(|f| f.wire_name == wire)
}
