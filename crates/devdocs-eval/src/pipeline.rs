//! The Phase 0 retrieval stub (docs/implementation/14 §Harness flow).
//!
//! While M4 is in flight (docs/implementation/13), the harness runs one
//! FTS5 rung through the store: `build_match_query` over the question text,
//! then the store search with the documented bm25 weights. Ordinary
//! questions scope the search to their library inside the store query, so
//! the limit counts same-library hits only; a `multi_library` question may
//! pin refs for other libraries, so its search stays global. The
//! devdocs-retrieve pipeline replaces this stub; the call site and the
//! timings stay the same.

use devdocs_store::{Store, build_match_query};

use crate::error::EvalError;
use crate::metrics::RetrievedRef;
use crate::suite::{Question, TaskType};

/// The result budget the harness retrieves with: the search default
/// (docs/implementation/08 §CLI contract).
const RESULT_LIMIT: u32 = 8;

/// Runs the stub retrieval for one question.
///
/// # Errors
/// Propagates store failures.
pub fn retrieve(store: &Store, question: &Question) -> Result<Vec<RetrievedRef>, EvalError> {
    let expr = build_match_query(&question.question);
    let hits = match question.task_type {
        TaskType::MultiLibrary => store.search_fts(&expr, RESULT_LIMIT)?,
        _ => store.search_fts_library(&expr, &question.library, RESULT_LIMIT)?,
    };
    let refs = hits
        .iter()
        .enumerate()
        .map(|(i, h)| RetrievedRef {
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
    use devdocs_testkit::{seed_chunk, seed_source};
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
        let src_a = seed_source(&conn, "next.js", true).unwrap();
        let src_b = seed_source(&conn, "serde", true).unwrap();
        seed_chunk(
            &conn,
            src_a,
            0,
            r#"["app","proxy"]"#,
            Some("middleware"),
            "the middleware rewrites the request",
        )
        .unwrap();
        seed_chunk(
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
}
