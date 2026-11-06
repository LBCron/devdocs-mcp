//! Core types, configuration, and errors for devdocs.

pub mod chunk;
pub mod config;
pub mod docref;
pub mod envelope;
pub mod error;
pub mod source;

pub use chunk::Chunk;
pub use config::{Config, Overrides, Paths, SourceConfig};
pub use docref::ParsedRef;
pub use envelope::{
    Example, ResultEnvelope, ResultItem, SourceListEntry, SourceListVersion, SourceSummary,
    VersionStatus, empty_envelope, now_rfc3339,
};
pub use error::{Error, ErrorEnvelope, MissReason, UpstreamCode};
pub use source::{LicenseStatus, SourceId, SourceKind, SourceRecord};
