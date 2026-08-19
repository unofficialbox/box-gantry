//! The "which names are still long" report (`gantry names`).
//!
//! Not a suggestion engine: it enumerates and explains, it doesn't invent
//! replacement text (see `overrides.rs`'s module doc for why — an invented
//! abbreviation risks a new collision and isn't structural naming). What it
//! does do is find the highest-leverage lever: many of the worst names
//! share one long top-level component prefix, so overriding that one
//! component (see `NameOverrides`) shortens every name grouped under it at
//! once, instead of writing one override per name.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use gantry_ir::naming::snake;

/// One synthesized name at or above the report's `min_length`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongName {
    pub name: String,
    pub location: String,
    /// Lowercase word-by-word breakdown (reuses the same tokenizer
    /// `gantry_ir::naming::append_without_repeating` uses), so a human can
    /// see at a glance which words make up the length.
    pub words: Vec<String>,
    /// The longest top-level component name (from this same lowering run)
    /// that `name` starts with, if any — the `components` override key that
    /// would shorten this name (and every sibling sharing the same prefix)
    /// in one shot.
    pub component: Option<String>,
}

/// Every long name found, grouped by shared component prefix and sorted by
/// how much a single override on that component would save in total —
/// highest leverage first — then names with no shared component prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LongNamesReport {
    pub min_length: usize,
    /// `(component name, its own length, the long names grouped under it)`,
    /// leverage-sorted descending.
    pub grouped: Vec<(String, usize, Vec<LongName>)>,
    /// Long names matching no known component prefix — a genuinely
    /// standalone 2-segment name, or an operation-derived one.
    pub ungrouped: Vec<LongName>,
}

impl LongNamesReport {
    pub fn total(&self) -> usize {
        self.grouped
            .iter()
            .map(|(_, _, names)| names.len())
            .sum::<usize>()
            + self.ungrouped.len()
    }

    /// A deterministic, human-readable report — same shape as
    /// `gantry_verify::SpecDiff::report`.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "long-names: {} name(s) at or above {} chars",
            self.total(),
            self.min_length
        );
        for (component, component_len, names) in &self.grouped {
            let saved: usize = names.len() * component_len;
            let _ = writeln!(
                out,
                "\n  component {component:?} ({component_len} chars) — {} name(s) share this prefix \
                 (~{saved} chars of it repeated across them; a `components` override on {component:?} \
                 shortens all {} at once):",
                names.len(),
                names.len()
            );
            for entry in names {
                write_entry(&mut out, entry, "    ");
            }
        }
        if !self.ungrouped.is_empty() {
            let _ = writeln!(
                out,
                "\n  no shared component prefix — needs its own `locations` override:"
            );
            for entry in &self.ungrouped {
                write_entry(&mut out, entry, "    ");
            }
        }
        out
    }
}

fn write_entry(out: &mut String, entry: &LongName, indent: &str) {
    let _ = writeln!(
        out,
        "{indent}{} ({} chars)\n{indent}  at {}\n{indent}  words: {}",
        entry.name,
        entry.name.len(),
        entry.location,
        entry.words.join(" "),
    );
}

/// Build the report from a lowering's `synthesis_log` (see
/// `Lowering::synthesis_log`'s doc for why it's the pre-merge, per-document
/// record rather than the final `Program`).
pub fn long_names(synthesis_log: &[(String, String)], min_length: usize) -> LongNamesReport {
    // A top-level component's own entry is logged at
    // `components.schemas.{name}` with no further path segment — that shape
    // is exactly how it's distinguished from a field/site synthesized
    // *under* one.
    let components: BTreeMap<&str, &str> = synthesis_log
        .iter()
        .filter_map(|(location, name)| {
            let rest = location.strip_prefix("components.schemas.")?;
            (!rest.contains('.')).then_some((name.as_str(), name.as_str()))
        })
        .collect();

    let mut grouped: BTreeMap<String, Vec<LongName>> = BTreeMap::new();
    let mut ungrouped: Vec<LongName> = Vec::new();

    for (location, name) in synthesis_log {
        if name.len() < min_length {
            continue;
        }
        let words: Vec<String> = snake(name).split('_').map(str::to_string).collect();
        // Longest-prefix match: if a name legitimately starts with more
        // than one component's name (nesting, or two components sharing a
        // common stem), the longer, more specific one is the more useful
        // override target.
        let component = components
            .keys()
            .filter(|candidate| name.starts_with(*candidate) && name.as_str() != **candidate)
            .max_by_key(|candidate| candidate.len())
            .map(|candidate| candidate.to_string());
        let entry = LongName {
            name: name.clone(),
            location: location.clone(),
            words,
            component: component.clone(),
        };
        match component {
            Some(component) => grouped.entry(component).or_default().push(entry),
            None => ungrouped.push(entry),
        }
    }

    let mut grouped: Vec<(String, usize, Vec<LongName>)> = grouped
        .into_iter()
        .map(|(component, names)| {
            let len = component.len();
            (component, len, names)
        })
        .collect();
    // Leverage first: more names sharing a longer prefix saves more total
    // characters from one override. Ties break on the component name so
    // the report is deterministic.
    grouped.sort_by(|a, b| {
        (b.2.len() * b.1)
            .cmp(&(a.2.len() * a.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    ungrouped.sort_by(|a, b| {
        b.name
            .len()
            .cmp(&a.name.len())
            .then_with(|| a.name.cmp(&b.name))
    });

    LongNamesReport {
        min_length,
        grouped,
        ungrouped,
    }
}
