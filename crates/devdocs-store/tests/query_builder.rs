//! Property tests for the FTS5 MATCH query builder.
//!
//! Required property (docs/implementation/04 §Properties): the query
//! escaper produces a valid FTS5 MATCH expression for any input string.
//! Validity is checked against the real FTS5 implementation: the built
//! expression must execute as a MATCH against a populated schema-0001
//! database without error.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use proptest::prelude::*;
use rusqlite::Connection;
use tempfile::tempdir;

use devdocs_store::build_match_query;

/// A migrated database with one seeded chunk, shared by both properties.
/// The seed body carries no `and`/`or`/`not` tokens, so operator-only
/// queries cannot match it.
fn seeded_db() -> (tempfile::TempDir, Connection) {
    let dir = tempdir().unwrap();
    let _store = devdocs_testkit::open_migrated(&dir.path().join("docs.db")).unwrap();
    let conn = Connection::open(dir.path().join("docs.db")).unwrap();
    let source_id = devdocs_testkit::seed_source(&conn, "p", true).unwrap();
    devdocs_testkit::seed_chunk(
        &conn,
        source_id,
        0,
        "[\"Docs\"]",
        None,
        "The router hook configures navigation. use-route stays whole.",
    )
    .unwrap();
    (dir, conn)
}

/// Runs a MATCH and fails with the offending expression on any engine error.
fn count_matches(conn: &Connection, expr: &str, input: &str) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
        rusqlite::params![expr],
        |row| row.get(0),
    )
    .unwrap_or_else(|e| panic!("MATCH failed for input {input:?} -> {expr:?}: {e}"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any input string yields an expression the FTS5 engine accepts.
    #[test]
    fn any_input_yields_a_valid_match_expression(input in ".*") {
        let expr = build_match_query(&input).into_owned();
        let (_dir, conn) = seeded_db();
        prop_assert!(count_matches(&conn, &expr, &input) >= 0);
    }

    /// Operator-only input becomes a valid query that matches nothing.
    #[test]
    fn operator_only_input_matches_nothing(ops in "(AND|OR|NOT|NEAR|-|\\*|\\^|:|\\(|\\)){1,10}") {
        let expr = build_match_query(&ops).into_owned();
        let (_dir, conn) = seeded_db();
        prop_assert_eq!(
            count_matches(&conn, &expr, &ops),
            0,
            "operator-only input must not match prose"
        );
    }
}
