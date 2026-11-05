//! llms.txt parse snapshots (docs/implementation/04 §Testing strategy: the
//! three fixture shapes pin the parsed index shape). The snapshots cover a
//! Mintlify-style index, an index with no links, and a v2 file with
//! `rel="alternate"` markers. Accept a changed snapshot only after reading
//! the diff.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use insta::{assert_json_snapshot, assert_snapshot};
use serde_json::json;

use devdocs_ingest::LlmsTxtIndex;
use devdocs_ingest::parse_llms_txt;
use devdocs_testkit::fixtures::{
    LLMS_TXT_MINTLIFY_STYLE, LLMS_TXT_NO_LINKS, LLMS_TXT_V2_RELATIONS,
};

/// Projects a parsed index to the snapshot shape: title, summary, and the
/// sections' links with their v2 relations.
fn snapshot_index(index: &LlmsTxtIndex) -> serde_json::Value {
    json!({
        "title": index.title,
        "summary": index.summary,
        "sections": index
            .sections
            .iter()
            .map(|section| {
                json!({
                    "heading": section.heading,
                    "links": section
                        .links
                        .iter()
                        .map(|link| {
                            json!({
                                "title": link.title(),
                                "url": link.url(),
                                "description": link.description(),
                                "markdown_twin": link.markdown_twin(),
                                "described_by": link.described_by(),
                            })
                        })
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
    })
}

/// The Mintlify-style index parses into its pinned shape: two H2 groups,
/// one `.md` link per page.
#[test]
fn the_mintlify_style_index_parses_stably() {
    let index = parse_llms_txt(LLMS_TXT_MINTLIFY_STYLE, "https://docs.acme.dev/llms.txt").unwrap();
    assert_json_snapshot!(snapshot_index(&index));
}

/// The zero-links index refuses with `LLMS_TXT_EMPTY` and the pinned
/// message.
#[test]
fn the_no_links_index_refuses_stably() {
    let error = parse_llms_txt(LLMS_TXT_NO_LINKS, "https://docs.acme.dev/llms.txt").unwrap_err();
    assert_snapshot!(error.to_string());
}

/// The v2 file parses with the relation hints pinned: the markdown twin
/// and the covering index ride the parenthesized per-page hints.
#[test]
fn the_v2_relations_file_parses_stably() {
    let index = parse_llms_txt(LLMS_TXT_V2_RELATIONS, "https://docs.acme.dev/llms.txt").unwrap();
    assert_json_snapshot!(snapshot_index(&index));
}
