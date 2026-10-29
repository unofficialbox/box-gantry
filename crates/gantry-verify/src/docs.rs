//! Generated-docs hygiene: code examples keep their indentation.
//!
//! The guides are Rust string literals, and a `\` line continuation strips
//! the next line's leading whitespace. An example written with plain spaces
//! then ships flush-left, which reads as broken code; the fix is to escape
//! the indent (`\x20`, `\t`). This catches the slip in generated output,
//! whichever backend made it.

/// The lines of `markdown`'s fenced code blocks that sit flush-left inside a
/// body: the line after one ending in `{`, `(` or `[` must be indented or
/// close it. Each entry is `"line N: <text>"`, 1-based.
pub fn unindented_block_bodies(markdown: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut in_block = false;
    let mut opens_body = false;
    for (number, line) in markdown.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            in_block = !in_block;
            opens_body = false;
            continue;
        }
        if !in_block || line.trim().is_empty() {
            continue;
        }
        let flush_left = !line.starts_with(char::is_whitespace);
        if opens_body && flush_left && !line.starts_with(['}', ')', ']']) {
            found.push(format!("line {}: {line}", number + 1));
        }
        opens_body = line.trim_end().ends_with(['{', '(', '[']);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::unindented_block_bodies;

    #[test]
    fn a_flush_left_body_after_an_opening_brace_or_paren_is_reported() {
        let doc =
            "text {\n\n```rust\nwhile x {\nlet y = 1;\n}\nf(\n\"a\",\n);\n```\nafter {\nprose\n";
        assert_eq!(
            unindented_block_bodies(doc),
            ["line 5: let y = 1;", "line 8: \"a\","]
        );
    }

    #[test]
    fn indented_bodies_and_empty_blocks_pass() {
        let doc = "```go\nfor {\n\tx()\n}\nif ok {\n}\nf(\n\ta,\n)\n```\n";
        assert!(unindented_block_bodies(doc).is_empty());
    }
}
