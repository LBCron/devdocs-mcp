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
use crate::matchexpr::rung_match_expr;
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

/// The packed pipeline outcome: the hits in served order with their fusion
/// scores, the pack state, and the per-item status inputs.
#[derive(Debug, Clone)]
pub struct PackedSearch {
    /// The packed hits with their fusion scores, in served order.
    pub items: Vec<(Hit, f64)>,
    /// Whether the token budget cut the page.
    pub truncated: bool,
    /// The resume cursor, when the budget cut the page.
    pub next_cursor: Option<String>,
    /// The pick status the version policy applied.
    pub pick_status: VersionStatus,
    /// The version the pipeline served, when one was picked.
    pub served_version: Option<String>,
    /// The requested version text, when the query carried one.
    pub requested_text: Option<String>,
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
    search_with_context(store, query, &ctx, plan)
}

/// The search pipeline with a caller-supplied project context: the eval
/// harness passes an empty context so host lockfiles never leak into a
/// run, while the CLI keeps the lockfile walk.
///
/// # Errors
/// The same paths as [`search`].
pub fn search_with_context(
    store: &Store,
    query: &Query,
    ctx: &ProjectContext,
    plan: &Plan,
) -> Result<ResultEnvelope, RetrieveError> {
    let packed = search_packed(store, query, ctx, plan)?;
    let results: Vec<ResultItem> = packed
        .items
        .iter()
        .map(|(hit, score)| {
            let status = item_status(
                &hit.version,
                packed.served_version.as_deref(),
                packed.requested_text.as_deref(),
                packed.pick_status,
            );
            to_result_item(hit, packed.requested_text.as_deref(), status, *score)
        })
        .collect();
    Ok(ResultEnvelope {
        results,
        truncated: packed.truncated,
        next_cursor: packed.next_cursor,
        served_at: now_rfc3339(),
    })
}

/// The search pipeline through fusion, cursor resume, and the budget pack,
/// returning the packed hits instead of the envelope — callers that grade
/// raw hits (the eval harness) skip the envelope mapping.
///
/// # Errors
/// The same paths as [`search`].
pub fn search_packed(
    store: &Store,
    query: &Query,
    ctx: &ProjectContext,
    plan: &Plan,
) -> Result<PackedSearch, RetrieveError> {
    let resolved = resolve(query, ctx);

    // Plan the rungs from the resolution. The headroom is 4× the requested
    // hits (docs/08 §Rungs, rungs 1 and 2).
    let headroom = plan.limit.saturating_mul(4).max(1);
    let mut rung_hits: Vec<Vec<Hit>> = vec![symbol_rung(
        store,
        &query.text,
        scope_for(&resolved),
        headroom,
    )?];
    let (pick_status, served_version, requested_text) =
        version_rung_hits(store, query, &resolved, headroom, &mut rung_hits)?;

    // Fuse the rungs and pack under the count cap and the token budget.
    let fused = fuse(plan.rrf_k, &rung_hits);
    if fused.is_empty() {
        return Err(empty_fusion_miss(store)?);
    }
    let fused = resume_after_cursor(fused, plan.cursor)?;

    let packed = pack(
        fused,
        usize::try_from(plan.limit).unwrap_or(usize::MAX),
        plan.max_tokens,
    );
    Ok(PackedSearch {
        items: packed.items,
        truncated: packed.truncated,
        next_cursor: packed.next_cursor,
        pick_status,
        served_version,
        requested_text,
    })
}

/// The exact symbol lookup (docs/08 §CLI contract: `devdocs symbol
/// <library@version> <symbol>`). The target resolves first — a library the
/// index lacks is [`MissReason::NotIndexed`], a version it lacks is
/// [`MissReason::VersionAbsent`] — then the symbol rung runs alone through
/// the same fusion-and-pack path (docs/implementation/08 §Retrieval ladder:
/// cheap, decisive). A pinned version scopes the rung to that version's
/// sources; without one the rung keeps the library scope. A resolved
/// target whose lookup serves nothing is the [`MissReason::SymbolAbsent`]
/// miss (docs/implementation/13 §M5).
///
/// # Errors
/// [`RetrieveError::Miss`] on the miss paths (exit 2) and
/// [`RetrieveError::Store`] for store failures.
pub fn symbol(
    store: &Store,
    library: &str,
    version: Option<&str>,
    symbol: &str,
    plan: &Plan,
) -> Result<ResultEnvelope, RetrieveError> {
    if store.library_versions(library)?.is_empty() {
        return Err(MissReason::NotIndexed.into());
    }
    let headroom = plan.limit.saturating_mul(4).max(1);
    let (rung_hits, served_version) = match version {
        Some(version) => {
            let source_ids = store.find_sources(library, version)?;
            if source_ids.is_empty() {
                return Err(MissReason::VersionAbsent.into());
            }
            let rungs = source_ids
                .iter()
                .map(|id| symbol_rung(store, symbol, Scope::Source(*id), headroom))
                .collect::<Result<Vec<_>, _>>()?;
            (rungs, Some(version.to_owned()))
        }
        None => (
            vec![symbol_rung(
                store,
                symbol,
                Scope::Library(library),
                headroom,
            )?],
            None,
        ),
    };
    // A single rung fuses as identity; several pinned sources rank their
    // best hits equally, ties by chunk id.
    let fused = fuse(plan.rrf_k, &rung_hits);
    if fused.is_empty() {
        return Err(MissReason::SymbolAbsent.into());
    }
    let pick_status = if served_version.is_some() {
        VersionStatus::Verified
    } else {
        VersionStatus::Alias
    };
    let packed = pack(
        fused,
        usize::try_from(plan.limit).unwrap_or(usize::MAX),
        plan.max_tokens,
    );
    let results: Vec<ResultItem> = packed
        .items
        .iter()
        .map(|(hit, score)| {
            let status = item_status(
                &hit.version,
                served_version.as_deref(),
                version,
                pick_status,
            );
            to_result_item(hit, version, status, *score)
        })
        .collect();
    Ok(ResultEnvelope {
        results,
        truncated: packed.truncated,
        next_cursor: packed.next_cursor,
        served_at: now_rfc3339(),
    })
}

/// The fallback rungs (docs/08 §Version resolution, step 4): search every
/// approved source. A version that the index carries narrows the fallback
/// to it — the flags carry top precedence (docs/08 §Version resolution,
/// step 1). An absent version follows the fallback ladder instead: serve
/// the nearest available and label it (docs/08 §Version policy). Per-item
/// status stays the alias default; an exact-request hit verifies through
/// `item_status`.
///
/// # Errors
/// [`RetrieveError::Store`] for store failures.
fn fallback_rung_hits(
    store: &Store,
    query: &Query,
    version: Option<&str>,
    headroom: u32,
    rung_hits: &mut Vec<Vec<Hit>>,
) -> Result<(VersionStatus, Option<String>, Option<String>), RetrieveError> {
    match version {
        Some(version) => {
            let expr = rung_match_expr(&query.text);
            let scoped = store.search_fts_version(&expr, version, headroom)?;
            if scoped.is_empty() {
                rung_hits.push(fts_rung(store, &query.text, Scope::All, headroom)?);
            } else {
                rung_hits.push(scoped);
            }
        }
        None => rung_hits.push(fts_rung(store, &query.text, Scope::All, headroom)?),
    }
    Ok((VersionStatus::Alias, None, version.map(str::to_owned)))
}

/// The version-resolution rungs (docs/08 §Version resolution): with a
/// resolved library, the served source's FTS rung plus the
/// migration-question rung via [`nearest_older`]; without one, the
/// fallback rungs via [`fallback_rung_hits`]. Appends the rungs to
/// `rung_hits` and returns the per-item status inputs — the pick status,
/// the served version, and the requested version.
///
/// # Errors
/// [`RetrieveError::Miss`] with `not_indexed` when the library has no
/// sources or the pick serves none, [`RetrieveError::Store`] for store
/// failures.
fn version_rung_hits(
    store: &Store,
    query: &Query,
    resolved: &crate::resolver::Resolved,
    headroom: u32,
    rung_hits: &mut Vec<Vec<Hit>>,
) -> Result<(VersionStatus, Option<String>, Option<String>), RetrieveError> {
    let Some(library) = &resolved.library else {
        // Without a library, search every approved source (docs/08
        // §Version resolution, step 4).
        return fallback_rung_hits(
            store,
            query,
            resolved.version.as_deref(),
            headroom,
            rung_hits,
        );
    };
    let available: Vec<Version> = store
        .library_versions(library)?
        .into_iter()
        .map(Version::from)
        .collect();
    if available.is_empty() {
        if query.library.is_none() {
            // The library came from a lockfile or a query mention — a
            // weak hint (docs/08 §Version resolution, steps 2 and 3). An
            // absent weak-hint library degrades to the fallback (step 4);
            // only a flag-named library is a hard miss. A version rides
            // along only from the flags (step 1), never from the weak
            // hint that named the absent library.
            return fallback_rung_hits(store, query, query.version.as_deref(), headroom, rung_hits);
        }
        return Err(MissReason::NotIndexed.into());
    }
    let pick = pick(&available, resolved.version.as_deref());
    let served = pick.version.clone().ok_or(MissReason::NotIndexed)?;
    // The pinned scope serves every active origin of the
    // `(library, version)` pair; the schema allows several per pair.
    let source_ids = store.find_sources(library, served.as_str())?;
    if source_ids.is_empty() {
        return Err(MissReason::NotIndexed.into());
    }
    let expr = rung_match_expr(&query.text);
    rung_hits.push(store.search_fts_sources(&expr, &source_ids, headroom)?);
    if let Some(migration) = migration_rung(store, query, library, &served, &available, headroom)? {
        rung_hits.push(migration);
    }
    Ok((
        pick.status,
        Some(served.as_str().to_owned()),
        resolved.version.clone(),
    ))
}

/// The migration-question rung: retrieve the served version and the
/// nearest older version, both labeled through their own `served_version`
/// (docs/08 §Version resolution). `None` when the query is not a
/// migration question or no older version is indexed.
///
/// # Errors
/// [`RetrieveError::Store`] for store failures.
fn migration_rung(
    store: &Store,
    query: &Query,
    library: &str,
    served: &Version,
    available: &[Version],
    headroom: u32,
) -> Result<Option<Vec<Hit>>, RetrieveError> {
    if !is_migration_query(&query.text) {
        return Ok(None);
    }
    let Some(older) = nearest_older(available, served) else {
        return Ok(None);
    };
    let Some(older_id) = store.find_source(library, older.as_str())? else {
        return Ok(None);
    };
    Ok(Some(fts_rung(
        store,
        &query.text,
        Scope::Source(older_id),
        headroom,
    )?))
}

/// The miss reason for a valid query that serves nothing (exit 2): the
/// reason distinguishes an empty index from a query that simply matched
/// nothing (docs/08 §Error codes). The store failure surfaces — the miss
/// classification must not swallow it (`.agents/instructions/`
/// error-handling: no swallowed errors).
///
/// # Errors
/// [`RetrieveError::Store`] when the source listing fails.
fn empty_fusion_miss(store: &Store) -> Result<RetrieveError, RetrieveError> {
    let has_active = store.list_sources()?.iter().any(|s| s.active);
    Ok(if has_active {
        MissReason::LowConfidence.into()
    } else {
        MissReason::NotIndexed.into()
    })
}

/// Cursor resume: continue after the last served chunk id in the same
/// fusion order. No cursor serves the fusion from the top.
///
/// # Errors
/// [`RetrieveError::Usage`] for a cursor that names no served chunk.
fn resume_after_cursor(
    fused: Vec<(Hit, f64)>,
    cursor: Option<u64>,
) -> Result<Vec<(Hit, f64)>, RetrieveError> {
    let Some(cursor) = cursor else {
        return Ok(fused);
    };
    let position = fused
        .iter()
        .position(|(hit, _)| hit.chunk_id == i64::try_from(cursor).unwrap_or(-1))
        .ok_or_else(|| UsageError::UnknownCursor(cursor.to_string()))?;
    Ok(fused[position + 1..].to_vec())
}

/// Per-item status: the picked version carries the request's status.
/// Without a picked version (the unfiltered fallback), an exact-request
/// hit verifies and everything else is the alias default — or a near
/// version served.
fn item_status(
    hit_version: &str,
    served_version: Option<&str>,
    requested_text: Option<&str>,
    pick_status: VersionStatus,
) -> VersionStatus {
    match (served_version, requested_text) {
        (Some(served), _) if hit_version == served => pick_status,
        (None, Some(requested)) if hit_version == requested => VersionStatus::Verified,
        (None, None) => VersionStatus::Alias,
        // A near version served, or an exact request on the
        // unfiltered path that another version answered.
        _ => VersionStatus::Fallback,
    }
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
    // The cursor is a char offset (docs/08 §CLI contract: pagination), so
    // it maps to a byte offset before slicing — `nth` walks to the char
    // at `start`, and past the end it maps to the full length.
    let start_byte = chunk
        .text
        .char_indices()
        .nth(start)
        .map_or(chunk.text.len(), |(byte, _)| byte);
    let rest = &chunk.text[start_byte..];
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
    // defensive: a chunk with an empty section path is reachable — the
    // placeholder ref `docs://<library>/v<version>/index` serves it — so
    // this arm derives the headerless shape and keeps the derivation
    // total.
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
