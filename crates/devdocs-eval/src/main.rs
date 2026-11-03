//! `devdocs-eval` entry point: the harness CLI (docs/implementation/14
//! §Harness flow).
//!
//! Exit codes: `0` every gate PASS; `1` invalid input; `2` any gate FAIL or
//! INCONCLUSIVE; `3` operational failure.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use devdocs_eval::report::render_gate_table;
use devdocs_eval::run::{RunOptions, run};

/// The golden-set harness (docs/implementation/14).
#[derive(Debug, Parser)]
#[command(
    name = "devdocs-eval",
    version,
    about = "Run a golden-set suite and print the gate table"
)]
struct Args {
    /// Machine output on stdout; diagnostics stay on stderr.
    #[arg(long)]
    json: bool,

    /// The command to run.
    #[command(subcommand)]
    command: Command,
}

/// The harness commands.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Load, run, score, and write the artifacts for one suite.
    Run {
        /// The suite name under eval/golden, or a direct `.jsonl` path.
        suite: String,
        /// The fixture database; built from the store schema when absent.
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        /// The directory that holds the golden suites.
        #[arg(long, value_name = "DIR")]
        golden_dir: Option<PathBuf>,
        /// The directory that holds the run artifacts.
        #[arg(long, value_name = "DIR")]
        runs_dir: Option<PathBuf>,
        /// The devdocs binary for the subprocess pass.
        #[arg(long, value_name = "PATH")]
        devdocs_bin: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    let Command::Run {
        suite,
        db,
        golden_dir,
        runs_dir,
        devdocs_bin,
    } = args.command;
    let options = RunOptions {
        suite,
        db,
        golden_dir,
        runs_dir,
        devdocs_bin,
    };
    match run(&options) {
        Ok(outcome) => {
            if args.json {
                // The same gate table as structured output (docs/implementation/14).
                let line = serde_json::json!({
                    "run_dir": outcome.run_dir.display().to_string(),
                    "metrics": outcome.metrics,
                    "p95_ms": outcome.p95_ms,
                    "gates": outcome.gates,
                    "all_gates_pass": outcome.all_gates_pass,
                })
                .to_string();
                write_stdout(&format!("{line}\n"));
            } else {
                write_stdout(&render_gate_table(&outcome.gates));
                let verdict = if outcome.all_gates_pass {
                    "all gates PASS"
                } else {
                    "gate failure"
                };
                write_stdout(&format!("run: {} — {verdict}\n", outcome.run_dir.display()));
            }
            if outcome.all_gates_pass {
                ExitCode::from(0)
            } else {
                ExitCode::from(2)
            }
        }
        Err(error) => {
            eprintln!("devdocs-eval: {error}");
            ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(3))
        }
    }
}

/// Writes text to stdout. A closed pipe is a normal end for a CLI; any
/// other failure surfaces on stderr without flipping the gate verdict —
/// the run already completed and its artifacts are written.
fn write_stdout(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let result = stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush());
    if let Err(e) = result
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        eprintln!("devdocs-eval: cannot write to stdout: {e}");
    }
}
