//! Core types, configuration, and errors for devdocs.

pub mod chunk;
pub mod config;
pub mod envelope;
pub mod error;
pub mod source;

pub use chunk::Chunk;
pub use config::{Config, Overrides, Paths, SourceConfig};
pub use envelope::{Example, ResultEnvelope, ResultItem, VersionStatus, empty_envelope};
pub use error::{Error, ErrorEnvelope, MissReason, UpstreamCode};
pub use source::{LicenseStatus, SourceId, SourceKind, SourceRecord};
