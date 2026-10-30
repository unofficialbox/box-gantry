//! Zip-download detection (Box's `downloadZip`).
//!
//! Box packs several items into one archive in two steps: a JSON `POST`
//! creates the zip and answers with a `download_url`, and the archive is then
//! fetched from the download host. The generated content method takes the
//! zip's id, which callers would otherwise have to dig out of that URL; Box's
//! official SDKs instead offer one `downloadZip` call that creates the zip and
//! streams the `download_url` as returned.
//!
//! The pair is found structurally, never from names: a body-only JSON `POST`
//! answered with a struct carrying an optional string `download_url`, and in
//! the same manager a `GET` on the download host, under the same leading path,
//! answered with raw bytes. A partial match is simply not a zip download, and
//! the real-spec test pins exactly which pair is detected, so a spec change
//! that breaks the shape fails loudly there.

use gantry_ir as ir;
use gantry_sema::Analysis;

/// A detected zip download: the operation that creates the archive, the
/// operation that serves its bytes, and the `download_url` field to follow.
#[derive(Debug, Clone)]
pub struct ZipDownload {
    /// Index into `program.operations` of the creating `POST`.
    pub create: usize,
    /// Index into `program.operations` of the content `GET`; the helper
    /// returns what it returns.
    pub content: usize,
    /// The optional string `download_url` on the creating `POST`'s answer.
    pub download_url: ir::Field,
}

/// Detect every zip download in the program, in operation order
/// (deterministic — FR-6.2).
pub fn detect_zip_downloads(analysis: &Analysis<'_>) -> Vec<ZipDownload> {
    let program = analysis.program;
    program
        .operations
        .iter()
        .enumerate()
        .filter_map(|(index, op)| detect_one(program, index, op))
        .collect()
}

fn detect_one(program: &ir::Program, create: usize, op: &ir::Operation) -> Option<ZipDownload> {
    // A JSON-body `POST` with no other parameters, answered with a struct.
    let request = op.request.as_ref()?;
    if op.method != ir::HttpMethod::Post
        || request.media != ir::RequestMedia::Json
        || !op.params.is_empty()
    {
        return None;
    }
    let ir::ResponseShape::Json(ty) = &op.response else {
        return None;
    };
    let ir::Type::Decl(answer) = *ty else {
        return None;
    };
    let ir::DeclKind::Struct(decl) = &program.decl(answer).kind else {
        return None;
    };
    let download_url = decl.fields.iter().find(|f| {
        f.wire_name == "download_url" && f.ty == ir::Type::Optional(Box::new(ir::Type::String))
    })?;

    // Exactly one byte-serving `GET` on the download host in the same manager,
    // under the creating `POST`'s path.
    let mut content = program.operations.iter().enumerate().filter(|(_, other)| {
        other.manager == op.manager
            && other.method == ir::HttpMethod::Get
            && other.base_url == ir::BaseUrl::Download
            && other.response == ir::ResponseShape::Binary
            && other.path.len() > op.path.len()
            && other.path.starts_with(&op.path)
    });
    let (content, _) = content.next()?;
    Some(ZipDownload {
        create,
        content,
        download_url: download_url.clone(),
    })
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

    fn decl(name: &str, fields: Vec<ir::Field>) -> ir::Decl {
        ir::Decl {
            name: ident(name),
            module: ir::ModulePath(vec![ident("schemas")]),
            api_version: None,
            kind: ir::DeclKind::Struct(ir::StructDecl {
                fields,
                extra: None,
            }),
        }
    }

    fn operation(
        name: &str,
        method: ir::HttpMethod,
        base_url: ir::BaseUrl,
        path: Vec<ir::PathSegment>,
        request: Option<ir::RequestBody>,
        response: ir::ResponseShape,
    ) -> ir::Operation {
        let params = path
            .iter()
            .filter_map(|segment| match segment {
                ir::PathSegment::Parameter(name) => Some(ir::Param {
                    name: name.clone(),
                    wire_name: name.as_str().to_string(),
                    location: ir::ParamLocation::Path,
                    ty: ir::Type::String,
                }),
                ir::PathSegment::Literal(_) | ir::PathSegment::Composite(_) => None,
            })
            .collect();
        ir::Operation {
            name: ident(name),
            variation: None,
            manager: ident("zip_downloads"),
            api_version: None,
            method,
            base_url,
            path,
            params,
            request,
            response,
            deprecated: false,
            conditional: None,
        }
    }

    /// Box's shape: Request(0) → Zip(1) from `POST /zip_downloads`, and the
    /// archive from `GET /zip_downloads/{id}/content` on `content_base`.
    fn program(content_base: ir::BaseUrl) -> ir::Program {
        let root = vec![ir::PathSegment::Literal("zip_downloads".into())];
        let mut content_path = root.clone();
        content_path.push(ir::PathSegment::Parameter(ident("zip_download_id")));
        content_path.push(ir::PathSegment::Literal("content".into()));
        ir::Program {
            decls: vec![
                decl(
                    "Request",
                    vec![field("download_file_name", optional(ir::Type::String))],
                ),
                decl(
                    "Zip",
                    vec![
                        field("download_url", optional(ir::Type::String)),
                        field("status_url", optional(ir::Type::String)),
                    ],
                ),
            ],
            operations: vec![
                operation(
                    "create",
                    ir::HttpMethod::Post,
                    ir::BaseUrl::Api,
                    root,
                    Some(ir::RequestBody {
                        media: ir::RequestMedia::Json,
                        ty: ir::Type::Decl(ir::DeclId(0)),
                    }),
                    ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(1))),
                ),
                operation(
                    "get_content",
                    ir::HttpMethod::Get,
                    content_base,
                    content_path,
                    None,
                    ir::ResponseShape::Binary,
                ),
            ],
        }
    }

    fn detect(program: &ir::Program) -> Vec<ZipDownload> {
        let analysis = gantry_sema::analyze(program).expect("the test program verifies");
        detect_zip_downloads(&analysis)
    }

    #[test]
    fn a_post_answering_a_download_url_with_a_download_host_content_get_is_detected() {
        let program = program(ir::BaseUrl::Download);
        let found = detect(&program);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].create, found[0].content), (0, 1));
        assert_eq!(found[0].download_url.wire_name, "download_url");
    }

    #[test]
    fn without_a_content_get_on_the_download_host_it_is_not_a_zip_download() {
        assert!(detect(&program(ir::BaseUrl::Api)).is_empty());
    }
}
