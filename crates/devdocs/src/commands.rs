//! The M3a command handlers: `add`, `list`, and `db migrate`
//! (docs/implementation/08 §CLI contract).
//!
//! Every path that opens the store runs the migration runner at startup
//! (docs/implementation/06 §Migrations). Logs go to stderr through
//! `tracing`; stdout carries only the protocol channel.

use std::sync::Arc;

use devdocs_ingest::{
    FixtureSet, GitPin, IngestJob, gate_fetch, pending_review_refusal, refused_status,
    registry_name,
};
use devdocs_model::config::Config;
use devdocs_model::envelope::{ResultEnvelope, SourceSummary};
use devdocs_model::error::{Error, UpstreamCode};
use devdocs_store::{MigrateOptions, Store};
use url::Url;

/// Opens the store at the configured path and runs the startup migration.
/// The parent directory is created first: a fresh data root must not fail
/// the first `add` or `list`.
///
/// # Errors
/// [`Error::Upstream`] when the directory cannot be created, the store
/// cannot open, or a migration fails.
pub fn open_store(config: &Config, force: bool) -> Result<Arc<Store>, Error> {
    let path = config.index.path.clone();
    if let Some(parent) = path.parent() {
        // The store's path checks expect a private data directory. Create a
        // missing one with mode 0700 in the same call — including the
        // intermediate ancestors. An existing directory keeps its mode; the
        // store warns on drift at open (docs/implementation/09 §Storage).
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|e| Error::Upstream {
                    code: UpstreamCode::UpstreamError,
                    detail: format!("cannot create the data directory {}: {e}", parent.display()),
                })?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(parent).map_err(|e| Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("cannot create the data directory {}: {e}", parent.display()),
        })?;
    }
    let store = Store::open(&path).map_err(|e| map_store_error(&e))?;
    store
        .migrate_with_options(&MigrateOptions { force })
        .map_err(|e| map_store_error(&e))?;
    Ok(Arc::new(store))
}

/// Maps a store error onto the upstream code (exit 3).
fn map_store_error(error: &devdocs_store::StoreError) -> Error {
    Error::Upstream {
        code: UpstreamCode::UpstreamError,
        detail: error.to_string(),
    }
}

/// Maps a retrieval error onto the CLI error model
/// (docs/implementation/08 §Error codes): misses exit 2 with their
/// `miss_reason`, broken refs and cursors exit 1, drift and store
/// failures exit 3.
fn map_retrieve_error(error: devdocs_retrieve::RetrieveError) -> Error {
    match error {
        // defensive: a store failure after the index opens is only
        // reachable through a corrupted database, which the store-layer
        // tests cover; this arm keeps the mapping total.
        devdocs_retrieve::RetrieveError::Store(e) => map_store_error(&e),
        devdocs_retrieve::RetrieveError::Miss(reason) => Error::NoResult { reason },
        devdocs_retrieve::RetrieveError::Usage(e) => Error::Usage(e.to_string()),
        devdocs_retrieve::RetrieveError::HashMismatch(detail) => Error::Upstream {
            code: UpstreamCode::HashMismatch,
            detail,
        },
    }
}

/// The current directory for the resolver's lockfile walk; a vanished
/// working directory is an upstream failure, not a usage error.
fn project_dir() -> Result<std::path::PathBuf, Error> {
    // defensive: the CLI never deletes its own working directory, so the
    // arm is unreachable from a normal invocation; kept for the resolver's
    // lockfile walk.
    std::env::current_dir().map_err(|e| Error::Upstream {
        code: UpstreamCode::UpstreamError,
        detail: format!("cannot read the current directory: {e}"),
    })
}

/// Runs `devdocs search <query>` (docs/implementation/08 §CLI contract).
///
/// # Errors
/// [`Error::NoResult`] on the cold-miss paths (exit 2 with `miss_reason`),
/// [`Error::Usage`] for a broken cursor, [`Error::Upstream`] for store
/// failures.
pub fn run_search(
    config: &Config,
    query: &str,
    library: Option<&str>,
    version: Option<&str>,
    limit: Option<u32>,
    cursor: Option<u64>,
) -> Result<ResultEnvelope, Error> {
    let store = open_store(config, false)?;
    let plan = devdocs_retrieve::Plan {
        limit: limit.unwrap_or(config.search.limit_default),
        max_tokens: config.search.max_tokens_default,
        rrf_k: config.search.rrf_k,
        cursor,
    };
    let query = devdocs_retrieve::Query::new(query, library, version);
    let project = project_dir()?;
    devdocs_retrieve::serve::search(&store, &query, &project, &plan).map_err(map_retrieve_error)
}

/// Runs `devdocs open <ref>`.
///
/// # Errors
/// [`Error::Usage`] for a malformed ref or cursor, [`Error::NoResult`] on
/// the cold-miss paths, [`Error::Upstream`] for store failures.
pub fn run_open(
    config: &Config,
    doc_ref: &str,
    cursor: Option<u64>,
) -> Result<ResultEnvelope, Error> {
    let store = open_store(config, false)?;
    let plan = devdocs_retrieve::Plan {
        max_tokens: config.search.max_tokens_default,
        cursor,
        ..devdocs_retrieve::Plan::defaults()
    };
    devdocs_retrieve::serve::open(&store, doc_ref, &plan).map_err(map_retrieve_error)
}

/// Runs `devdocs verify <ref>`: re-derives the content hash and reports
/// `HASH_MISMATCH` on drift (docs/implementation/06 §Hashing policy).
///
/// # Errors
/// [`Error::Upstream`] with the `HASH_MISMATCH` code on drift,
/// [`Error::Usage`] for a malformed ref, [`Error::NoResult`] on the
/// cold-miss paths, [`Error::Upstream`] for store failures.
pub fn run_verify(config: &Config, doc_ref: &str) -> Result<ResultEnvelope, Error> {
    let store = open_store(config, false)?;
    devdocs_retrieve::serve::verify(&store, doc_ref).map_err(map_retrieve_error)
}

/// Runs `devdocs versions <library>`.
///
/// # Errors
/// [`Error::NoResult`] with `not_indexed` when the library is absent,
/// [`Error::Upstream`] for store failures.
pub fn run_versions(
    config: &Config,
    library: &str,
) -> Result<ResultEnvelope<SourceSummary>, Error> {
    let store = open_store(config, false)?;
    devdocs_retrieve::serve::versions(&store, library).map_err(map_retrieve_error)
}

/// A gate refusal carrying a terminal status (`denied`, `expired`,
/// `revoked`) withdraws the previously published generation before the
/// refusal returns, so a revoked source takes itself out of retrieval even
/// though the publish-time withdrawal never runs
/// (docs/implementation/09 §License gate enforcement points, point 3).
/// A refusal without a terminal status — no entry, no store row,
/// `pending_review` — writes nothing. The withdrawal is not best-effort:
/// when the store cannot record it, the command returns `UPSTREAM_ERROR`
/// instead of `FETCH_BLOCKED`, so a revoked source is never left serving
/// while the command claims `FETCH_BLOCKED`.
fn withdraw_on_terminal_refusal(
    store: &Arc<Store>,
    config: &Config,
    index_url: &Url,
) -> Result<(), Error> {
    let Some(status) =
        refused_status(store, config, index_url).filter(|status| status.is_terminal())
    else {
        return Ok(());
    };
    let withdrawn = store
        .withdraw_source_by_origin_url(index_url.as_str(), status)
        .map_err(|error| Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("gate refused with {status} but the withdrawal failed: {error}"),
        })?;
    if withdrawn > 0 {
        tracing::info!(
            status = status.as_str(),
            withdrawn,
            "withdrew the source on the gate refusal"
        );
    }
    Ok(())
}

/// Runs `devdocs add <source>`.
///
/// Order matters: the license gate denies by default. A URL with no
/// registry entry refuses before the store opens, so a refused source costs
/// no bytes and writes no database. A registered entry consults the store —
/// the authoritative license state, swept for passed expiries first — and a
/// refusal carrying a terminal status withdraws the previously published
/// generation before it returns (docs/implementation/09 §License gate
/// enforcement points, point 3): the publish-time withdrawal cannot run,
/// because this refusal happens before any publish.
///
/// The effective kind dispatches the adapter (docs/implementation/07
/// §Priority ladder): an explicit `--kind` wins, then the config
/// `[sources.<name>].kind`, then `llms_txt`.
///
/// # Errors
/// [`Error`] for the gate refusal (`FETCH_BLOCKED`), guard refusals and
/// failures, parse failures, git failures, and store failures.
pub fn run_add(
    config: &Config,
    source: &str,
    version: Option<&str>,
    kind: Option<&str>,
    fixture_dir: Option<&std::path::Path>,
) -> Result<serde_json::Value, Error> {
    let index_url = Url::parse(source).map_err(|e| Error::Usage(format!("bad source URL: {e}")))?;

    // 1. The license gate: deny by default. The unregistered refusal runs
    //    before the store opens, and the usage validation (the effective
    //    kind) runs before the store opens too, so a rejected add still
    //    writes no database. The registered path then sweeps the store and
    //    reads its authoritative state.
    let Some(name) = registry_name(config, &index_url)? else {
        return Err(pending_review_refusal(&index_url));
    };

    // 2. The effective kind. `--kind` wins, then the config entry.
    let effective_kind = kind
        .or_else(|| {
            config
                .sources
                .get(&name)
                .and_then(|entry| entry.kind.as_deref())
        })
        .unwrap_or("llms_txt");

    if effective_kind == "git" {
        // `--fixture` replays recorded HTTP responses; the git adapter has
        // no replay surface, so a silent drop would run against the network.
        if fixture_dir.is_some() {
            return Err(Error::Usage(
                "--fixture replays HTTP recordings and does not apply to git sources".to_owned(),
            ));
        }
        // The pin must be explicit: there is no "latest" tag to resolve.
        let Some(pin) = version else {
            return Err(Error::Usage(
                "git sources require --version (a tag or a commit SHA)".to_owned(),
            ));
        };
        let store = open_store(config, false)?;
        let grant = match gate_fetch(&store, config, &index_url) {
            Ok(grant) => grant,
            Err(error) => {
                withdraw_on_terminal_refusal(&store, config, &index_url)?;
                return Err(error);
            }
        };
        return run_add_git(config, &index_url, &grant.name, pin);
    }

    let store = open_store(config, false)?;
    let grant = match gate_fetch(&store, config, &index_url) {
        Ok(grant) => grant,
        Err(error) => {
            withdraw_on_terminal_refusal(&store, config, &index_url)?;
            return Err(error);
        }
    };

    // 3. The fixture set, when replaying.
    let fixture = fixture_dir
        .map(FixtureSet::load)
        .transpose()
        .map_err(map_ingest_error)?;

    // 4. The guard and the ingest run. The tokio runtime is local to the
    // command: the rest of the CLI stays synchronous.
    let guard = devdocs_ingest::FetchGuard::from_fetch_config(&config.fetch);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("cannot start the async runtime: {e}"),
        })?;

    let version = version.unwrap_or("latest");
    let job = IngestJob::new(config, &guard, fixture.as_ref());
    let result = runtime.block_on(job.run(Arc::clone(&store), &index_url, &grant.name, version));
    // A run that hit the total cap can leave its in-flight segmentation
    // blocking task running; do not wait for it when the runtime drops.
    runtime.shutdown_background();
    let outcome = result.map_err(map_ingest_error)?;
    add_report(&outcome)
}

/// Runs the git branch of `devdocs add`: the pin is the `--version` value
/// (40 hex → commit pin, otherwise tag pin), the clone runs inside
/// [`IngestJob::run_git`]'s `spawn_blocking` — not through the HTTP fetch
/// guard, which does not govern the git transport
/// (docs/implementation/07 §Git adapter) — and the version label is the
/// pin itself.
///
/// # Errors
/// [`Error`] for any ingest failure (`IngestJob::run_git` maps the whole
/// plan/fetch/segment/publish chain).
fn run_add_git(
    config: &Config,
    repo_url: &Url,
    library: &str,
    pin: &str,
) -> Result<serde_json::Value, Error> {
    let guard = devdocs_ingest::FetchGuard::from_fetch_config(&config.fetch);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("cannot start the async runtime: {e}"),
        })?;

    let store = open_store(config, false)?;
    // The clone directory lands under the store's data root, the directory
    // `open_store` already created with mode 0700.
    let data_root = config.index.path.parent().map_or_else(
        || std::path::PathBuf::from("."),
        std::path::Path::to_path_buf,
    );
    let job = IngestJob::new(config, &guard, None);
    let result = runtime.block_on(job.run_git(
        Arc::clone(&store),
        repo_url,
        library,
        pin,
        GitPin::from(pin),
        &data_root,
    ));
    // A run that hit the total cap can leave its in-flight segmentation
    // blocking task running; do not wait for it when the runtime drops.
    runtime.shutdown_background();
    let outcome = result.map_err(map_ingest_error)?;
    add_report(&outcome)
}

/// The `add --json` report both adapters return; the event report also
/// goes to stderr as log lines (the default filter is `warn`, so the lines
/// surface only under `--verbose`)
/// (docs/implementation/07 §Normalization pipeline).
fn add_report(outcome: &devdocs_ingest::IngestOutcome) -> Result<serde_json::Value, Error> {
    tracing::info!(
        pages = outcome.pages,
        skipped = outcome.skipped_pages,
        chunks = outcome.chunks,
        "ingest finished"
    );
    outcome.events.log();
    // defensive: `IngestEvents` is a plain struct of integer counters, so
    // `serde_json::to_value` cannot fail on it; the mapping exists only to
    // fit the `Result`-returning call chain, never to run.
    let events = serde_json::to_value(&outcome.events).map_err(|e| Error::Upstream {
        code: UpstreamCode::UpstreamError,
        detail: format!("cannot serialize the ingest events: {e}"),
    })?;
    Ok(serde_json::json!({
        "library": outcome.library,
        "version": outcome.version,
        "origin_url": outcome.origin_url,
        "license_status": outcome.license_status.to_string(),
        "pages": outcome.pages,
        "skipped_pages": outcome.skipped_pages,
        "chunks": outcome.chunks,
        "commit_sha": outcome.commit_sha,
        "events": events,
    }))
}

/// Runs `devdocs list`.
///
/// # Errors
/// [`Error::Upstream`] when the store cannot open or the query fails.
pub fn run_list(config: &Config) -> Result<ResultEnvelope<SourceSummary>, Error> {
    let store = open_store(config, false)?;
    let results = store.list_sources().map_err(|e| map_store_error(&e))?;
    Ok(ResultEnvelope {
        results,
        truncated: false,
        next_cursor: None,
        served_at: devdocs_model::envelope::now_rfc3339(),
    })
}

/// Runs `devdocs db migrate`.
///
/// # Errors
/// [`Error::Upstream`] when the store cannot open or a migration fails;
/// a destructive pending migration without `--force` maps to the same
/// upstream code with a hint naming `--force`.
pub fn run_db_migrate(config: &Config, force: bool) -> Result<serde_json::Value, Error> {
    let store = open_store(config, force)?;
    // The startup migration inside open_store already ran; report the
    // schema state.
    let version = store.user_version().map_err(|e| map_store_error(&e))?;
    Ok(serde_json::json!({
        "schema_version": version,
        "forced": force,
    }))
}

/// Maps an ingest error onto the CLI error model: guard refusals become
/// `FETCH_BLOCKED`, everything else `UPSTREAM_ERROR` (docs/implementation/08
/// §Error codes).
fn map_ingest_error(error: devdocs_ingest::IngestError) -> Error {
    use devdocs_ingest::IngestError as E;
    match error {
        E::Guard(guard) if guard.is_refusal() => Error::Upstream {
            code: UpstreamCode::FetchBlocked,
            detail: guard.to_string(),
        },
        E::Guard(guard) => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: guard.to_string(),
        },
        E::License(model) => model,
        E::LicenseNameMismatch { expected, actual } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!(
                "LICENSE_MISMATCH: library `{actual}` does not match the registry entry `{expected}`"
            ),
        },
        E::Store(store) => map_store_error(&store),
        E::LlmsTxtEmpty { url } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("LLMS_TXT_EMPTY: no link lines in {url}"),
        },
        E::LlmsTxtInvalid { url, reason } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("LLMS_TXT_INVALID: {reason} ({url})"),
        },
        E::Fixture { path, message } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("fixture {path}: {message}"),
        },
        E::Tokenizer { message } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("tokenizer: {message}"),
        },
        E::TotalTimeout { seconds } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("the ingest run exceeded the {seconds}s total cap"),
        },
        E::Join(message) => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("blocking task failed: {message}"),
        },
        E::NoIngestablePages { index } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("no ingestable pages for {index}"),
        },
        E::GitBinaryMissing => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: "GIT_BINARY_MISSING: the git executable was not found on PATH".to_owned(),
        },
        E::GitInvalidPin { pin } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("GIT_INVALID_PIN: pin `{pin}` starts with `-`"),
        },
        E::GitUrlMismatch { planned, actual } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!(
                "GIT_URL_MISMATCH: fetch URL `{actual}` does not match the planned URL `{planned}`"
            ),
        },
        E::GitCommand { command, stderr } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("GIT_COMMAND_FAILED: `{command}` failed: {stderr}"),
        },
        E::GitSpawn { message } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("GIT_SPAWN_FAILED: {message}"),
        },
        E::GitTempDir { message } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("GIT_TEMP_DIR: {message}"),
        },
        E::GitWalk { message } => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("GIT_WALK_FAILED: {message}"),
        },
        E::GitCommitShaInvalid(sha) => Error::Upstream {
            code: UpstreamCode::UpstreamError,
            detail: format!("GIT_COMMIT_SHA_INVALID: rev-parse returned `{sha}`"),
        },
    }
}
