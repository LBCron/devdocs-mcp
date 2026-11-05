//! Source adapters, normalization, and symbol extraction for devdocs.
//!
//! The fetch guard is the only network code in the tool
//! (docs/implementation/09 §Fetch guard invariants).

pub mod chrome;
pub mod error;
pub mod events;
pub mod fixture;
pub mod git;
pub mod guard;
pub mod license;
pub mod llms_txt;
pub mod pipeline;
pub mod relations;
pub mod resolver;

pub mod segment;

pub use chrome::{ChromeReport, PageDocument, strip_chrome};
pub use error::IngestError;
pub use events::IngestEvents;
pub use fixture::{FixtureSet, RecordedResponse};
pub use git::{GitFetch, GitPlan, PinKind, TempClone, install_signal_cleanup, plan as plan_git};
pub use guard::{FetchGuard, GuardError};
pub use license::{LicenseGrant, gate_fetch, refused_status};
pub use llms_txt::{LlmsLink, LlmsSection, LlmsTxtIndex, parse_llms_txt};
pub use pipeline::{IngestJob, IngestOutcome, TOTAL_INGEST_CAP};
pub use segment::{PreparedChunk, Segmenter, contextual_header};
