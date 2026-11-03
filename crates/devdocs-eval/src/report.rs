//! The run artifacts (docs/implementation/14 §Run artifacts):
//! `results.json`, `report.md`, and `manifest.json`.
//!
//! "The corpus identity matters: every result names the exact corpus it ran
//! against. A gate claim without the manifest is not a claim."

use std::path::Path;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::EvalError;
use crate::gates::Gate;
use crate::metrics::{QuestionScore, SuiteMetrics};
use crate::suite::{Question, TaskType};

/// The RFC 3339 UTC timestamp the artifacts carry.
#[must_use]
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Appends one rendered line to the report being built. String writes
/// cannot fail, so the report stays panic-free without unwrap.
fn push_line(out: &mut String, line: impl std::fmt::Display) {
    use std::fmt::Write as _;
    // writeln! on a String cannot fail.
    let _ = writeln!(out, "{line}");
}

/// One retrieved ref as the `results.json` row records it.
#[derive(Debug, Clone, Serialize)]
pub struct HitRow {
    /// 1-based rank.
    pub rank: usize,
    /// The canonical `docs://` ref of the hit.
    #[serde(rename = "ref")]
    pub reference: String,
    /// The hit symbol, when the chunk documents one.
    pub symbol: Option<String>,
    /// The exact version the hit was served from.
    pub version: String,
}

/// The in-process pass for one question.
#[derive(Debug, Clone, Serialize)]
pub struct PassResult {
    /// Wall time in milliseconds.
    pub wall_ms: f64,
    /// The retrieved refs in rank order.
    pub hits: Vec<HitRow>,
}

/// One question's row in `results.json` (docs/implementation/14: id, rank
/// list, timings, per-metric score).
#[derive(Debug, Clone, Serialize)]
pub struct QuestionResult {
    /// The question id.
    pub id: String,
    /// The task category.
    pub task_type: &'static str,
    /// The grader.
    pub grader: &'static str,
    /// The in-process pass.
    pub in_process: PassResult,
    /// The CLI-subprocess pass.
    pub subprocess: crate::subprocess::SubprocessSample,
    /// The per-metric score.
    pub score: QuestionScore,
}

/// The `results.json` document.
#[derive(Debug, Serialize)]
pub struct ResultsDocument<'a> {
    /// The suite name.
    pub suite: &'a str,
    /// The run date, RFC 3339 UTC.
    pub date: &'a str,
    /// The harness version.
    pub tool_version: &'a str,
    /// SHA-256 hex of the database file bytes at run end.
    pub db_hash: &'a str,
    /// SHA-256 hex of the suite file bytes.
    pub suite_hash: &'a str,
    /// One row per question.
    pub questions: &'a [QuestionResult],
}

/// The host identity the manifest records.
#[derive(Debug, Clone, Serialize)]
pub struct Host {
    /// The operating system.
    pub os: &'static str,
    /// The CPU architecture.
    pub arch: &'static str,
}

/// The `manifest.json` document: enough to reconstruct the denominator
/// (research: benchmark-protocol.md Step 4 criterion).
#[derive(Debug, Serialize)]
pub struct Manifest<'a> {
    /// The tool name and version.
    pub tool: String,
    /// The suite name.
    pub suite: &'a str,
    /// SHA-256 hex of the suite file bytes.
    pub suite_hash: &'a str,
    /// SHA-256 hex of the database file bytes at run end.
    pub db_hash: &'a str,
    /// The host the run executed on.
    pub host: Host,
    /// The run date, RFC 3339 UTC.
    pub date: &'a str,
    /// The question count.
    pub questions: usize,
    /// SHA-256 hex of the baseline record bytes, when one is loaded.
    pub baseline_hash: Option<String>,
}

/// The Phase 0 baseline record (docs/implementation/12 Phase 0, docs/14
/// §Standing re-measurement): schema rent and per-query cost of the
/// current (pre-devdocs) docs workflow.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// The date the baseline was recorded, RFC 3339.
    pub recorded: String,
    /// The workflow the baseline measures.
    pub workflow: String,
    /// The schema rent of the current workflow.
    pub schema_rent: SchemaRent,
    /// The per-query cost of the current workflow.
    pub per_query_cost: PerQueryCost,
}

/// The schema-rent half of the baseline.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaRent {
    /// The published per-tool band in tokens: `[low, high]`.
    pub band_tokens_per_tool: [u64; 2],
    /// The source citation.
    pub source: String,
    /// Context that locates the band within the published measurements.
    pub notes: String,
}

/// The per-query-cost half of the baseline.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerQueryCost {
    /// Hosted-API cost in USD per 1,000 queries.
    pub hosted_api_usd_per_1000_queries: f64,
    /// Raw-MCP cost in USD per 1,000 queries.
    pub raw_mcp_usd_per_1000_queries: f64,
    /// The pricing assumptions behind the model.
    pub assumptions: String,
    /// The source citation.
    pub source: String,
}

/// Loads the baseline record; `None` when the file does not exist.
///
/// # Errors
/// [`EvalError::Io`] when the file is unreadable; [`EvalError::InvalidSuite`]
/// when it does not parse.
pub fn load_baseline(path: &Path) -> Result<Option<(Baseline, String)>, EvalError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let baseline: Baseline =
                serde_json::from_slice(&bytes).map_err(|e| EvalError::InvalidSuite {
                    suite: "baseline".to_owned(),
                    reason: format!("{} is invalid: {e}", path.display()),
                })?;
            Ok(Some((baseline, crate::suite::hex_hash(&bytes))))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(EvalError::Io {
            action: "read",
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// One per-task-type breakdown row.
#[derive(Debug, Clone, Serialize)]
pub struct TaskRow {
    /// The task category.
    pub task_type: &'static str,
    /// The questions in the category.
    pub total: usize,
    /// The questions the binary grader passed.
    pub passed: usize,
    /// The questions the binary grader failed.
    pub failed: usize,
}

/// The per-task-type breakdown, in the eight-category union order.
#[must_use]
pub fn task_breakdown(scored: &[(Question, QuestionScore)]) -> Vec<TaskRow> {
    let mut rows = Vec::new();
    for task_type in [
        TaskType::ExactApi,
        TaskType::HowTo,
        TaskType::Discovery,
        TaskType::Migration,
        TaskType::MultiLibrary,
        TaskType::AgentTask,
        TaskType::Unanswerable,
        TaskType::Poisoned,
    ] {
        let in_type: Vec<_> = scored
            .iter()
            .filter(|(q, _)| q.task_type == task_type)
            .collect();
        if in_type.is_empty() {
            continue;
        }
        rows.push(TaskRow {
            task_type: task_type.as_str(),
            total: in_type.len(),
            passed: in_type.iter().filter(|(_, s)| s.passed).count(),
            failed: in_type.iter().filter(|(_, s)| !s.passed).count(),
        });
    }
    rows
}

/// Renders the gate table as a Markdown table; the report embeds it and the
/// CLI prints it.
#[must_use]
pub fn render_gate_table(gates: &[Gate]) -> String {
    let mut out = String::from("| Gate | Measured | Threshold | Phase | Status |\n");
    out.push_str("|---|---|---|---|---|\n");
    for gate in gates {
        push_line(
            &mut out,
            format!(
                "| {} | {} | {} | {} | {} |",
                gate.gate,
                gate.measured,
                gate.threshold,
                gate.phase,
                gate.status.label()
            ),
        );
    }
    out
}

/// Everything the report needs; the run supplies it.
#[derive(Debug)]
pub struct ReportInput<'a> {
    /// The suite name.
    pub suite: &'a str,
    /// The run date, RFC 3339 UTC.
    pub date: &'a str,
    /// The suite-level metrics.
    pub metrics: SuiteMetrics,
    /// The subprocess p95 in milliseconds.
    pub p95_ms: f64,
    /// The evaluated gate rows.
    pub gates: &'a [Gate],
    /// The scored questions.
    pub scored: &'a [(Question, QuestionScore)],
    /// Curation minutes from the suite sidecar, when recorded.
    pub curation_minutes: Option<u64>,
    /// The Phase 0 baseline record, when present.
    pub baseline: Option<&'a Baseline>,
    /// SHA-256 hex of the suite file bytes.
    pub suite_hash: &'a str,
    /// SHA-256 hex of the database file bytes.
    pub db_hash: &'a str,
}

/// Renders `report.md`.
#[must_use]
pub fn render_report(input: &ReportInput<'_>) -> String {
    let mut out = String::new();
    push_line(
        &mut out,
        format!("# Eval run: {} — {}\n", input.suite, input.date),
    );

    out.push_str("## Gate table\n\n");
    out.push_str(&render_gate_table(input.gates));
    out.push_str(
        "\nA gate that is not PASS blocks the phase transition \
         (docs/implementation/14 §Gate table).\n\n",
    );

    out.push_str(&render_metrics(input));
    out.push_str(&render_breakdown(input));
    out.push_str(&render_curation(input));
    out.push_str(&render_baseline(input));
    out.push_str(&render_corpus(input));
    out
}

/// The metrics table section.
fn render_metrics(input: &ReportInput<'_>) -> String {
    let mut out = String::from("## Metrics\n\n| Metric | Value |\n|---|---|\n");
    push_line(
        &mut out,
        format!("| hit@5 | {} |", input.metrics.hit_at_5.display()),
    );
    push_line(
        &mut out,
        format!(
            "| symbol recall | {} |",
            input.metrics.symbol_recall.display()
        ),
    );
    push_line(
        &mut out,
        format!(
            "| p95 latency | {:.1} ms (subprocess, after one warmup; placeholder CLI before M4) |",
            input.p95_ms
        ),
    );
    push_line(
        &mut out,
        format!(
            "| failure rate | {} |",
            input.metrics.failure_rate.display()
        ),
    );
    push_line(
        &mut out,
        format!(
            "| version correctness | {} |",
            input.metrics.version_correctness.display()
        ),
    );
    out.push_str(
        "| cost per query | $0.00 (local BM25; ingest embedding cost counted in Phase 3) |\n\n",
    );
    out
}

/// The per-task-type breakdown section.
fn render_breakdown(input: &ReportInput<'_>) -> String {
    let mut out = String::from(
        "## Per-task-type breakdown\n\n| task_type | n | pass | fail |\n|---|---|---|---|\n",
    );
    for row in task_breakdown(input.scored) {
        push_line(
            &mut out,
            format!(
                "| {} | {} | {} | {} |",
                row.task_type, row.total, row.passed, row.failed
            ),
        );
    }
    out.push('\n');
    out
}

/// The curation section.
fn render_curation(input: &ReportInput<'_>) -> String {
    let mut out = String::from("## Curation\n\n");
    push_line(
        &mut out,
        match input.curation_minutes {
            Some(minutes) => format!("- curation minutes: {minutes} (suite sidecar)"),
            None => "- curation minutes: not recorded".to_owned(),
        },
    );
    out.push('\n');
    out
}

/// The Phase 0 baseline section.
fn render_baseline(input: &ReportInput<'_>) -> String {
    let mut out = String::from("## Phase 0 baseline (pre-devdocs docs workflow)\n\n");
    out.push_str(&match input.baseline {
        Some(baseline) => {
            let mut section = format!(
                "- recorded: {}\n- workflow: {}\n- schema rent: {}–{} tokens per tool — {}\n  - {}\n- per-query cost: hosted API ${:.2} / raw MCP ${:.2} per 1,000 queries — {}\n  - {}\n",
                baseline.recorded,
                baseline.workflow,
                baseline.schema_rent.band_tokens_per_tool[0],
                baseline.schema_rent.band_tokens_per_tool[1],
                baseline.schema_rent.source,
                baseline.schema_rent.notes,
                baseline.per_query_cost.hosted_api_usd_per_1000_queries,
                baseline.per_query_cost.raw_mcp_usd_per_1000_queries,
                baseline.per_query_cost.source,
                baseline.per_query_cost.assumptions,
            );
            section.push_str(
                "\nRe-run the probe quarterly and on the reversal conditions \
                 (docs/implementation/14 §Standing re-measurement).\n",
            );
            section
        }
        None => "- baseline record: not found (eval/baseline/phase0.json)\n".to_owned(),
    });
    out
}

/// The corpus-identity section.
fn render_corpus(input: &ReportInput<'_>) -> String {
    let mut out = String::from("\n## Corpus\n\n");
    push_line(
        &mut out,
        format!(
            "- suite: {} — sha256 {}\n- database: sha256 {}",
            input.suite, input.suite_hash, input.db_hash
        ),
    );
    out.push_str(
        "- pipeline: Phase 0 stub — one FTS5 rung through devdocs-store; the \
         hybrid pipeline replaces it at M4 (docs/implementation/13)\n",
    );
    out.push_str(
        "- CLI search path: placeholder until M4. The p95 row times process \
         start and dispatch only.\n",
    );
    out
}

/// Writes `text` to `path`, creating parent directories.
///
/// # Errors
/// [`EvalError::Io`] when the write fails.
pub fn write_artifact(path: &Path, text: &str) -> Result<(), EvalError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| EvalError::Io {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, text).map_err(|source| EvalError::Io {
        action: "write",
        path: path.to_path_buf(),
        source,
    })
}

/// Serializes one artifact as pretty JSON with a trailing newline.
///
/// # Errors
/// [`EvalError::Serialize`] when the value cannot serialize.
pub fn json_artifact<T: Serialize>(what: &'static str, value: &T) -> Result<String, EvalError> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|source| EvalError::Serialize { what, source })?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{
        Baseline, PerQueryCost, ReportInput, SchemaRent, render_gate_table, render_report,
    };
    use crate::gates;
    use crate::metrics::{Fraction, QuestionScore, SuiteMetrics};
    use crate::suite::{Grader, Question, TaskType};

    /// One graded question fixture for the report rendering.
    fn fixture_question() -> (Question, QuestionScore) {
        let question = Question {
            id: "nextjs-proxy-rewrite-001".to_owned(),
            library: "next.js".to_owned(),
            version: "16.2.9".to_owned(),
            task_type: TaskType::ExactApi,
            question: "How do I rewrite an incoming request path?".to_owned(),
            expect_refs: Some(vec!["docs://next.js/v16.2.9/proxy#rewrite".to_owned()]),
            expect_symbols: Some(vec!["rewrite".to_owned()]),
            grader: Grader::RefMatch,
            notes: "n".to_owned(),
        };
        let score = QuestionScore {
            passed: false,
            hit_at_5: false,
            symbols_found: 0,
            symbols_pinned: 1,
            version_correct: false,
        };
        (question, score)
    }

    /// A Phase 0 baseline fixture for the report rendering.
    fn fixture_baseline() -> Baseline {
        Baseline {
            recorded: "2026-11-02".to_owned(),
            workflow: "hosted docs MCP + web search".to_owned(),
            schema_rent: SchemaRent {
                band_tokens_per_tool: [550, 1400],
                source: "research: interfaces.md §1 (GEM §4)".to_owned(),
                notes: "raw multi-tool MCP dumps run higher".to_owned(),
            },
            per_query_cost: PerQueryCost {
                hosted_api_usd_per_1000_queries: 1.66,
                raw_mcp_usd_per_1000_queries: 3.66,
                assumptions: "frontier list pricing".to_owned(),
                source: "research: interfaces.md §1 (GLMP Ch.9)".to_owned(),
            },
        }
    }

    #[test]
    fn report_snapshot() {
        let (question, score) = fixture_question();
        let metrics = SuiteMetrics::new(&[(question.clone(), score.clone())]);
        let gates = gates::evaluate(&metrics, Some(12.5));
        let scored = vec![(question, score)];
        let baseline = fixture_baseline();
        let suite_hash = "0".repeat(64);
        let db_hash = "1".repeat(64);
        let input = ReportInput {
            suite: "fixture",
            date: "2026-11-02T00:00:00.000000000Z",
            metrics,
            p95_ms: 12.5,
            gates: &gates,
            scored: &scored,
            curation_minutes: Some(90),
            baseline: Some(&baseline),
            suite_hash: &suite_hash,
            db_hash: &db_hash,
        };
        insta::assert_snapshot!("render_report_fixture", render_report(&input));
    }

    #[test]
    fn a_broken_baseline_record_is_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("phase0.json");
        std::fs::write(&path, "{nope").unwrap();
        let err = super::load_baseline(&path).unwrap_err();
        assert!(err.to_string().contains("is invalid"), "{err}");
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn an_unreadable_baseline_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("phase0.json");
        std::fs::create_dir(&path).unwrap();
        let err = super::load_baseline(&path).unwrap_err();
        assert!(
            matches!(err, crate::error::EvalError::Io { action: "read", .. }),
            "{err}"
        );
    }

    #[test]
    fn artifact_writes_surface_io_failures() {
        let dir = tempfile::tempdir().unwrap();

        // A parent that is a file fails the directory creation.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let err = super::write_artifact(&blocker.join("sub").join("report.md"), "x").unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::EvalError::Io {
                    action: "create",
                    ..
                }
            ),
            "{err}"
        );

        // A target that is a directory fails the write itself.
        let target = dir.path().join("report.md");
        std::fs::create_dir(&target).unwrap();
        let err = super::write_artifact(&target, "x").unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::EvalError::Io {
                    action: "write",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn gate_table_renders_every_row() {
        let table = render_gate_table(&gates::evaluate(
            &SuiteMetrics {
                hit_at_5: Fraction { hits: 0, total: 1 },
                symbol_recall: Fraction { hits: 0, total: 0 },
                failure_rate: Fraction { hits: 1, total: 1 },
                version_correctness: Fraction { hits: 0, total: 1 },
            },
            Some(9.9),
        ));
        for gate in [
            "Symbol recall",
            "hit@5",
            "p95 latency",
            "Cost per 1,000 queries",
            "Retrieval failures",
            "Version correctness",
        ] {
            assert!(table.contains(gate), "{table}");
        }
        assert!(table.contains("INCONCLUSIVE"));
    }
}
