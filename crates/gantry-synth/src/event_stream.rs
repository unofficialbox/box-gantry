//! Event-stream detection (Box's long-poll protocol).
//!
//! Box streams user events by long polling: `OPTIONS` on the events path
//! returns real-time servers (`entries` of `url` / `max_retries` /
//! `retry_timeout`); a `GET` on a server URL blocks until Box answers
//! `new_change` or `reconnect`; on `new_change` the `GET` on the events path
//! with `stream_position` returns the new `entries` and the
//! `next_stream_position` to wait from. Each event carries an `event_id`
//! the caller dedupes on (Box may deliver an event twice).
//!
//! The pair is discovered structurally — an `OPTIONS` and a `GET` on the
//! same path whose parameters and response envelopes have those wire
//! fields — never from an operation's name. Like pagination, a partial
//! match is simply not a stream: the plain methods remain, and the real-spec
//! test pins exactly which pairs are detected, so a spec change that breaks
//! the shape fails loudly there.

use gantry_ir as ir;
use gantry_sema::Analysis;

/// How the page envelope carries the next position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionShape {
    /// A plain string.
    String,
    /// A structural (undiscriminated) union of a string and an integer —
    /// how Box's spec declares `next_stream_position`.
    StringOrInteger,
}

/// A detected event stream: the two operations and every field a backend
/// reads to drive the long-poll loop over the generated typed methods.
/// Fields are the IR's own, so each backend renders them with its own field
/// naming (or wire name).
#[derive(Debug, Clone)]
pub struct EventStream {
    /// Index into `program.operations` of the `GET` page operation.
    pub list: usize,
    /// Index into `program.operations` of the `OPTIONS` server operation.
    pub options: usize,
    /// The page operation's optional query parameter carrying the position.
    pub position_param: ir::Param,
    /// The page envelope struct, and its event list.
    pub page: ir::DeclId,
    pub entries: ir::Field,
    /// The page envelope's next position, and its shape.
    pub next_position: ir::Field,
    pub position: PositionShape,
    /// The event struct, and its optional string `event_id`.
    pub event: ir::DeclId,
    pub event_id: ir::Field,
    /// The server list struct and its entries; the server struct and its
    /// optional `url`, `max_retries` (a string), and `retry_timeout` (seconds).
    pub server_list: ir::DeclId,
    pub servers: ir::Field,
    pub server: ir::DeclId,
    pub url: ir::Field,
    pub max_retries: ir::Field,
    pub retry_timeout: ir::Field,
}

/// Detect every event stream in the program, in page-operation order
/// (deterministic — FR-6.2).
pub fn detect_event_streams(analysis: &Analysis<'_>) -> Vec<EventStream> {
    let program = analysis.program;
    program
        .operations
        .iter()
        .enumerate()
        .filter_map(|(index, op)| detect_one(program, index, op))
        .collect()
}

fn detect_one(program: &ir::Program, list: usize, op: &ir::Operation) -> Option<EventStream> {
    // A plain `GET`: no body, and nothing required beyond the position the
    // stream fills in.
    if op.method != ir::HttpMethod::Get
        || op.request.is_some()
        || op
            .params
            .iter()
            .any(|p| !matches!(p.ty, ir::Type::Optional(_)))
    {
        return None;
    }
    // The position is an optional string query parameter.
    let position_param = op.params.iter().find(|p| {
        p.location == ir::ParamLocation::Query
            && p.wire_name == "stream_position"
            && p.ty == ir::Type::Optional(Box::new(ir::Type::String))
    })?;
    // The page envelope: an event list and the next position.
    let page_id = json_decl(&op.response)?;
    let page = struct_of(program, page_id)?;
    let entries = field(page, "entries")?;
    let event = list_of_decl(&entries.ty)?;
    let next_position = field(page, "next_stream_position")?;
    let position = position_shape(program, &next_position.ty)?;
    let event_id = field(struct_of(program, event)?, "event_id")?;
    if !is_optional(&event_id.ty, &ir::Type::String) {
        return None;
    }

    // Its server operation: `OPTIONS` on the same path and manager, with no
    // parameters, listing servers.
    let (options, options_op) = program.operations.iter().enumerate().find(|(_, o)| {
        o.method == ir::HttpMethod::Options
            && o.path == op.path
            && o.base_url == op.base_url
            && o.manager == op.manager
            && o.params.is_empty()
    })?;
    let server_list = json_decl(&options_op.response)?;
    let servers = field(struct_of(program, server_list)?, "entries")?;
    let server_id = list_of_decl(&servers.ty)?;
    let server = struct_of(program, server_id)?;
    let url = field(server, "url")?;
    let max_retries = field(server, "max_retries")?;
    let retry_timeout = field(server, "retry_timeout")?;
    if !is_optional(&url.ty, &ir::Type::String)
        || !is_optional(&max_retries.ty, &ir::Type::String)
        || !is_optional(&retry_timeout.ty, &ir::Type::Int64)
    {
        return None;
    }

    Some(EventStream {
        list,
        options,
        position_param: position_param.clone(),
        page: page_id,
        entries: entries.clone(),
        next_position: next_position.clone(),
        position,
        event,
        event_id: event_id.clone(),
        server_list,
        servers: servers.clone(),
        server: server_id,
        url: url.clone(),
        max_retries: max_retries.clone(),
        retry_timeout: retry_timeout.clone(),
    })
}

fn json_decl(response: &ir::ResponseShape) -> Option<ir::DeclId> {
    let ir::ResponseShape::Json(ir::Type::Decl(id)) = response else {
        return None;
    };
    Some(*id)
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

/// An optional list of a declaration: the element's id.
fn list_of_decl(ty: &ir::Type) -> Option<ir::DeclId> {
    let ir::Type::Optional(inner) = ty else {
        return None;
    };
    let ir::Type::List(element) = inner.as_ref() else {
        return None;
    };
    let ir::Type::Decl(id) = element.as_ref() else {
        return None;
    };
    Some(*id)
}

fn is_optional(ty: &ir::Type, inner: &ir::Type) -> bool {
    matches!(ty, ir::Type::Optional(t) if t.as_ref() == inner)
}

/// An optional string, or an optional structural union of exactly a string
/// and an integer.
fn position_shape(program: &ir::Program, ty: &ir::Type) -> Option<PositionShape> {
    let ir::Type::Optional(inner) = ty else {
        return None;
    };
    if inner.as_ref() == &ir::Type::String {
        return Some(PositionShape::String);
    }
    let ir::Type::Decl(id) = inner.as_ref() else {
        return None;
    };
    let ir::DeclKind::Union(union) = &program.decl(*id).kind else {
        return None;
    };
    let mut types: Vec<&ir::Type> = union.variants.iter().map(|v| &v.ty).collect();
    types.sort_by_key(|t| matches!(t, ir::Type::Int64));
    (union.discriminator.is_none() && types == [&ir::Type::String, &ir::Type::Int64])
        .then_some(PositionShape::StringOrInteger)
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

    fn op(
        name: &str,
        method: ir::HttpMethod,
        params: Vec<ir::Param>,
        response: u32,
    ) -> ir::Operation {
        ir::Operation {
            name: ident(name),
            variation: None,
            manager: ident("events"),
            api_version: None,
            method,
            base_url: ir::BaseUrl::Api,
            path: vec![ir::PathSegment::Literal("events".into())],
            params,
            request: None,
            response: ir::ResponseShape::Json(ir::Type::Decl(ir::DeclId(response))),
            deprecated: false,
            conditional: None,
        }
    }

    /// Box's shape: Event(0), Events(1), RealtimeServer(2), RealtimeServers(3),
    /// with the page `GET` and the server `OPTIONS` on `/events`.
    fn program(server_has_url: bool) -> ir::Program {
        let event = decl("Event", vec![field("event_id", optional(ir::Type::String))]);
        let events = decl(
            "Events",
            vec![
                field("next_stream_position", optional(ir::Type::String)),
                field(
                    "entries",
                    optional(ir::Type::List(Box::new(ir::Type::Decl(ir::DeclId(0))))),
                ),
            ],
        );
        let mut server_fields = vec![
            field("max_retries", optional(ir::Type::String)),
            field("retry_timeout", optional(ir::Type::Int64)),
        ];
        if server_has_url {
            server_fields.push(field("url", optional(ir::Type::String)));
        }
        let server = decl("RealtimeServer", server_fields);
        let servers = decl(
            "RealtimeServers",
            vec![field(
                "entries",
                optional(ir::Type::List(Box::new(ir::Type::Decl(ir::DeclId(2))))),
            )],
        );
        let position = ir::Param {
            name: ident("stream_position"),
            wire_name: "stream_position".into(),
            location: ir::ParamLocation::Query,
            ty: optional(ir::Type::String),
        };
        ir::Program {
            decls: vec![event, events, server, servers],
            operations: vec![
                op("list", ir::HttpMethod::Get, vec![position], 1),
                op("options", ir::HttpMethod::Options, vec![], 3),
            ],
        }
    }

    #[test]
    fn the_get_and_options_pair_is_an_event_stream() {
        let program = program(true);
        let analysis = gantry_sema::analyze(&program).unwrap();
        let streams = detect_event_streams(&analysis);
        assert_eq!(streams.len(), 1);
        let stream = &streams[0];
        assert_eq!((stream.list, stream.options), (0, 1));
        assert_eq!(stream.position, PositionShape::String);
        assert_eq!(stream.event, ir::DeclId(0));
        assert_eq!(stream.event_id.wire_name, "event_id");
        assert_eq!(stream.url.wire_name, "url");
    }

    #[test]
    fn servers_without_a_url_are_not_a_stream() {
        let program = program(false);
        let analysis = gantry_sema::analyze(&program).unwrap();
        assert!(detect_event_streams(&analysis).is_empty());
    }
}
