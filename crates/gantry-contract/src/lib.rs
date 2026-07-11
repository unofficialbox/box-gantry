//! Runtime contracts (FR-5).
//!
//! The hand-written runtime surface each SDK ships, declared
//! machine-readably (name, arity, types, error behavior, cancellation
//! threading). Generated code calls only through the contract, and
//! generation fails on signature drift (FR-5.2). Compilable per-target
//! stubs live here so output compile-verifies without a real runtime
//! (FR-5.3).
//!
//! Lands in M2 (see PLAN.md); this crate is the seam.
