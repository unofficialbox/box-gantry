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

/// Append `suffix` (already Pascal-cased — a synthesized field name, or a
/// fixed naming-convention word like `Options`) to `owner`, collapsing a
/// duplicate seam: when `suffix`'s own leading word(s) repeat `owner`'s
/// trailing word(s), a plain concatenation stutters. Box occasionally names
/// a schema `…Validation` and then nests a `validation_type` field inside
/// it (`…ValidationValidationType`); an operation already named
/// `…FieldOptions` gets a backend's own `Options`/`Paginator` suffix
/// appended too (`…FieldOptionsOptions`). Word-level generalization of the
/// token-level `IdId` collapse in gantry-spec's operation-name synthesis —
/// same bug family, same fix shape, wherever an owner and a suffix are
/// concatenated instead of one being derived from the other.
///
/// May consume `suffix` entirely when it is wholly redundant (`owner`
/// already ends exactly in `suffix`). That is only safe because every
/// caller's `owner`/`suffix` pair is already unique on its own before this
/// collapse — gantry-spec's synthesized names re-run their own collision
/// check after calling this (D-127's `ancestor`/numeral fallback), and a
/// backend's `Options`/`Paginator` suffix is a fixed convention marker, not
/// the source of a name's uniqueness (the method name it's appended to
/// already is). A caller relying on the *appended suffix itself* to
/// disambiguate two different names must not use this.
pub fn append_without_repeating(owner: &str, suffix: &str) -> String {
    // `snake`'s acronym-aware word boundaries (`ZIP4Validation` →
    // `zip4_validation`) do the tokenizing; a naive uppercase-letter split
    // would wrongly cut `ZIP4` into individual letters.
    let owner_snake = snake(owner);
    let suffix_snake = snake(suffix);
    let owner_words: Vec<&str> = owner_snake.split('_').filter(|w| !w.is_empty()).collect();
    let suffix_words: Vec<&str> = suffix_snake.split('_').filter(|w| !w.is_empty()).collect();
    let max_overlap = owner_words.len().min(suffix_words.len());
    let overlap = (1..=max_overlap)
        .rev()
        .find(|&k| owner_words[owner_words.len() - k..] == suffix_words[..k])
        .unwrap_or(0);
    if overlap == 0 {
        format!("{owner}{suffix}")
    } else {
        format!("{owner}{}", pascal(&suffix_words[overlap..].join("_")))
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

    #[test]
    fn append_without_repeating_collapses_a_single_word_seam() {
        // SignRequestSignerInputNumberWithPeriodValidation + ValidationType.
        assert_eq!(
            append_without_repeating(
                "SignRequestSignerInputNumberWithPeriodValidation",
                "ValidationType"
            ),
            "SignRequestSignerInputNumberWithPeriodValidationType"
        );
    }

    #[test]
    fn append_without_repeating_consumes_a_wholly_redundant_suffix() {
        // MetadataTaxonomiesListMetadataTemplateFieldOptions + Options.
        assert_eq!(
            append_without_repeating(
                "MetadataTaxonomiesListMetadataTemplateFieldOptions",
                "Options"
            ),
            "MetadataTaxonomiesListMetadataTemplateFieldOptions"
        );
    }

    #[test]
    fn append_without_repeating_is_a_no_op_without_a_seam() {
        assert_eq!(
            append_without_repeating("File", "SharedLink"),
            "FileSharedLink"
        );
        assert_eq!(append_without_repeating("User", "Id"), "UserId");
    }

    #[test]
    fn append_without_repeating_is_case_insensitive_at_the_seam() {
        assert_eq!(
            append_without_repeating("FileVALIDATION", "ValidationType"),
            "FileVALIDATIONType"
        );
    }

    #[test]
    fn append_without_repeating_prefers_the_longest_overlap() {
        // Both a 1-word and a 2-word overlap are possible here; the longer
        // one must win so nothing is left half-collapsed.
        assert_eq!(
            append_without_repeating("FooBarBaz", "BarBazQux"),
            "FooBarBazQux"
        );
    }

    #[test]
    fn append_without_repeating_does_not_catch_a_whole_prefix_duplicate() {
        // ShieldInformationBarrierSegmentMember + ShieldInformationBarrierSegment:
        // the overlap isn't at the owner/suffix seam (owner's last word is
        // "Member", suffix's first word is "Shield") — a documented gap, not
        // this function's job. Pinned so a future "smarter" rewrite doesn't
        // silently change this on-purpose non-fix into a guess.
        assert_eq!(
            append_without_repeating(
                "ShieldInformationBarrierSegmentMember",
                "ShieldInformationBarrierSegment"
            ),
            "ShieldInformationBarrierSegmentMemberShieldInformationBarrierSegment"
        );
    }

    #[test]
    fn append_without_repeating_preserves_an_owner_acronym() {
        // The overlap-consuming side is always the suffix; an acronym inside
        // `owner` (never re-pascal-cased) must survive untouched.
        assert_eq!(
            append_without_repeating("SignRequestSignerInputZIP4Validation", "ValidationType"),
            "SignRequestSignerInputZIP4ValidationType"
        );
    }
}
