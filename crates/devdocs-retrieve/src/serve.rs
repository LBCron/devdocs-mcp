//! The serving pipeline (docs/implementation/08 §Query pipeline).
//!
//! `Query → resolve (library, version) → plan rungs → execute rungs → RRF
//! fusion (k=60) → budget pack → result envelope`. The serve layer owns
//! the version policy application, the migration-question rung, and the
//! `open`/`verify` read paths; the CLI edge maps
//! [`RetrieveError`] onto the model error and exit codes.

use std::path::Path;

use devdocs_model::envelope::{ResultEnvelope, ResultItem, SourceSummary};
use devdocs_model::error::MissReason;
use devdocs_model::{VersionStatus, now_rfc3339};
use devdocs_store::{Hit, Store};

use crate::budget::pack;
use crate::error::{RetrieveError, UsageError};
use crate::fusion::fuse;
use crate::item::{match_ref, to_result_item};
use crate::resolver::{ProjectContext, Query, resolve};
use crate::rung::{Scope, fts_rung, symbol_rung};
use crate::version::{Version, is_migration_query, pick};

/// The search/open knobs the CLI carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The result count cap (docs/08: `--limit`, default 8).
    pub limit: u32,
    /// The output budget in tokens (docs/08: `--max-tokens`, default 2000).
    pub max_tokens: u64,
    /// The RRF constant (docs/08: k = 60).
    pub rrf_k: u32,
    /// The resume point: the last served chunk id (`search`) or the char
    /// offset (`open`) the next page continues after.
    pub cursor: Option<u64>,
}

impl Plan {
    /// The documented defaults: limit 8, max-tokens 2000, k 60, no cursor.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            limit: 8,
            max_tokens: 2000,
            rrf_k: crate::fusion::RRF_K_DEFAULT,
            cursor: None,
        }
    }
}

/// Runs the search pipeline end to end.
///
/// `project` is the directory the resolver starts its lockfile walk from —
/// the CLI passes the current directory.
///
/// # Errors
/// [`RetrieveError::Miss`] on the cold-miss paths (exit 2),
/// [`RetrieveError::Usage`] for a broken cursor (exit 1), and
/// [`RetrieveError::Store`] for store failures (exit 3).
pub fn search(
    store: &Store,
    query: &Query,
    project: &Path,
    plan: &Plan,
) -> Result<ResultEnvelope, RetrieveError> {
    let ctx = ProjectContext::detect(project);
    let resolved = resolve(query, &ctx);

    // Plan the rungs from the resolution. The headroom is 4× the requested
    // hits (docs/08 §Rungs, rung 2).
    let headroom = plan.limit.saturating_mul(4).max(1);
    let mut rung_hits: Vec<Vec<devdocs_store::Hit>> =
        vec![symbol_rung(store, scope_for(&resolved))];

    let (pick_status, served_version, requested_text): (
        VersionStatus,
        Option<String>,
        Option<String>,
    ) = if let Some(library) = &resolved.library {
        let available: Vec<Version> = store
            .library_versions(library)?
            .into_iter()
            .map(Version::from)
            .collect();
        if available.is_empty() {
            return Err(MissReason::NotIndexed.into());
        }
        let pick = pick(&available, resolved.version.as_deref());
        let served = pick.version.clone().ok_or(MissReason::NotIndexed)?;
        let source_id = store
            .find_source(library, served.as_str())?
            .ok_or(MissReason::NotIndexed)?;
        let scope = Scope::Source(source_id);
        rung_hits.push(fts_rung(store, &query.text, scope, headroom)?);

        // The migration-question path: retrieve the served version and
        // the nearest older version, both labeled through their own
        // served_version (docs/08 §Version resolution).
        if is_migration_query(&query.text)
            && let Some(older) = nearest_older(&available, &served)
            && let Some(older_id) = store.find_source(library, older.as_str())?
        {
            rung_hits.push(fts_rung(
                store,
                &query.text,
                Scope::Source(older_id),
                headroom,
            )?);
        }
        (
            pick.status,
            Some(served.as_str().to_owned()),
            resolved.version.clone(),
        )
    } else {
        // Unfiltered fallback: search all approved sources. Per-item
        // status is the alias default (no version requested).
        rung_hits.push(fts_rung(store, &query.text, Scope::All, headroom)?);
        (VersionStatus::Alias, None, resolved.version.clone())
    };

    let fused = fuse(plan.rrf_k, &rung_hits);
    if fused.is_empty() {
        // A valid query that serves nothing is a miss (exit 2). The
        // reason distinguishes an empty index from a query that simply
        // matched nothing (docs/08 §Error codes).
        let has_active = store
            .list_sources()
            .is_ok_and(|sources| sources.iter().any(|s| s.active));
        let reason = if has_active {
            MissReason::LowConfidence
        } else {
            MissReason::NotIndexed
        };
        return Err(reason.into());
    }

    // Cursor resume: continue after the last served chunk id in the same
    // fusion order.
    let fused: Vec<_> = match plan.cursor {
        None => fused,
        Some(cursor) => {
            let position = fused
                .iter()
                .position(|(hit, _)| hit.chunk_id == i64::try_from(cursor).unwrap_or(-1))
                .ok_or_else(|| UsageError::UnknownCursor(cursor.to_string()))?;
            fused[position + 1..].to_vec()
        }
    };

    let packed = pack(fused, plan.max_tokens);
    let results: Vec<ResultItem> = packed
        .items
        .iter()
        .map(|(hit, score)| {
            // Per-item status: the picked version carries the request's
            // status. Without a picked version (the unfiltered fallback),
            // an exact-request hit verifies and everything else is the
            // alias default — or a near version served.
            let status = match (&served_version, &requested_text) {
                (Some(served), _) if hit.version == *served => pick_status,
                (None, Some(requested)) if hit.version == *requested => VersionStatus::Verified,
                (None, None) => VersionStatus::Alias,
                // A near version served, or an exact request on the
                // unfiltered path that another version answered.
                _ => VersionStatus::Fallback,
            };
            to_result_item(hit, requested_text.as_deref(), status, *score)
        })
        .collect();

    Ok(ResultEnvelope {
        results,
        truncated: packed.truncated,
        next_cursor: packed.next_cursor,
        served_at: now_rfc3339(),
    })
}

/// The rung scope for a resolution before the version policy narrows it.
fn scope_for(resolved: &crate::resolver::Resolved) -> Scope<'_> {
    match resolved.library.as_deref() {
        Some(library) => Scope::Library(library),
        None => Scope::All,
    }
}

/// The highest version strictly below the served one — the migration
/// rung's second target.
fn nearest_older(available: &[Version], served: &Version) -> Option<Version> {
    available
        .iter()
        .filter(|v| v.cmp_version(served) == std::cmp::Ordering::Less)
        .max_by(|a, b| a.cmp_version(b))
        .cloned()
}

/// Opens one chunk by `docs://` ref, bounded by the token budget
/// (docs/08 §CLI contract). The envelope carries the served page; when
/// the chunk exceeds the budget, the rest continues through `next_cursor`
/// — the chunk-at-open-time rule.
///
/// # Errors
/// [`RetrieveError::Usage`] for a malformed ref or cursor, [`RetrieveError::Miss`]
/// on the cold-miss paths, [`RetrieveError::Store`] for store failures.
pub fn open(store: &Store, doc_ref: &str, plan: &Plan) -> Result<ResultEnvelope, RetrieveError> {
    let parsed = devdocs_model::docref::ParsedRef::parse(doc_ref)
        .ok_or_else(|| UsageError::BadRef(doc_ref.to_owned()))?;
    let chunk = resolve_chunk(store, &parsed)?;

    // Page the chunk text through the budget. The per-char token cost
    // scales from the stored token_count, so the estimate is stable for
    // identical DB state. The first line always serves — an empty page
    // would serve nothing and lie about the budget.
    let total_tokens = u64::from(chunk.token_count).max(1);
    let total_chars = chunk.text.chars().count();
    let start = usize::try_from(plan.cursor.unwrap_or(0))
        .unwrap_or(0)
        .min(total_chars);
    let rest = &chunk.text[start..];
    let rest_chars = rest.chars().count();
    let mut page = String::new();
    let mut used: u64 = 0;
    for line in rest.split_inclusive('\n') {
        let cost = (u64::try_from(line.chars().count()).unwrap_or(u64::MAX))
            .saturating_mul(total_tokens)
            .saturating_div(u64::try_from(total_chars).unwrap_or(u64::MAX).max(1))
            .max(1);
        if !page.is_empty() && used + cost > plan.max_tokens {
            break;
        }
        page.push_str(line);
        used += cost;
    }
    let served_all = page.chars().count() >= rest_chars;
    let truncated = !served_all;
    let next_cursor = if truncated {
        Some(
            u64::try_from(start + page.chars().count())
                .unwrap_or(u64::MAX)
                .to_string(),
        )
    } else {
        None
    };

    let item = served_item(&chunk, doc_ref, Some(&parsed.version), page, used, 1.0);

    Ok(ResultEnvelope {
        results: vec![item],
        truncated,
        next_cursor,
        served_at: now_rfc3339(),
    })
}

/// Resolves the active source for a parsed ref's `(library, version)`
/// pair, distinguishing the two cold-miss reasons.
fn resolve_source(store: &Store, library: &str, version: &str) -> Result<i64, RetrieveError> {
    if store.library_versions(library)?.is_empty() {
        return Err(MissReason::NotIndexed.into());
    }
    store
        .find_source(library, version)?
        .ok_or_else(|| MissReason::VersionAbsent.into())
}

/// Resolves a parsed ref to the chunk that serves it: the ref's active
/// source ([`resolve_source`], keeping its cold-miss mapping), that
/// source's chunks, and the ref match
/// ([`MissReason::LowConfidence`] when nothing matches).
fn resolve_chunk(
    store: &Store,
    parsed: &devdocs_model::docref::ParsedRef,
) -> Result<Hit, RetrieveError> {
    let source_id = resolve_source(store, &parsed.library, &parsed.version)?;
    let chunks = store.source_chunks(source_id)?;
    let chunk = match_ref(&chunks, parsed).ok_or(MissReason::LowConfidence)?;
    Ok(chunk.clone())
}

/// Builds the envelope item both ref-serving paths return. `open` and
/// `verify` each serve an exact pinned ref version, so the status is
/// always [`VersionStatus::Verified`] and the confidence full; `snippet`,
/// `token_count`, and `confidence` come from the caller because `open`
/// serves a budgeted page while `verify` serves the stored chunk.
fn served_item(
    chunk: &Hit,
    doc_ref: &str,
    requested_version: Option<&str>,
    snippet: String,
    token_count: u64,
    confidence: f64,
) -> ResultItem {
    ResultItem {
        doc_ref: doc_ref.to_owned(),
        title: chunk
            .section_path
            .last()
            .cloned()
            .unwrap_or_else(|| chunk.library.clone()),
        section: chunk.section_path.join(" > "),
        snippet,
        examples: Vec::new(),
        source_url: chunk.source_url.clone(),
        source_commit: chunk.source_commit.clone().unwrap_or_default(),
        requested_version: requested_version.map(str::to_owned),
        served_version: chunk.version.clone(),
        version_status: VersionStatus::Verified,
        content_hash: crate::item::hex(&chunk.content_hash),
        license_status: chunk.license_status.clone(),
        confidence,
        token_count,
    }
}

/// Re-derives the content hash of the chunk a ref names and compares it
/// to the stored value (docs/implementation/06 §Hashing policy). The hash
/// covers the canonical body — the stored text without the contextual
/// header (docs/implementation/07 §Normalization pipeline stage 4).
///
/// # Errors
/// [`RetrieveError::Usage`] for a malformed ref, [`RetrieveError::Miss`]
/// on the cold-miss paths, [`RetrieveError::HashMismatch`] when the index
/// drifted, [`RetrieveError::Store`] for store failures.
pub fn verify(store: &Store, doc_ref: &str) -> Result<ResultEnvelope, RetrieveError> {
    let parsed = devdocs_model::docref::ParsedRef::parse(doc_ref)
        .ok_or_else(|| UsageError::BadRef(doc_ref.to_owned()))?;
    let chunk = resolve_chunk(store, &parsed)?;

    // Reconstruct the ingest-time contextual header
    // (docs/implementation/07 §Normalization pipeline stage 4) and strip
    // it; the hash covers the body alone. A text without the expected
    // header is itself drift and reports the mismatch honestly.
    // defensive: a chunk with an empty section path has no title and no
    // symbol, so no parseable ref reaches it (the ref grammar requires a
    // path); this arm only keeps the header derivation total.
    let header = if chunk.section_path.is_empty() {
        format!("[{} {}]", chunk.library, chunk.version)
    } else {
        format!(
            "[{} {} › {}]",
            chunk.library,
            chunk.version,
            chunk.section_path.join(" › ")
        )
    };
    let body = chunk
        .text
        .strip_prefix(&format!("{header}\n\n"))
        .unwrap_or(chunk.text.as_str());
    let rederived = devdocs_store::content_hash(body);
    if rederived != chunk.content_hash {
        return Err(RetrieveError::HashMismatch(format!(
            "ref {doc_ref}: stored {} does not match the re-derived {}",
            crate::item::hex(&chunk.content_hash),
            crate::item::hex(&rederived)
        )));
    }

    let item = served_item(
        &chunk,
        doc_ref,
        Some(&parsed.version),
        chunk.snippet.clone().unwrap_or_else(|| chunk.text.clone()),
        u64::from(chunk.token_count),
        1.0,
    );

    Ok(ResultEnvelope {
        results: vec![item],
        truncated: false,
        next_cursor: None,
        served_at: now_rfc3339(),
    })
}

/// Lists the exact versions one library serves (docs/08 §CLI contract:
/// `devdocs versions <library> --json` is the per-library discovery
/// surface). Only active sources list — a withdrawn or replaced
/// generation is not an available version.
///
/// # Errors
/// [`RetrieveError::Miss`] with `not_indexed` when the library has no
/// active sources, [`RetrieveError::Store`] for store failures.
pub fn versions(
    store: &Store,
    library: &str,
) -> Result<ResultEnvelope<SourceSummary>, RetrieveError> {
    if store.library_versions(library)?.is_empty() {
        return Err(MissReason::NotIndexed.into());
    }
    let results: Vec<SourceSummary> = store
        .list_sources()?
        .into_iter()
        .filter(|s| s.active && s.library.eq_ignore_ascii_case(library))
        .collect();
    Ok(ResultEnvelope {
        results,
        truncated: false,
        next_cursor: None,
        served_at: now_rfc3339(),
    })
}
