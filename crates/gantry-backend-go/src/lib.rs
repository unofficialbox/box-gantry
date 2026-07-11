//! The Go backend: lowering + printer (FR-6, TR-Go).
//!
//! Lowers the verified IR to Go: (T, error) returns, pointer optionals,
//! context.Context-first methods, iter.Seq2 pagination, oneOf variant
//! structs — gofmt-clean by construction (FR-6.4).
//!
//! Lands in M3 (see PLAN.md); this crate is the seam.
