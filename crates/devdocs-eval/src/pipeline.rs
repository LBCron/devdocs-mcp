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

/// Runs the retrieval pipeline for one question. The harness pins the
/// question's library and version like explicit flags would — except a
/// `multi_library` question, whose refs may target other libraries, so its
/// search stays global.
///
/// # Errors
/// [`EvalError::Store`] for store failures and
/// [`EvalError::Retrieve`] for a pipeline failure the harness's own inputs
/// cannot cause.
pub fn retrieve(store: &Store, question: &Question) -> Result<Vec<RetrievedRef>, EvalError> {
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
        Err(RetrieveError::Miss(_)) => return Ok(Vec::new()),
        Err(RetrieveError::Store(error)) => return Err(EvalError::from(error)),
        Err(other) => return Err(EvalError::Retrieve(other.to_string())),
    };
    let refs = packed
        .items
        .iter()
        .enumerate()
        .map(|(i, (h, _))| RetrievedRef {
            rank: i + 1,
            library: h.library.clone(),
            version: h.version.clone(),
            segments: h.section_path.clone(),
            symbol: h.symbol.clone(),
        })
        .collect();
    Ok(refs)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use devdocs_store::Store;
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::retrieve;
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
        let refs = retrieve(&store, &question(TaskType::ExactApi, "next.js")).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].library, "next.js");
        assert_eq!(refs[0].version, "1.0.0");
        assert_eq!(refs[0].rank, 1);

        // The other library keeps its own hits.
        let refs = retrieve(&store, &question(TaskType::ExactApi, "serde")).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].library, "serde");
    }

    #[test]
    fn multi_library_questions_search_across_libraries() {
        let dir = TempDir::new().unwrap();
        let store = seeded(&dir);
        let refs = retrieve(&store, &question(TaskType::MultiLibrary, "next.js")).unwrap();
        let mut libraries: Vec<&str> = refs.iter().map(|r| r.library.as_str()).collect();
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
        let refs = retrieve(&store, &q).unwrap();
        assert_eq!(refs, Vec::new());
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
        devdocs_testkit::seed_chunk(
            &conn,
            src,
            0,
            r#"["app","proxy"]"#,
            Some("rewriteMiddleware"),
            "the middleware rewrites the request",
        )
        .unwrap();
        let chunk_id: i64 = conn
            .query_row("SELECT chunk_id FROM chunks LIMIT 1", [], |row| row.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO symbols(symbol, chunk_id, kind) VALUES ('rewriteMiddleware', ?1, 'function')",
            [chunk_id],
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&path).unwrap();

        let mut q = question(TaskType::ExactApi, "next.js");
        q.question = "rewriteMiddleware()".to_owned();
        q.expect_symbols = Some(vec!["rewriteMiddleware".to_owned()]);
        let refs = retrieve(&store, &q).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].symbol.as_deref(), Some("rewriteMiddleware"));
        assert_eq!(refs[0].rank, 1, "the exact symbol hit retrieves first");
    }
}
