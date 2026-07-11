use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::error::IngestError;
use crate::raw::{RawDocument, RawParameter, RawPathItem, RawSchema};

/// The version-aware model of one ingestion run: every document loaded,
/// each contributing a distinct API version (FR-1.1).
#[derive(Debug)]
pub struct SpecSet {
    /// In load order: base spec first by convention (the caller passes it
    /// first), versioned overlays after.
    pub documents: Vec<Document>,
}

/// One ingested OpenAPI document.
#[derive(Debug)]
pub struct Document {
    pub file: PathBuf,
    pub title: String,
    /// The Box API version this document contributes (e.g. `"2024.0"`).
    pub api_version: String,
    pub operations: Vec<OperationSummary>,
    /// Named schemas, in spec order (deterministic — FR-6.2). Lowered into
    /// the typed IR by [`crate::lower`].
    pub schemas: IndexMap<String, RawSchema>,
    /// Raw path items, in spec order — the operation-lowering input.
    pub paths: IndexMap<String, RawPathItem>,
    /// Shared `components.parameters`.
    pub parameters: IndexMap<String, RawParameter>,
}

/// One operation, as ingestion sees it. Naming rules (FR-1.2) will apply
/// here — in the ingestion layer, not in any driver.
#[derive(Debug)]
pub struct OperationSummary {
    /// The raw spec `operationId` (may carry a `#variation` suffix — a
    /// spec-authoring convention that ingestion will translate into
    /// structured variation data, never pass through as a name).
    pub id: String,
    pub method: HttpMethod,
    pub path: String,
    /// Manager grouping key, from `x-box-tag` (FR-7.1).
    pub manager: String,
    pub deprecated: bool,
    /// `x-stability-level` when the spec marks one (e.g. `"alpha"`).
    pub stability_level: Option<String>,
}

/// The closed set of HTTP methods OpenAPI 3.x defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Put,
    Post,
    Delete,
    Options,
    Head,
    Patch,
    Trace,
}

impl HttpMethod {
    fn from_key(key: &'static str) -> Self {
        match key {
            "get" => Self::Get,
            "put" => Self::Put,
            "post" => Self::Post,
            "delete" => Self::Delete,
            "options" => Self::Options,
            "head" => Self::Head,
            "patch" => Self::Patch,
            "trace" => Self::Trace,
            other => unreachable!(
                "RawPathItem::operations only yields OpenAPI method keys, got {other:?}"
            ),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Put => "PUT",
            Self::Post => "POST",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Head => "HEAD",
            Self::Patch => "PATCH",
            Self::Trace => "TRACE",
        }
    }
}

impl SpecSet {
    /// Load and validate a set of spec documents. Any error fails the whole
    /// run (FR-1.4): no partial ingestion, no skipped documents.
    pub fn load(files: &[PathBuf]) -> Result<Self, IngestError> {
        if files.is_empty() {
            return Err(IngestError::NoDocuments);
        }
        let mut documents: Vec<Document> = Vec::with_capacity(files.len());
        for file in files {
            let doc = Document::load(file)?;
            if let Some(other) = documents.iter().find(|d| d.api_version == doc.api_version) {
                return Err(IngestError::DuplicateApiVersion {
                    file: file.clone(),
                    api_version: doc.api_version,
                    other_file: other.file.clone(),
                });
            }
            documents.push(doc);
        }
        Ok(Self { documents })
    }
}

impl Document {
    fn load(file: &Path) -> Result<Self, IngestError> {
        let text = std::fs::read_to_string(file).map_err(|source| IngestError::Io {
            file: file.to_path_buf(),
            source,
        })?;
        let mut deserializer = serde_json::Deserializer::from_str(&text);
        let raw: RawDocument =
            serde_path_to_error::deserialize(&mut deserializer).map_err(|err| {
                let json_path = err.path().to_string();
                IngestError::Parse {
                    file: file.to_path_buf(),
                    json_path,
                    source: err.into_inner(),
                }
            })?;
        Self::validate(file, raw)
    }

    fn validate(file: &Path, raw: RawDocument) -> Result<Self, IngestError> {
        if !raw.openapi.starts_with("3.") {
            return Err(IngestError::UnsupportedOpenApiVersion {
                file: file.to_path_buf(),
                found: raw.openapi,
            });
        }

        let mut operations = Vec::new();
        let mut seen_ids: HashMap<String, String> = HashMap::new();
        for (path, item) in &raw.paths {
            for (method, op) in item.operations() {
                let id = match &op.operation_id {
                    Some(id) if !id.is_empty() => id.clone(),
                    _ => {
                        return Err(IngestError::MissingOperationId {
                            file: file.to_path_buf(),
                            path: path.clone(),
                            method,
                        });
                    }
                };
                let manager = match &op.box_tag {
                    Some(tag) if !tag.is_empty() => tag.clone(),
                    _ => {
                        return Err(IngestError::MissingBoxTag {
                            file: file.to_path_buf(),
                            path: path.clone(),
                            method,
                            operation_id: id,
                        });
                    }
                };
                if let Some(other_path) = seen_ids.insert(id.clone(), path.clone()) {
                    return Err(IngestError::DuplicateOperationId {
                        file: file.to_path_buf(),
                        operation_id: id,
                        other_path,
                    });
                }
                operations.push(OperationSummary {
                    id,
                    method: HttpMethod::from_key(method),
                    path: path.clone(),
                    manager,
                    deprecated: op.deprecated,
                    stability_level: op.stability_level.clone(),
                });
            }
        }

        Ok(Self {
            file: file.to_path_buf(),
            title: raw.info.title,
            api_version: raw.info.version,
            operations,
            schemas: raw.components.schemas,
            paths: raw.paths,
            parameters: raw.components.parameters,
        })
    }

    /// Distinct manager grouping keys, sorted (deterministic — FR-6.2).
    pub fn managers(&self) -> BTreeSet<&str> {
        self.operations
            .iter()
            .map(|op| op.manager.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_spec(dir: &Path, name: &str, json: &serde_json::Value) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, serde_json::to_string_pretty(json).unwrap()).unwrap();
        path
    }

    fn minimal_spec(api_version: &str) -> serde_json::Value {
        serde_json::json!({
            "openapi": "3.0.2",
            "info": { "title": "Box Platform API", "version": api_version },
            "paths": {
                "/files/{file_id}": {
                    "get": { "operationId": "get_files_id", "x-box-tag": "files" }
                }
            },
            "components": { "schemas": { "File": { "type": "object" } } }
        })
    }

    #[test]
    fn loads_a_minimal_document() {
        let dir = tempdir();
        let file = write_spec(&dir, "base.json", &minimal_spec("2024.0"));
        let set = SpecSet::load(&[file]).unwrap();
        let doc = &set.documents[0];
        assert_eq!(doc.api_version, "2024.0");
        assert_eq!(doc.operations.len(), 1);
        assert_eq!(doc.operations[0].manager, "files");
        assert_eq!(doc.operations[0].method, HttpMethod::Get);
        assert_eq!(doc.schemas.keys().collect::<Vec<_>>(), ["File"]);
    }

    #[test]
    fn rejects_openapi_2() {
        let dir = tempdir();
        let mut spec = minimal_spec("2024.0");
        spec["openapi"] = "2.0".into();
        let file = write_spec(&dir, "old.json", &spec);
        let err = SpecSet::load(&[file]).unwrap_err();
        assert!(
            matches!(err, IngestError::UnsupportedOpenApiVersion { .. }),
            "{err}"
        );
    }

    #[test]
    fn missing_operation_id_names_the_path_and_method() {
        let dir = tempdir();
        let mut spec = minimal_spec("2024.0");
        spec["paths"]["/files/{file_id}"]["get"]
            .as_object_mut()
            .unwrap()
            .remove("operationId");
        let file = write_spec(&dir, "noid.json", &spec);
        let err = SpecSet::load(&[file]).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("/files/{file_id}") && message.contains("get"),
            "{message}"
        );
    }

    #[test]
    fn missing_box_tag_fails_rather_than_skipping() {
        let dir = tempdir();
        let mut spec = minimal_spec("2024.0");
        spec["paths"]["/files/{file_id}"]["get"]
            .as_object_mut()
            .unwrap()
            .remove("x-box-tag");
        let file = write_spec(&dir, "notag.json", &spec);
        let err = SpecSet::load(&[file]).unwrap_err();
        assert!(matches!(err, IngestError::MissingBoxTag { .. }), "{err}");
    }

    #[test]
    fn malformed_json_reports_the_json_path() {
        let dir = tempdir();
        // `info.version` must be a string; hand it a number.
        let mut spec = minimal_spec("2024.0");
        spec["info"]["version"] = 2024.into();
        let file = write_spec(&dir, "badinfo.json", &spec);
        let err = SpecSet::load(&[file]).unwrap_err();
        let IngestError::Parse { json_path, .. } = &err else {
            panic!("expected Parse error, got {err}");
        };
        assert!(
            json_path.contains("info.version"),
            "json_path = {json_path}"
        );
    }

    #[test]
    fn duplicate_api_versions_across_documents_fail() {
        let dir = tempdir();
        let a = write_spec(&dir, "a.json", &minimal_spec("2025.0"));
        let b = write_spec(&dir, "b.json", &minimal_spec("2025.0"));
        let err = SpecSet::load(&[a, b]).unwrap_err();
        assert!(
            matches!(err, IngestError::DuplicateApiVersion { .. }),
            "{err}"
        );
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gantry-spec-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
