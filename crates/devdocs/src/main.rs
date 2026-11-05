//! The devdocs CLI entry point: parse, dispatch, and map errors to exit
//! codes (docs/implementation/05 §Error model).

use std::io::Write as _;

use clap::Parser as _;
use devdocs::cli::{Cli, run};
use devdocs_model::{Error, UpstreamCode};

fn main() {
    std::process::exit(real_main());
}

fn real_main() -> i32 {
    match Cli::try_parse() {
        Ok(cli) => {
            init_tracing(&cli);
            if let Some(error) = maybe_inject() {
                return report_error(&error);
            }
            match run(&cli) {
                Ok(()) => 0,
                Err(error) => report_error(&error),
            }
        }
        Err(parse_error) => report_parse_error(&parse_error),
    }
}

/// Initialize the stderr-only tracing subscriber. stdout is a protocol
/// channel and never a log sink (.agents/instructions/observability).
fn init_tracing(cli: &Cli) {
    use tracing_subscriber::EnvFilter;

    let filter = if cli.quiet {
        "off"
    } else if cli.verbose {
        "trace"
    } else {
        "warn"
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::new(filter))
        .without_time()
        .init();
}

/// Remap a clap parse error: help and version print on stdout and exit 0;
/// every other parse error prints the USAGE envelope on stderr and exits 1,
/// so clap's default exit 2 is never observable (docs/05 §Error model).
fn report_parse_error(parse_error: &clap::Error) -> i32 {
    if !parse_error.use_stderr() {
        let rendered = parse_error.render().to_string();
        return match devdocs::cli::write_stdout_text(&rendered) {
            Ok(()) => 0,
            Err(e) => report_error(&e),
        };
    }
    let error = Error::Usage(parse_error.render().to_string());
    report_error(&error)
}

/// Print the JSON error envelope on stderr and return the mapped exit code.
fn report_error(error: &Error) -> i32 {
    let envelope = error.to_envelope();
    let json = envelope.to_json_string().unwrap_or_else(|_| {
        format!(
            "{{\"error\":\"{}\",\"hint\":null,\"detail\":null}}",
            error.code()
        )
    });
    eprintln!("{json}");
    let _ = std::io::stderr().flush();
    error.exit_code()
}

/// The M1 test seam: `DEVDOCS_INJECT_ERROR=<stable code>` makes the CLI
/// print that error envelope and exit with its mapped code. It exists so
/// the exit-code table test can drive codes 2-4 through the real binary;
/// the owning milestones remove it with the placeholders (docs/12 M2+).
fn injected_error(code: &str) -> Option<Error> {
    let error = match code {
        "USAGE" => Error::Usage("injected usage error".to_owned()),
        "CONFIG_INVALID" => Error::ConfigInvalid {
            key_path: "fetch.concurrency".to_owned(),
            reason: "injected config error".to_owned(),
        },
        // The cold-miss code carries `not_indexed`; plain `NO_RESULT` maps
        // to a valid-query miss without a colder reason.
        "PKG_NOT_INDEXED" => Error::NoResult {
            reason: devdocs_model::MissReason::NotIndexed,
        },
        "NO_RESULT" => Error::NoResult {
            reason: devdocs_model::MissReason::LowConfidence,
        },
        "FETCH_BLOCKED" | "UPSTREAM_ERROR" | "HASH_MISMATCH" => Error::Upstream {
            code: match code {
                "FETCH_BLOCKED" => UpstreamCode::FetchBlocked,
                "HASH_MISMATCH" => UpstreamCode::HashMismatch,
                _ => UpstreamCode::UpstreamError,
            },
            detail: "injected upstream error".to_owned(),
        },
        "BUDGET_EXCEEDED" => Error::BudgetExceeded,
        _ => return None,
    };
    Some(error)
}

/// Handle the test seam before dispatch, when the variable is set.
fn maybe_inject() -> Option<Error> {
    std::env::var("DEVDOCS_INJECT_ERROR")
        .ok()
        .filter(|code| !code.is_empty())
        .and_then(|code| injected_error(&code))
}
