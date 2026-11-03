//! The golden-set suite: the question schema, validation, and JSONL loading
//! (docs/implementation/14 §Golden-set format).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::EvalError;
use crate::refs::{ensure_pinned_version, validate_ref};

/// The eight-category task-type union (docs/implementation/14
/// §Golden-set format; research: benchmark-protocol.md Step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    /// Known library and version, exact API lookup.
    ExactApi,
    /// Known library, unknown page, natural-language how-to.
    HowTo,
    /// Unknown or niche library discovery.
    Discovery,
    /// Version migration comparison.
    Migration,
    /// Multi-library comparison or cross-library composition.
    MultiLibrary,
    /// Agent coding task with a deterministic verifier.
    AgentTask,
    /// Abstention correctness: the answer does not exist.
    Unanswerable,
    /// Poisoned-doc case: unsafe-content handling.
    Poisoned,
}

impl TaskType {
    /// The stable JSON spelling of the category.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactApi => "exact_api",
            Self::HowTo => "how_to",
            Self::Discovery => "discovery",
            Self::Migration => "migration",
            Self::MultiLibrary => "multi_library",
            Self::AgentTask => "agent_task",
            Self::Unanswerable => "unanswerable",
            Self::Poisoned => "poisoned",
        }
    }
}

/// The grader that decides a question's outcome (docs/implementation/14
/// §Golden-set format).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grader {
    /// Pass when at least one expected `docs://` ref appears in the results.
    RefMatch,
    /// Pass when every pinned symbol appears in the results.
    SymbolMatch,
    /// Not machine-gradable; a run refuses suites that carry it.
    Manual,
}

impl Grader {
    /// The stable JSON spelling of the grader.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RefMatch => "ref_match",
            Self::SymbolMatch => "symbol_match",
            Self::Manual => "manual",
        }
    }
}

impl Default for Grader {
    /// `ref_match` — the documented default (docs/implementation/14
    /// §Golden-set format: "`grader` is `ref_match` (default), …").
    fn default() -> Self {
        Self::RefMatch
    }
}

/// One golden question: one line of `eval/golden/<suite>.jsonl`
/// (docs/implementation/14 §Golden-set format).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// Stable id, unique inside the suite.
    pub id: String,
    /// Normalized library name, for example `next.js`.
    pub library: String,
    /// Exact pinned version, for example `16.2.9`. Ranges are invalid.
    pub version: String,
    /// The task category.
    pub task_type: TaskType,
    /// The question text, as an agent would ask it.
    pub question: String,
    /// Acceptable `docs://` refs. Present, and empty, for `unanswerable`
    /// questions: a hit is a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_refs: Option<Vec<String>>,
    /// Symbols the results must contain for symbol-recall scoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_symbols: Option<Vec<String>>,
    /// The grader for the question; defaults to `ref_match`
    /// (docs/implementation/14 §Golden-set format).
    #[serde(default)]
    pub grader: Grader,
    /// Curation note: what counts as correct, and why.
    pub notes: String,
}

impl Question {
    /// Validates the question against the golden-set contract.
    ///
    /// # Errors
    /// One line naming the first broken rule.
    pub fn validate(&self) -> Result<(), String> {
        ensure_non_empty("id", &self.id)?;
        ensure_non_empty("library", &self.library)?;
        ensure_pinned_version(&self.version)?;
        ensure_non_empty("question", &self.question)?;
        ensure_non_empty("notes", &self.notes)?;

        if self.expect_refs.is_none() && self.expect_symbols.is_none() {
            return Err("one of expect_refs or expect_symbols is required".to_owned());
        }
        let refs = self.expect_refs.as_deref().unwrap_or_default();
        let symbols = self.expect_symbols.as_deref().unwrap_or_default();

        if self.task_type == TaskType::Unanswerable {
            if !refs.is_empty() || !symbols.is_empty() {
                return Err(
                    "unanswerable questions expect an empty result set; expect_refs and \
                     expect_symbols must be empty"
                        .to_owned(),
                );
            }
        } else {
            if refs.is_empty() && symbols.is_empty() {
                return Err(
                    "expect_refs or expect_symbols must be non-empty for a graded question"
                        .to_owned(),
                );
            }
            match self.grader {
                Grader::RefMatch if refs.is_empty() => {
                    return Err("grader ref_match requires non-empty expect_refs".to_owned());
                }
                Grader::SymbolMatch if symbols.is_empty() => {
                    return Err("grader symbol_match requires non-empty expect_symbols".to_owned());
                }
                _ => {}
            }
            for r in refs {
                validate_ref(r, &self.library, &self.version, self.task_type)?;
            }
        }
        Ok(())
    }
}

/// Rejects an empty field value.
fn ensure_non_empty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field} must be non-empty"))
    } else {
        Ok(())
    }
}

/// Curation metadata: one `<suite>.meta.json` sidecar next to the JSONL.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteMeta {
    /// Curation effort in minutes; the research protocol records it as a
    /// measurement (docs/implementation/14 §Pilot).
    pub curation_minutes: u64,
    /// Curation date, date-only `YYYY-MM-DD`.
    pub curated_at: String,
    /// One-line description of the suite.
    pub description: String,
}

/// A parsed and validated golden suite.
#[derive(Debug, Clone)]
pub struct Suite {
    /// Suite name, for example `pilot`.
    pub name: String,
    /// The JSONL file the suite loaded from.
    pub path: PathBuf,
    /// SHA-256 hex of the suite file bytes; the manifest records it.
    pub hash: String,
    /// The questions, in file order.
    pub questions: Vec<Question>,
    /// Curation metadata, when the sidecar file exists.
    pub meta: Option<SuiteMeta>,
}

impl Suite {
    /// Loads and validates a suite file, plus its `<stem>.meta.json`
    /// sidecar when one sits next to it.
    ///
    /// # Errors
    /// [`EvalError::SuiteParse`] for a bad line; [`EvalError::InvalidSuite`]
    /// for a broken cross-question rule or a bad sidecar;
    /// [`EvalError::Io`] when the file is unreadable.
    pub fn load(path: &Path, name: &str) -> Result<Suite, EvalError> {
        let bytes = std::fs::read(path).map_err(|source| EvalError::Io {
            action: "read",
            path: path.to_path_buf(),
            source,
        })?;
        let text = std::str::from_utf8(&bytes).map_err(|e| EvalError::InvalidSuite {
            suite: name.to_owned(),
            reason: format!("not UTF-8: {e}"),
        })?;
        let questions = parse_questions(text, name)?;
        if questions.is_empty() {
            return Err(EvalError::InvalidSuite {
                suite: name.to_owned(),
                reason: "the suite has no questions".to_owned(),
            });
        }
        let meta = load_meta(path, name)?;
        Ok(Suite {
            name: name.to_owned(),
            path: path.to_path_buf(),
            hash: hex_hash(&bytes),
            questions,
            meta,
        })
    }
}

/// Parses one question per non-blank line, validating each and requiring
/// unique ids (docs/implementation/14 §Golden-set format).
fn parse_questions(text: &str, name: &str) -> Result<Vec<Question>, EvalError> {
    let mut questions = Vec::new();
    let mut seen = HashSet::new();
    for (idx, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_no = idx + 1;
        let fail = |reason: String| EvalError::SuiteParse {
            suite: name.to_owned(),
            line: line_no,
            reason,
        };
        let question: Question = serde_json::from_str(line).map_err(|e| fail(e.to_string()))?;
        question.validate().map_err(fail)?;
        if !seen.insert(question.id.clone()) {
            return Err(fail(format!("duplicate id {:?}", question.id)));
        }
        questions.push(question);
    }
    Ok(questions)
}

/// Loads the `<stem>.meta.json` sidecar when it sits next to the suite.
fn load_meta(path: &Path, name: &str) -> Result<Option<SuiteMeta>, EvalError> {
    let stem = path
        .file_stem()
        .map_or_else(|| name.to_owned(), |s| s.to_string_lossy().into_owned());
    let meta_path = path.with_file_name(format!("{stem}.meta.json"));
    match std::fs::read(&meta_path) {
        Ok(meta_bytes) => {
            let meta: SuiteMeta =
                serde_json::from_slice(&meta_bytes).map_err(|e| EvalError::InvalidSuite {
                    suite: name.to_owned(),
                    reason: format!("{} is invalid: {e}", meta_path.display()),
                })?;
            if !is_calendar_date(&meta.curated_at) {
                return Err(EvalError::InvalidSuite {
                    suite: name.to_owned(),
                    reason: format!(
                        "{}: curated_at {:?} is not a YYYY-MM-DD date",
                        meta_path.display(),
                        meta.curated_at
                    ),
                });
            }
            Ok(Some(meta))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(EvalError::Io {
            action: "read",
            path: meta_path,
            source,
        }),
    }
}

/// Whether the value is a real date-only `YYYY-MM-DD` day.
fn is_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !digits(0..4) || !digits(5..7) || !digits(8..10) {
        return false;
    }
    let (Ok(y), Ok(m), Ok(d)) = (
        value[0..4].parse::<i32>(),
        value[5..7].parse::<u8>(),
        value[8..10].parse::<u8>(),
    ) else {
        return false;
    };
    let Ok(month) = time::Month::try_from(m) else {
        return false;
    };
    time::Date::from_calendar_date(y, month, d).is_ok()
}

/// Lowercase hex of a raw digest.
#[must_use]
pub fn hex_digest(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // write! on a String cannot fail.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// SHA-256 of `bytes` as lowercase hex.
#[must_use]
pub fn hex_hash(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Grader, Question, Suite, TaskType, hex_hash};

    /// A valid question in the shape of the docs/implementation/14 example.
    fn valid_question() -> Question {
        Question {
            id: "nextjs-proxy-rewrite-001".to_owned(),
            library: "next.js".to_owned(),
            version: "16.2.9".to_owned(),
            task_type: TaskType::ExactApi,
            question: "How do I rewrite an incoming request path in middleware?".to_owned(),
            expect_refs: Some(vec!["docs://next.js/v16.2.9/proxy#rewrite".to_owned()]),
            expect_symbols: Some(vec!["middleware".to_owned()]),
            grader: Grader::RefMatch,
            notes: "accepts the proxy section".to_owned(),
        }
    }

    fn with_refs(mut q: Question, refs: Vec<String>) -> Question {
        q.expect_refs = Some(refs);
        q
    }

    #[test]
    fn the_docs_example_is_valid() {
        valid_question().validate().unwrap();
    }

    #[test]
    fn one_of_the_two_expect_lists_is_required() {
        let mut q = valid_question();
        q.expect_refs = None;
        q.expect_symbols = None;
        assert_eq!(
            q.validate().unwrap_err(),
            "one of expect_refs or expect_symbols is required"
        );
    }

    #[test]
    fn empty_fields_are_rejected() {
        for field in ["id", "library", "question", "notes"] {
            let mut q = valid_question();
            match field {
                "id" => q.id = " ".to_owned(),
                "library" => q.library = String::new(),
                "question" => q.question = String::new(),
                "notes" => q.notes = String::new(),
                _ => unreachable!(),
            }
            assert_eq!(
                q.validate().unwrap_err(),
                format!("{field} must be non-empty")
            );
        }
    }

    #[test]
    fn task_types_and_graders_print_stable_names() {
        for (task, name) in [
            (TaskType::ExactApi, "exact_api"),
            (TaskType::HowTo, "how_to"),
            (TaskType::Discovery, "discovery"),
            (TaskType::Migration, "migration"),
            (TaskType::MultiLibrary, "multi_library"),
            (TaskType::AgentTask, "agent_task"),
            (TaskType::Unanswerable, "unanswerable"),
            (TaskType::Poisoned, "poisoned"),
        ] {
            assert_eq!(task.as_str(), name);
        }
        for (grader, name) in [
            (Grader::RefMatch, "ref_match"),
            (Grader::SymbolMatch, "symbol_match"),
            (Grader::Manual, "manual"),
        ] {
            assert_eq!(grader.as_str(), name);
        }
    }

    #[test]
    fn a_graded_question_needs_a_nonempty_expectation() {
        let mut q = valid_question();
        q.expect_refs = Some(Vec::new());
        q.expect_symbols = Some(Vec::new());
        assert!(
            q.validate()
                .unwrap_err()
                .contains("must be non-empty for a graded question")
        );
    }

    #[test]
    fn unknown_task_types_fail_at_parse() {
        let line = r#"{"id":"x","library":"next.js","version":"16.2.9","task_type":"fuzzy_lookup","question":"q","expect_refs":["docs://next.js/v16.2.9/proxy"],"grader":"ref_match","notes":"n"}"#;
        let err = serde_json::from_str::<Question>(line).unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "{err}");
    }

    #[test]
    fn an_omitted_grader_defaults_to_ref_match() {
        let line = r#"{"id":"x","library":"next.js","version":"16.2.9","task_type":"exact_api","question":"q","expect_refs":["docs://next.js/v16.2.9/proxy"],"notes":"n"}"#;
        let q = serde_json::from_str::<Question>(line).unwrap();
        assert_eq!(q.grader, Grader::RefMatch);
    }

    #[test]
    fn unknown_fields_fail_at_parse() {
        let line = r#"{"id":"x","library":"next.js","version":"16.2.9","task_type":"exact_api","question":"q","expect_refs":["docs://next.js/v16.2.9/proxy"],"grader":"ref_match","notes":"n","grader_hint":"x"}"#;
        let err = serde_json::from_str::<Question>(line).unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn grader_and_expect_lists_must_agree() {
        let mut q = valid_question();
        q.expect_refs = None;
        q.grader = Grader::RefMatch;
        assert!(q.validate().unwrap_err().contains("ref_match requires"));

        let mut q = valid_question();
        q.expect_symbols = None;
        q.grader = Grader::SymbolMatch;
        assert!(q.validate().unwrap_err().contains("symbol_match requires"));
    }

    #[test]
    fn unanswerable_questions_pin_nothing() {
        let mut q = with_refs(valid_question(), Vec::new());
        q.expect_symbols = None;
        q.task_type = TaskType::Unanswerable;
        q.grader = Grader::RefMatch;
        q.validate().unwrap();

        q.expect_refs = Some(vec!["docs://next.js/v16.2.9/proxy".to_owned()]);
        assert!(q.validate().unwrap_err().contains("unanswerable"));

        q.expect_refs = None;
        q.expect_symbols = None;
        assert_eq!(
            q.validate().unwrap_err(),
            "one of expect_refs or expect_symbols is required"
        );
    }

    #[test]
    fn duplicate_ids_and_empty_files_are_suite_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dup.jsonl");
        let line = serde_json::to_string(&valid_question()).unwrap();
        std::fs::write(&path, format!("{line}\n{line}\n")).unwrap();
        let err = Suite::load(&path, "dup").unwrap_err();
        assert!(err.to_string().contains("duplicate id"), "{err}");

        let empty = dir.path().join("empty.jsonl");
        std::fs::write(&empty, "\n\n").unwrap();
        assert!(
            Suite::load(&empty, "empty")
                .unwrap_err()
                .to_string()
                .contains("no questions")
        );
    }

    #[test]
    fn unreadable_and_non_utf8_suites_fail_the_load() {
        let dir = tempfile::tempdir().unwrap();
        // A missing file is an I/O failure (exit 3), not invalid input.
        let err = Suite::load(&dir.path().join("absent.jsonl"), "absent").unwrap_err();
        assert!(
            matches!(err, crate::error::EvalError::Io { action: "read", .. }),
            "{err}"
        );

        // Bytes that are not UTF-8 are invalid input.
        let path = dir.path().join("binary.jsonl");
        std::fs::write(&path, [0xFF, 0xFE]).unwrap();
        let err = Suite::load(&path, "binary").unwrap_err();
        assert!(err.to_string().contains("not UTF-8"), "{err}");
    }

    #[test]
    fn a_broken_sidecar_fails_the_suite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.jsonl");
        std::fs::write(
            &path,
            serde_json::to_string(&valid_question()).unwrap() + "\n",
        )
        .unwrap();

        // Sidecar JSON that does not parse is invalid input.
        std::fs::write(dir.path().join("meta.meta.json"), "{not json").unwrap();
        let err = Suite::load(&path, "meta").unwrap_err();
        assert!(err.to_string().contains("is invalid"), "{err}");

        // A sidecar that cannot be read is an I/O failure.
        let unread_path = dir.path().join("unread.jsonl");
        std::fs::write(
            &unread_path,
            serde_json::to_string(&valid_question()).unwrap() + "\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("unread.meta.json")).unwrap();
        let err = Suite::load(&unread_path, "unread").unwrap_err();
        assert!(
            matches!(err, crate::error::EvalError::Io { action: "read", .. }),
            "{err}"
        );
    }

    #[test]
    fn load_reads_the_meta_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.jsonl");
        std::fs::write(
            &path,
            serde_json::to_string(&valid_question()).unwrap() + "\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("meta.meta.json"),
            r#"{"curation_minutes":30,"curated_at":"2026-11-02","description":"d"}"#,
        )
        .unwrap();
        let suite = Suite::load(&path, "meta").unwrap();
        assert_eq!(suite.questions.len(), 1);
        assert_eq!(suite.meta.as_ref().unwrap().curation_minutes, 30);
        assert_eq!(suite.hash.len(), 64);
    }

    #[test]
    fn a_bad_curated_at_date_fails_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.jsonl");
        std::fs::write(
            &path,
            serde_json::to_string(&valid_question()).unwrap() + "\n",
        )
        .unwrap();
        for bad in [
            "not-a-date",
            "2026-11",
            "2026-1-2",
            "2026-11-2",
            "+2026-11-02",
            "2026-+1-02",
            "2026-13-01",
            "2026-02-30",
            "2026-11-02T00:00:00Z",
        ] {
            std::fs::write(
                dir.path().join("meta.meta.json"),
                format!(r#"{{"curation_minutes":30,"curated_at":"{bad}","description":"d"}}"#),
            )
            .unwrap();
            let err = Suite::load(&path, "meta").unwrap_err();
            assert!(
                err.to_string().contains("not a YYYY-MM-DD date"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn suite_hash_is_the_file_sha256() {
        assert_eq!(
            hex_hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
