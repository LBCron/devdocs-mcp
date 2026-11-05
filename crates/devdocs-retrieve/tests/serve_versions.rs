//! The serve-layer read surfaces: the `versions` discovery list
//! (docs/implementation/08 §CLI contract), the search pipeline's
//! migration rung, miss reasons, and cursor resume, and the `open` and
//! `verify` ref paths.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use devdocs_model::MissReason;
use devdocs_model::VersionStatus;
use devdocs_retrieve::Plan;
use devdocs_retrieve::Query;
use devdocs_retrieve::RetrieveError;
use devdocs_retrieve::serve::{open, search, versions};
use devdocs_store::Store;
use tempfile::tempdir;

/// Seeds one active and one inactive source of `acme`, plus one active
/// source of another library.
fn seeded() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated_and(&dir.path().join("docs.db"), |conn| {
        devdocs_testkit::seed_source(conn, "acme", true)?;
        conn.execute(
            "INSERT INTO sources(library, version, kind, origin_url, license_status, fetched_at, active) \
             VALUES ('acme', '0.9.0', 'llms_txt', 'https://example.test/acme-0', 'approved_index', \
             '2026-01-01T00:00:00Z', 0)",
            [],
        )?;
        devdocs_testkit::seed_source(conn, "other", true)?;
        Ok(())
    })
    .unwrap();
    (dir, store)
}

/// Only active sources list; a withdrawn generation is not an available
/// version.
#[test]
fn versions_list_only_active_sources() {
    let (_dir, store) = seeded();
    let envelope = versions(&store, "acme").unwrap();
    let listed: Vec<&str> = envelope
        .results
        .iter()
        .map(|s| s.version.as_str())
        .collect();
    assert_eq!(listed, vec!["1.0.0"]);
}

/// A library with no active sources misses with `not_indexed`.
#[test]
fn an_unknown_library_misses() {
    let (_dir, store) = seeded();
    let error = versions(&store, "missing").unwrap_err();
    assert!(
        matches!(error, RetrieveError::Miss(MissReason::NotIndexed)),
        "unknown library must miss, got {error}"
    );
}

/// Seeds `acme` with two active versions — 1.0.0 and 0.9.0 — one routing
/// chunk per version.
fn seeded_two_versions() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated_and(&dir.path().join("docs.db"), |conn| {
        let current = devdocs_testkit::seed_source(conn, "acme", true)?;
        conn.execute(
            "INSERT INTO sources(library, version, kind, origin_url, license_status, fetched_at, active) \
             VALUES ('acme', '0.9.0', 'llms_txt', 'https://example.test/acme-old', 'approved_index', \
             '2026-01-01T00:00:00Z', 1)",
            [],
        )?;
        let older = conn.last_insert_rowid();
        devdocs_testkit::seed_chunk(
            conn,
            current,
            0,
            "[\"Routing\"]",
            None,
            "The routing layer helps you migrate routing requests.",
        )?;
        devdocs_testkit::seed_chunk(
            conn,
            older,
            0,
            "[\"Routing\"]",
            None,
            "The routing layer helps you migrate routing requests.",
        )?;
        Ok(())
    })
    .unwrap();
    (dir, store)
}

/// Seeds `acme` with three routing chunks in distinct sections so each
/// carries its own ref.
fn seeded_three_chunks() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated_and(&dir.path().join("docs.db"), |conn| {
        let source = devdocs_testkit::seed_source(conn, "acme", true)?;
        for (ordinal, section) in ["Routing", "Storage", "Cache"].iter().enumerate() {
            devdocs_testkit::seed_chunk(
                conn,
                source,
                i64::try_from(ordinal).unwrap(),
                &format!("[\"{section}\"]"),
                None,
                "The routing layer maps requests.",
            )?;
        }
        Ok(())
    })
    .unwrap();
    (dir, store)
}

/// A migration question retrieves the served version and the nearest
/// older version — the migration rung runs and labels the older hits.
#[test]
fn a_migration_question_retrieves_the_older_version() {
    let (dir, store) = seeded_two_versions();
    let query = Query::new("Migrate routing", Some("acme"), None);
    let envelope = search(&store, &query, dir.path(), &Plan::defaults()).unwrap();
    let served: Vec<(String, VersionStatus)> = envelope
        .results
        .iter()
        .map(|item| (item.served_version.clone(), item.version_status))
        .collect();
    assert!(
        served.contains(&("1.0.0".to_owned(), VersionStatus::Alias)),
        "the served version must answer with the alias default, got {served:?}"
    );
    assert!(
        served.contains(&("0.9.0".to_owned(), VersionStatus::Fallback)),
        "the migration rung must fetch the older version, got {served:?}"
    );
}

/// A version-only resolution (`--version` without `--library`) filters
/// the fallback search to the requested version — no other version
/// serves (docs/08 §Version resolution: explicit flags take top
/// precedence).
#[test]
fn a_version_only_request_filters_to_that_version() {
    let (dir, store) = seeded_two_versions();
    let query = Query::new("routing", None, Some("0.9.0"));
    let envelope = search(&store, &query, dir.path(), &Plan::defaults()).unwrap();
    let versions: Vec<&str> = envelope
        .results
        .iter()
        .map(|item| item.served_version.as_str())
        .collect();
    assert_eq!(versions, vec!["0.9.0"]);
    assert!(
        envelope
            .results
            .iter()
            .all(|item| item.version_status == VersionStatus::Verified),
        "the exact-request hits must verify"
    );
}

/// Searching an empty migrated store misses with `not_indexed`, not
/// `low_confidence` — there is no active source to search.
#[test]
fn an_empty_index_misses_not_indexed() {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
    let query = Query::new("anything", None, None);
    let error = search(&store, &query, dir.path(), &Plan::defaults()).unwrap_err();
    assert!(
        matches!(error, RetrieveError::Miss(MissReason::NotIndexed)),
        "an empty index must miss with not_indexed, got {error}"
    );
}

/// A store failure while classifying the empty fusion surfaces as the
/// store error — the miss classification must not swallow it.
#[test]
fn a_store_failure_on_the_empty_fusion_path_surfaces() {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated_and(&dir.path().join("docs.db"), |conn| {
        devdocs_testkit::seed_source(conn, "acme", true)?;
        // Corrupt the active flag behind the store's back: a non-numeric
        // value clears the CHECK-free column, `list_sources` fails to
        // read it back as an integer, and the FTS path only filters the
        // row out.
        conn.execute("UPDATE sources SET active = 'x'", [])?;
        Ok(())
    })
    .unwrap();
    let query = Query::new("zzz_no_match", None, None);
    let error = search(&store, &query, dir.path(), &Plan::defaults()).unwrap_err();
    assert!(
        matches!(error, RetrieveError::Store(_)),
        "the store failure must surface, got {error}"
    );
}

/// Resuming with `next_cursor` continues after the served chunk in the
/// same fusion order.
#[test]
fn search_resumes_after_the_cursor() {
    let (dir, store) = seeded_three_chunks();
    let query = Query::new("routing", Some("acme"), None);
    let full = search(&store, &query, dir.path(), &Plan::defaults()).unwrap();
    assert!(!full.truncated);
    assert_eq!(full.next_cursor, None);
    let all_refs: Vec<String> = full
        .results
        .iter()
        .map(|item| item.doc_ref.clone())
        .collect();
    assert_eq!(all_refs.len(), 3);

    let budget = Plan {
        max_tokens: 8,
        ..Plan::defaults()
    };
    let first = search(&store, &query, dir.path(), &budget).unwrap();
    assert!(first.truncated);
    let cursor: u64 = first.next_cursor.unwrap().parse().unwrap();
    let resumed_plan = Plan {
        cursor: Some(cursor),
        ..budget
    };
    let resumed = search(&store, &query, dir.path(), &resumed_plan).unwrap();

    // The resume continues in the same fusion order: it serves the chunk
    // after the cursor's chunk and nothing before it.
    let served = first.results.first().unwrap().doc_ref.clone();
    let position = all_refs.iter().position(|r| *r == served).unwrap();
    let resumed_refs: Vec<String> = resumed
        .results
        .iter()
        .map(|item| item.doc_ref.clone())
        .collect();
    assert_eq!(resumed_refs, vec![all_refs[position + 1].clone()]);
}

/// The envelope never carries more than `plan.limit` results, pages carry
/// `truncated` plus a cursor while more hits remain, and the pages cover
/// every hit exactly once in fusion order.
#[test]
fn search_caps_results_at_the_plan_limit() {
    let (dir, store) = seeded_three_chunks();
    let query = Query::new("routing", Some("acme"), None);
    let capped = Plan {
        limit: 2,
        ..Plan::defaults()
    };
    let first = search(&store, &query, dir.path(), &capped).unwrap();
    assert_eq!(first.results.len(), 2);
    assert!(first.truncated);
    let cursor: u64 = first.next_cursor.clone().unwrap().parse().unwrap();

    let resumed = Plan {
        cursor: Some(cursor),
        ..capped
    };
    let second = search(&store, &query, dir.path(), &resumed).unwrap();
    assert!(second.results.len() <= 2);
    assert!(!second.truncated);
    assert_eq!(second.next_cursor, None);

    let paged_refs: Vec<String> = first
        .results
        .iter()
        .chain(&second.results)
        .map(|item| item.doc_ref.clone())
        .collect();
    let full = search(&store, &query, dir.path(), &Plan::defaults()).unwrap();
    let all_refs: Vec<String> = full
        .results
        .iter()
        .map(|item| item.doc_ref.clone())
        .collect();
    assert_eq!(paged_refs, all_refs);
}

/// Opening a ref whose library has no active source misses with
/// `not_indexed`.
#[test]
fn open_of_an_unindexed_library_misses() {
    let (_dir, store) = seeded();
    let error = open(&store, "docs://nope/v1/x", &Plan::defaults()).unwrap_err();
    assert!(
        matches!(error, RetrieveError::Miss(MissReason::NotIndexed)),
        "unindexed library must miss, got {error}"
    );
}

/// Seeds `acme` with two headerless chunks (content before the first
/// heading).
fn seeded_headless_chunks() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated_and(&dir.path().join("docs.db"), |conn| {
        let source = devdocs_testkit::seed_source(conn, "acme", true)?;
        devdocs_testkit::seed_chunk(conn, source, 0, "[]", None, "Intro text before a heading.")?;
        devdocs_testkit::seed_chunk(conn, source, 1, "[]", None, "More head content.")?;
        Ok(())
    })
    .unwrap();
    (dir, store)
}

/// A headerless chunk (an empty section path) serves through the
/// placeholder ref: the ref grammar requires a path, and `open` resolves
/// `index` to the lowest chunk id when two headless chunks share it.
#[test]
fn open_serves_headerless_chunks_through_the_placeholder_ref() {
    let (_dir, store) = seeded_headless_chunks();
    let envelope = open(&store, "docs://acme/v1.0.0/index", &Plan::defaults()).unwrap();
    let served = envelope.results.first().unwrap();
    assert_eq!(served.section, "");
    assert_eq!(served.snippet, "Intro text before a heading.");
}

/// Seeds `acme` with one multibyte chunk whose every line ends in a wide
/// character, so a char-offset cursor sliced as a byte offset would land
/// inside a character and panic.
fn seeded_multibyte_chunk() -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = devdocs_testkit::open_migrated_and(&dir.path().join("docs.db"), |conn| {
        let source = devdocs_testkit::seed_source(conn, "acme", true)?;
        devdocs_testkit::seed_chunk(conn, source, 0, "[\"Guide\"]", None, "A ›\nB →\nC ∴\n")?;
        Ok(())
    })
    .unwrap();
    (dir, store)
}

/// Resuming an `open` page with the returned cursor keeps working on a
/// multibyte chunk: the cursor is a char offset, no page panics, and the
/// pages concatenate back to the chunk text losslessly.
#[test]
fn open_resumes_a_multibyte_chunk_without_panic() {
    let (_dir, store) = seeded_multibyte_chunk();
    let budget = Plan {
        max_tokens: 2,
        ..Plan::defaults()
    };
    let full_text = "A ›\nB →\nC ∴\n";

    let mut served: Vec<String> = Vec::new();
    let mut cursor: Option<u64> = None;
    for _ in 0..8 {
        let page_plan = Plan {
            cursor,
            ..budget.clone()
        };
        let envelope = open(&store, "docs://acme/v1.0.0/guide", &page_plan).unwrap();
        served.push(envelope.results.first().unwrap().snippet.clone());
        if !envelope.truncated {
            break;
        }
        cursor = Some(envelope.next_cursor.unwrap().parse().unwrap());
    }
    assert!(served.len() > 1, "the budget must cut the chunk");
    assert_eq!(served.concat(), full_text);
}
