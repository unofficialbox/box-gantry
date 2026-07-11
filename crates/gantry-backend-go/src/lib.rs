//! The Go backend: lowering + printer (FR-6, TR-Go).
//!
//! M3 model slice: every IR declaration lowers to Go source — structs
//! with `encoding/json` struct tags (no per-model serializers, TR-Go.2),
//! open enums as string types with constants (D-105), discriminated
//! unions as variant structs with generated `MarshalJSON`/`UnmarshalJSON`
//! (D-012 lineage, the *only* generated serializers), aliases as type
//! aliases. Output is deterministic (FR-6.2) and gofmt-clean by
//! construction (G-17), verified by compiling with the real toolchain
//! (VR-1.1 — the primary CI signal).
//!
//! Tri-state mapping (D-110):
//! - `T` (required, non-null): the bare Go type.
//! - `Optional<T>`: pointer (or bare nilable — slice/map/any) with
//!   `,omitempty` — absent when nil (TR-Go.1).
//! - `Nullable<T>`: pointer *without* `omitempty` — always serialized,
//!   `null` when nil.
//! - `Optional<Nullable<T>>`: collapses to `*T,omitempty` in this slice —
//!   explicit-null-to-clear needs the serialization package (BG-1).

mod models;

pub use models::{GeneratedFile, generate_models};
