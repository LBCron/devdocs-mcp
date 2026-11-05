//! The run loop (docs/implementation/14 §Harness flow): load and validate
//! the suite, run the retrieval pipeline, run the CLI-subprocess pass,
//! score, write the artifacts, and evaluate the gates.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use devdocs_store::Store;
use sha2::{Digest, Sha256};

use crate::error::EvalError;
use crate::gates::{self, Gate};
use crate::metrics::{self, QuestionScore, SuiteMetrics};
use crate::pipeline;
use crate::report::{
    self, HitRow, Host, Manifest, PassResult, QuestionResult, ReportInput, ResultsDocument,
};
use crate::subprocess::{self, SubprocessSample};
use crate::suite::{Grader, Question, Suite};

/// Everything a run needs (docs/implementation/14 §Harness flow).
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The suite name under the golden dir, or a direct `.jsonl` path.
    pub suite: String,
    /// The fixture database; built from the store schema when absent.
    pub db: Option<PathBuf>,
    /// The directory that holds the golden suites; discovered from the
    /// working directory when absent.
    pub golden_dir: Option<PathBuf>,
    /// The directory that holds the run artifacts; next to the golden dir
    /// when absent.
    pub runs_dir: Option<PathBuf>,
    /// The `devdocs` binary for the subprocess pass; located when absent.
    pub devdocs_bin: Option<PathBuf>,
}

/// What a completed run reports.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// The directory holding the artifacts.
    pub run_dir: PathBuf,
    /// The suite-level metrics.
    pub metrics: SuiteMetrics,
    /// The evaluated gate rows.
    pub gates: Vec<Gate>,
    /// The subprocess p95 in milliseconds.
    pub p95_ms: f64,
    /// Whether every gate passed; `false` means the caller exits 2.
    pub all_gates_pass: bool,
}

/// Runs one suite end-to-end.
///
/// # Errors
/// [`EvalError::InvalidSuite`] or [`EvalError::SuiteParse`] for bad input
/// (exit 1); [`EvalError::Store`], [`EvalError::Subprocess`], and
/// [`EvalError::Io`] for operational failures (exit 3).
pub fn run(options: &RunOptions) -> Result<RunOutcome, EvalError> {
    let (golden_dir, runs_root, baseline_path) = resolve_dirs(options)?;
    let suite = load_suite(&golden_dir, &options.suite)?;
    refuse_manual_graders(&suite)?;

    let date = report::now_rfc3339();
    let run_dir = fresh_run_dir(&runs_root, &date, &suite.name)?;

    // The fixture database comes from the store schema, never a copy: the
    // open runs the same fschecks and pragmas, and the migration is
    // idempotent (docs/implementation/06).
    let db_path = options
        .db
        .clone()
        .unwrap_or_else(|| run_dir.join("fixture.db"));
    let store = Store::open(&db_path)?;
    store.migrate()?;

    let mut passes = in_process_pass(&store, &suite)?;
    let p95_ms = subprocess_pass(options, &db_path, &suite, &mut passes.rows)?;
    drop(store);

    let metrics = SuiteMetrics::new(&passes.graded);
    let gate_rows = gates::evaluate(&metrics, Some(p95_ms));

    let db_hash = hash_file(&db_path)?;
    let bundle = RunData {
        suite: &suite,
        date: &date,
        metrics,
        p95_ms,
        gates: &gate_rows,
        db_hash: &db_hash,
        baseline_path: &baseline_path,
    };
    write_artifacts(&run_dir, &bundle, &passes)?;

    let all_gates_pass = gates::all_pass(&gate_rows);
    Ok(RunOutcome {
        run_dir,
        metrics,
        gates: gate_rows,
        p95_ms,
        all_gates_pass,
    })
}

/// Loads and validates the suite by name or direct path.
fn load_suite(golden_dir: &Path, requested: &str) -> Result<Suite, EvalError> {
    let requested_path = Path::new(requested);
    let (suite_path, suite_name) = if requested_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
        || requested_path.is_absolute()
        || requested.contains(['/', '\\'])
    {
        let path = PathBuf::from(requested);
        let name = path
            .file_stem()
            .map_or_else(|| "suite".to_owned(), |s| s.to_string_lossy().into_owned());
        (path, name)
    } else {
        (
            golden_dir.join(format!("{requested}.jsonl")),
            requested.to_owned(),
        )
    };
    Suite::load(&suite_path, &suite_name)
}

/// Refuses suites the harness cannot grade (docs/implementation/14: the
/// pilot uses binary ref/symbol graders).
fn refuse_manual_graders(suite: &Suite) -> Result<(), EvalError> {
    let manual = suite
        .questions
        .iter()
        .filter(|q| q.grader == Grader::Manual)
        .count();
    if manual > 0 {
        return Err(EvalError::InvalidSuite {
            suite: suite.name.clone(),
            reason: format!(
                "{manual} question(s) use the manual grader; the harness grades \
                 ref_match and symbol_match only"
            ),
        });
    }
    Ok(())
}

/// Accepts a subprocess call that exited 0 or 2 and refuses the rest. An
/// exit 2 is the documented miss — a valid query that served nothing
/// (docs/implementation/08 §Error codes) — and its wall time is a real
/// timed search. Any other nonzero exit says the call died before doing
/// real work, so the run fails (exit 3) instead of letting a dead binary
/// pass the p95 gate.
fn check_exit(sample: SubprocessSample, label: &str) -> Result<(), EvalError> {
    if sample.exit_code == 0 || sample.exit_code == 2 {
        return Ok(());
    }
    Err(EvalError::Subprocess(format!(
        "devdocs search exited {} for {label}",
        sample.exit_code
    )))
}

/// The in-process pass: one timed pipeline call per question, then the
/// per-question score (docs/implementation/14 §Harness flow step 2–3).
struct InProcess {
    /// The artifact rows, one per question.
    rows: Vec<QuestionResult>,
    /// The question/score pairs the metrics aggregate.
    graded: Vec<(Question, QuestionScore)>,
}

/// The in-process pass: one timed pipeline call per question, then the
/// per-question score (docs/implementation/14 §Harness flow step 2–3).
fn in_process_pass(store: &Store, suite: &Suite) -> Result<InProcess, EvalError> {
    let mut rows = Vec::with_capacity(suite.questions.len());
    let mut graded = Vec::with_capacity(suite.questions.len());
    for question in &suite.questions {
        let start = Instant::now();
        let hits = pipeline::retrieve(store, question)?;
        let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
        let verdict = metrics::grade_question(question, &hits);
        rows.push(QuestionResult {
            id: question.id.clone(),
            task_type: question.task_type.as_str(),
            grader: question.grader.as_str(),
            in_process: PassResult {
                wall_ms,
                hits: hits
                    .iter()
                    .map(|h| HitRow {
                        rank: h.rank,
                        reference: h.reference(),
                        symbol: h.symbol.clone(),
                        version: h.version.clone(),
                    })
                    .collect(),
            },
            subprocess: SubprocessSample {
                wall_ms: 0.0,
                exit_code: 0,
            },
            score: verdict.clone(),
        });
        graded.push((question.clone(), verdict));
    }
    Ok(InProcess { rows, graded })
}

/// The CLI-subprocess pass: one warmup excluded, then one timed
/// `devdocs search` per question (docs/implementation/14 §Harness flow).
///
/// # Errors
/// [`EvalError::Subprocess`] when the binary is missing or a spawn fails.
fn subprocess_pass(
    options: &RunOptions,
    db_path: &Path,
    suite: &Suite,
    rows: &mut [QuestionResult],
) -> Result<f64, EvalError> {
    let bin = subprocess::locate_devdocs(options.devdocs_bin.as_deref())?;
    // One warmup call; the cold start is excluded (docs/implementation/14).
    let warmup = subprocess::run_once(&bin, db_path, &suite.questions[0])?;
    check_exit(warmup, "warmup")?;
    let mut samples = Vec::with_capacity(rows.len());
    for (row, question) in rows.iter_mut().zip(&suite.questions) {
        let sample = subprocess::run_once(&bin, db_path, question)?;
        check_exit(sample, &question.id)?;
        samples.push(sample.wall_ms);
        row.subprocess = sample;
    }
    Ok(metrics::p95(&samples))
}

/// Everything the artifacts need, assembled once by the run.
struct RunData<'a> {
    /// The validated suite.
    suite: &'a Suite,
    /// The run date, RFC 3339 UTC.
    date: &'a str,
    /// The suite-level metrics.
    metrics: SuiteMetrics,
    /// The subprocess p95 in milliseconds.
    p95_ms: f64,
    /// The evaluated gate rows.
    gates: &'a [Gate],
    /// SHA-256 hex of the database file bytes.
    db_hash: &'a str,
    /// The baseline record the report and manifest carry.
    baseline_path: &'a Path,
}

/// Writes `results.json`, `manifest.json`, and `report.md` into the run
/// directory (docs/implementation/14 §Run artifacts).
fn write_artifacts(
    run_dir: &Path,
    data: &RunData<'_>,
    passes: &InProcess,
) -> Result<(), EvalError> {
    let baseline = report::load_baseline(data.baseline_path)?;
    write_results(run_dir, data, &passes.rows)?;
    write_manifest(
        run_dir,
        data,
        baseline.as_ref().map(|(_, hash)| hash.clone()),
    )?;
    write_report(
        run_dir,
        data,
        &passes.graded,
        baseline.as_ref().map(|(b, _)| b),
    )
}

/// Writes the per-question `results.json` rows.
fn write_results(
    run_dir: &Path,
    data: &RunData<'_>,
    rows: &[QuestionResult],
) -> Result<(), EvalError> {
    let results = ResultsDocument {
        suite: &data.suite.name,
        date: data.date,
        tool_version: env!("CARGO_PKG_VERSION"),
        db_hash: data.db_hash,
        suite_hash: &data.suite.hash,
        questions: rows,
    };
    report::write_artifact(
        &run_dir.join("results.json"),
        &report::json_artifact("results.json", &results)?,
    )
}

/// Writes the `manifest.json` that reconstructs the denominator.
fn write_manifest(
    run_dir: &Path,
    data: &RunData<'_>,
    baseline_hash: Option<String>,
) -> Result<(), EvalError> {
    let manifest = Manifest {
        tool: format!("devdocs-eval {}", env!("CARGO_PKG_VERSION")),
        suite: &data.suite.name,
        suite_hash: &data.suite.hash,
        db_hash: data.db_hash,
        host: Host {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        },
        date: data.date,
        questions: data.suite.questions.len(),
        baseline_hash,
    };
    report::write_artifact(
        &run_dir.join("manifest.json"),
        &report::json_artifact("manifest.json", &manifest)?,
    )
}

/// Writes the rendered `report.md`.
fn write_report(
    run_dir: &Path,
    data: &RunData<'_>,
    graded: &[(Question, QuestionScore)],
    baseline: Option<&report::Baseline>,
) -> Result<(), EvalError> {
    let input = ReportInput {
        suite: &data.suite.name,
        date: data.date,
        metrics: data.metrics,
        p95_ms: data.p95_ms,
        gates: data.gates,
        scored: graded,
        curation_minutes: data.suite.meta.as_ref().map(|meta| meta.curation_minutes),
        baseline,
        suite_hash: &data.suite.hash,
        db_hash: data.db_hash,
    };
    report::write_artifact(&run_dir.join("report.md"), &report::render_report(&input))
}

/// Resolves the golden dir, the runs dir, and the baseline path.
///
/// With an explicit `--golden-dir`, its parent is the eval directory and
/// the default runs and baseline paths derive from it. Otherwise the eval
/// directory is discovered by walking up from the working directory until
/// an `eval/golden` pair appears.
fn resolve_dirs(options: &RunOptions) -> Result<(PathBuf, PathBuf, PathBuf), EvalError> {
    let eval_root = match options
        .golden_dir
        .as_ref()
        .and_then(|g| g.parent().map(Path::to_path_buf))
    {
        Some(eval_dir) => eval_dir.clone(),
        None => discover_eval_root().ok_or_else(|| EvalError::InvalidSuite {
            suite: options.suite.clone(),
            reason: "cannot locate the eval/ directory from the working \
                     directory; pass --golden-dir"
                .to_owned(),
        })?,
    };
    let golden_dir = options
        .golden_dir
        .clone()
        .unwrap_or_else(|| eval_root.join("golden"));
    let runs_root = options
        .runs_dir
        .clone()
        .unwrap_or_else(|| eval_root.join("runs"));
    Ok((
        golden_dir,
        runs_root,
        eval_root.join("baseline").join("phase0.json"),
    ))
}

/// Walks up from the working directory to the `eval` directory that holds
/// `golden`.
fn discover_eval_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("eval").join("golden").is_dir() {
            return Some(dir.join("eval"));
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Creates `eval/runs/<date>-<suite>`, deduplicated with `-2`, `-3`, ….
///
/// Creation is atomic per candidate: `create_dir` fails with
/// `AlreadyExists` when another process took the directory first, so two
/// concurrent runs of one suite never share a run directory.
fn fresh_run_dir(runs_root: &Path, date: &str, suite: &str) -> Result<PathBuf, EvalError> {
    std::fs::create_dir_all(runs_root).map_err(|source| EvalError::Io {
        action: "create",
        path: runs_root.to_path_buf(),
        source,
    })?;
    let day = date.get(..10).unwrap_or("run");
    let mut run_dir = runs_root.join(format!("{day}-{suite}"));
    let mut suffix = 2;
    loop {
        match std::fs::create_dir(&run_dir) {
            Ok(()) => return Ok(run_dir),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                run_dir = runs_root.join(format!("{day}-{suite}-{suffix}"));
                suffix += 1;
            }
            Err(source) => {
                return Err(EvalError::Io {
                    action: "create",
                    path: run_dir,
                    source,
                });
            }
        }
    }
}

/// SHA-256 of the file at `path` as lowercase hex, read in buffered
/// chunks so the database never sits in memory whole (the manifest hash).
fn hash_file(db_path: &Path) -> Result<String, EvalError> {
    let file = std::fs::File::open(db_path).map_err(|source| EvalError::Io {
        action: "open",
        path: db_path.to_path_buf(),
        source,
    })?;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut chunk).map_err(|source| EvalError::Io {
            action: "read",
            path: db_path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
    }
    Ok(crate::suite::hex_digest(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{EvalError, RunOptions, discover_eval_root, fresh_run_dir, hash_file, run};
    use crate::process_lock;

    #[test]
    fn hashing_a_missing_database_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = hash_file(&dir.path().join("absent.db")).unwrap_err();
        assert!(matches!(err, EvalError::Io { action: "open", .. }), "{err}");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn an_uncreatable_runs_root_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let err =
            fresh_run_dir(&blocker.join("runs"), "2026-11-02T00:00:00Z", "suite").unwrap_err();
        assert!(
            matches!(
                err,
                EvalError::Io {
                    action: "create",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_runs_root_is_an_io_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(runs.join(".probe"), "x").is_ok() {
            return; // the process ignores the mode bits; nothing to test
        }
        let err = fresh_run_dir(&runs, "2026-11-02T00:00:00Z", "suite").unwrap_err();
        std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            matches!(
                err,
                EvalError::Io {
                    action: "create",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn discovery_walks_up_to_the_nearest_eval_directory() {
        let _guard = process_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("eval").join("golden")).unwrap();
        std::fs::create_dir_all(dir.path().join("a").join("b")).unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path().join("a").join("b")).unwrap();
        let found = discover_eval_root();
        std::env::set_current_dir(previous).unwrap();
        // The walk reports the real path (symlinks resolved, no verbatim
        // prefix), so compare both sides canonicalized.
        assert_eq!(
            std::fs::canonicalize(found.unwrap()).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap().join("eval")
        );
    }

    #[test]
    fn a_run_without_an_eval_ancestor_or_golden_dir_is_input_error() {
        let _guard = process_lock();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        // The tempdir can sit under an ancestor that has eval/golden, for
        // example a nested worktree. The walk then resolves it, so the
        // missing-ancestor precondition does not hold and the test skips.
        if discover_eval_root().is_some() {
            std::env::set_current_dir(previous).unwrap();
            return;
        }
        let outcome = run(&RunOptions {
            suite: "anything".to_owned(),
            db: None,
            golden_dir: None,
            runs_dir: None,
            devdocs_bin: None,
        });
        std::env::set_current_dir(previous).unwrap();
        let err = outcome.unwrap_err();
        assert_eq!(err.exit_code(), 1, "{err}");
        assert!(
            err.to_string()
                .contains("cannot locate the eval/ directory"),
            "{err}"
        );
    }
}
