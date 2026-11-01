//! Chunk shape shared by the ingest and store layers.

/// One stored retrieval unit: a normalized piece of one source document.
///
/// Field names follow docs/implementation/05 §Core types. `text` carries the
/// contextual header prepended to the body; `content_hash` hashes the
/// canonical body without that header (docs/implementation/06 §Hashing
/// policy).
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// Store-assigned row identity (`chunks.chunk_id`).
    pub id: i64,
    /// The source this chunk belongs to.
    pub source: crate::SourceId,
    /// Breadcrumb of section titles, e.g. `["Routing", "Proxy"]`.
    pub section_path: Vec<String>,
    /// Exact symbol this chunk documents, when one applies.
    pub symbol: Option<String>,
    /// Body with the contextual header prepended.
    pub text: String,
    /// Token count of `text`.
    pub token_count: u32,
    /// SHA-256 of the canonical body, 32 raw bytes.
    pub content_hash: [u8; 32],
    /// Position of the chunk inside its source, zero-based.
    pub ordinal: u32,
}
