//! Source adapters, normalization, and symbol extraction for devdocs.
//!
//! The fetch guard is the only network code in the tool
//! (docs/implementation/09 §Fetch guard invariants).

pub mod guard;

pub use guard::{FetchGuard, GuardError};
