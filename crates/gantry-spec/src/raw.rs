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
    #[serde(default)]
    pub schemas: IndexMap<String, RawSchema>,
    /// Shared parameters referenced as `#/components/parameters/Name`.
    #[serde(default)]
    pub parameters: IndexMap<String, RawParameter>,
}

/// One schema node, covering every shape the vendored Box specs use:
/// plain objects, `allOf` composition (base + extension), `oneOf`/`anyOf`
/// unions, string enums, arrays, maps (`additionalProperties`), `$ref`s,
/// and `nullable`. Anything this model cannot classify is a loud lowering
/// error (NF-1), never a pass-through.
#[derive(Debug, Deserialize)]
pub struct RawSchema {
    #[serde(rename = "$ref")]
    pub reference: Option<String>,
    #[serde(rename = "type")]
    pub schema_type: Option<String>,
    pub format: Option<String>,
    #[serde(default)]
    pub nullable: bool,
    #[serde(default)]
    pub properties: IndexMap<String, RawSchema>,
    #[serde(default)]
    pub required: Vec<String>,
    pub items: Option<Box<RawSchema>>,
    #[serde(rename = "additionalProperties")]
    pub additional_properties: Option<RawAdditionalProperties>,
    #[serde(rename = "oneOf", default)]
    pub one_of: Vec<RawSchema>,
    #[serde(rename = "allOf", default)]
    pub all_of: Vec<RawSchema>,
    #[serde(rename = "anyOf", default)]
    pub any_of: Vec<RawSchema>,
    /// Values are raw JSON: real specs mix in `null` to signal nullability,
    /// and non-string enums must be classified deliberately.
    #[serde(rename = "enum")]
    pub enumeration: Option<Vec<serde_json::Value>>,
}

/// `additionalProperties` is either a boolean or a schema.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum RawAdditionalProperties {
    Bool(bool),
    Schema(Box<RawSchema>),
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
    #[serde(default)]
    pub parameters: Vec<RawParameter>,
    #[serde(rename = "requestBody")]
    pub request_body: Option<RawRequestBody>,
    #[serde(default)]
    pub responses: IndexMap<String, RawResponse>,
    /// Operation-level base-URL override (the G-2 quirk).
    #[serde(default)]
    pub servers: Vec<RawServer>,
}

/// A parameter — inline, or a `$ref` into `components.parameters` (in
/// which case every other field is `None`/default).
#[derive(Debug, Deserialize)]
pub struct RawParameter {
    #[serde(rename = "$ref")]
    pub reference: Option<String>,
    pub name: Option<String>,
    #[serde(rename = "in")]
    pub location: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub schema: Option<RawSchema>,
}

#[derive(Debug, Deserialize)]
pub struct RawRequestBody {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub content: IndexMap<String, RawMediaType>,
}

#[derive(Debug, Deserialize)]
pub struct RawMediaType {
    pub schema: Option<RawSchema>,
}

#[derive(Debug, Deserialize)]
pub struct RawResponse {
    #[serde(default)]
    pub content: IndexMap<String, RawMediaType>,
}

#[derive(Debug, Deserialize)]
pub struct RawServer {
    pub url: String,
}
