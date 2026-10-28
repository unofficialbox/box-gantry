//! Scratch directories for verification builds and tests.
//!
//! The compile gates generate whole SDKs under the system temp dir and build
//! them with the real toolchains; a Rust build alone leaves a ~2 GB `target/`.
//! Naming a directory by PID and clearing it *before* use never reclaims
//! anything — the next run has a different PID — so every run leaked its
//! output. A [`ScratchDir`] removes itself on drop instead, including when a
//! failing assertion unwinds through it.
//!
//! Set `GANTRY_KEEP_SCRATCH=1` to keep every scratch directory for inspection;
//! its path is printed when it would have been removed.

use std::ffi::OsStr;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// Keep scratch directories instead of removing them (debugging aid).
pub const KEEP_ENV: &str = "GANTRY_KEEP_SCRATCH";

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh, empty directory under the system temp dir, removed when dropped.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
    keep: bool,
}

impl ScratchDir {
    /// Create `<temp>/<prefix>-<pid>-<n>`, unique within and across runs.
    ///
    /// Panics if the directory can't be created: every caller is a test or a
    /// verification build that can't proceed without it.
    pub fn new(prefix: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
        // A crashed run that happened to reuse this PID may have left one.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|err| panic!("cannot create {}: {err}", path.display()));
        ScratchDir { path, keep: false }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Don't remove the directory on drop; returns its path. For keeping
    /// output a failure should be inspected in.
    pub fn keep(mut self) -> PathBuf {
        self.keep = true;
        self.path.clone()
    }
}

impl Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<OsStr> for ScratchDir {
    fn as_ref(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if std::env::var_os(KEEP_ENV).is_some_and(|v| !v.is_empty() && v != "0") {
            eprintln!("{KEEP_ENV}: kept {}", self.path.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_dir_is_fresh_unique_and_removed_on_drop() {
        let a = ScratchDir::new("gantry-scratch-test");
        let b = ScratchDir::new("gantry-scratch-test");
        assert_ne!(a.path(), b.path());
        assert!(a.is_dir() && std::fs::read_dir(&*a).unwrap().next().is_none());
        std::fs::write(a.join("file"), "x").unwrap();
        let path = a.to_path_buf();
        drop(a);
        assert!(
            !path.exists(),
            "{} should be removed on drop",
            path.display()
        );
    }

    #[test]
    fn a_scratch_dir_is_removed_when_a_panic_unwinds_through_it() {
        let path = std::panic::catch_unwind(|| {
            let dir = ScratchDir::new("gantry-scratch-test");
            let path = dir.to_path_buf();
            std::panic::panic_any(path);
        })
        .unwrap_err()
        .downcast::<PathBuf>()
        .unwrap();
        assert!(
            !path.exists(),
            "{} should be removed on unwind",
            path.display()
        );
    }

    #[test]
    fn a_kept_scratch_dir_survives_drop() {
        let path = ScratchDir::new("gantry-scratch-test").keep();
        assert!(path.is_dir());
        std::fs::remove_dir_all(path).unwrap();
    }
}
