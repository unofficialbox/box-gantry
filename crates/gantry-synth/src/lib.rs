//! Feature synthesis (FR-7).
//!
//! Language-agnostic analysis over the verified IR that backends lower
//! to idiomatic surfaces. Detection lives here — once — so Go
//! (`iter.Seq2`), Apex (transaction-bounded continuations), and Rust
//! (`Stream`) all consume the same decisions rather than each
//! re-deriving them (the FR-4.2 discipline: shared structure, per-target
//! lowering).
//!
//! Current surface: pagination detection (FR-7.3, D-013), Box's long-poll
//! event stream, and its token exchange (downscoping). Auth flows,
//! upload synthesis, tests, and docs follow.

mod event_stream;
mod pagination;
mod token_exchange;

pub use event_stream::{EventStream, PositionShape, detect_event_streams};
pub use pagination::{PageStyle, PagedOperation, detect_pagination};
pub use token_exchange::{
    ACCESS_TOKEN_TYPE, TOKEN_EXCHANGE_GRANT, TokenExchange, detect_token_exchanges,
};
