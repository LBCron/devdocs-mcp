//! The retrieval pass (docs/implementation/14 §Harness flow): the real
//! devdocs-retrieve pipeline — resolve, symbol + FTS rungs, RRF fusion,
//! budget pack — one call per question. The call site and the timings are
//! the same ones the Phase 0 FTS-only stub used; a pipeline miss (a valid
//! query that served nothing) is an empty result list, the shape the
//! grading of `unanswerable` questions expects.

use devdocs_retrieve::error::RetrieveError;
use devdocs_retrieve::{Plan, ProjectContext, Query};
use devdocs_store::Store;

use crate::error::EvalError;
use crate::metrics::RetrievedRef;
use crate::suite::{Question, TaskType};

/// One question's retrieval inputs for grading (docs/implementation/14
/// §Metrics): the packed (served) refs drive `ref_match`, `hit@5`, and
/// version correctness; the symbol-rung refs — the rung-1 hits before
/// fusion and packing — drive symbol recall only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retrieval {
    /// The packed refs, in served order.
    pub refs: Vec<RetrievedRef>,
    /// The symbol-rung refs, in rung order.
    pub symbol_refs: Vec<RetrievedRef>,
}

/// Maps raw hits to grading refs, ranks starting at `first_rank`.
fn to_refs<'a>(
    hits: impl Iterator<Item = &'a devdocs_store::Hit>,
    first_rank: usize,
) -> Vec<RetrievedRef> {
    hits.enumerate()
        .map(|(i, h)| RetrievedRef {
            rank: first_rank + i,
            library: h.library.clone(),
            version: h.version.clone(),
            segments: h.section_path.clone(),
            symbol: h.symbol.clone(),
        })
        .collect()
}

/// Runs the retrieval pipeline for one question. The harness pins the
/// question's library and version like explicit flags would — except a
/// `multi_library` question, whose refs may target other libraries, so its
/// search stays global.
///
/// # Errors
/// [`EvalError::Store`] for store failures and
/// [`EvalError::Retrieve`] for a pipeline failure the harness's own inputs
/// cannot cause.
pub fn retrieve(store: &Store, question: &Question) -> Result<Retrieval, EvalError> {
    let plan = Plan::defaults();
    let query = match question.task_type {
        TaskType::MultiLibrary => Query::new(&question.question, None, None),
        _ => Query::new(
            &question.question,
            Some(&question.library),
            Some(&question.version),
        ),
    };
    let packed = match devdocs_retrieve::serve::search_packed(
        store,
        &query,
        &ProjectContext::empty(),
        &plan,
    ) {
        Ok(packed) => packed,
        // A miss is the documented "valid query, nothing served" outcome:
        // an empty result list, which the graders score as-is.
        Err(RetrieveError::Miss(_)) => {
            return Ok(Retrieval {
                refs: Vec::new(),
                symbol_refs: Vec::new(),
            });
        }
        Err(RetrieveError::Store(error)) => return Err(EvalError::from(error)),
        Err(other) => return Err(EvalError::Retrieve(other.to_string())),
    };
    Ok(Retrieval {
        refs: to_refs(packed.items.iter().map(|(h, _)| h), 1),
        // The rung hits precede the packed page, so their ranks do not
        // collide with the served ranks; grading reads only
        // library/version/symbol from them.
        symbol_refs: to_refs(packed.symbol_rung_hits.iter(), 1),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use devdocs_store::Store;
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::retrieve;
    use crate::metrics::grade_question;
    use crate::suite::{Grader, Question, TaskType};

    /// Seeds a migrated store with one chunk in each library, both
    /// documenting a "middleware" token.
    fn seeded(dir: &TempDir) -> Store {
        let path = dir.path().join("seed.db");
        devdocs_testkit::open_migrated(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        let src_a = devdocs_testkit::seed_source(&conn, "next.js", true).unwrap();
        let src_b = devdocs_testkit::seed_source(&conn, "serde", true).unwrap();
        devdocs_testkit::seed_chunk(
            &conn,
            src_a,
            0,
            r#"["app","proxy"]"#,
            Some("middleware"),
            "the middleware rewrites the request",
        )
        .unwrap();
        devdocs_testkit::seed_chunk(
            &conn,
            src_b,
            0,
            r#"["field-attrs"]"#,
            Some("rename"),
            "serde middleware rename attribute",
        )
        .unwrap();
        drop(conn);
        Store::open(&path).unwrap()
    }

    /// Seeds one chunk plus its symbol row, returning its `chunk_id`.
    fn seeded_symbol_chunk(
        conn: &Connection,
        source_id: i64,
        ordinal: i64,
        symbol: &str,
        text: &str,
    ) -> i64 {
        devdocs_testkit::seed_chunk(
            conn,
            source_id,
            ordinal,
            r#"["app","proxy"]"#,
            Some(symbol),
            text,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols(symbol, chunk_id, kind) VALUES (?1, \
             (SELECT chunk_id FROM chunks WHERE ordinal = ?2 AND source_id = ?3), 'function')",
            rusqlite::params![symbol, ordinal, source_id],
        )
        .unwrap();
        conn.query_row(
            "SELECT chunk_id FROM chunks WHERE ordinal = ?1 AND source_id = ?2",
            rusqlite::params![ordinal, source_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn question(task_type: TaskType, library: &str) -> Question {
        Question {
            id: "q-001".to_owned(),
            library: library.to_owned(),
            version: "1.0.0".to_owned(),
            task_type,
            question: "middleware".to_owned(),
            expect_refs: None,
            expect_symbols: None,
            grader: Grader::RefMatch,
            notes: "n".to_owned(),
        }
    }

    #[test]
    fn ordinary_questions_search_their_library_only() {
        let dir = TempDir::new().unwrap();
        let store = seeded(&dir);
        let retrieval = retrieve(&store, &question(TaskType::ExactApi, "next.js")).unwrap();
        assert_eq!(retrieval.refs.len(), 1);
        assert_eq!(retrieval.refs[0].library, "next.js");
        assert_eq!(retrieval.refs[0].version, "1.0.0");
        assert_eq!(retrieval.refs[0].rank, 1);

        // The other library keeps its own hits.
        let retrieval = retrieve(&store, &question(TaskType::ExactApi, "serde")).unwrap();
        assert_eq!(retrieval.refs.len(), 1);
        assert_eq!(retrieval.refs[0].library, "serde");
    }

    #[test]
    fn multi_library_questions_search_across_libraries() {
        let dir = TempDir::new().unwrap();
        let store = seeded(&dir);
        let retrieval = retrieve(&store, &question(TaskType::MultiLibrary, "next.js")).unwrap();
        let mut libraries: Vec<&str> = retrieval.refs.iter().map(|r| r.library.as_str()).collect();
        libraries.sort_unstable();
        assert_eq!(libraries, vec!["next.js", "serde"]);
    }

    /// A question whose text matches nothing retrieves the empty list —
    /// the miss outcome the `unanswerable` grading expects.
    #[test]
    fn a_pipeline_miss_retrieves_no_refs() {
        let dir = TempDir::new().unwrap();
        let store = seeded(&dir);
        let mut q = question(TaskType::Unanswerable, "next.js");
        q.question = "nonexistent widget flux".to_owned();
        let retrieval = retrieve(&store, &q).unwrap();
        assert_eq!(retrieval.refs, Vec::new());
        assert_eq!(retrieval.symbol_refs, Vec::new());
    }

    /// A pinned symbol rides through the symbol rung: the chunk that
    /// documents the identifier retrieves first with its raw symbol, so
    /// symbol-recall grading sees it.
    #[test]
    fn pinned_symbols_retrieve_through_the_symbol_rung() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("seed.db");
        devdocs_testkit::open_migrated(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        let src = devdocs_testkit::seed_source(&conn, "next.js", true).unwrap();
        seeded_symbol_chunk(
            &conn,
            src,
            0,
            "rewriteMiddleware",
            "the middleware rewrites the request",
        );
        drop(conn);
        let store = Store::open(&path).unwrap();

        let mut q = question(TaskType::ExactApi, "next.js");
        q.question = "rewriteMiddleware()".to_owned();
        q.expect_symbols = Some(vec!["rewriteMiddleware".to_owned()]);
        let retrieval = retrieve(&store, &q).unwrap();
        assert_eq!(retrieval.refs.len(), 1);
        assert_eq!(
            retrieval.refs[0].symbol.as_deref(),
            Some("rewriteMiddleware")
        );
        assert_eq!(
            retrieval.refs[0].rank, 1,
            "the exact symbol hit retrieves first"
        );
        assert_eq!(retrieval.symbol_refs.len(), 1);
        assert_eq!(
            retrieval.symbol_refs[0].symbol.as_deref(),
            Some("rewriteMiddleware")
        );
    }

    /// The FTS-only counterexample (docs/implementation/14 §Metrics): a
    /// chunk whose `chunks.symbol` carries the expected symbol but with no
    /// symbol-rung entry enters only through the FTS rung. It packs into
    /// the served page, yet it must not count toward symbol recall — the
    /// metric grades the symbol rung, not the packed page.
    #[test]
    fn an_fts_only_hit_does_not_count_toward_symbol_recall() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("seed.db");
        devdocs_testkit::open_migrated(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        let src = devdocs_testkit::seed_source(&conn, "next.js", true).unwrap();
        // No symbols row: the symbol rung serves nothing for this chunk.
        devdocs_testkit::seed_chunk(
            &conn,
            src,
            0,
            r#"["app","proxy"]"#,
            Some("ghostSymbol"),
            "the middleware rewrites the request",
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&path).unwrap();

        let mut q = question(TaskType::ExactApi, "next.js");
        q.question = "ghostSymbol()".to_owned();
        q.expect_symbols = Some(vec!["ghostSymbol".to_owned()]);
        q.grader = Grader::SymbolMatch;
        let retrieval = retrieve(&store, &q).unwrap();
        // The FTS hit serves and carries the chunk symbol…
        assert_eq!(retrieval.refs.len(), 1);
        assert_eq!(retrieval.refs[0].symbol.as_deref(), Some("ghostSymbol"));
        // …but the symbol rung stayed empty, so recall grades 0.
        assert!(
            retrieval.symbol_refs.is_empty(),
            "the symbol rung served nothing"
        );
        let graded = grade_question(&q, &retrieval.refs, &retrieval.symbol_refs);
        assert_eq!(
            graded.symbols_found, 0,
            "an FTS-only hit is not symbol recall"
        );
        assert!(!graded.passed);
    }

    /// The packed-out counterexample (docs/implementation/14 §Metrics): a
    /// symbol-rung hit the token budget pushed off the served page still
    /// counts toward symbol recall, because the metric grades the rung
    /// results, not the packed page.
    #[test]
    fn a_symbol_rung_hit_packed_out_of_the_page_still_counts() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("seed.db");
        devdocs_testkit::open_migrated(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        let src = devdocs_testkit::seed_source(&conn, "next.js", true).unwrap();
        // Both symbols enter the rung through the prefix attempt ("rewrite"
        // exact matches nothing); chunk-id order puts the small hit first.
        seeded_symbol_chunk(&conn, src, 0, "rewriteMiddleware", "rewrite doc body");
        let huge = seeded_symbol_chunk(&conn, src, 1, "rewriteSerial", "rewrite doc body");
        conn.execute(
            "UPDATE chunks SET token_count = 3000 WHERE chunk_id = ?1",
            [huge],
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&path).unwrap();

        let mut q = question(TaskType::ExactApi, "next.js");
        q.question = "rewrite()".to_owned();
        q.expect_symbols = Some(vec![
            "rewriteMiddleware".to_owned(),
            "rewriteSerial".to_owned(),
        ]);
        q.grader = Grader::SymbolMatch;
        let retrieval = retrieve(&store, &q).unwrap();
        // The budget dropped the huge chunk off the served page…
        let packed_symbols: Vec<_> = retrieval
            .refs
            .iter()
            .filter_map(|r| r.symbol.as_deref())
            .collect();
        assert!(
            !packed_symbols.contains(&"rewriteSerial"),
            "the packed page must not serve the huge chunk: {packed_symbols:?}"
        );
        // …while the rung results keep it, so recall stays full.
        let rung_symbols: Vec<_> = retrieval
            .symbol_refs
            .iter()
            .filter_map(|r| r.symbol.as_deref())
            .collect();
        assert!(
            rung_symbols.contains(&"rewriteSerial"),
            "the rung result keeps the packed-out hit: {rung_symbols:?}"
        );
        let graded = grade_question(&q, &retrieval.refs, &retrieval.symbol_refs);
        assert_eq!(graded.symbols_found, 2);
        assert!(graded.passed, "the packed-out rung hit still counts");
        assert_eq!(graded.symbol_recall(), Some(1.0));
    }

    /// A store failure surfaces as the eval store error — a corrupted
    /// index must not grade as an empty retrieval.
    #[test]
    fn a_store_failure_surfaces_as_an_eval_error() {
        let dir = TempDir::new().unwrap();
        let store = seeded(&dir);
        let path = dir.path().join("seed.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute("DROP TABLE symbols", []).unwrap();
        drop(conn);

        let error = retrieve(&store, &question(TaskType::ExactApi, "next.js")).unwrap_err();
        assert!(
            matches!(error, crate::error::EvalError::Store(_)),
            "{error}"
        );
    }
}
