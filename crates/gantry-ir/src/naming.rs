//! Casing utilities shared by backends and synthesis.
//!
//! These are *mechanical* conversions from the IR's canonical snake_case
//! identifiers. Which name an artifact gets is decided in the ingestion
//! layer (FR-1.2); how it is cased per target is decided by the backend
//! using these helpers. Initialism policy (Id vs ID) is deliberately
//! simple-Pascal everywhere until a casing decision record says
//! otherwise.

/// `get_files_id` → `GetFilesId`; splits on `_` and `-`.
pub fn pascal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for part in text.split(['_', '-']) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// `get_files_id` → `getFilesId`.
pub fn camel(text: &str) -> String {
    let pascal = pascal(text);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => pascal,
    }
}

/// A constant-friendly name from an arbitrary enum value:
/// `viewer uploader` → `ViewerUploader`, `2025.0` → `V20250`.
pub fn constant(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let name = pascal(&cleaned);
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("V{name}")
    } else if name.is_empty() {
        "Empty".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(pascal("get_files_id"), "GetFilesId");
        assert_eq!(pascal("x-rep-hints"), "XRepHints");
        assert_eq!(camel("content_md5"), "contentMd5");
        assert_eq!(constant("viewer uploader"), "ViewerUploader");
        assert_eq!(constant("2025.0"), "V20250");
        assert_eq!(constant("editor"), "Editor");
    }
}
