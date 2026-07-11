//! Semantic analysis (FR-3).
//!
//! Exactly one pass between ingestion and backends. It verifies every
//! program-level invariant the lowering is supposed to uphold and
//! produces a queryable [`Analysis`]; backends receive only verified
//! programs (FR-3.2). Unlike ingestion — which fails fast on the first
//! spec error — analysis collects *all* findings before reporting
//! (FR-3.3, NF-3): a semantic report is a work list, not a first symptom.
//!
//! Two error classes, distinguished for FR-8.3 exit codes:
//! - **Spec-level** findings (duplicate operations, colliding wire
//!   names): the input is at fault.
//! - **Engine bugs** ([`SemaError::is_engine_bug`]): the `Program` itself
//!   is malformed (dangling ids, `Optional<Optional<…>>`) — the lowering
//!   broke its own contract, and generation must stop loudly (NF-1).

use std::collections::BTreeMap;
use std::collections::HashSet;

use gantry_ir as ir;

/// One semantic finding. `context` names the declaration or operation so
/// every message answers *what* and *where* (NF-3).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SemaError {
    #[error(
        "{context}: DeclId({id}) is out of bounds (program has {decl_count} declarations) — \
         engine bug in the lowering"
    )]
    DanglingRef {
        context: String,
        id: u32,
        decl_count: usize,
    },

    #[error(
        "{context}: Optional<Optional<…>> — optionality must collapse during lowering — engine bug"
    )]
    DoubleOptional { context: String },

    #[error("duplicate declaration name {name:?} in module {module:?}")]
    DuplicateDeclName { module: String, name: String },

    #[error("{context}: duplicate wire name {wire_name:?}")]
    DuplicateWireName { context: String, wire_name: String },

    #[error("{context}: union has no variants")]
    EmptyUnion { context: String },

    #[error(
        "{context}: discriminated union must give every variant a distinct discriminator value"
    )]
    BadDiscriminatorValues { context: String },

    #[error("{context}: enum has no values")]
    EmptyEnum { context: String },

    #[error("duplicate operation {key:?} — same manager, name, variation, and API version")]
    DuplicateOperation { key: String },

    #[error("{context}: duplicate {location:?} parameter {wire_name:?}")]
    DuplicateParam {
        context: String,
        location: ir::ParamLocation,
        wire_name: String,
    },
}

impl SemaError {
    /// `true` when the program itself is malformed — the lowering, not
    /// the spec, is at fault (exit-code class: engine bug, FR-8.3).
    pub fn is_engine_bug(&self) -> bool {
        match self {
            Self::DanglingRef { .. } | Self::DoubleOptional { .. } => true,
            Self::DuplicateDeclName { .. }
            | Self::DuplicateWireName { .. }
            | Self::EmptyUnion { .. }
            | Self::BadDiscriminatorValues { .. }
            | Self::EmptyEnum { .. }
            | Self::DuplicateOperation { .. }
            | Self::DuplicateParam { .. } => false,
        }
    }
}

/// The verified program plus the indices backends and feature synthesis
/// query (FR-3.1). Constructing one is only possible through [`analyze`].
#[derive(Debug)]
pub struct Analysis<'p> {
    pub program: &'p ir::Program,
    /// Manager grouping key → indices into `program.operations`, in
    /// program order. Managers spanning API versions appear once; each
    /// operation carries its own version (FR-7.5).
    pub managers: BTreeMap<String, Vec<usize>>,
}

/// Run the semantic pass. Returns the queryable [`Analysis`] or *every*
/// finding, engine bugs sorted first.
pub fn analyze(program: &ir::Program) -> Result<Analysis<'_>, Vec<SemaError>> {
    let mut errors = Vec::new();

    // Declarations: names unique per module; every kind well-formed.
    let mut decl_names: HashSet<(String, &str)> = HashSet::new();
    for decl in &program.decls {
        let module = module_string(&decl.module);
        let context = format!("{module}::{}", decl.name.as_str());
        if !decl_names.insert((module.clone(), decl.name.as_str())) {
            errors.push(SemaError::DuplicateDeclName {
                module,
                name: decl.name.as_str().to_string(),
            });
        }
        match &decl.kind {
            ir::DeclKind::Struct(s) => {
                let mut wires = HashSet::new();
                for field in &s.fields {
                    if !wires.insert(field.wire_name.as_str()) {
                        errors.push(SemaError::DuplicateWireName {
                            context: context.clone(),
                            wire_name: field.wire_name.clone(),
                        });
                    }
                    check_type(program, &context, &field.ty, false, &mut errors);
                }
            }
            ir::DeclKind::Union(u) => {
                if u.variants.is_empty() {
                    errors.push(SemaError::EmptyUnion {
                        context: context.clone(),
                    });
                }
                if u.discriminator.is_some() {
                    let mut values = HashSet::new();
                    let all_distinct = u.variants.iter().all(|v| {
                        v.discriminator_value
                            .as_deref()
                            .is_some_and(|value| values.insert(value))
                    });
                    if !all_distinct {
                        errors.push(SemaError::BadDiscriminatorValues {
                            context: context.clone(),
                        });
                    }
                }
                for variant in &u.variants {
                    check_type(program, &context, &variant.ty, false, &mut errors);
                }
            }
            ir::DeclKind::Enum(e) => {
                if e.values.is_empty() {
                    errors.push(SemaError::EmptyEnum {
                        context: context.clone(),
                    });
                }
            }
            ir::DeclKind::Alias(ty) => check_type(program, &context, ty, false, &mut errors),
        }
    }

    // Operations: identity unique; params unique per location; types
    // well-formed; the manager index is built alongside.
    let mut managers: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut op_keys: HashSet<String> = HashSet::new();
    for (index, op) in program.operations.iter().enumerate() {
        let context = operation_context(op);
        let key = format!(
            "{}/{}{}@{}",
            op.manager.as_str(),
            op.name.as_str(),
            op.variation
                .as_ref()
                .map(|v| format!("#{}", v.as_str()))
                .unwrap_or_default(),
            op.api_version
                .as_ref()
                .map(|v| v.0.as_str())
                .unwrap_or("unversioned"),
        );
        if !op_keys.insert(key.clone()) {
            errors.push(SemaError::DuplicateOperation { key });
        }
        let mut param_keys: HashSet<(ir::ParamLocation, &str)> = HashSet::new();
        for param in &op.params {
            if !param_keys.insert((param.location, param.wire_name.as_str())) {
                errors.push(SemaError::DuplicateParam {
                    context: context.clone(),
                    location: param.location,
                    wire_name: param.wire_name.clone(),
                });
            }
            check_type(program, &context, &param.ty, false, &mut errors);
        }
        if let Some(body) = &op.request {
            check_type(program, &context, &body.ty, false, &mut errors);
        }
        match &op.response {
            ir::ResponseShape::Json(ty) => check_type(program, &context, ty, false, &mut errors),
            ir::ResponseShape::None
            | ir::ResponseShape::Binary
            | ir::ResponseShape::Text
            | ir::ResponseShape::Redirect => {}
        }
        managers
            .entry(op.manager.as_str().to_string())
            .or_default()
            .push(index);
    }

    if errors.is_empty() {
        Ok(Analysis { program, managers })
    } else {
        errors.sort_by_key(|e| !e.is_engine_bug());
        Err(errors)
    }
}

/// Walk a type: every `DeclId` must be in bounds, and `Optional` must
/// never directly wrap `Optional`.
fn check_type(
    program: &ir::Program,
    context: &str,
    ty: &ir::Type,
    inside_optional: bool,
    errors: &mut Vec<SemaError>,
) {
    match ty {
        ir::Type::Optional(inner) => {
            if inside_optional {
                errors.push(SemaError::DoubleOptional {
                    context: context.to_string(),
                });
            }
            check_type(program, context, inner, true, errors);
        }
        ir::Type::List(inner) | ir::Type::Map(inner) => {
            check_type(program, context, inner, false, errors);
        }
        ir::Type::Decl(id) => {
            if id.0 as usize >= program.decls.len() {
                errors.push(SemaError::DanglingRef {
                    context: context.to_string(),
                    id: id.0,
                    decl_count: program.decls.len(),
                });
            }
        }
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

fn module_string(module: &ir::ModulePath) -> String {
    module
        .0
        .iter()
        .map(ir::Identifier::as_str)
        .collect::<Vec<_>>()
        .join("::")
}

fn operation_context(op: &ir::Operation) -> String {
    format!("operation {}/{}", op.manager.as_str(), op.name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> ir::Identifier {
        ir::Identifier::new(name).unwrap()
    }

    fn decl(name: &str, kind: ir::DeclKind) -> ir::Decl {
        ir::Decl {
            name: ident(name),
            module: ir::ModulePath(vec![ident("schemas")]),
            api_version: None,
            kind,
        }
    }

    fn program_with(decls: Vec<ir::Decl>) -> ir::Program {
        ir::Program {
            decls,
            operations: vec![],
        }
    }

    #[test]
    fn a_well_formed_program_verifies() {
        let program = program_with(vec![decl(
            "File",
            ir::DeclKind::Struct(ir::StructDecl {
                fields: vec![ir::Field {
                    name: ident("id"),
                    wire_name: "id".into(),
                    ty: ir::Type::String,
                }],
            }),
        )]);
        let analysis = analyze(&program).unwrap();
        assert!(analysis.managers.is_empty());
    }

    #[test]
    fn dangling_ids_and_double_optionals_are_engine_bugs_and_sort_first() {
        let program = program_with(vec![
            decl(
                "Bad",
                ir::DeclKind::Struct(ir::StructDecl {
                    fields: vec![
                        ir::Field {
                            name: ident("dangling"),
                            wire_name: "dangling".into(),
                            ty: ir::Type::Decl(ir::DeclId(99)),
                        },
                        ir::Field {
                            name: ident("double"),
                            wire_name: "double".into(),
                            ty: ir::Type::Optional(Box::new(ir::Type::Optional(Box::new(
                                ir::Type::String,
                            )))),
                        },
                    ],
                }),
            ),
            decl(
                "AlsoBad",
                ir::DeclKind::Enum(ir::EnumDecl {
                    values: vec![],
                    extensibility: ir::Extensibility::Open,
                }),
            ),
        ]);
        let errors = analyze(&program).unwrap_err();
        assert_eq!(errors.len(), 3);
        // Engine bugs sort before spec-level findings.
        assert!(errors[0].is_engine_bug() && errors[1].is_engine_bug());
        assert!(!errors[2].is_engine_bug());
        assert!(matches!(errors[2], SemaError::EmptyEnum { .. }));
    }

    #[test]
    fn duplicate_wire_names_and_decl_names_are_reported() {
        let field = |wire: &str| ir::Field {
            name: ident(wire),
            wire_name: wire.into(),
            ty: ir::Type::String,
        };
        let program = program_with(vec![
            decl(
                "File",
                ir::DeclKind::Struct(ir::StructDecl {
                    fields: vec![field("id"), field("id")],
                }),
            ),
            decl("File", ir::DeclKind::Alias(ir::Type::String)),
        ]);
        let errors = analyze(&program).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, SemaError::DuplicateWireName { .. }))
        );
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, SemaError::DuplicateDeclName { .. }))
        );
    }

    #[test]
    fn discriminated_unions_need_distinct_values() {
        let variant = |value: Option<&str>| ir::UnionVariant {
            discriminator_value: value.map(String::from),
            ty: ir::Type::String,
        };
        let union = |variants| {
            ir::DeclKind::Union(ir::UnionDecl {
                discriminator: Some("type".into()),
                variants,
                extensibility: ir::Extensibility::Open,
            })
        };
        // Duplicate values.
        let program = program_with(vec![decl(
            "A",
            union(vec![variant(Some("x")), variant(Some("x"))]),
        )]);
        assert!(matches!(
            analyze(&program).unwrap_err().as_slice(),
            [SemaError::BadDiscriminatorValues { .. }]
        ));
        // A missing value on a discriminated union.
        let program = program_with(vec![decl(
            "B",
            union(vec![variant(Some("x")), variant(None)]),
        )]);
        assert!(matches!(
            analyze(&program).unwrap_err().as_slice(),
            [SemaError::BadDiscriminatorValues { .. }]
        ));
    }

    #[test]
    fn duplicate_operations_are_reported_and_managers_indexed() {
        let op = || ir::Operation {
            name: ident("get_files_id"),
            variation: None,
            manager: ident("files"),
            api_version: Some(ir::ApiVersion("2024.0".into())),
            method: ir::HttpMethod::Get,
            base_url: ir::BaseUrl::Api,
            path: vec![ir::PathSegment::Literal("files".into())],
            params: vec![],
            request: None,
            response: ir::ResponseShape::None,
            deprecated: false,
        };
        let mut program = program_with(vec![]);
        program.operations = vec![op(), op()];
        let errors = analyze(&program).unwrap_err();
        assert!(matches!(
            errors.as_slice(),
            [SemaError::DuplicateOperation { .. }]
        ));

        // Distinct variation → no duplicate; both group under one manager.
        program.operations[1].variation = Some(ident("refresh"));
        let analysis = analyze(&program).unwrap();
        assert_eq!(analysis.managers.len(), 1);
        assert_eq!(analysis.managers["files"], vec![0, 1]);
    }
}
