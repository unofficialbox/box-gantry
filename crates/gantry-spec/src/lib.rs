//! OpenAPI ingestion (FR-1).
//!
//! The layer where *all* spec-shaping knowledge lives: naming rules,
//! manager grouping, Box quirks (FR-1.2, FR-1.3). Drivers cannot bypass it
//! — the `PostFolders` lesson (G-18).
//!
//! Built loud-fail first (FR-1.4, per PLAN.md): every error names the
//! offending file and the JSON path inside it, and any error fails the
//! whole run. There is no partial ingestion.
//!
//! Current slice (M1): multi-document loading keyed by API version
//! (FR-1.1 start), the document-level invariants the rest of the engine
//! depends on (unique `operationId`s, `x-box-tag` manager grouping on
//! every operation), and summary reporting for `gantry check`. The full
//! schema model and quirk handling (FR-1.3) land as M1 progresses.

mod error;
mod ingest;
mod lower;
mod raw;

pub use error::IngestError;
pub use ingest::{Document, HttpMethod, OperationSummary, SpecSet};
pub use lower::{Lowering, LoweringStats, lower};
pub use raw::RawSchema;
