use std::path::PathBuf;

/// Ingestion failures (FR-1.4, NF-3).
///
/// Every variant answers *what* went wrong and *where* — the file, and for
/// in-document problems the JSON path or the `paths` entry. Any of these
/// fails the whole run; there is no partial ingestion.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("{file}: cannot read spec: {source}")]
    Io {
        file: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// JSON that doesn't match the OpenAPI shape we consume. `json_path` is
    /// the path inside the document (from `serde_path_to_error`).
    #[error("{file}: at {json_path}: {source}")]
    Parse {
        file: PathBuf,
        json_path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("{file}: unsupported OpenAPI version {found:?}; this engine ingests OpenAPI 3.x")]
    UnsupportedOpenApiVersion { file: PathBuf, found: String },

    #[error("{file}: paths[{path:?}].{method}: operation has no operationId")]
    MissingOperationId {
        file: PathBuf,
        path: String,
        method: &'static str,
    },

    /// `x-box-tag` is the machine-readable manager grouping key; `tags` is
    /// display-only (at least one real operation has `x-box-tag` but no
    /// `tags`). An operation without it cannot be assigned to a manager
    /// (FR-7.1), so it fails ingestion rather than being silently skipped
    /// (NF-1).
    #[error(
        "{file}: paths[{path:?}].{method} ({operation_id}): operation has no x-box-tag (manager grouping key)"
    )]
    MissingBoxTag {
        file: PathBuf,
        path: String,
        method: &'static str,
        operation_id: String,
    },

    #[error(
        "{file}: duplicate operationId {operation_id:?} (also declared at paths[{other_path:?}])"
    )]
    DuplicateOperationId {
        file: PathBuf,
        operation_id: String,
        other_path: String,
    },

    #[error(
        "{file}: declares API version {api_version:?}, already loaded from {other_file}; \
         each document in a run must contribute a distinct version (FR-1.1)"
    )]
    DuplicateApiVersion {
        file: PathBuf,
        api_version: String,
        other_file: PathBuf,
    },

    #[error("no spec documents given")]
    NoDocuments,

    /// A `$ref` that does not resolve to a schema in the same document.
    /// Unresolved references are ingestion errors, never a backend concern
    /// (FR-2.4).
    #[error("{file}: {location}: $ref {reference:?} does not resolve to a schema in this document")]
    UnresolvedRef {
        file: PathBuf,
        location: String,
        reference: String,
    },

    /// A schema shape the lowering cannot classify. Deliberately loud
    /// (NF-1): the alternative — passing the shape through as something
    /// vague — is the silent-miss bug class this engine exists to remove.
    #[error("{file}: {location}: {detail}")]
    UnsupportedSchema {
        file: PathBuf,
        location: String,
        detail: String,
    },

    /// Two versions of a schema share a name but cannot be merged into one
    /// superset type (D-190): the same wire field carries genuinely different
    /// (non-equivalent) types across versions, or the name is bound to
    /// different declaration kinds. Loud rather than silently picking one
    /// version's shape (NF-1).
    #[error(
        "schema {name:?}: incompatible definitions across API versions cannot be merged: {detail}"
    )]
    SchemaVersionConflict { name: String, detail: String },
}
