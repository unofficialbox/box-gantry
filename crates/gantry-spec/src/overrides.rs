//! Human-supplied replacements for synthesized names too long to be
//! comfortable to work with, but not wrong (FR-1.2 lineage).
//!
//! A 2-segment synthesized name (`Owner` + PascalCase(property)) can be
//! long without being a bug: Box sometimes names a top-level schema
//! something genuinely long, and gives it a field with a genuinely long
//! flat wire name — concatenating two real, non-redundant names is the
//! naming rule working as designed, and there is no *structural* way to
//! shorten it (an invented abbreviation risks a new collision and isn't
//! structural naming anymore, D-... naming discipline).
//!
//! Rather than have the engine guess an abbreviation, the person running
//! `gantry generate` can supply one explicitly. `gantry names` (in
//! `gantry-cli`) enumerates the candidates; this module is where the
//! resulting overrides file is loaded and validated before lowering ever
//! sees it.

use std::collections::HashMap;
use std::path::Path;

use crate::error::IngestError;

/// Parsed, not-yet-applied name overrides, loaded from a JSON file.
///
/// Two independent kinds, keyed differently because they act at different
/// scopes:
///
/// - `components`: keyed by a top-level schema's own raw key under
///   `components.schemas` in the spec (e.g. `"EnterpriseConfigurationContentAndSharing"`).
///   Every field synthesized under that schema inherits the override —
///   overriding one verbose parent shortens every descendant name, since a
///   named schema reseeds its own children's naming purely from its own
///   name (see `lower.rs`'s `lower_named`/`lower_document`).
/// - `locations`: keyed by the exact JSON-path-like `location` string a
///   single synthesized declaration was minted at (the same strings
///   `gantry check`/`gantry names` and ingestion errors already report,
///   e.g. `"components.schemas.Foo.properties.bar"`). Replaces just that
///   one declaration's name; does not cascade.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameOverrides {
    pub(crate) components: HashMap<String, String>,
    pub(crate) locations: HashMap<String, String>,
}

/// The on-disk shape: a plain JSON object with the two maps, both optional.
#[derive(Debug, serde::Deserialize)]
struct RawOverridesFile {
    #[serde(default)]
    components: HashMap<String, String>,
    #[serde(default)]
    locations: HashMap<String, String>,
}

impl NameOverrides {
    /// No overrides — lowering behaves exactly as it always has. What
    /// `lower()` uses internally, and what every caller that doesn't pass
    /// `--overrides` gets.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load and validate an overrides file. Every replacement name is
    /// checked as a valid identifier here — at load time, before lowering
    /// ever starts — so a malformed override fails fast with a clear
    /// pointer to the bad entry, rather than surfacing as a confusing
    /// downstream identifier error deep in synthesis.
    pub fn load(path: &Path) -> Result<Self, IngestError> {
        let text = std::fs::read_to_string(path).map_err(|source| IngestError::OverridesIo {
            file: path.to_path_buf(),
            source,
        })?;
        let raw: RawOverridesFile =
            serde_json::from_str(&text).map_err(|source| IngestError::OverridesParse {
                file: path.to_path_buf(),
                source,
            })?;
        for (key, value) in &raw.components {
            validate_override_value("component", key, value)?;
        }
        for (key, value) in &raw.locations {
            validate_override_value("location", key, value)?;
        }
        Ok(Self {
            components: raw.components,
            locations: raw.locations,
        })
    }

    pub(crate) fn component(&self, key: &str) -> Option<&str> {
        self.components.get(key).map(String::as_str)
    }

    pub(crate) fn location(&self, key: &str) -> Option<&str> {
        self.locations.get(key).map(String::as_str)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.components.is_empty() && self.locations.is_empty()
    }
}

fn validate_override_value(kind: &'static str, key: &str, value: &str) -> Result<(), IngestError> {
    gantry_ir::Identifier::new(value).map_err(|err| IngestError::InvalidOverrideName {
        kind,
        key: key.to_string(),
        value: value.to_string(),
        detail: err.to_string(),
    })?;
    Ok(())
}
