//! Per-language capability manifests (FR-4).
//!
//! One declarative value per target language. Feature synthesis keys off
//! these axes and *only* these axes — comparing against a language name
//! anywhere outside this crate is prohibited (FR-4.2; the old engine had
//! 31 `=== 'CSharp'` sites).
//!
//! The axes are drafted here in M0/M1 so the {Go, Apex, Rust} extremes are
//! visible from the start (assessment §4); the full manifests are M2 work.

/// The capability axes of one target language (FR-4.1).
///
/// Every field is total — there is no "unknown" — so adding an axis forces
/// every manifest to answer for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityManifest {
    /// Stable key used in CLI arguments and output paths (e.g. `"go"`).
    pub key: &'static str,
    pub modules: ModuleSystem,
    pub generics: Generics,
    pub error_model: ErrorModel,
    pub async_model: AsyncModel,
    pub streaming: Streaming,
    /// Platform transaction budgets that shape API design (Apex governor
    /// limits — assessment §4). `None` for targets without them.
    pub callout_limits: Option<CalloutLimits>,
    /// Test coverage the platform mandates before deployment, as a
    /// percentage (Apex: 75). Generated tests are ship-blocking when set.
    pub mandated_test_coverage: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleSystem {
    /// Real packages/modules; the IR module tree lowers directly.
    Hierarchical,
    /// One flat namespace; modules lower to outer-class grouping + name
    /// mangling (TR-Apex.1).
    Flat { identifier_limit: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generics {
    Full,
    /// No user-defined generics: shared containers lower to per-type code
    /// or typed `Object` wrappers (TR-Apex.2).
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorModel {
    /// Go: `(T, error)`.
    ValueAndError,
    /// Apex: exceptions.
    Exceptions,
    /// Rust: `Result<T, E>`.
    Result,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncModel {
    /// Blocking calls; concurrency is the caller's business (Go, Apex).
    Sync,
    /// Async-first (Rust: `reqwest` + `tokio`, TR-Rust.3).
    Async,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Streaming {
    /// Bodies can stream (Go readers, Rust `Stream`).
    Supported,
    /// Bodies are buffered; sizes bounded by platform heap (Apex — FR-7.4).
    Buffered { max_body_bytes: u32 },
}

/// Per-transaction callout budgets (Apex governor limits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalloutLimits {
    pub max_callouts_per_transaction: u16,
    pub max_heap_bytes: u32,
}

/// The Go manifest (TR-Go). Draft — reviewed and frozen in M2.
pub fn go() -> CapabilityManifest {
    CapabilityManifest {
        key: "go",
        modules: ModuleSystem::Hierarchical,
        generics: Generics::Full,
        error_model: ErrorModel::ValueAndError,
        async_model: AsyncModel::Sync,
        streaming: Streaming::Supported,
        callout_limits: None,
        mandated_test_coverage: None,
    }
}

/// The Apex manifest (TR-Apex). Draft — the stress-test axes, kept visible
/// from day one so nothing upstream assumes them away (assessment §8).
pub fn apex() -> CapabilityManifest {
    CapabilityManifest {
        key: "apex",
        modules: ModuleSystem::Flat {
            identifier_limit: 40,
        },
        generics: Generics::None,
        error_model: ErrorModel::Exceptions,
        async_model: AsyncModel::Sync,
        streaming: Streaming::Buffered {
            max_body_bytes: 6 * 1024 * 1024,
        },
        callout_limits: Some(CalloutLimits {
            max_callouts_per_transaction: 100,
            max_heap_bytes: 6 * 1024 * 1024,
        }),
        mandated_test_coverage: Some(75),
    }
}

/// The Rust manifest (TR-Rust). Draft — reviewed and frozen in M2.
pub fn rust() -> CapabilityManifest {
    CapabilityManifest {
        key: "rust",
        modules: ModuleSystem::Hierarchical,
        generics: Generics::Full,
        error_model: ErrorModel::Result,
        async_model: AsyncModel::Async,
        streaming: Streaming::Supported,
        callout_limits: None,
        mandated_test_coverage: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_keys_are_distinct() {
        let keys = [go().key, apex().key, rust().key];
        let mut sorted = keys.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len());
    }

    #[test]
    fn the_three_targets_cover_the_extremes() {
        // The IR is designed against near-opposite extremes (assessment §4);
        // if these ever converge, the design pressure is gone.
        assert_ne!(go().error_model, rust().error_model);
        assert_eq!(apex().generics, Generics::None);
        assert!(matches!(apex().modules, ModuleSystem::Flat { .. }));
        assert!(apex().callout_limits.is_some());
        assert_eq!(apex().mandated_test_coverage, Some(75));
    }
}
