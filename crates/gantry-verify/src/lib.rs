//! Verification (VR) and spec evolution (FR-9).
//!
//! The conformance checklist (VR-3), determinism double-generate diff
//! (VR-5), skip-summary allowlist (VR-6), round-trip fixtures (VR-4), and
//! the spec-diff/breaking-change report (FR-9).
//!
//! Grows alongside the Go backend in M3; this crate is the
//! seam. Shipped so far: the FR-9 [`diff`] module and the VR-3
//! [`conformance`] checklist.

pub mod conformance;
pub mod diff;

pub use conformance::{
    Check, CheckStatus, ConformanceReport, Exclusion, GeneratedView, TargetShape, apex_shape,
    conformance, go_shape, rust_shape, typescript_shape,
};
pub use diff::{Change, ChangeKind, Severity, SpecDiff, VersionBump, diff};

#[cfg(test)]
mod tests {
    use gantry_ir as ir;

    use crate::diff::{ChangeKind, Severity, VersionBump, diff};

    fn ident(name: &str) -> ir::Identifier {
        ir::Identifier::new(name).unwrap()
    }

    fn schema_module() -> ir::ModulePath {
        ir::ModulePath(vec![ident("schemas")])
    }

    fn struct_decl(name: &str, fields: Vec<ir::Field>) -> ir::Decl {
        ir::Decl {
            name: ident(name),
            module: schema_module(),
            api_version: None,
            kind: ir::DeclKind::Struct(ir::StructDecl { fields }),
        }
    }

    fn field(name: &str, ty: ir::Type) -> ir::Field {
        ir::Field {
            name: ident(name),
            wire_name: name.to_string(),
            ty,
        }
    }

    fn operation(name: &str) -> ir::Operation {
        ir::Operation {
            name: ident(name),
            variation: None,
            manager: ident("files"),
            api_version: None,
            method: ir::HttpMethod::Get,
            base_url: ir::BaseUrl::Api,
            path: vec![ir::PathSegment::Literal("files".into())],
            params: vec![],
            request: None,
            response: ir::ResponseShape::None,
            deprecated: false,
        }
    }

    #[test]
    fn identical_programs_have_no_diff() {
        let mut program = ir::Program::default();
        program.add(struct_decl("File", vec![field("id", ir::Type::String)]));
        program.operations.push(operation("GetFiles"));

        let result = diff(&program, &program);
        assert!(result.changes.is_empty());
        assert_eq!(result.bump(), VersionBump::None);
    }

    #[test]
    fn added_schema_and_operation_are_compatible() {
        let mut old = ir::Program::default();
        old.add(struct_decl("File", vec![field("id", ir::Type::String)]));

        let mut new = old.clone();
        new.add(struct_decl("Folder", vec![field("id", ir::Type::String)]));
        new.operations.push(operation("GetFolders"));

        let result = diff(&old, &new);
        assert_eq!(result.breaking(), 0);
        assert_eq!(result.compatible(), 2);
        assert_eq!(result.bump(), VersionBump::Minor);
        assert!(
            result
                .changes
                .iter()
                .all(|c| c.kind == ChangeKind::Added && c.severity == Severity::Compatible)
        );
    }

    #[test]
    fn removed_schema_is_breaking() {
        let mut old = ir::Program::default();
        old.add(struct_decl("File", vec![field("id", ir::Type::String)]));
        old.add(struct_decl("Folder", vec![field("id", ir::Type::String)]));

        let mut new = ir::Program::default();
        new.add(struct_decl("File", vec![field("id", ir::Type::String)]));

        let result = diff(&old, &new);
        assert_eq!(result.breaking(), 1);
        assert_eq!(result.bump(), VersionBump::Major);
        let removed = &result.changes[0];
        assert_eq!(removed.kind, ChangeKind::Removed);
        assert_eq!(removed.category, "schema");
        assert!(removed.name.contains("Folder"));
    }

    #[test]
    fn field_type_change_is_breaking_but_added_field_is_not() {
        let mut old = ir::Program::default();
        old.add(struct_decl("File", vec![field("size", ir::Type::String)]));

        let mut new = ir::Program::default();
        new.add(struct_decl(
            "File",
            vec![
                field("size", ir::Type::Int64), // type changed: breaking
                field("etag", ir::Type::Optional(Box::new(ir::Type::String))), // added: ok
            ],
        ));

        let result = diff(&old, &new);
        assert_eq!(result.breaking(), 1);
        assert_eq!(result.bump(), VersionBump::Major);
        let change = &result.changes[0];
        assert_eq!(change.kind, ChangeKind::Changed);
        assert!(change.detail.contains("size"), "detail: {}", change.detail);
        assert!(
            change.detail.contains("+field etag"),
            "detail: {}",
            change.detail
        );
    }

    #[test]
    fn open_enum_value_added_is_compatible_removed_is_breaking() {
        let enum_decl = |values: Vec<&str>| ir::Decl {
            name: ident("ItemType"),
            module: schema_module(),
            api_version: None,
            kind: ir::DeclKind::Enum(ir::EnumDecl {
                values: values.into_iter().map(String::from).collect(),
                extensibility: ir::Extensibility::Open,
            }),
        };
        let mut old = ir::Program::default();
        old.add(enum_decl(vec!["file", "folder"]));
        let mut added = ir::Program::default();
        added.add(enum_decl(vec!["file", "folder", "web_link"]));
        let mut removed = ir::Program::default();
        removed.add(enum_decl(vec!["file"]));

        assert_eq!(diff(&old, &added).bump(), VersionBump::Minor);
        assert_eq!(diff(&old, &removed).bump(), VersionBump::Major);
    }

    #[test]
    fn new_required_param_breaks_new_optional_does_not() {
        let with_param = |ty: ir::Type| {
            let mut op = operation("GetFiles");
            op.params.push(ir::Param {
                name: ident("fields"),
                wire_name: "fields".into(),
                location: ir::ParamLocation::Query,
                ty,
            });
            let mut program = ir::Program::default();
            program.operations.push(op);
            program
        };
        let mut base = ir::Program::default();
        base.operations.push(operation("GetFiles"));

        let required = with_param(ir::Type::String);
        assert_eq!(diff(&base, &required).bump(), VersionBump::Major);

        let optional = with_param(ir::Type::Optional(Box::new(ir::Type::String)));
        assert_eq!(diff(&base, &optional).bump(), VersionBump::Minor);
    }

    #[test]
    fn deprecation_is_compatible() {
        let mut old = ir::Program::default();
        old.operations.push(operation("GetFiles"));
        let mut new = ir::Program::default();
        let mut op = operation("GetFiles");
        op.deprecated = true;
        new.operations.push(op);

        let result = diff(&old, &new);
        assert_eq!(result.bump(), VersionBump::Minor);
        assert_eq!(result.breaking(), 0);
        assert!(result.changes[0].detail.contains("deprecated"));
    }

    #[test]
    fn report_is_deterministic_and_readable() {
        let mut old = ir::Program::default();
        old.add(struct_decl("File", vec![field("id", ir::Type::String)]));
        old.add(struct_decl("Folder", vec![field("id", ir::Type::String)]));
        let mut new = ir::Program::default();
        new.add(struct_decl("File", vec![field("id", ir::Type::Int64)]));

        let once = diff(&old, &new).report();
        let twice = diff(&old, &new).report();
        assert_eq!(once, twice);
        assert!(once.contains("recommended bump: major"));
        assert!(once.contains("BREAK"));
    }
}
