//! Token-exchange detection (Box's token downscoping).
//!
//! Box downscopes a token through its token endpoint: a form-encoded `POST`
//! whose `grant_type` is `urn:ietf:params:oauth:grant-type:token-exchange`,
//! carrying the token to restrict (`subject_token`, of `subject_token_type`
//! `urn:ietf:params:oauth:token-type:access_token`), the space-separated
//! `scope` it keeps, and optionally the one item (`resource`, the item's API
//! URL) or shared link (`box_shared_link`) it is limited to. The answer is a
//! plain access token.
//!
//! The operation is found structurally — a URL-encoded body with those wire
//! fields, whose two enums offer those exact values, answered with JSON —
//! never from its name. A partial match is simply not an exchange: the plain
//! token method remains, and the real-spec test pins exactly which operation
//! is detected, so a spec change that breaks the shape fails loudly there.

use gantry_ir as ir;
use gantry_sema::Analysis;

/// The `grant_type` value that asks for a token exchange.
pub const TOKEN_EXCHANGE_GRANT: &str = "urn:ietf:params:oauth:grant-type:token-exchange";

/// The `subject_token_type` value for an access token.
pub const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

/// A detected token exchange: the operation and every body field a backend
/// sets to downscope the session's current token through the generated
/// typed method. Fields are the IR's own, so each backend renders them with
/// its own naming.
#[derive(Debug, Clone)]
pub struct TokenExchange {
    /// Index into `program.operations` of the token `POST`.
    pub operation: usize,
    /// The request body struct (required or not, the typed method takes it).
    pub body: ir::DeclId,
    /// The required `grant_type`, and its enum (offering [`TOKEN_EXCHANGE_GRANT`]).
    pub grant_type: ir::Field,
    pub grant_type_enum: ir::DeclId,
    /// The optional string `subject_token`.
    pub subject_token: ir::Field,
    /// The optional `subject_token_type`, and its enum (offering
    /// [`ACCESS_TOKEN_TYPE`]).
    pub subject_token_type: ir::Field,
    pub subject_token_type_enum: ir::DeclId,
    /// The optional strings `scope`, `resource`, and `box_shared_link`.
    pub scope: ir::Field,
    pub resource: ir::Field,
    pub shared_link: ir::Field,
}

/// Detect every token exchange in the program, in operation order
/// (deterministic — FR-6.2).
pub fn detect_token_exchanges(analysis: &Analysis<'_>) -> Vec<TokenExchange> {
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
) -> Option<TokenExchange> {
    // A form-encoded `POST` with a body and no other parameters, answered
    // with JSON.
    let request = op.request.as_ref()?;
    if op.method != ir::HttpMethod::Post
        || request.media != ir::RequestMedia::UrlEncoded
        || !op.params.is_empty()
        || !matches!(op.response, ir::ResponseShape::Json(_))
    {
        return None;
    }
    let body_ty = if let ir::Type::Optional(inner) = &request.ty {
        &**inner
    } else {
        &request.ty
    };
    let ir::Type::Decl(body) = *body_ty else {
        return None;
    };
    let ir::DeclKind::Struct(decl) = &program.decl(body).kind else {
        return None;
    };
    let field = |wire: &str| decl.fields.iter().find(|f| f.wire_name == wire);
    let optional_string =
        |wire: &str| field(wire).filter(|f| f.ty == ir::Type::Optional(Box::new(ir::Type::String)));

    let grant_type = field("grant_type")?;
    let ir::Type::Decl(grant_type_enum) = grant_type.ty else {
        return None;
    };
    if !offers(program, grant_type_enum, TOKEN_EXCHANGE_GRANT) {
        return None;
    }
    let subject_token_type = field("subject_token_type")?;
    let ir::Type::Optional(inner) = &subject_token_type.ty else {
        return None;
    };
    let ir::Type::Decl(subject_token_type_enum) = **inner else {
        return None;
    };
    if !offers(program, subject_token_type_enum, ACCESS_TOKEN_TYPE) {
        return None;
    }

    Some(TokenExchange {
        operation,
        body,
        grant_type: grant_type.clone(),
        grant_type_enum,
        subject_token: optional_string("subject_token")?.clone(),
        subject_token_type: subject_token_type.clone(),
        subject_token_type_enum,
        scope: optional_string("scope")?.clone(),
        resource: optional_string("resource")?.clone(),
        shared_link: optional_string("box_shared_link")?.clone(),
    })
}

/// Whether `id` is an enum offering `value`.
fn offers(program: &ir::Program, id: ir::DeclId, value: &str) -> bool {
    matches!(&program.decl(id).kind, ir::DeclKind::Enum(e) if e.values.iter().any(|v| v == value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> ir::Identifier {
        ir::Identifier::new(name).unwrap()
    }

    fn optional(ty: ir::Type) -> ir::Type {
        ir::Type::Optional(Box::new(ty))
    }

    fn field(wire: &str, ty: ir::Type) -> ir::Field {
        ir::Field {
            name: ident(wire),
            wire_name: wire.into(),
            ty,
        }
    }

    fn decl(name: &str, kind: ir::DeclKind) -> ir::Decl {
        ir::Decl {
            name: ident(name),
            module: ir::ModulePath(vec![ident("schemas")]),
            api_version: None,
            kind,
        }
    }

    fn enumeration(values: &[&str]) -> ir::DeclKind {
        ir::DeclKind::Enum(ir::EnumDecl {
            values: values.iter().map(|v| v.to_string()).collect(),
            extensibility: ir::Extensibility::Open,
        })
    }

    /// Box's shape: GrantType(0), TokenType(1), PostToken(2), AccessToken(3),
    /// with the token `POST` (`media`) answering `AccessToken`.
    fn program(media: ir::RequestMedia, grants: &[&str]) -> ir::Program {
        let body = vec![
            field("grant_type", ir::Type::Decl(ir::DeclId(0))),
            field("subject_token", optional(ir::Type::String)),
            field(
                "subject_token_type",
                optional(ir::Type::Decl(ir::DeclId(1))),
            ),
            field("scope", optional(ir::Type::String)),
            field("resource", optional(ir::Type::String)),
            field("box_shared_link", optional(ir::Type::String)),
        ];
        ir::Program {
            decls: vec![
                decl("GrantType", enumeration(grants)),
                decl("TokenType", enumeration(&[ACCESS_TOKEN_TYPE])),
                decl(
                    "PostToken",
                    ir::DeclKind::Struct(ir::StructDecl {
                        fields: body,
                        extra: None,
                    }),
                ),
                decl(
                    "AccessToken",
                    ir::DeclKind::Struct(ir::StructDecl {
                        fields: vec![field("access_token", optional(ir::Type::String))],
                        extra: None,
                    }),
                ),
            ],
            operations: vec![ir::Operation {
                name: ident("create_token"),
                variation: None,
                manager: ident("authorization"),
                api_version: None,
                method: ir::HttpMethod::Post,
                base_url: ir::BaseUrl::Api,
                path: vec![ir::PathSegment::Literal("token".into())],
                params: Vec::new(),
                request: Some(ir::RequestBody {
                    media,
                    ty: ir::Type::Decl(ir::DeclId(2)),
                }),
                response: ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(3))),
                deprecated: false,
                conditional: None,
            }],
        }
    }

    fn detect(program: &ir::Program) -> Vec<TokenExchange> {
        let analysis = gantry_sema::analyze(program).expect("the test program verifies");
        detect_token_exchanges(&analysis)
    }

    #[test]
    fn a_form_token_post_offering_the_exchange_grant_is_detected() {
        let program = program(
            ir::RequestMedia::UrlEncoded,
            &["client_credentials", TOKEN_EXCHANGE_GRANT],
        );
        let found = detect(&program);
        assert_eq!(found.len(), 1);
        let exchange = &found[0];
        assert_eq!((exchange.operation, exchange.body), (0, ir::DeclId(2)));
        assert_eq!(exchange.grant_type_enum, ir::DeclId(0));
        assert_eq!(exchange.subject_token_type_enum, ir::DeclId(1));
        assert_eq!(exchange.shared_link.wire_name, "box_shared_link");
    }

    #[test]
    fn without_the_exchange_grant_or_a_form_body_it_is_not_an_exchange() {
        assert!(
            detect(&program(
                ir::RequestMedia::UrlEncoded,
                &["client_credentials"]
            ))
            .is_empty()
        );
        assert!(detect(&program(ir::RequestMedia::Json, &[TOKEN_EXCHANGE_GRANT])).is_empty());
    }
}
