//! Source adapters, normalization, and symbol extraction for devdocs.
//!
//! The fetch guard is the only network code in the tool
//! (docs/implementation/09 §Fetch guard invariants).

pub mod error;
pub mod guard;
pub mod llms_txt;
pub mod segment;

pub use error::IngestError;
pub use guard::{FetchGuard, GuardError};
pub use llms_txt::{LlmsLink, LlmsSection, LlmsTxtIndex, parse_llms_txt};
pub use segment::{PreparedChunk, Segmenter, contextual_header};
