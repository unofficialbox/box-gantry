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
//! the part count (e.g. a 1 GB file at 8 MB = 128 links). **Caveat (review):**
//! Developer and Trial orgs cap a chained-Queueable stack at **5 by default**, so
//! a long chain there needs `AsyncOptions.MaximumQueueableStackDepth` set on the
//! *initial* enqueue (production orgs have no such cap). Progress is serialized
//! Queueable state, so a failed link can resume and the Box session (7-day
//! expiry) survives — but see the durable-checkpoint item below.
//!
//! **Two things remain genuinely off-platform — they are inputs, not work:**
//!
//! 1. *Whole-file SHA-1 for commit.* Apex has no streaming/incremental digest
//!    and the file is never in heap whole, so the commit `Digest` must be
//!    precomputed off-platform and passed in. (Box **requires** the whole-file
//!    digest on commit and verifies it against the assembled parts, so it isn't
//!    optional — confirmed against the Box docs.)
//! 2. *A range-readable source.* An external URL with `Accept-Ranges: bytes`
//!    works; a raw `ContentVersion` does not (querying `VersionData` loads the
//!    whole `Blob` into heap). Pre-chunked per-part records are the alternative.
//!
//! **Source must be immutable for the whole session (review).** Because parts
//! are fetched across many transactions, a mid-upload change to the source makes
//! the precomputed whole-file digest — and already-uploaded parts — no longer
//! match, so the commit fails after wasted work. The production path must pin the
//! source: an immutable/versioned URL, or a stable `ETag` carried with
//! `If-Range`/`If-Match` on every part, aborting and restarting the session when
//! it changes.
//!
//! **Serialization caveat.** The `Box client` (with its `BoxHttpClient` +
//! token provider) is carried as Queueable state and must round-trip through
//! serialization between links; all-primitive provider fields are fine, but the
//! interface-typed `tokens` field is worth verifying on-platform.
//!
//! ## What the production impl must add (beyond this spike)
//!
//! The prototype deliberately omits hardening a throwaway won't exercise; a real
//! implementation must:
//!
//! - **Durable checkpoints.** Persist `sessionId` / `nextOffset` / accepted
//!   parts to a record (not just Queueable instance state) after each part, so a
//!   crash between links can resume rather than orphan the Box session.
//! - **Validate each range fetch.** Reject any status other than `206`, and check
//!   `Content-Range` + body length equal the requested slice, before hashing or
//!   uploading — a source that ignores `Range` returns the whole file and blows
//!   the heap; a bad `206` ships wrong bytes to Box.
//! - **Guard `part_size`.** Reject null / zero / negative (zero loops forever)
//!   and any value above the measured async-heap budget, before allocating a
//!   part `Blob` or enqueuing the next link.
//! - **Pin the source** (see above) and **configure chain depth** for Dev/Trial.
//!
//! **Conclusion.** The Queueable design is the right production path and removes
//! the fundamental D-136 blockers — feasibility hinges only on the app supplying
//! an immutable, range-readable source and a precomputed whole-file digest, both
//! integration contracts rather than Apex limits. Recommended next step: build a
//! hardened version (the checklist above) behind that contract, verified on a
//! live Box session.

/// The Apex `Queueable` prototype under exploration (never shipped/deployed).
pub const SKETCH: &str = include_str!("../BoxChunkedUploadJob.cls");
