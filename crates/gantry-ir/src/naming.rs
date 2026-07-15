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

/// `displayName` → `display_name`, `colorID` → `color_id`,
/// `JSONValue` → `json_value`, `Box__Security__Key` → `box_security_key`.
/// Splits on case boundaries — including the acronym-run→word boundary, so an
/// uppercase run followed by a lowercase-led word breaks before that word's
/// leading capital — and on separators (`_`, `-`, space), collapsing runs.
/// Used where a target's idiom is snake_case (Rust fields, TR-Rust.4) even
/// though the IR name may arrive camelCased from the spec.
pub fn snake(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 4);
    let mut prev_word_char = false;
    for (index, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == ' ' {
            if prev_word_char {
                out.push('_');
            }
            prev_word_char = false;
        } else if c.is_ascii_uppercase() {
            // Break before this capital when it starts a new word: either the
            // previous char was a lowercase/digit (`colorID` → `color_id`) or
            // it closes an acronym run before a lowercase-led word
            // (`JSONValue` → `json_value`).
            let closes_acronym = chars.get(index + 1).is_some_and(|n| n.is_ascii_lowercase())
                && index
                    .checked_sub(1)
                    .and_then(|i| chars.get(i))
                    .is_some_and(|p| p.is_ascii_uppercase());
            if (prev_word_char || closes_acronym) && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(c.to_lowercase());
            prev_word_char = false;
        } else {
            out.push(c);
            prev_word_char = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    out.trim_matches('_').to_string()
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

    #[test]
    fn snake_case() {
        assert_eq!(snake("displayName"), "display_name");
        assert_eq!(snake("colorID"), "color_id");
        assert_eq!(snake("JSONValue"), "json_value");
        assert_eq!(snake("fooBARBaz"), "foo_bar_baz");
        assert_eq!(snake("Box__Security__Key"), "box_security_key");
        assert_eq!(snake("can_edit"), "can_edit");
        assert_eq!(snake("type"), "type");
        assert_eq!(snake("-leading-trailing-"), "leading_trailing");
    }
}
