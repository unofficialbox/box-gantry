//! **Queueable chunked-upload spike — throwaway by design.**
//!
//! Follow-up to D-136, which shipped `BoxChunkedUpload` as an honest *reference*
//! implementation: it can't perform a real Box chunked upload in a single Apex
//! transaction because (a) Box only offers chunked upload for files ≥ 20 MB,
//! which can't fit the 6/12 MB heap, and (b) Apex has no `Blob` byte-slice, so
//! the base64 workaround needs a 3-byte-aligned `part_size` while Box's are
//! powers of two. This spike asks: does a **Queueable-chained** design — one
//! part per transaction, bytes pulled from a range-readable source — clear both?
//!
//! The prototype under exploration is [`SKETCH`] (`BoxChunkedUploadJob.cls`,
//! never shipped/deployed). The tests pin the properties that make the design
//! work, so this crate is the durable record of the finding.
//!
//! ## Findings
//!
//! **Both D-136 blockers dissolve.**
//!
//! - *Heap.* Each `execute()` handles exactly one part (≤ `part_size`, ~8 MB) in
//!   its own fresh 12 MB async heap. The whole file is never in memory, so the
//!   "≥ 20 MB can't fit heap" contradiction is gone. (Risk: for multi-GB files
//!   Box can issue part sizes beyond the async heap — the real impl must cap or
//!   reject those.)
//! - *Slicing.* The bytes come from an HTTP `Range` GET against the source, so
//!   the *source* slices, not Apex. No base64 substring, no 3-byte-alignment
//!   restriction — arbitrary `part_size` works.
//!
//! **Per-transaction budget is tiny.** create = 1 callout; each part = 2 (Range
//! GET + PUT); commit = 1. Far under the 100-callout cap. The chain length is
//! the part count (e.g. a 1 GB file at 8 MB = 128 links) — within async-Apex job
//! limits. Progress is serialized state, so a failed link is resumable and the
//! Box session (7-day expiry) survives across the chain.
//!
//! **Two things remain genuinely off-platform — they are inputs, not work:**
//!
//! 1. *Whole-file SHA-1 for commit.* Apex has no streaming/incremental digest
//!    and the file is never in heap whole, so the commit `Digest` must be
//!    precomputed off-platform and passed in — **or** shown to be optional
//!    (each part is already sha1-verified on its PUT; confirming this against
//!    the Box API is the key open question that decides how ergonomic this is).
//! 2. *A range-readable source.* An external URL with `Accept-Ranges: bytes`
//!    works; a raw `ContentVersion` does not (querying `VersionData` loads the
//!    whole `Blob` into heap). Pre-chunked per-part records are the alternative.
//!
//! **Serialization caveat.** The `Box client` (with its `BoxHttpClient` +
//! token provider) is carried as Queueable state and must round-trip through
//! serialization between links; all-primitive provider fields are fine, but the
//! interface-typed `tokens` field is worth verifying on-platform.
//!
//! **Conclusion.** The Queueable design is the right production path and removes
//! the fundamental blockers — feasibility hinges on the commit-`Digest`
//! question and on the app supplying a range-readable source. Neither is an Apex
//! limitation; both are integration contracts. Recommended next step: confirm
//! the commit `Digest` requirement against a live Box session, then promote a
//! hardened version into the runtime behind that contract.

/// The Apex `Queueable` prototype under exploration (never shipped/deployed).
pub const SKETCH: &str = include_str!("../BoxChunkedUploadJob.cls");
