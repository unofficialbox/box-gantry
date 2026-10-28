//! The names lock: synthesized type names frozen across spec refreshes.
//!
//! Structural dedupe (D-127) names a shared inline shape after whichever
//! location lowers first. That is deterministic for one spec snapshot, but
//! not stable across snapshots: when Box adds an identical shape that lowers
//! earlier, the shared type silently takes the newcomer's name and an
//! existing public type is renamed in every SDK. Box's v2026.0 adding its own
//! `FileReference` renamed the unchanged `NotesConvertResponseType` this way.
//!
//! The lock records, per API version, the name each synthesized declaration
//! was minted under, keyed by its minting location. Lowering with a lock is
//! two-pass ([`crate::lower`]): which locations share a declaration depends
//! only on shapes, never on names, so pass 1 learns every group's members and
//! pass 2 gives each group the locked name of its first member that has one.
//! A refresh therefore only changes a name when the lock is regenerated — a
//! reviewed diff, like `checksums.txt` — and the full-set test fails on a lock
//! that no longer matches what lowering produced.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::error::IngestError;

/// API version → minting location → name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NamesLock {
    pub(crate) versions: BTreeMap<String, BTreeMap<String, String>>,
}

/// One dedupe group from lowering pass 1: every location that resolved to one
/// synthesized declaration, minting location first.
#[derive(Debug, Clone)]
pub(crate) struct ShapeGroup {
    pub(crate) version: String,
    pub(crate) members: Vec<String>,
}

impl NamesLock {
    pub fn load(path: &Path) -> Result<Self, IngestError> {
        let text = std::fs::read_to_string(path).map_err(|source| IngestError::NamesLockIo {
            file: path.to_path_buf(),
            source,
        })?;
        let versions =
            serde_json::from_str(&text).map_err(|source| IngestError::NamesLockParse {
                file: path.to_path_buf(),
                source,
            })?;
        Ok(Self { versions })
    }

    /// Pretty-printed, sorted JSON with a trailing newline — byte-for-byte
    /// deterministic, so a regenerated lock diffs cleanly.
    pub fn to_json(&self) -> String {
        let mut out =
            serde_json::to_string_pretty(&self.versions).expect("a string map serializes");
        out.push('\n');
        out
    }

    /// The locked name for each group's minting location, per version: the
    /// lock entry of the group's first member that has one.
    pub(crate) fn preferred(
        &self,
        groups: &[ShapeGroup],
    ) -> HashMap<String, HashMap<String, String>> {
        let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
        for group in groups {
            let Some(locked) = self.versions.get(&group.version) else {
                continue;
            };
            if let Some(name) = group.members.iter().find_map(|m| locked.get(m)) {
                out.entry(group.version.clone())
                    .or_default()
                    .insert(group.members[0].clone(), name.clone());
            }
        }
        out
    }

    /// This lock restricted to `versions` — what a lowering of only those
    /// documents can be compared against.
    pub fn restricted_to<'v>(&self, versions: impl IntoIterator<Item = &'v str>) -> Self {
        let mut out = Self::default();
        for version in versions {
            if let Some(entries) = self.versions.get(version) {
                out.versions.insert(version.to_string(), entries.clone());
            }
        }
        out
    }

    /// `other`'s versions replace this lock's; versions only this lock has
    /// are kept — regenerating from a subset never drops the rest.
    pub fn updated_with(&self, other: &Self) -> Self {
        let mut out = self.clone();
        for (version, entries) in &other.versions {
            out.versions.insert(version.clone(), entries.clone());
        }
        out
    }

    /// How `current` differs from this lock: entries added, removed, and
    /// renamed (same location, different name).
    pub fn drift(&self, current: &Self) -> LockDrift {
        let mut drift = LockDrift::default();
        let empty = BTreeMap::new();
        let versions: std::collections::BTreeSet<&String> = self
            .versions
            .keys()
            .chain(current.versions.keys())
            .collect();
        for version in versions {
            let old = self.versions.get(version).unwrap_or(&empty);
            let new = current.versions.get(version).unwrap_or(&empty);
            for (location, name) in new {
                match old.get(location) {
                    None => drift.added += 1,
                    Some(locked) if locked != name => drift.renamed += 1,
                    Some(_) => {}
                }
            }
            drift.removed += old.keys().filter(|l| !new.contains_key(*l)).count();
        }
        drift
    }

    /// Entry count across versions.
    pub fn len(&self) -> usize {
        self.versions.values().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// How far a lock is from the names lowering actually produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LockDrift {
    pub added: usize,
    pub removed: usize,
    pub renamed: usize,
}

impl LockDrift {
    pub fn is_clean(&self) -> bool {
        *self == Self::default()
    }
}
