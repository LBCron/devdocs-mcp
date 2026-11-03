//! The metric math (docs/implementation/14 §Metrics): per-question grading
//! and the suite aggregates.
//!
//! Everything here is pure and checked against hand-computed fixtures. The
//! binary grader has no partial credit: partial counts as wrong (research:
//! benchmark-protocol.md Step 6).

use serde::Serialize;

use crate::refs::ParsedRef;
use crate::suite::{Grader, Question, TaskType};

/// One retrieval result, shaped for grading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetrievedRef {
    /// 1-based rank in the result list.
    pub rank: usize,
    /// The library the hit was served from.
    pub library: String,
    /// The exact version the hit was served from.
    pub version: String,
    /// The section-path segments of the hit.
    pub segments: Vec<String>,
    /// The hit's symbol, when the chunk documents one.
    pub symbol: Option<String>,
}

impl RetrievedRef {
    /// The canonical `docs://` spelling, for the run artifacts.
    #[must_use]
    pub fn reference(&self) -> String {
        let path = self.segments.join("/");
        match &self.symbol {
            Some(symbol) => format!(
                "docs://{}/v{}/{}#{symbol}",
                self.library, self.version, path
            ),
            None => format!("docs://{}/v{}/{}", self.library, self.version, path),
        }
    }
}

/// Whether an expected ref matches a hit.
///
/// The libraries compare case-insensitively; the version must be exact. The
/// expected path matches when it equals the hit path or is a trailing run
/// of its segments, so a pinned `proxy` matches a hit served from
/// `app/proxy`. A pinned fragment requires the hit's symbol to equal it.
#[must_use]
pub fn ref_matches(expected: &str, hit: &RetrievedRef) -> bool {
    let Some(expected) = ParsedRef::parse(expected) else {
        return false;
    };
    if !expected.library.eq_ignore_ascii_case(&hit.library) || expected.version != hit.version {
        return false;
    }
    let hit_tail = hit.segments.len().checked_sub(expected.segments.len());
    let Some(offset) = hit_tail else {
        return false;
    };
    if hit.segments[offset..] != expected.segments[..] {
        return false;
    }
    match &expected.fragment {
        Some(fragment) => hit.symbol.as_deref() == Some(fragment.as_str()),
        None => true,
    }
}

/// hit@5: at least one expected ref within the first five results
/// (docs/implementation/14 §Metrics).
#[must_use]
pub fn hit_at_5(expected_refs: &[String], hits: &[RetrievedRef]) -> bool {
    expected_refs
        .iter()
        .any(|e| hits.iter().take(5).any(|h| ref_matches(e, h)))
}

/// The binary ref grader: at least one expected ref anywhere in the result
/// list. hit@5 is the ranking-quality subset of this.
#[must_use]
pub fn ref_match(expected_refs: &[String], hits: &[RetrievedRef]) -> bool {
    expected_refs
        .iter()
        .any(|e| hits.iter().any(|h| ref_matches(e, h)))
}

/// numerator/denominator; 0.0 for an empty denominator.
#[allow(clippy::cast_precision_loss)] // suite sizes stay far below 2^53
#[must_use]
pub fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// The p95 of wall-time samples by nearest rank: the ceil(0.95 * n)-th
/// smallest sample. 0.0 for no samples.
#[must_use]
pub fn p95(samples: &[f64]) -> f64 {
    percentile_nearest_rank(samples, 0.95)
}

/// The nearest-rank percentile of the samples.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)] // rank stays in 1..=len, so the casts lose nothing
fn percentile_nearest_rank(samples: &[f64], q: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// The graded outcome of one question (docs/implementation/14 §Metrics).
#[derive(Debug, Clone, Serialize)]
pub struct QuestionScore {
    /// The binary grader outcome: partial counts as wrong.
    pub passed: bool,
    /// Any expected ref within the top five.
    pub hit_at_5: bool,
    /// Pinned symbols the results contain.
    pub symbols_found: usize,
    /// The symbols the question pins.
    pub symbols_pinned: usize,
    /// The top result serves the exact pinned version.
    pub version_correct: bool,
}

impl QuestionScore {
    /// Found / pinned symbols; `None` when the question pins none.
    #[must_use]
    pub fn symbol_recall(&self) -> Option<f64> {
        (self.symbols_pinned > 0).then(|| ratio(self.symbols_found, self.symbols_pinned))
    }
}

/// Grades one question against its retrieved results.
///
/// `unanswerable` questions pass only with an empty result set — a hit is a
/// failure (docs/implementation/14 §Golden-set format). `ref_match` scans
/// the full result list; `symbol_match` passes only at full symbol recall.
/// A symbol counts only from a hit on the question's own library and
/// version pin — a `multi_library` question grades its symbols against
/// that primary pin, even though its refs may target other libraries.
#[must_use]
pub fn grade_question(question: &Question, hits: &[RetrievedRef]) -> QuestionScore {
    let refs = question.expect_refs.as_deref().unwrap_or_default();
    let symbols = question.expect_symbols.as_deref().unwrap_or_default();
    // A symbol counts only when a hit from the question's own library and
    // pinned version documents it — the same library/version rule the ref
    // grader applies (docs/implementation/14 §Version correctness).
    let symbols_found = symbols
        .iter()
        .filter(|s| {
            hits.iter().any(|h| {
                h.symbol.as_deref() == Some(s.as_str())
                    && h.version == question.version
                    && h.library.eq_ignore_ascii_case(&question.library)
            })
        })
        .count();
    let passed = if question.task_type == TaskType::Unanswerable {
        hits.is_empty()
    } else {
        match question.grader {
            Grader::RefMatch => ref_match(refs, hits),
            Grader::SymbolMatch => !symbols.is_empty() && symbols_found == symbols.len(),
            Grader::Manual => false, // runs refuse manual-grader suites
        }
    };
    QuestionScore {
        passed,
        hit_at_5: hit_at_5(refs, hits),
        symbols_found,
        symbols_pinned: symbols.len(),
        // The top result serves the exact pinned version of the question's
        // own library — the same library/version rule the graders apply.
        version_correct: hits.first().is_some_and(|h| {
            h.version == question.version && h.library.eq_ignore_ascii_case(&question.library)
        }),
    }
}

/// A count-over-count fraction with its denominator, so gates can tell a
/// measured 0.0 from "nothing qualified for the denominator".
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Fraction {
    /// The count that met the bar.
    pub hits: usize,
    /// The count the metric applies to.
    pub total: usize,
}

impl Fraction {
    /// The fraction; 0.0 when nothing qualifies for the denominator.
    #[must_use]
    pub fn value(self) -> f64 {
        ratio(self.hits, self.total)
    }

    /// The value with its raw counts, as the gate table and the report
    /// print it: `0.750 (3/4)`.
    #[must_use]
    pub fn display(self) -> String {
        format!("{:.3} ({}/{})", self.value(), self.hits, self.total)
    }
}

/// The suite-level aggregates (docs/implementation/14 §Metrics).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SuiteMetrics {
    /// Questions with non-empty `expect_refs` that had an expected ref in
    /// the top five.
    pub hit_at_5: Fraction,
    /// Pinned symbols present in the results.
    pub symbol_recall: Fraction,
    /// Questions with a wrong or partial result under the binary grader.
    pub failure_rate: Fraction,
    /// Questions whose top result serves the exact pinned version.
    pub version_correctness: Fraction,
}

impl SuiteMetrics {
    /// Aggregates the per-question scores. hit@5 denominators count only
    /// questions with non-empty `expect_refs`; symbol recall counts pinned
    /// symbols; the other two count every question.
    #[must_use]
    pub fn new(scored: &[(Question, QuestionScore)]) -> SuiteMetrics {
        let mut refs_total = 0;
        let mut refs_hits = 0;
        let mut symbols_pinned = 0;
        let mut symbols_found = 0;
        let mut failed = 0;
        let mut version_hits = 0;
        for (question, score) in scored {
            if score.symbols_pinned > 0 {
                symbols_pinned += score.symbols_pinned;
                symbols_found += score.symbols_found;
            }
            if !question
                .expect_refs
                .as_deref()
                .is_none_or(<[String]>::is_empty)
            {
                refs_total += 1;
                if score.hit_at_5 {
                    refs_hits += 1;
                }
            }
            if !score.passed {
                failed += 1;
            }
            if score.version_correct {
                version_hits += 1;
            }
        }
        SuiteMetrics {
            hit_at_5: Fraction {
                hits: refs_hits,
                total: refs_total,
            },
            symbol_recall: Fraction {
                hits: symbols_found,
                total: symbols_pinned,
            },
            failure_rate: Fraction {
                hits: failed,
                total: scored.len(),
            },
            version_correctness: Fraction {
                hits: version_hits,
                total: scored.len(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{
        Fraction, QuestionScore, RetrievedRef, SuiteMetrics, grade_question, hit_at_5, p95, ratio,
        ref_match, ref_matches,
    };
    use crate::suite::{Grader, Question, TaskType};

    /// A hit with the given rank, library, version, path segments, symbol.
    fn hit(
        rank: usize,
        library: &str,
        version: &str,
        path: &str,
        symbol: Option<&str>,
    ) -> RetrievedRef {
        RetrievedRef {
            rank,
            library: library.to_owned(),
            version: version.to_owned(),
            segments: path.split('/').map(str::to_owned).collect(),
            symbol: symbol.map(str::to_owned),
        }
    }

    fn refs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    /// A ref-graded question pinning one ref.
    fn question(grader: Grader, task_type: TaskType, refs: Vec<String>) -> Question {
        Question {
            id: "q-001".to_owned(),
            library: "next.js".to_owned(),
            version: "16.2.9".to_owned(),
            task_type,
            question: "q".to_owned(),
            expect_refs: (!refs.is_empty()).then_some(refs),
            expect_symbols: None,
            grader,
            notes: "n".to_owned(),
        }
    }

    #[test]
    fn references_render_with_and_without_a_symbol() {
        let with = hit(1, "next.js", "16.2.9", "app/proxy", Some("rewrite"));
        assert_eq!(with.reference(), "docs://next.js/v16.2.9/app/proxy#rewrite");
        let without = hit(1, "next.js", "16.2.9", "app/proxy", None);
        assert_eq!(without.reference(), "docs://next.js/v16.2.9/app/proxy");
    }

    #[test]
    fn broken_refs_and_longer_expectations_never_match() {
        let h = hit(1, "next.js", "16.2.9", "proxy", None);
        // A ref that does not parse matches nothing.
        assert!(!ref_matches("not-a-ref", &h));
        // The expected path is longer than the hit path: no trailing run
        // of the hit segments can equal it.
        assert!(!ref_matches("docs://next.js/v16.2.9/app/proxy/deep", &h));
    }

    #[test]
    fn hit_at_5_counts_only_the_top_five() {
        let hits: Vec<RetrievedRef> = (1..=8)
            .map(|i| hit(i, "next.js", "16.2.9", &format!("page{i}"), None))
            .collect();
        let at_three = refs(&["docs://next.js/v16.2.9/page3"]);
        let at_six = refs(&["docs://next.js/v16.2.9/page6"]);
        let absent = refs(&["docs://next.js/v16.2.9/other"]);

        assert!(hit_at_5(&at_three, &hits));
        assert!(!hit_at_5(&at_six, &hits), "rank 6 is outside the top five");
        assert!(!hit_at_5(&absent, &hits));
        assert!(ref_match(&at_six, &hits), "the full list reaches rank 6");
    }

    #[test]
    fn expected_paths_match_trailing_hit_segments() {
        let h = hit(1, "next.js", "16.2.9", "app/proxy", Some("rewrite"));
        assert!(ref_matches("docs://next.js/v16.2.9/proxy#rewrite", &h));
        assert!(ref_matches("docs://next.js/v16.2.9/app/proxy#rewrite", &h));
        assert!(!ref_matches("docs://next.js/v16.2.9/proxy#other", &h));
        assert!(
            ref_matches("docs://next.js/v16.2.9/app/proxy", &h),
            "a ref without a fragment ignores the hit symbol"
        );
        assert!(ref_matches(
            "docs://next.js/v16.2.9/app/proxy",
            &hit(1, "next.js", "16.2.9", "app/proxy", None)
        ));
    }

    #[test]
    fn library_is_case_insensitive_and_version_exact() {
        let h = hit(1, "Next.js", "16.2.9", "proxy", None);
        assert!(ref_matches("docs://next.js/v16.2.9/proxy", &h));
        assert!(!ref_matches("docs://next.js/v16.3.0/proxy", &h));
    }

    #[test]
    fn symbol_recall_counts_found_over_pinned() {
        let score = QuestionScore {
            passed: true,
            hit_at_5: true,
            symbols_found: 2,
            symbols_pinned: 3,
            version_correct: true,
        };
        assert_eq!(score.symbol_recall(), Some(2.0 / 3.0));

        let none = QuestionScore {
            passed: true,
            hit_at_5: true,
            symbols_found: 0,
            symbols_pinned: 0,
            version_correct: true,
        };
        assert_eq!(none.symbol_recall(), None);
    }

    #[test]
    fn p95_is_the_nearest_rank_sample() {
        let hundred: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(p95(&hundred), 95.0);
        assert_eq!(p95(&[100.0, 200.0, 300.0, 400.0]), 400.0);
        assert_eq!(p95(&[7.0]), 7.0);
        assert_eq!(p95(&[]), 0.0);
        // Unsorted input is ranked, not assumed ordered.
        assert_eq!(p95(&[50.0, 10.0, 30.0, 20.0, 40.0, 5.0]), 50.0);
    }

    #[test]
    fn ratio_of_zero_denominator_is_zero() {
        assert_eq!(ratio(0, 0), 0.0);
        assert_eq!(ratio(3, 4), 0.75);
    }

    #[test]
    fn unanswerable_questions_pass_only_with_no_hits() {
        let q = question(Grader::RefMatch, TaskType::Unanswerable, Vec::new());
        let passed = grade_question(&q, &[]).passed;
        assert!(passed, "an empty result set passes");

        let hits = vec![hit(1, "next.js", "16.2.9", "proxy", None)];
        let failed = grade_question(&q, &hits).passed;
        assert!(!failed, "a hit is a failure");
    }

    #[test]
    fn ref_grader_needs_any_expected_ref() {
        let q = question(
            Grader::RefMatch,
            TaskType::ExactApi,
            refs(&[
                "docs://next.js/v16.2.9/proxy",
                "docs://next.js/v16.2.9/config",
            ]),
        );
        let hits = vec![hit(1, "next.js", "16.2.9", "config", None)];
        let score = grade_question(&q, &hits);
        assert!(score.passed);
        assert!(score.hit_at_5);

        let misses = vec![hit(1, "next.js", "16.2.9", "routing", None)];
        assert!(!grade_question(&q, &misses).passed);
    }

    #[test]
    fn symbol_grader_needs_full_recall() {
        let mut q = question(Grader::SymbolMatch, TaskType::ExactApi, Vec::new());
        q.expect_symbols = Some(refs(&["middleware", "rewrite"]));

        let hits = vec![
            hit(1, "next.js", "16.2.9", "proxy", Some("middleware")),
            hit(2, "next.js", "16.2.9", "proxy", Some("other")),
        ];
        let score = grade_question(&q, &hits);
        assert!(!score.passed, "partial recall counts as wrong");
        assert_eq!(score.symbol_recall(), Some(0.5));

        let full = vec![
            hit(1, "next.js", "16.2.9", "proxy", Some("middleware")),
            hit(2, "next.js", "16.2.9", "proxy", Some("rewrite")),
        ];
        assert!(grade_question(&q, &full).passed);
    }

    #[test]
    fn a_manual_grader_never_passes() {
        let q = question(
            Grader::Manual,
            TaskType::ExactApi,
            refs(&["docs://next.js/v16.2.9/proxy"]),
        );
        let hits = vec![hit(1, "next.js", "16.2.9", "proxy", None)];
        assert!(
            !grade_question(&q, &hits).passed,
            "runs refuse manual-grader suites; the grader still never passes"
        );
    }

    #[test]
    fn symbols_count_only_from_the_pinned_library_and_version() {
        let mut q = question(Grader::SymbolMatch, TaskType::ExactApi, Vec::new());
        q.expect_symbols = Some(refs(&["middleware"]));

        let foreign_version = vec![hit(1, "next.js", "16.1.0", "proxy", Some("middleware"))];
        let score = grade_question(&q, &foreign_version);
        assert_eq!(score.symbols_found, 0, "another version does not count");

        let foreign_library = vec![hit(1, "zod", "16.2.9", "proxy", Some("middleware"))];
        let score = grade_question(&q, &foreign_library);
        assert_eq!(score.symbols_found, 0, "another library does not count");

        let case_insensitive = vec![hit(1, "Next.js", "16.2.9", "proxy", Some("middleware"))];
        let score = grade_question(&q, &case_insensitive);
        assert_eq!(score.symbols_found, 1, "library case does not matter");
    }

    #[test]
    fn version_correctness_checks_the_top_result() {
        let q = question(
            Grader::RefMatch,
            TaskType::ExactApi,
            refs(&["docs://next.js/v16.2.9/proxy"]),
        );
        let exact = vec![hit(1, "next.js", "16.2.9", "proxy", None)];
        assert!(grade_question(&q, &exact).version_correct);

        let fallback = vec![hit(1, "next.js", "16.1.0", "proxy", None)];
        assert!(!grade_question(&q, &fallback).version_correct);

        let foreign = vec![hit(1, "zod", "16.2.9", "proxy", None)];
        assert!(
            !grade_question(&q, &foreign).version_correct,
            "another library with the same version string does not count"
        );
    }

    #[test]
    fn suite_aggregates_use_the_documented_denominators() {
        let q_ref = question(
            Grader::RefMatch,
            TaskType::ExactApi,
            refs(&["docs://next.js/v16.2.9/proxy"]),
        );
        let q_sym = {
            let mut q = question(Grader::SymbolMatch, TaskType::ExactApi, Vec::new());
            q.expect_symbols = Some(refs(&["a", "b"]));
            q
        };
        let q_unans = question(Grader::RefMatch, TaskType::Unanswerable, Vec::new());

        let hit_proxy = vec![hit(1, "next.js", "16.2.9", "proxy", Some("a"))];
        let sym_score = grade_question(&q_sym, &hit_proxy); // partial recall → fail
        let unans_score = grade_question(&q_unans, &hit_proxy.clone()); // hit on unanswerable → fail
        let scored = vec![
            (q_ref.clone(), grade_question(&q_ref, &hit_proxy)), // pass, hit@5, 1/2 symbols, version ok
            (q_sym, sym_score),
            (q_unans, unans_score),
        ];
        let m = SuiteMetrics::new(&scored);
        // hit@5: only q_ref pins refs, and its expected ref is in the top five.
        assert_eq!((m.hit_at_5.hits, m.hit_at_5.total), (1, 1));
        // symbols: q_sym pins a,b; only a found (via the shared hit).
        assert_eq!((m.symbol_recall.hits, m.symbol_recall.total), (1, 2));
        // failures: q_sym and the unanswerable hit fail.
        assert_eq!((m.failure_rate.hits, m.failure_rate.total), (2, 3));
        // every top hit serves the pinned version, so 3 of 3.
        assert_eq!(
            (m.version_correctness.hits, m.version_correctness.total),
            (3, 3)
        );
        assert_eq!(m.failure_rate.value(), 2.0 / 3.0);
        let f = Fraction { hits: 1, total: 4 };
        assert_eq!(f.value(), 0.25);
    }
}
