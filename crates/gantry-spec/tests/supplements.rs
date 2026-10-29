//! Spec supplements (`supplements.json` beside the base spec): they may only
//! add, must land on a declared operation, and must name their evidence — any
//! other supplement fails ingestion loudly.

use gantry_spec::{IngestError, SUPPLEMENTS_FILE, SpecSet};
use gantry_verify::scratch::ScratchDir;

/// A one-operation spec: `GET /things/{id}`, with an `If-Match` header
/// declared through `components.parameters`.
fn spec() -> serde_json::Value {
    serde_json::json!({
        "openapi": "3.0.2",
        "info": { "title": "Box Platform API", "version": "2024.0" },
        "paths": {
            "/things/{id}": {
                "get": {
                    "operationId": "get_things_id",
                    "x-box-tag": "things",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "$ref": "#/components/parameters/IfMatch" }
                    ],
                    "responses": {
                        "200": {
                            "description": "ok",
                            "content": {
                                "application/json": { "schema": { "$ref": "#/components/schemas/Thing" } }
                            }
                        }
                    }
                }
            }
        },
        "components": {
            "schemas": {
                "Thing": { "type": "object", "properties": { "etag": { "type": "string" } } }
            },
            "parameters": {
                "IfMatch": { "name": "if-match", "in": "header", "schema": { "type": "string" } }
            }
        }
    })
}

fn load(tag: &str, supplements: serde_json::Value) -> (ScratchDir, Result<SpecSet, IngestError>) {
    let dir = ScratchDir::new(&format!("gantry-supplements-{tag}"));
    std::fs::write(dir.join("spec.json"), spec().to_string()).unwrap();
    std::fs::write(dir.join(SUPPLEMENTS_FILE), supplements.to_string()).unwrap();
    let loaded = SpecSet::load(&[dir.join("spec.json")]);
    (dir, loaded)
}

fn supplement(path: &str, evidence: &str, parameter: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "operations": [{
            "path": path,
            "method": "get",
            "evidence": evidence,
            "parameters": [parameter],
            "responses": { "304": { "description": "unchanged" } }
        }]
    })
}

fn if_none_match() -> serde_json::Value {
    serde_json::json!({ "name": "if-none-match", "in": "header", "schema": { "type": "string" } })
}

#[test]
fn a_supplement_adds_the_header_and_response_and_moves_the_fingerprint() {
    let (_dir, loaded) = load("adds", supplement("/things/{id}", "probe", if_none_match()));
    let set = loaded.expect("the supplement applies");
    let lowering = gantry_spec::lower(&set).expect("the supplemented spec lowers");
    let get = lowering
        .program
        .operations
        .iter()
        .find(|op| op.name.as_str() == "get")
        .expect("the get operation");
    let read = get
        .conditional
        .as_ref()
        .expect("If-None-Match + 304 make a conditional read");
    assert_eq!(read.etag.as_str(), "if-none-match");

    let dir = ScratchDir::new("gantry-supplements-none");
    std::fs::write(dir.join("spec.json"), spec().to_string()).unwrap();
    let plain = SpecSet::load(&[dir.join("spec.json")]).unwrap();
    assert!(plain.supplements_file.is_none());
    assert_ne!(
        set.fingerprint(),
        plain.fingerprint(),
        "the supplements are an input"
    );
}

#[test]
fn a_supplement_upstream_already_declares_fails_loudly() {
    // Declared through a `$ref`, matched case-insensitively.
    let parameter =
        serde_json::json!({ "name": "If-Match", "in": "header", "schema": { "type": "string" } });
    let (_dir, loaded) = load("redundant", supplement("/things/{id}", "probe", parameter));
    let err = loaded.expect_err("a redundant supplement is an error");
    assert!(
        matches!(err, IngestError::RedundantSupplement { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("delete this supplement"), "{err}");
}

#[test]
fn a_supplement_for_an_undeclared_operation_fails_loudly() {
    let (_dir, loaded) = load(
        "unmatched",
        supplement("/thingz/{id}", "probe", if_none_match()),
    );
    let err = loaded.expect_err("an unmatched supplement is an error");
    assert!(
        matches!(err, IngestError::InvalidSupplement { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("no loaded spec declares"), "{err}");
}

#[test]
fn a_supplement_without_evidence_fails_loudly() {
    let (_dir, loaded) = load("evidence", supplement("/things/{id}", " ", if_none_match()));
    let err = loaded.expect_err("a supplement must say why");
    assert!(err.to_string().contains("names no evidence"), "{err}");
}
