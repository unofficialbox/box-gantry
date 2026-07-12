//! Runtime contracts (FR-5).
//!
//! The hand-written runtime surface each SDK ships, declared
//! machine-readably: name, arity, types, error behavior, and
//! context/cancellation threading (FR-5.1). Generated code calls only
//! through these declarations, and per-target stubs are *rendered from
//! the same data* — so the stub and the contract cannot drift (FR-5.2),
//! and generated output compile-verifies against the stubs without the
//! real SDK runtime (FR-5.3, G-1).
//!
//! Stub rendering keys off the capability manifest's axes (error model,
//! async model), never a language name (FR-4.2).

mod go_stubs;

pub use go_stubs::{GO_MOD, go_stubs};

/// Target-agnostic types at the generated-code ↔ runtime boundary.
///
/// A closed set (FR-2.1 discipline applies here too): a stub renderer
/// that can't map one of these is a compile error, not a hole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractType {
    /// Cancellation/deadline carrier (Go `context.Context`; absent on
    /// platforms without one — the manifest decides).
    Context,
    String,
    Bytes,
    Bool,
    Int64,
    /// A raw JSON value (`Type::JsonValue` at the IR level).
    Json,
    /// The runtime-owned HTTP request envelope.
    Request,
    /// The runtime-owned HTTP response envelope.
    Response,
    /// A streaming body where the platform allows; buffered otherwise
    /// (manifest `Streaming` axis).
    Stream,
}

/// Whether a runtime function needs the per-client session (config: auth,
/// base URLs, HTTP client, retry) or is pure over its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver {
    /// A method on the runtime session/client (holds config).
    Session,
    /// A free function operating only on its arguments (request/response
    /// builders and accessors).
    Free,
}

/// One function of the runtime surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractFn {
    /// Canonical snake_case name; each backend renders its own casing.
    pub name: &'static str,
    pub receiver: Receiver,
    /// Parameters after the (optional) context parameter.
    pub params: &'static [(&'static str, ContractType)],
    /// `None` means the function returns nothing (beyond an error, when
    /// fallible).
    pub ret: Option<ContractType>,
    /// Fallible per the target's error model (manifest axis: Go
    /// `(T, error)`, Apex exceptions, Rust `Result`).
    pub fallible: bool,
    /// The context/cancellation carrier threads through as the first
    /// parameter (D-003, TR-Go.3) on targets that have one.
    pub takes_context: bool,
    /// What the hand-written implementation must do — the behavioral
    /// half of the contract, carried into stub doc comments.
    pub behavior: &'static str,
}

/// The full declared surface for one SDK runtime.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeContract {
    pub functions: &'static [ContractFn],
}

/// The v1 runtime contract — the surface generated code calls.
///
/// Draft until the Go backend consumes it (M3); frozen then, like the
/// manifests (D-109). Informed by the capability contract (R§1):
/// retrying network layer, auth token acquisition, body handling.
pub const V1: RuntimeContract = RuntimeContract {
    functions: &[
        ContractFn {
            name: "fetch",
            receiver: Receiver::Session,
            params: &[("request", ContractType::Request)],
            ret: Some(ContractType::Response),
            fallible: true,
            takes_context: true,
            behavior: "Execute the request with retries: exponential backoff + jitter, \
                       401 token refresh, Retry-After handling (R§1 network layer).",
        },
        ContractFn {
            name: "access_token",
            receiver: Receiver::Session,
            params: &[],
            ret: Some(ContractType::String),
            fallible: true,
            takes_context: true,
            behavior: "Return a valid access token for the configured auth flow, \
                       acquiring or refreshing through the pluggable token store as needed.",
        },
        ContractFn {
            name: "base_url",
            receiver: Receiver::Session,
            params: &[("name", ContractType::String)],
            ret: Some(ContractType::String),
            fallible: false,
            takes_context: false,
            behavior: "The configured base URL for a D-106 class (\"api\", \"api_root\", \
                       \"upload\", \"upload_session\", \"oauth_authorize\", \"download\"), \
                       without a trailing slash. Runtime configuration may override any of \
                       them (custom deployments).",
        },
        ContractFn {
            name: "new_request",
            receiver: Receiver::Session,
            params: &[
                ("method", ContractType::String),
                ("url", ContractType::String),
            ],
            ret: Some(ContractType::Request),
            fallible: false,
            takes_context: false,
            behavior: "Create a request envelope for the given method and fully built URL.",
        },
        ContractFn {
            name: "with_header",
            receiver: Receiver::Free,
            params: &[
                ("request", ContractType::Request),
                ("name", ContractType::String),
                ("value", ContractType::String),
            ],
            ret: Some(ContractType::Request),
            fallible: false,
            takes_context: false,
            behavior: "Return the request with the header set.",
        },
        ContractFn {
            name: "with_query",
            receiver: Receiver::Free,
            params: &[
                ("request", ContractType::Request),
                ("name", ContractType::String),
                ("value", ContractType::String),
            ],
            ret: Some(ContractType::Request),
            fallible: false,
            takes_context: false,
            behavior: "Return the request with the query parameter appended, encoded.",
        },
        ContractFn {
            name: "with_json_body",
            receiver: Receiver::Free,
            params: &[
                ("request", ContractType::Request),
                ("body", ContractType::Bytes),
            ],
            ret: Some(ContractType::Request),
            fallible: false,
            takes_context: false,
            behavior: "Return the request with the serialized JSON body and content type set.",
        },
        ContractFn {
            name: "with_form_body",
            receiver: Receiver::Free,
            params: &[
                ("request", ContractType::Request),
                ("form", ContractType::Bytes),
            ],
            ret: Some(ContractType::Request),
            fallible: false,
            takes_context: false,
            behavior: "Return the request with an application/x-www-form-urlencoded body \
                       (the OAuth2 token endpoints).",
        },
        ContractFn {
            name: "with_stream_body",
            receiver: Receiver::Free,
            params: &[
                ("request", ContractType::Request),
                ("body", ContractType::Stream),
                ("content_type", ContractType::String),
            ],
            ret: Some(ContractType::Request),
            fallible: false,
            takes_context: false,
            behavior: "Return the request with a streaming body (buffered on platforms \
                       whose manifest says so — FR-7.4).",
        },
        ContractFn {
            name: "with_multipart_body",
            receiver: Receiver::Free,
            params: &[
                ("request", ContractType::Request),
                ("attributes", ContractType::Bytes),
                ("file_name", ContractType::String),
                ("file", ContractType::Stream),
            ],
            ret: Some(ContractType::Request),
            fallible: false,
            takes_context: false,
            behavior: "Return the request with a Box-style multipart body: an `attributes` \
                       JSON part plus a file part (G-7).",
        },
        ContractFn {
            name: "response_bytes",
            receiver: Receiver::Free,
            params: &[("response", ContractType::Response)],
            ret: Some(ContractType::Bytes),
            fallible: true,
            takes_context: false,
            behavior: "Read the whole response body.",
        },
        ContractFn {
            name: "response_stream",
            receiver: Receiver::Free,
            params: &[("response", ContractType::Response)],
            ret: Some(ContractType::Stream),
            fallible: false,
            takes_context: false,
            behavior: "The response body as a stream, for binary downloads (FR-7.4).",
        },
        ContractFn {
            name: "response_header",
            receiver: Receiver::Free,
            params: &[
                ("response", ContractType::Response),
                ("name", ContractType::String),
            ],
            ret: Some(ContractType::String),
            fallible: false,
            takes_context: false,
            behavior: "A response header value, empty when absent (redirect Location, \
                       Retry-After surfacing).",
        },
        ContractFn {
            name: "status_code",
            receiver: Receiver::Free,
            params: &[("response", ContractType::Response)],
            ret: Some(ContractType::Int64),
            fallible: false,
            takes_context: false,
            behavior: "The response status code.",
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_names_are_unique_and_canonical() {
        let mut seen = std::collections::HashSet::new();
        for f in V1.functions {
            assert!(seen.insert(f.name), "duplicate contract fn {:?}", f.name);
            assert!(
                f.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "{:?} is not canonical snake_case",
                f.name
            );
            assert!(
                !f.behavior.is_empty(),
                "{:?} lacks a behavior clause",
                f.name
            );
        }
    }

    #[test]
    fn the_network_entry_point_threads_context_and_fails() {
        let fetch = V1.functions.iter().find(|f| f.name == "fetch").unwrap();
        assert!(fetch.takes_context && fetch.fallible);
        assert_eq!(fetch.ret, Some(ContractType::Response));
    }
}
