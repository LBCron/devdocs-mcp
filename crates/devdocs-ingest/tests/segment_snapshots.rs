//! Segmentation snapshot tests (docs/implementation/04 §Testing strategy:
//! "Segmentation snapshots are stable"). The snapshots pin the chunk shape
//! — breadcrumbs, headers, bodies, token counts, and page ranges — for two
//! realistic pages. Accept a changed snapshot only after reading the diff.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use insta::assert_json_snapshot;
use serde_json::json;

use devdocs_ingest::segment::Segmenter;

/// The quickstart page: sections, fences, a long paragraph.
const QUICKSTART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/pages/quickstart.md"
));

/// The API reference page: nested headings and an oversized fence.
const API_REFERENCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/pages/api_reference.md"
));

/// Projects one chunk to the snapshot shape; the content hash renders as
/// hex so the snapshot stays readable.
fn snapshot_chunk(chunk: &devdocs_ingest::PreparedChunk) -> serde_json::Value {
    let hash: String = chunk
        .content_hash
        .iter()
        .fold(String::new(), |acc, b| format!("{acc}{b:02x}"));
    json!({
        "section_path": chunk.section_path,
        "header": chunk.header,
        "body": chunk.body,
        "token_count": chunk.token_count,
        "content_hash": hash,
        "page_range": [chunk.page_range.start, chunk.page_range.end],
    })
}

#[test]
fn quickstart_page_segments_stably() {
    let chunks: Vec<_> = Segmenter::new()
        .unwrap()
        .segment("acme", "2.1.0", QUICKSTART)
        .iter()
        .map(snapshot_chunk)
        .collect();
    assert_json_snapshot!(chunks);
}

#[test]
fn api_reference_page_segments_stably() {
    let chunks: Vec<_> = Segmenter::new()
        .unwrap()
        .segment("acme", "2.1.0", API_REFERENCE)
        .iter()
        .map(snapshot_chunk)
        .collect();
    assert_json_snapshot!(chunks);
}
