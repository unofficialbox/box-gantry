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
use std::collections::BTreeSet;
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

    #[error(
        "{context}: {detail} — optionality wrappers must nest canonically \
         (Optional<Nullable<T>>, D-110) — engine bug"
    )]
    BadNullability {
        context: String,
        detail: &'static str,
    },

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
            Self::DanglingRef { .. }
            | Self::DoubleOptional { .. }
            | Self::BadNullability { .. } => true,
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
    /// Which managers' operations transitively reach each declaration,
    /// indexed **positionally** by `DeclId` (== position in `program.decls`),
    /// so `decl_managers.len() == program.decls.len()` always holds. An empty
    /// set means no operation reaches the declaration (an orphan schema) —
    /// stated explicitly rather than by absence, so a bucketing bug cannot
    /// masquerade as an orphan (NF-1). Backends use it to split a module's
    /// models across per-manager files; see [`Analysis::bucket_decls`].
    pub decl_managers: Vec<BTreeSet<String>>,
}

impl Analysis<'_> {
    /// The single manager whose operations are the *only* ones reaching a
    /// declaration, or `None` when two or more managers share it or none
    /// reaches it at all.
    pub fn sole_manager(&self, decl: usize) -> Option<&str> {
        let owners = &self.decl_managers[decl];
        if owners.len() == 1 {
            owners.iter().next().map(String::as_str)
        } else {
            None
        }
    }

    /// Split a module's declaration indices into the shared catch-all and the
    /// per-manager buckets: a declaration exclusively reached by one manager
    /// goes to that manager's bucket, everything else (shared by two or more,
    /// or reached by none) stays in the catch-all.
    ///
    /// A **partition**, never a filter — every input index appears in
    /// exactly one output group and program order is preserved within each,
    /// so a bucketing bug cannot silently drop a declaration (NF-1). Bucket
    /// keys are sorted (`BTreeMap`), so file emission is deterministic
    /// (FR-6.2).
    pub fn bucket_decls(&self, indices: &[usize]) -> (Vec<usize>, BTreeMap<String, Vec<usize>>) {
        let mut shared = Vec::new();
        let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &index in indices {
            match self.sole_manager(index) {
                Some(manager) => buckets.entry(manager.to_string()).or_default().push(index),
                None => shared.push(index),
            }
        }
        (shared, buckets)
    }
}

/// Push every declaration a type names, descending through the wrapper
/// constructors. Arms are enumerated, never wildcarded — a new `ir::Type`
/// variant breaks this walk at compile time rather than silently losing a
/// reference (FR-2.1, NF-1).
pub fn decl_refs(ty: &ir::Type, out: &mut Vec<ir::DeclId>) {
    match ty {
        ir::Type::Decl(id) => out.push(*id),
        ir::Type::Optional(inner)
        | ir::Type::Nullable(inner)
        | ir::Type::List(inner)
        | ir::Type::Map(inner) => decl_refs(inner, out),
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

/// Which managers' operations transitively reach each declaration.
///
/// Roots come from every operation's signature — `params[].ty`,
/// `request.ty`, and a `ResponseShape::Json` payload; `None`/`Binary`/`Text`/
/// `Redirect` name no declaration. From each root the decl graph is closed
/// over fields, `extra` bags, union variants, and alias targets.
///
/// One breadth-first pass **per manager** with one visited set, so a
/// declaration reached by twenty of a manager's operations is walked once,
/// and the self-referential and mutually-recursive shapes the real spec
/// contains (a tree schema referencing itself) terminate instead of
/// recursing forever.
///
/// Only called after every finding has been collected and none was reported,
/// so every `DeclId` is known in bounds (`check_type`'s `DanglingRef` arm)
/// and indexing `program.decls` cannot panic.
fn decl_managers(
    program: &ir::Program,
    managers: &BTreeMap<String, Vec<usize>>,
) -> Vec<BTreeSet<String>> {
    let mut owners: Vec<BTreeSet<String>> = vec![BTreeSet::new(); program.decls.len()];
    for (manager, op_indices) in managers {
        // Roots: every declaration this manager's operations name directly.
        let mut queue: Vec<ir::DeclId> = Vec::new();
        for &op_index in op_indices {
            let op = &program.operations[op_index];
            for param in &op.params {
                decl_refs(&param.ty, &mut queue);
            }
            if let Some(body) = &op.request {
                decl_refs(&body.ty, &mut queue);
            }
            match &op.response {
                ir::ResponseShape::Json(ty) => decl_refs(ty, &mut queue),
                ir::ResponseShape::None
                | ir::ResponseShape::Binary
                | ir::ResponseShape::Text
                | ir::ResponseShape::Redirect => {}
            }
        }
        // Transitive closure. The visited set is what makes a cyclic decl
        // graph terminate; without it this does not halt.
        let mut seen: HashSet<u32> = HashSet::new();
        while let Some(id) = queue.pop() {
            if !seen.insert(id.0) {
                continue;
            }
            let index = id.0 as usize;
            owners[index].insert(manager.clone());
            match &program.decls[index].kind {
                ir::DeclKind::Struct(s) => {
                    for field in &s.fields {
                        decl_refs(&field.ty, &mut queue);
                    }
                    if let Some(extra) = &s.extra {
                        decl_refs(extra, &mut queue);
                    }
                }
                ir::DeclKind::Union(u) => {
                    for variant in &u.variants {
                        decl_refs(&variant.ty, &mut queue);
                    }
                }
                ir::DeclKind::Alias(ty) => decl_refs(ty, &mut queue),
                ir::DeclKind::Enum(_) => {}
            }
        }
    }
    owners
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
                    check_type(program, &context, &field.ty, Wrapper::None, &mut errors);
                }
                if let Some(extra) = &s.extra {
                    check_type(program, &context, extra, Wrapper::None, &mut errors);
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
                    check_type(program, &context, &variant.ty, Wrapper::None, &mut errors);
                }
            }
            ir::DeclKind::Enum(e) => {
                if e.values.is_empty() {
                    errors.push(SemaError::EmptyEnum {
                        context: context.clone(),
                    });
                }
            }
            ir::DeclKind::Alias(ty) => {
                check_type(program, &context, ty, Wrapper::None, &mut errors)
            }
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
            check_type(program, &context, &param.ty, Wrapper::None, &mut errors);
        }
        if let Some(body) = &op.request {
            check_type(program, &context, &body.ty, Wrapper::None, &mut errors);
        }
        match &op.response {
            ir::ResponseShape::Json(ty) => {
                check_type(program, &context, ty, Wrapper::None, &mut errors)
            }
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
        // Reachability runs only on a program with no findings, so every
        // `DeclId` is in bounds and the walk cannot panic.
        let decl_managers = decl_managers(program, &managers);
        Ok(Analysis {
            program,
            managers,
            decl_managers,
        })
    } else {
        errors.sort_by_key(|e| !e.is_engine_bug());
        Err(errors)
    }
}

/// What optionality wrapper we are directly inside of, for canonical
/// nesting checks (D-110: only `Optional<Nullable<T>>` may stack).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wrapper {
    None,
    Optional,
    Nullable,
}

/// Walk a type: every `DeclId` must be in bounds, and optionality
/// wrappers must nest canonically.
fn check_type(
    program: &ir::Program,
    context: &str,
    ty: &ir::Type,
    wrapper: Wrapper,
    errors: &mut Vec<SemaError>,
) {
    match ty {
        ir::Type::Optional(inner) => {
            match wrapper {
                Wrapper::Optional => errors.push(SemaError::DoubleOptional {
                    context: context.to_string(),
                }),
                Wrapper::Nullable => errors.push(SemaError::BadNullability {
                    context: context.to_string(),
                    detail: "Nullable<Optional<…>> (Optional must be outermost)",
                }),
                Wrapper::None => {}
            }
            check_type(program, context, inner, Wrapper::Optional, errors);
        }
        ir::Type::Nullable(inner) => {
            if wrapper == Wrapper::Nullable {
                errors.push(SemaError::BadNullability {
                    context: context.to_string(),
                    detail: "Nullable<Nullable<…>>",
                });
            }
            check_type(program, context, inner, Wrapper::Nullable, errors);
        }
        ir::Type::List(inner) | ir::Type::Map(inner) => {
            check_type(program, context, inner, Wrapper::None, errors);
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

    fn program_full(decls: Vec<ir::Decl>, operations: Vec<ir::Operation>) -> ir::Program {
        ir::Program { decls, operations }
    }

    fn op_full(
        name: &str,
        manager: &str,
        params: Vec<ir::Param>,
        request: Option<ir::RequestBody>,
        response: ir::ResponseShape,
    ) -> ir::Operation {
        ir::Operation {
            name: ident(name),
            variation: None,
            manager: ident(manager),
            api_version: None,
            method: ir::HttpMethod::Get,
            base_url: ir::BaseUrl::Api,
            path: vec![],
            params,
            request,
            response,
            deprecated: false,
        }
    }

    fn empty_struct(name: &str) -> ir::Decl {
        decl(
            name,
            ir::DeclKind::Struct(ir::StructDecl {
                fields: vec![],
                extra: None,
            }),
        )
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
                extra: None,
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
                    extra: None,
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
                    extra: None,
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

    #[test]
    fn nullability_must_nest_canonically() {
        let field = |name: &str, ty: ir::Type| ir::Field {
            name: ident(name),
            wire_name: name.into(),
            ty,
        };
        let program = program_with(vec![decl(
            "Bad",
            ir::DeclKind::Struct(ir::StructDecl {
                extra: None,
                fields: vec![
                    field(
                        "double_null",
                        ir::Type::Nullable(Box::new(ir::Type::Nullable(Box::new(
                            ir::Type::String,
                        )))),
                    ),
                    field(
                        "inverted",
                        ir::Type::Nullable(Box::new(ir::Type::Optional(Box::new(
                            ir::Type::String,
                        )))),
                    ),
                    field(
                        "canonical",
                        ir::Type::Optional(Box::new(ir::Type::Nullable(Box::new(
                            ir::Type::String,
                        )))),
                    ),
                ],
            }),
        )]);
        let errors = analyze(&program).unwrap_err();
        // Two engine bugs; the canonical field contributes none.
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().all(SemaError::is_engine_bug));
        assert!(
            errors
                .iter()
                .all(|e| matches!(e, SemaError::BadNullability { .. }))
        );
    }

    // --- decl_managers / bucket_decls (D-201: per-manager schema files) ---

    #[test]
    fn a_decl_used_by_one_manager_has_one_owner() {
        let program = program_full(
            vec![empty_struct("A"), empty_struct("B")],
            vec![
                op_full(
                    "get",
                    "files",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(0))),
                ),
                op_full(
                    "get",
                    "folders",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(1))),
                ),
                op_full(
                    "get2",
                    "files",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(1))),
                ),
            ],
        );
        let analysis = analyze(&program).unwrap();
        assert_eq!(
            analysis.decl_managers[0],
            BTreeSet::from(["files".to_string()])
        );
        assert_eq!(
            analysis.decl_managers[1],
            BTreeSet::from(["files".to_string(), "folders".to_string()])
        );
        assert_eq!(analysis.sole_manager(0), Some("files"));
        assert_eq!(analysis.sole_manager(1), None);
    }

    #[test]
    fn reachability_is_transitive_through_fields() {
        let program = program_full(
            vec![
                empty_struct("Item"),
                decl(
                    "Envelope",
                    ir::DeclKind::Struct(ir::StructDecl {
                        fields: vec![ir::Field {
                            name: ident("items"),
                            wire_name: "items".into(),
                            ty: ir::Type::List(Box::new(ir::Type::Decl(ir::DeclId(0)))),
                        }],
                        extra: None,
                    }),
                ),
            ],
            vec![op_full(
                "list",
                "files",
                vec![],
                None,
                ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(1))),
            )],
        );
        let analysis = analyze(&program).unwrap();
        assert_eq!(analysis.sole_manager(1), Some("files"));
        assert_eq!(analysis.sole_manager(0), Some("files"));
    }

    #[test]
    fn reachability_terminates_on_a_cyclic_decl_graph() {
        // Node.children: List<Node>, Node.peer: Optional<Other>;
        // Other.back: Optional<Node>. A missing visited set makes this hang.
        let program = program_full(
            vec![
                decl(
                    "Node",
                    ir::DeclKind::Struct(ir::StructDecl {
                        fields: vec![
                            ir::Field {
                                name: ident("children"),
                                wire_name: "children".into(),
                                ty: ir::Type::List(Box::new(ir::Type::Decl(ir::DeclId(0)))),
                            },
                            ir::Field {
                                name: ident("peer"),
                                wire_name: "peer".into(),
                                ty: ir::Type::Optional(Box::new(ir::Type::Decl(ir::DeclId(1)))),
                            },
                        ],
                        extra: None,
                    }),
                ),
                decl(
                    "Other",
                    ir::DeclKind::Struct(ir::StructDecl {
                        fields: vec![ir::Field {
                            name: ident("back"),
                            wire_name: "back".into(),
                            ty: ir::Type::Optional(Box::new(ir::Type::Decl(ir::DeclId(0)))),
                        }],
                        extra: None,
                    }),
                ),
            ],
            vec![op_full(
                "get",
                "trees",
                vec![],
                None,
                ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(0))),
            )],
        );
        let analysis = analyze(&program).unwrap();
        assert_eq!(analysis.sole_manager(0), Some("trees"));
        assert_eq!(analysis.sole_manager(1), Some("trees"));
    }

    #[test]
    fn reachability_walks_params_request_and_json_responses() {
        let program = program_full(
            vec![
                decl(
                    "ViaParam",
                    ir::DeclKind::Enum(ir::EnumDecl {
                        values: vec!["a".into()],
                        extensibility: ir::Extensibility::Open,
                    }),
                ),
                empty_struct("ViaRequest"),
                empty_struct("ViaResponse"),
                empty_struct("Unreached"),
            ],
            vec![
                op_full(
                    "create",
                    "files",
                    vec![ir::Param {
                        name: ident("filter"),
                        wire_name: "filter".into(),
                        location: ir::ParamLocation::Query,
                        ty: ir::Type::Decl(ir::DeclId(0)),
                    }],
                    Some(ir::RequestBody {
                        media: ir::RequestMedia::Json,
                        ty: ir::Type::Decl(ir::DeclId(1)),
                    }),
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(2))),
                ),
                // A binary response names no declaration.
                op_full("download", "files", vec![], None, ir::ResponseShape::Binary),
            ],
        );
        let analysis = analyze(&program).unwrap();
        assert_eq!(analysis.sole_manager(0), Some("files"));
        assert_eq!(analysis.sole_manager(1), Some("files"));
        assert_eq!(analysis.sole_manager(2), Some("files"));
        assert!(analysis.decl_managers[3].is_empty());
    }

    #[test]
    fn reachability_follows_aliases_unions_and_extra_bags() {
        let program = program_full(
            vec![
                empty_struct("Target"),
                decl(
                    "Aliased",
                    ir::DeclKind::Alias(ir::Type::Decl(ir::DeclId(0))),
                ),
                decl(
                    "VariantType",
                    ir::DeclKind::Struct(ir::StructDecl {
                        fields: vec![ir::Field {
                            name: ident("kind"),
                            wire_name: "kind".into(),
                            ty: ir::Type::String,
                        }],
                        extra: None,
                    }),
                ),
                decl(
                    "Choice",
                    ir::DeclKind::Union(ir::UnionDecl {
                        discriminator: Some("kind".into()),
                        variants: vec![ir::UnionVariant {
                            discriminator_value: Some("v".into()),
                            ty: ir::Type::Decl(ir::DeclId(2)),
                        }],
                        extensibility: ir::Extensibility::Open,
                    }),
                ),
                empty_struct("ExtraValue"),
                decl(
                    "WithExtra",
                    ir::DeclKind::Struct(ir::StructDecl {
                        fields: vec![],
                        extra: Some(ir::Type::Decl(ir::DeclId(4))),
                    }),
                ),
            ],
            vec![
                op_full(
                    "aliasOp",
                    "widgets",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(1))),
                ),
                op_full(
                    "unionOp",
                    "widgets",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(3))),
                ),
                op_full(
                    "extraOp",
                    "widgets",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(5))),
                ),
            ],
        );
        let analysis = analyze(&program).unwrap();
        assert_eq!(analysis.sole_manager(0), Some("widgets")); // Target, via Aliased
        assert_eq!(analysis.sole_manager(2), Some("widgets")); // VariantType, via the union
        assert_eq!(analysis.sole_manager(4), Some("widgets")); // ExtraValue, via the extra bag
    }

    #[test]
    fn an_unreferenced_decl_has_no_owner() {
        let program = program_with(vec![empty_struct("Orphan")]);
        let analysis = analyze(&program).unwrap();
        assert_eq!(analysis.decl_managers.len(), 1);
        assert!(analysis.decl_managers[0].is_empty());
    }

    #[test]
    fn sole_manager_is_none_for_shared_and_orphan_decls() {
        let program = program_full(
            vec![empty_struct("Shared"), empty_struct("Orphan")],
            vec![
                op_full(
                    "a",
                    "files",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(0))),
                ),
                op_full(
                    "b",
                    "folders",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(0))),
                ),
            ],
        );
        let analysis = analyze(&program).unwrap();
        assert_eq!(analysis.sole_manager(0), None);
        assert_eq!(analysis.sole_manager(1), None);
    }

    #[test]
    fn bucket_decls_is_a_partition() {
        let program = program_full(
            vec![
                empty_struct("Sole"),
                empty_struct("Shared"),
                empty_struct("Orphan"),
            ],
            vec![
                op_full(
                    "a",
                    "files",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(0))),
                ),
                op_full(
                    "b",
                    "files",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(1))),
                ),
                op_full(
                    "c",
                    "folders",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(1))),
                ),
            ],
        );
        let analysis = analyze(&program).unwrap();
        let indices = vec![0, 1, 2];
        let (shared, buckets) = analysis.bucket_decls(&indices);
        assert_eq!(shared, vec![1, 2]);
        assert_eq!(buckets.get("files"), Some(&vec![0]));
        assert_eq!(buckets.get("folders"), None);
        // Partition: every input index appears exactly once across the
        // catch-all and the buckets — none lost, none duplicated.
        let mut all: Vec<usize> = shared.clone();
        all.extend(buckets.values().flatten().copied());
        all.sort_unstable();
        assert_eq!(all, indices);
    }

    #[test]
    fn a_bucketed_decl_never_references_another_bucket() {
        // A sole-owned-by-"files" decl referencing a decl also reached
        // directly by "folders" pulls "files" into that decl's owner set too
        // — it becomes shared, never sole-owned by a *different* manager than
        // its referrer. This is the invariant PR 3/4's cross-file reference
        // resolution (Rust `super::`, TS's fixed catch-all-or-own-bucket
        // imports) both rely on.
        let program = program_full(
            vec![
                empty_struct("B"),
                decl(
                    "A",
                    ir::DeclKind::Struct(ir::StructDecl {
                        fields: vec![ir::Field {
                            name: ident("b"),
                            wire_name: "b".into(),
                            ty: ir::Type::Decl(ir::DeclId(0)),
                        }],
                        extra: None,
                    }),
                ),
            ],
            vec![
                op_full(
                    "getA",
                    "files",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(1))),
                ),
                op_full(
                    "getB",
                    "folders",
                    vec![],
                    None,
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(0))),
                ),
            ],
        );
        let analysis = analyze(&program).unwrap();
        assert_eq!(analysis.sole_manager(0), None); // B: shared (files + folders)
        assert_eq!(analysis.sole_manager(1), Some("files")); // A: sole
    }
}
