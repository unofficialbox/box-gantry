//! The serde-facing shape of an OpenAPI document — only the fields this
//! slice of the engine consumes. Unknown fields are permitted (OpenAPI
//! documents carry many `x-*` extensions); *missing* required structure is
//! a loud parse error with a JSON path (FR-1.4).

use indexmap::IndexMap;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct RawDocument {
    pub openapi: String,
    pub info: RawInfo,
    #[serde(default)]
    pub paths: IndexMap<String, RawPathItem>,
    #[serde(default)]
    pub components: RawComponents,
}

#[derive(Debug, Deserialize)]
pub struct RawInfo {
    pub title: String,
    /// The Box API version this document contributes (e.g. `"2025.0"`).
    pub version: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawComponents {
    /// Schemas are held as raw JSON for now; the typed schema model is the
    /// next M1 increment.
    #[serde(default)]
    pub schemas: IndexMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct RawPathItem {
    pub get: Option<RawOperation>,
    pub put: Option<RawOperation>,
    pub post: Option<RawOperation>,
    pub delete: Option<RawOperation>,
    pub options: Option<RawOperation>,
    pub head: Option<RawOperation>,
    pub patch: Option<RawOperation>,
    pub trace: Option<RawOperation>,
}

impl RawPathItem {
    /// The operations of this path item, in a fixed method order so every
    /// downstream listing is deterministic (FR-6.2).
    pub fn operations(&self) -> impl Iterator<Item = (&'static str, &RawOperation)> {
        [
            ("get", &self.get),
            ("put", &self.put),
            ("post", &self.post),
            ("delete", &self.delete),
            ("options", &self.options),
            ("head", &self.head),
            ("patch", &self.patch),
            ("trace", &self.trace),
        ]
        .into_iter()
        .filter_map(|(m, op)| op.as_ref().map(|op| (m, op)))
    }
}

#[derive(Debug, Deserialize)]
pub struct RawOperation {
    #[serde(rename = "operationId")]
    pub operation_id: Option<String>,
    /// Machine-readable manager grouping key. `tags` is display-only and
    /// not always present; this is the key the engine groups by.
    #[serde(rename = "x-box-tag")]
    pub box_tag: Option<String>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(rename = "x-stability-level")]
    pub stability_level: Option<String>,
}
