//! Semantic analysis (FR-3).
//!
//! Exactly one pass between ingestion and backends, producing a complete,
//! queryable type environment: every expression typed, every reference
//! bound, every error carrying a spec-level location (FR-3.1, FR-3.3).
//! Backends receive only verified programs (FR-3.2).
//!
//! Lands in M2 (see PLAN.md); this crate is the seam.
