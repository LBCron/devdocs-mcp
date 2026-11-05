//! The command surface and the dispatch (docs/implementation/08 §CLI contract).
//!
//! Every command from docs/08 is registered here. Commands whose owning
//! milestones have not landed print the empty result envelope and exit 0;
//! `config` is real. The CLI edge in the binary entry point maps errors to
//! exit codes and JSON error envelopes on stderr.

use std::io::Write as _;
use std::path::PathBuf;

use clap::Subcommand;
use clap::builder::Styles;
use clap::builder::styling;
use devdocs_model::config;
use devdocs_model::{Config, Error, Overrides, empty_envelope};
use serde::Serialize;

/// One-screen help template colors (no color when stdout is not a TTY;
/// clap handles the TTY detection).
fn help_styles() -> Styles {
    styling::Styles::styled()
        .header(styling::AnsiColor::Yellow.on_default())
        .literal(styling::AnsiColor::Green.on_default())
        .placeholder(styling::AnsiColor::Cyan.on_default())
}

/// The `devdocs` command-line interface.
#[derive(Debug, clap::Parser)]
#[command(
    name = "devdocs",
    version,
    about = "Local documentation index for coding agents",
    disable_help_subcommand = true,
    subcommand_required = true,
    arg_required_else_help = true,
    styles = help_styles()
)]
pub struct Cli {
    /// Machine output on stdout; logs stay on stderr.
    #[arg(long, global = true)]
    pub json: bool,

    /// Override the index database path.
    #[arg(long, value_name = "PATH", global = true)]
    pub db: Option<PathBuf>,

    /// Use this config file instead of the project `devdocs.toml`.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    /// Cap the output budget in tokens.
    #[arg(long, value_name = "N", global = true)]
    pub max_tokens: Option<u64>,

    /// Cap the fetch timeout in milliseconds.
    #[arg(long, value_name = "N", global = true)]
    pub timeout_ms: Option<u64>,

    /// Trace logs on stderr.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Silence diagnostics on stderr.
    #[arg(long, global = true)]
    pub quiet: bool,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Every devdocs command (docs/implementation/08 §CLI contract table).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Ingest a source through the ladder.
    Add {
        /// The source to ingest, for example a GitHub URL.
        source: String,
        /// The version to pin.
        #[arg(long)]
        version: Option<String>,
        /// The source kind override.
        #[arg(long)]
        kind: Option<String>,
        /// Replay recorded responses from this directory instead of using
        /// the network (docs/implementation/13 §M3 step 5).
        #[arg(long, value_name = "DIR")]
        fixture: Option<PathBuf>,
    },
    /// Refresh stale approved sources.
    Sync {
        /// Restrict the refresh to one library.
        library: Option<String>,
    },
    /// List indexed libraries and versions (the discovery surface).
    List,
    /// Hybrid search over the local index.
    Search {
        /// The query text.
        query: String,
        /// Restrict to one library.
        #[arg(long)]
        library: Option<String>,
        /// Restrict to one version.
        #[arg(long)]
        version: Option<String>,
        /// Cap the result count.
        #[arg(long)]
        limit: Option<u32>,
        /// Resume after the last served chunk id from a truncated page.
        #[arg(long)]
        cursor: Option<u64>,
    },
    /// Exact symbol lookup.
    Symbol {
        /// The library@version to search.
        target: String,
        /// The symbol name.
        symbol: String,
    },
    /// Read one section by ref.
    Open {
        /// The document ref, for example `docs://next.js/v16.2.9/proxy#rewrite`.
        doc_ref: String,
        /// Resume at the character offset from a truncated page.
        #[arg(long)]
        cursor: Option<u64>,
    },
    /// Re-derive and compare the content hash of a ref.
    Verify {
        /// The document ref to verify.
        doc_ref: String,
    },
    /// List available versions for a library.
    Versions {
        /// The library name.
        library: String,
    },
    /// License-registry operations.
    Source {
        /// The source operation.
        #[command(subcommand)]
        op: SourceOp,
    },
    /// Write the SKILL.md companion file.
    Skill {
        /// The skill operation.
        #[command(subcommand)]
        op: SkillOp,
    },
    /// Start the MCP server on stdio.
    Mcp {
        /// The transport, only `stdio` today.
        #[arg(
            long,
            default_value = "stdio",
            value_parser = clap::builder::PossibleValuesParser::new(["stdio"])
        )]
        transport: String,
    },
    /// Database operations.
    Db {
        /// The database operation.
        #[command(subcommand)]
        op: DbOp,
    },
    /// Configuration operations.
    Config {
        /// The config operation.
        #[command(subcommand)]
        op: ConfigOp,
    },
    /// Run a golden-set suite.
    Eval {
        /// The eval operation.
        #[command(subcommand)]
        op: EvalOp,
    },
}

/// `devdocs source <op>`.
#[derive(Debug, Subcommand)]
pub enum SourceOp {
    /// Approve a source in the license registry.
    Approve {
        /// The registry entry name.
        name: String,
    },
    /// Deny a source in the license registry.
    Deny {
        /// The registry entry name.
        name: String,
    },
    /// List registry entries.
    List,
}

/// `devdocs skill <op>`.
#[derive(Debug, Subcommand)]
pub enum SkillOp {
    /// Write the SKILL.md companion file.
    Install {
        /// Where to write the file.
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
    },
}

/// `devdocs db <op>`.
#[derive(Debug, Subcommand)]
pub enum DbOp {
    /// Run pending schema migrations.
    Migrate {
        /// Admit destructive (drop) migrations for this run
        /// (docs/implementation/06 §Migrations).
        #[arg(long)]
        force: bool,
    },
    /// Reclaim space in the database file.
    Vacuum,
    /// Print database statistics.
    Stats,
    /// Quarantine operations.
    Quarantine {
        /// The quarantine operation.
        #[command(subcommand)]
        op: QuarantineOp,
    },
}

/// `devdocs db quarantine <op>`.
#[derive(Debug, Subcommand)]
pub enum QuarantineOp {
    /// List quarantined chunks.
    List,
}

/// `devdocs config <op>`.
#[derive(Debug, Subcommand)]
pub enum ConfigOp {
    /// Write the commented user config file.
    Init {
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Print the merged effective configuration as JSON.
    Show,
    /// Validate a config file and report the failing key path.
    Validate {
        /// Validate this file instead of the resolved layers.
        path: Option<PathBuf>,
    },
    /// Print the resolved config paths as JSON.
    Path,
}

/// `devdocs eval <op>`.
#[derive(Debug, Subcommand)]
pub enum EvalOp {
    /// Run one golden-set suite.
    Run {
        /// The suite name.
        suite: String,
    },
}

/// Dispatch the parsed command.
///
/// # Errors
/// Returns the [`Error`] for the CLI edge to map to an exit code.
pub fn run(cli: &Cli) -> Result<(), Error> {
    match &cli.command {
        Command::Config { op } => run_config(cli, op),
        Command::Add {
            source,
            version,
            kind,
            fixture,
        } => {
            // Only the llms.txt and git adapters exist in this slice; a
            // silent flag drop would run the wrong pipeline with no error.
            if let Some(kind) = kind
                && kind != "llms_txt"
                && kind != "git"
            {
                return Err(Error::Usage(format!(
                    "--kind {kind} is not supported yet; this slice ingests llms_txt and git sources only"
                )));
            }
            let config = Config::load(&overrides(cli))?;
            let result = crate::commands::run_add(
                &config,
                source,
                version.as_deref(),
                kind.as_deref(),
                fixture.as_deref(),
            )?;
            if cli.json {
                print_line(&result)
            } else {
                write_stdout_line(&summarize_add(&result))
            }
        }
        Command::List => {
            let config = Config::load(&overrides(cli))?;
            let envelope = crate::commands::run_list(&config)?;
            if cli.json {
                print_line(&envelope)
            } else {
                for item in &envelope.results {
                    let state = if item.active { "active" } else { "inactive" };
                    write_stdout_line(&format!(
                        "{} {} ({} · {} · {})",
                        item.library, item.version, item.kind, item.license_status, state
                    ))?;
                }
                Ok(())
            }
        }
        Command::Db {
            op: DbOp::Migrate { force },
        } => {
            let config = Config::load(&overrides(cli))?;
            let result = crate::commands::run_db_migrate(&config, *force)?;
            print_line(&result)
        }
        Command::Search {
            query,
            library,
            version,
            limit,
            cursor,
        } => run_search_command(
            cli,
            query,
            library.as_deref(),
            version.as_deref(),
            *limit,
            *cursor,
        ),
        Command::Open { doc_ref, cursor } => run_open_command(cli, doc_ref, *cursor),
        Command::Verify { doc_ref } => run_verify_command(cli, doc_ref),
        Command::Versions { library } => run_versions_command(cli, library),
        Command::Sync { .. }
        | Command::Symbol { .. }
        | Command::Source { .. }
        | Command::Skill { .. }
        | Command::Mcp { .. }
        | Command::Db { .. }
        | Command::Eval { .. } => print_placeholder(),
    }
}

/// Runs `devdocs search` and prints the envelope.
fn run_search_command(
    cli: &Cli,
    query: &str,
    library: Option<&str>,
    version: Option<&str>,
    limit: Option<u32>,
    cursor: Option<u64>,
) -> Result<(), Error> {
    let config = Config::load(&overrides(cli))?;
    let envelope = crate::commands::run_search(&config, query, library, version, limit, cursor)?;
    if cli.json {
        print_line(&envelope)
    } else {
        write_stdout_text(&summarize_search(&envelope))
    }
}

/// Runs `devdocs open` and prints the served page.
fn run_open_command(cli: &Cli, doc_ref: &str, cursor: Option<u64>) -> Result<(), Error> {
    let config = Config::load(&overrides(cli))?;
    let envelope = crate::commands::run_open(&config, doc_ref, cursor)?;
    if cli.json {
        print_line(&envelope)
    } else {
        write_stdout_text(&summarize_open(&envelope))
    }
}

/// Runs `devdocs verify` and prints the verification outcome.
fn run_verify_command(cli: &Cli, doc_ref: &str) -> Result<(), Error> {
    let config = Config::load(&overrides(cli))?;
    let envelope = crate::commands::run_verify(&config, doc_ref)?;
    if cli.json {
        print_line(&envelope)
    } else {
        let item = envelope.results.first();
        write_stdout_line(&match item {
            Some(item) => format!("{} verified ({})", item.doc_ref, item.content_hash),
            // defensive: `verify` answers with exactly one result or an
            // error, so the envelope never arrives empty; the arm keeps
            // the match total.
            None => String::from("nothing to verify"),
        })
    }
}

/// Runs `devdocs versions` and prints the per-library discovery surface.
fn run_versions_command(cli: &Cli, library: &str) -> Result<(), Error> {
    let config = Config::load(&overrides(cli))?;
    let envelope = crate::commands::run_versions(&config, library)?;
    if cli.json {
        print_line(&envelope)
    } else {
        for item in &envelope.results {
            let state = if item.active { "active" } else { "inactive" };
            write_stdout_line(&format!(
                "{} {} ({} · {} · {})",
                item.library, item.version, item.kind, item.license_status, state
            ))?;
        }
        Ok(())
    }
}

/// The human-readable search summary without `--json`: one line per
/// result, in fusion order, plus the truncation notice.
fn summarize_search(envelope: &devdocs_model::ResultEnvelope) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for item in &envelope.results {
        let _ = writeln!(
            out,
            "{}  {}  [{} · {} · {} tokens]",
            item.doc_ref, item.title, item.served_version, item.version_status, item.token_count
        );
    }
    if envelope.truncated
        && let Some(cursor) = &envelope.next_cursor
    {
        let _ = writeln!(out, "…truncated; resume with --cursor {cursor}");
    }
    out
}

/// The human-readable `open` output: the served text, then the
/// continuation notice when the budget cut the chunk.
fn summarize_open(envelope: &devdocs_model::ResultEnvelope) -> String {
    use std::fmt::Write as _;

    let mut out = envelope
        .results
        .first()
        .map(|item| item.snippet.clone())
        .unwrap_or_default();
    if envelope.truncated
        && let Some(cursor) = &envelope.next_cursor
    {
        let _ = writeln!(out, "\n…truncated; resume with --cursor {cursor}");
    }
    out
}

/// The human-readable one-line summary `add` prints without `--json`.
fn summarize_add(result: &serde_json::Value) -> String {
    let library = result
        .get("library")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let version = result
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let chunks = result
        .get("chunks")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let pages = result
        .get("pages")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    format!("indexed {library} {version}: {pages} pages, {chunks} chunks")
}

/// Print the empty result envelope: the deliberate M1 placeholder that the
/// owning milestones replace (docs/implementation/13 §M1).
fn print_placeholder() -> Result<(), Error> {
    print_line(&empty_envelope())
}

/// Print one JSON value as a single line on stdout.
fn print_line<T: Serialize>(value: &T) -> Result<(), Error> {
    // A serialization failure of our own plain structs is a local bug, not
    // a fetch/network failure. docs/05 reserves exit 3 for fetch, network,
    // git, or parse failures; a local serialization bug is the closest
    // parse failure until docs/05 gains an internal-error kind.
    let line = serde_json::to_string(value).map_err(|e| Error::Upstream {
        code: devdocs_model::UpstreamCode::UpstreamError,
        detail: format!("cannot serialize the output: {e}"),
    })?;
    write_stdout_line(&line)
}

/// Write raw text to stdout without panicking. A closed pipe (the reader
/// went away, for example `| head`) is a normal end for a CLI and maps to
/// success; any other write failure maps to the upstream code, the same
/// decision the envelope serialization carries. The envelope lines, the
/// help text, and the version text all go through here, so the policy
/// lives once.
///
/// # Errors
/// [`Error::Upstream`] when stdout fails for a reason other than a closed
/// pipe.
pub fn write_stdout_text(text: &str) -> Result<(), Error> {
    let mut stdout = std::io::stdout().lock();
    let result = stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush());
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(Error::Upstream {
            code: devdocs_model::UpstreamCode::UpstreamError,
            detail: format!("cannot write to stdout: {e}"),
        }),
    }
}

/// Write one envelope line to stdout, newline included.
fn write_stdout_line(line: &str) -> Result<(), Error> {
    write_stdout_text(&format!("{line}\n"))
}

/// Build the flag layer from the parsed global flags.
fn overrides(cli: &Cli) -> Overrides {
    Overrides {
        config_path: cli.config.clone(),
        db_path: cli.db.clone(),
        max_tokens: cli.max_tokens,
        timeout_ms: cli.timeout_ms,
    }
}

/// Run the `config` subcommands.
fn run_config(cli: &Cli, op: &ConfigOp) -> Result<(), Error> {
    match op {
        ConfigOp::Show => {
            let conf = Config::load(&overrides(cli))?;
            print_line(&conf)
        }
        ConfigOp::Path => {
            let paths = Config::paths(&overrides(cli))?;
            print_line(&paths)
        }
        ConfigOp::Init { force } => config_init(&config::require_user_config_path()?, *force),
        ConfigOp::Validate { path } => config_validate(path.as_deref()),
    }
}

/// Write the commented user config file; refuse to overwrite without force.
/// The create is exclusive when force is off, so a concurrent run cannot
/// lose its file to a truncate (docs/08 defines no code for "file exists";
/// USAGE with exit 1 is the least-wrong documented code, and the detail
/// names --force).
fn config_init(path: &std::path::Path, force: bool) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::ConfigInvalid {
            key_path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    }
    let open = if force {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    } else {
        // Exclusive create: no check-then-write race window.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    };
    let mut file = open.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Error::Usage(format!(
                "user config already exists at {}; pass --force to overwrite",
                path.display()
            ))
        } else {
            Error::ConfigInvalid {
                key_path: path.display().to_string(),
                reason: e.to_string(),
            }
        }
    })?;
    file.write_all(user_config_template().as_bytes())
        .map_err(|e| Error::ConfigInvalid {
            key_path: path.display().to_string(),
            reason: e.to_string(),
        })?;
    write_stdout_line(&serde_json::json!({"wrote": path.display().to_string()}).to_string())
}

/// Validate one file, or the resolved layer chain when no path is given.
fn config_validate(path: Option<&std::path::Path>) -> Result<(), Error> {
    if let Some(path) = path {
        Config::validate_file(path)?;
        write_stdout_line(
            &serde_json::json!({"valid": true, "path": path.display().to_string()}).to_string(),
        )?;
    } else {
        // No path given: the merged load validates every layer in turn.
        Config::load(&Overrides::default())?;
        write_stdout_line(&serde_json::json!({"valid": true}).to_string())?;
    }
    Ok(())
}

/// The commented template `config init` writes.
fn user_config_template() -> String {
    String::from(concat!(
        "# devdocs user configuration.\n",
        "# Precedence: flags > DEVDOCS_* environment > project devdocs.toml > this file > defaults.\n",
        "\n",
        "[fetch]\n",
        "concurrency = 4\n",
        "timeout_ms = 15000\n",
        "max_bytes = 20971520\n",
        "max_redirects = 3\n",
        "user_agent = \"devdocs/0.1 (+https://github.com/OWNER/devdocs-mcp)\"\n",
        "\n",
        "[index]\n",
        "# path = \"~/.local/share/devdocs/docs.db\"\n",
        "embeddings = false\n",
        "\n",
        "[search]\n",
        "max_tokens_default = 2000\n",
        "limit_default = 8\n",
        "rrf_k = 60\n",
        "\n",
        "# [sources.nextjs]\n",
        "# kind = \"llms_txt\"\n",
        "# url = \"https://nextjs.org/docs/llms.txt\"\n",
        "# license = \"MIT\"\n",
        "# expires = \"2027-11-06\"\n",
        "# refresh_days = 2\n",
    ))
}
