//! Facts about the live Box API that the vendored specs leave out.
//!
//! The specs are vendored verbatim (NF-6), and the engine never guesses at
//! behavior a spec doesn't declare. A few gaps are real, though: Box honors
//! `If-None-Match`/`If-Match` on web links exactly as on files and folders,
//! but `box/box-openapi` declares neither header there. Rather than patch the
//! vendored files (and lose their upstream checksums), the gaps live in one
//! reviewed file beside the base spec, [`SUPPLEMENTS_FILE`], loaded with it
//! like the names lock.
//!
//! Each supplement adds parameters and responses to one operation and names
//! its evidence. It is narrow and loud (NF-1):
//!
//! - it may only *add* — a parameter or response the operation already
//!   declares fails the run, so the supplement is deleted the day upstream
//!   fixes the spec instead of silently shadowing it;
//! - it must land — a path and method that no loaded document declares fails
//!   the run (a typo, or the spec moved the operation);
//! - it must say why — an empty `evidence` fails the run.

use std::path::Path;

use indexmap::IndexMap;
use serde::Deserialize;

use crate::error::IngestError;
use crate::ingest::Document;
use crate::raw::{RawParameter, RawResponse};

/// The supplements file [`crate::SpecSet::load`] picks up beside the base
/// spec.
pub const SUPPLEMENTS_FILE: &str = "supplements.json";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupplementsFile {
    /// Free-form notes for the human reader.
    #[serde(rename = "//", default)]
    _note: Option<serde_json::Value>,
    operations: Vec<OperationSupplement>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationSupplement {
    path: String,
    method: String,
    /// What shows the live API behaves this way (a probe, a doc page).
    evidence: String,
    /// Inline OpenAPI parameter objects, as upstream would write them.
    #[serde(default)]
    parameters: Vec<serde_json::Value>,
    /// OpenAPI response objects keyed by status code.
    #[serde(default)]
    responses: IndexMap<String, serde_json::Value>,
}

/// Apply the supplements file at `file` to every loaded document that
/// declares each supplemented operation.
pub(crate) fn apply(file: &Path, documents: &mut [Document]) -> Result<(), IngestError> {
    let text = std::fs::read_to_string(file).map_err(|source| IngestError::SupplementsIo {
        file: file.to_path_buf(),
        source,
    })?;
    let supplements: SupplementsFile =
        serde_json::from_str(&text).map_err(|source| IngestError::SupplementsParse {
            file: file.to_path_buf(),
            source,
        })?;
    for supplement in &supplements.operations {
        apply_one(file, supplement, documents)?;
    }
    Ok(())
}

fn apply_one(
    file: &Path,
    supplement: &OperationSupplement,
    documents: &mut [Document],
) -> Result<(), IngestError> {
    let invalid = |detail: String| IngestError::InvalidSupplement {
        file: file.to_path_buf(),
        operation: format!("{} {}", supplement.method, supplement.path),
        detail,
    };
    if supplement.evidence.trim().is_empty() {
        return Err(invalid("names no evidence".into()));
    }
    if supplement.parameters.is_empty() && supplement.responses.is_empty() {
        return Err(invalid("adds nothing".into()));
    }
    let mut landed = false;
    for document in documents.iter_mut() {
        let shared = &document.parameters;
        let Some(operation) = document
            .paths
            .get_mut(&supplement.path)
            .and_then(|item| item.operation_mut(&supplement.method))
        else {
            continue;
        };
        landed = true;
        let spec = document.file.clone();
        for value in &supplement.parameters {
            let parameter: RawParameter = serde_json::from_value(value.clone())
                .map_err(|err| invalid(format!("malformed parameter: {err}")))?;
            let (Some(name), Some(location)) = (&parameter.name, &parameter.location) else {
                return Err(invalid(
                    "a parameter needs an inline `name` and `in`".into(),
                ));
            };
            let declared = operation.parameters.iter().any(|existing| {
                let existing = match &existing.reference {
                    Some(reference) => reference
                        .strip_prefix("#/components/parameters/")
                        .and_then(|key| shared.get(key))
                        .unwrap_or(existing),
                    None => existing,
                };
                existing.location.as_deref() == Some(location)
                    && existing
                        .name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
            });
            if declared {
                return Err(redundant(
                    file,
                    supplement,
                    &spec,
                    format!("{location} parameter {name:?}"),
                ));
            }
            operation.parameters.push(parameter);
        }
        for (status, value) in &supplement.responses {
            if operation.responses.contains_key(status) {
                return Err(redundant(
                    file,
                    supplement,
                    &spec,
                    format!("response {status}"),
                ));
            }
            let response: RawResponse = serde_json::from_value(value.clone())
                .map_err(|err| invalid(format!("malformed {status} response: {err}")))?;
            operation.responses.insert(status.clone(), response);
        }
    }
    if !landed {
        return Err(invalid("no loaded spec declares this operation".into()));
    }
    Ok(())
}

fn redundant(
    file: &Path,
    supplement: &OperationSupplement,
    spec: &Path,
    what: String,
) -> IngestError {
    IngestError::RedundantSupplement {
        file: file.to_path_buf(),
        operation: format!("{} {}", supplement.method, supplement.path),
        spec: spec.to_path_buf(),
        what,
    }
}
