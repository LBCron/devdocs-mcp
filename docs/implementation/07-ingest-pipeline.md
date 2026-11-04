---
type: guide
title: Ingest Pipeline (L1 + L2)
description: The source adapters, the fetch guard, the license gate, the normalization and segmentation pipeline, dedup, the injection filter, and the refresh policy for devdocs.
tags: [devdocs, ingest, llms-txt, git, openapi, html, segmentation]
timestamp: 2026-11-04T00:00:00Z
resource: docs
derives-from: [docs/research]
---

# Ingest Pipeline (L1 + L2)

## Priority ladder

The adapter picker tries source types in this order (research: architecture.md §L1):

1. `llms.txt` / `llms-full.txt` at the docs root. Publisher-sanctioned. Markdown or linkable pages.
2. Git repository clone pinned to a tag or commit. Version-exact and reproducible.
3. OpenAPI specification. Structured, high-signal for API surface.
4. HTML crawl. Last resort only, gated harder than the others.

The user can force a kind with `--kind llms_txt|git|openapi|html`. Auto-detection order: an explicit `--kind` wins. Then the config `[sources]` entry. Then probes in ladder order. A probe is a bounded HEAD or GET with the fetch guard active.

## Adapter trait

All adapters implement one trait in `devdocs-ingest`:

```rust
pub trait IngestAdapter {
    fn kind(&self) -> SourceKind;
    /// Probe the source and produce an ingest plan without fetching bodies.
    fn plan(&self, ctx: &IngestContext) -> Result<IngestPlan, IngestError>;
    /// Fetch raw documents per the plan. Emits progress events.
    fn fetch(&self, ctx: &IngestContext, plan: IngestPlan) -> Result<Vec<RawDocument>, IngestError>;
}

pub struct RawDocument {
    pub path: String,        // logical path, e.g. "docs/proxy.mdx"
    pub body: DocumentBody,  // Text(Markdown|Html|Json|Yaml) with media hints
    pub provenance: Provenance,
}

pub struct Provenance {
    pub origin_url: String,
    pub commit_sha: Option<String>,
    pub fetched_at: OffsetDateTime,
    pub etag: Option<String>,
}
```

`fetch` streams through the fetch guard. It never opens a connection outside the guard.

## llms.txt adapter (v2)

The spec is llmstxt.org v2 (verified 2026-10-28. Updated August 2026). Parse rules:

1. Fetch `/llms.txt` at the docs root. Parse the H1 (library title), the blockquote (summary), and the H2 sections of links. The H1 must carry a title: an empty one fails the parse as `LLMS_TXT_INVALID`.
2. Each link line is `- [title](url): description`. The optional parenthesized info carries per-page hints. V2 link relations (`rel="alternate" type="text/markdown"`, `rel="describedby"`) mark markdown twins. Prefer a `.md` twin over its HTML page.
3. If `/llms-full.txt` exists, fetch it and treat it as one pre-built export. The research bundle treats llms-full as the bulk feed (research: interfaces.md §6). It is an unofficial convention, so detection is a probe, not an assumption.
4. v2 subpath scoping: a more specific `llms.txt` deeper in the tree applies to that subtree. The plan walks links breadth-first and uses the most specific index found per path.
5. Quality gate: an index with zero links (the documented ADK failure, research: interfaces.md §6) fails the plan with `LLMS_TXT_EMPTY` and the picker falls to the next rung.

Implemented shape (M3b): the rules above land as follows.

- **Page shapes.** An index link is a page when its path ends in `.md`, `.mdx`, `.html`, `.htm`, `.xhtml`, or carries no extension at all (directory-style URLs). `llms.txt` and `llms-full.txt` are never pages.
- **Link relations.** The relations arrive three ways: the parenthesized per-page hints of a link line (`- [Page](url) (rel="alternate" type="text/markdown" twin-url): notes`), the page response's `Link:` headers (an RFC 8288 subset), and `<link>` elements scanned from an HTML body. A response relation wins over the index hint.
- **Twin preference.** An HTML page with a markdown twin ingests the twin (a markdown-twin event counts it). A page without a twin falls back to its HTML path; the media split skips it, because no HTML extractor exists in this slice. The same page never buffers twice: routes and twins dedup by URL, and the corpus dedup collapses equal bodies.
- **Subpath scoping.** The walk is a FIFO queue of scopes: a link naming an `llms.txt` opens a deeper scope, a page's `rel="describedby"` response can open one, and the queue drains breadth-first. A page ingests under the scope with the longest directory path prefix — the most specific index wins; a page listed by a shallower scope defers to the deeper scope's turn. Each scope joins its links against the redirect-aware base its index response came from.
- **llms-full probe.** Every scope probes `llms-full.txt` at its directory. Absence is an expected outcome: a 404 or 410 on the wire, a missing replay response, or a non-UTF-8 body all read as "not there"; every other fetch error still fails the run and the guard policy still applies. When present, the export is one pre-built document that joins the corpus and is segmented like any other markdown body.
- **Ingest events.** The run reports pages, skipped pages, skipped blocks (chrome pattern drops plus content-less blocks), unknown fence languages (a named language outside the known set; unnamed fences never count), chrome drops (frequency-rule drops), dedup dropped pages, markdown twins used, subpath indexes, and llms-full exports. The counts surface as stderr log lines under `--verbose` and as the `events` object of the `--json` add envelope; they carry provenance only, never chunk text.

## Git adapter

Clone and checkout shell out to the system `git` binary. This matches cargo, uv, and gh (verified 2026-10-28). Rules:

- Never pass arguments through a shell. Build an argv vector. No string interpolation into a command line.
- Set `GIT_TERMINAL_PROMPT=0` so private repos fail fast instead of hanging.
- Set `GIT_CONFIG_NOSYSTEM=1`. Do not read the user's global git config for clones, except credential helpers, which the default config provides.
- Tag pin: `git clone --depth 1 --branch <tag> --single-branch <url> <dir>`.
- Commit pin: shallow clone the default branch, then `git fetch --depth 1 origin <sha>` and `git checkout --detach <sha>`. If the server rejects a reachable-SHA fetch, fall back to a full clone. Record the fallback in the ingest event log.
- Extract docs content with the `ignore` crate walk (respects `.gitignore`) filtered by an extension allowlist: `md, mdx, rst, adoc, txt, json, yaml, yml`. The config can extend the list.
- Record `commit_sha` from `git rev-parse HEAD` in the temp clone. It becomes the provenance commit for every chunk of that source.

## OpenAPI adapter

- Parse with the `oas3` crate (0.22.0, verified active 2026-10-28).
- Emit one chunk per operation: method + path as the symbol (`GET /v1/files`), summary as the title, description, parameters table, and the first request/response example. The API-specification-alone evidence supports signature-first chunks (research: retrieval-design.md §1, CloudAPIBench ablation).
- Emit one chunk per schema object with its properties. Symbols take the schema name.
- `$ref` cycles produce a warning and a skipped chunk, never a hang.

## HTML adapter (last resort)

- Pre-strip with `lol_html` (3.0.2): remove `script`, `style`, `noscript`, `iframe`, `svg`, nav and footer containers by common selectors.
- Extract main content with `dom_smoothie` (0.19.0) in Markdown output mode. On extraction failure or empty output, fall back to `rust-trafilatura` (2.2.8). Record which extractor produced each chunk.
- Check robots.txt before crawling more than one page of a host. Parse it with a minimal internal parser (the `texting_robots` crate is dormant since 2023, verified 2026-10-28. Vendor its grammar or implement the small subset: user-agent groups, allow, disallow, crawl-delay, sitemap). A disallow for the user agent blocks the source at the license gate level with status `denied`.
- Crawl depth defaults to 2, page cap 200, same-host only. These are config floors. A source may lower them.

## Fetch guard

The fetch guard is the only network code in the tool. Design from the research egress discipline (research: security-licensing.md §2 item 10). Full invariants and tests live in [09](09-security-implementation.md). Summary:

1. HTTPS only. Reject other schemes, userinfo in URLs, and non-HTTP(S) ports.
2. Resolve DNS, then connect only to the resolved IP. Re-check the target IP against the blocklist at connect time, for every redirect hop.
3. Blocklist: private, loopback, link-local, multicast, unspecified, documentation ranges, and cloud metadata endpoints.
4. Redirect limit 3 (config `max_redirects`). Each hop re-validates the scheme, host resolution, and IP.
5. Response cap 20 MiB (config `max_bytes`), enforced on decompressed bytes. Timeout 15 s per request (config `timeout_ms`).
6. User agent from config. Identify honestly as `devdocs/VERSION`.

Implementation: reqwest 0.13 with `dns_resolver(Arc<dyn Resolve>)`. The custom resolver returns one pinned address per lookup, and a connect-time hook re-validates it. reqwest 0.13 keeps this hook (verified 2026-10-28: `dns_resolver<R: IntoResolve>`). The crypto default is aws-lc-rs under rustls, so no OpenSSL link.

## License gate

The gate runs before any fetch, embed, or serve decision for a source (research: security-licensing.md §3). The registry lives in the config `[sources]` table. Deny by default: a source with no entry is `pending_review` and cannot fetch. The library name a run publishes under is the registry entry: `IngestJob::run` refuses a library argument that does not match the grant's `[sources.<name>]` key.

State machine:

```text
pending_review → approved_fetch → approved_index → approved_embed
               → approved_cache → approved_snippet
terminal: denied | expired | revoked
```

| Transition | Requires |
|---|---|
| pending_review → approved_fetch | Publisher identity recorded, license recorded (SPDX or `unknown`), robots and ToS note recorded |
| approved_fetch → approved_index | A human confirms indexing rights for the recorded license |
| approved_index → approved_embed | Rights for derivative embeddings (usually same as index) |
| any → denied | Operator decision, robots disallow, or ToS conflict |
| any → expired / revoked | Takedown contact fires, or the recorded expiry passes |

Runtime consequences:

- `approved_snippet` or below never emits a full-document render. Only spans within the token budget.
- Revoked or expired sources serve `not_available` and drop from search results at the next index update.
- Attribution: sources whose license requires it carry the attribution line in CLI `open_doc` output and in the MCP result metadata.

`devdocs source approve <library> <stage>` moves the state and stamps the operator and date. Every transition appends to `meta` as an audit line: `(audit:<ts>, library, from, to, operator)`.

## Normalization pipeline

Order matters. Each stage is a pure function over `RawDocument` streams, so snapshots test it deterministically.

1. **Media split.** JSON/YAML documents route to the OpenAPI-shaped path. Markdown routes to segmentation. HTML routes through the HTML extractor first, which emits Markdown. In this slice the HTML extractor does not exist yet: the ingest loop skips a link whose body starts with a doctype or an `html` tag (case-insensitive, after leading whitespace and a leading BOM), and counts it as skipped. This check looks only at the start of the body. A body that starts with an HTML comment or an XML declaration is not skipped, and HTML inside markdown is not removed, so markup can reach the chunks.
2. **Chrome strip.** For HTML: the adapter extracts main content before this stage. For Markdown: strip leading site navigation blocks, "edit this page" links, cookie banners, and repeated boilerplate detected by exact-match frequency across the source (a block identical in more than 60% of pages is chrome). Implemented (M3b): the corpus buffers first, so the strip runs over the whole source; the pattern rules match link-only blocks before the first heading (leading navigation), short edit-link phrasing, and short cookie mentions; the frequency rule counts distinct pages, not occurrences, and needs at least two pages. A dropped block collapses with the whitespace runs around it, so kept bytes survive byte-exactly and the chunks reassemble the stripped body. Pattern drops count as skipped blocks, frequency drops as chrome drops.
3. **Segmentation.** Algorithm below.
4. **Contextual headers.** Prepend the breadcrumb and version to every chunk body: `[{library} {version} › {section path}]` (research: architecture.md §L2. Measured effect −49% retrieval failures, −67% with reranking).
5. **Symbol extraction.** Tree-sitter pass over fenced code blocks (per grammar) plus structured sources (OpenAPI operations, front-matter routes). Populates `chunks.symbol` and `symbols`.
6. **Dedup.** Exact by SHA-256 across the corpus. Near-dup (cosine over embeddings) is phase 3, feature-gated. Implemented (M3b): the dedup runs at page granularity before the chrome strip, so a duplicate page cannot skew the frequency counts; the hash is the store's canonical hash (CRLF folds to LF), and the dropped count lands in the ingest events.
7. **Injection filter.** Rules below. Quarantine, never silent edit: a flagged chunk is stored with a quarantine flag and excluded from serving until reviewed.
8. **Publish.** Atomic write, below.

## Segmentation algorithm

Goal: chunk shape with heading, breadcrumb, signature, example, and explanation together. Never split a code block (research: retrieval-design.md §3.2).

1. Parse the Markdown with comrak (0.59.0) and read `sourcepos` for every block node. comrak's code-block source positions are reliable at 0.59 (verified 2026-10-28, sourcepos fixes landed through 0.50).
2. Walk the block tree. Build sections at heading boundaries (ATX headings). A section keeps its nested content and its breadcrumb path.
3. Inside a section, pack blocks into chunks up to the 400-token target (separator bytes between packed blocks join the budget). A block between the target and 2× the target splits with text-splitter (`MarkdownSplitter`, overlap 80 tokens — the research reference engine shape, research: retrieval-design.md §3.2). The splitter treats code blocks as atomic units, so it never splits them; a fenced block is never handed to the splitter at all. Section boundaries and packed block boundaries carry no overlap: both sides of the boundary stay whole, so no content is cut.
4. Cross-check with sourcepos: if a chunk boundary would fall inside a fenced block (the splitter guarantees it does not, but the check catches regressions), merge the two candidate chunks and log a warning.
5. A single block larger than 2× the target becomes its own chunk, never truncated.
6. Token counts use the chars-per-4 heuristic by default. With feature `exact-tokens`, tiktoken o200k. The count is stored per chunk. Budget enforcement uses stored counts and never recounts at query time.

## Injection filter (v0)

Scope: flag, quarantine, and report. Do not attempt language-model detection in v0. The rules are cheap and auditable (research: security-licensing.md §2 item 3).

| Rule | Action |
|---|---|
| Directives addressed to a model ("ignore previous instructions", "disregard your rules", "system prompt") in text outside code blocks | quarantine |
| Instructions that request action ("visit", "download", "run", "curl this", exfiltration verbs) outside code blocks | quarantine |
| Hidden text: HTML comments larger than 2k chars, zero-width-heavy spans, text-colored-on-text patterns (pre-strip leftovers) | quarantine |
| A fenced code block whose info string or first line is a shell command that writes outside the workspace (`curl | sh`, `rm -rf`, `git push --force`) | keep, annotate result with `code_caution: true` |
| Link-to-text ratio above 0.5 in a chunk | flag for review, serve with `confidence` penalty |

Quarantined chunks are visible with `devdocs db quarantine list` and can be released by an operator flag. The tool never serves a quarantined chunk.

## Atomic publish

An ingest run writes nothing visible until it succeeds:

1. Compute all chunks in memory (or spill to `tmp/` for very large sources). A run that segments to zero chunks fails closed before this step: it never replaces the active generation with an empty one.
2. In one SQLite transaction: deactivate the old chunk set for the source, insert the new chunks, symbols, and FTS rows (triggers fire), update the source row (`fetched_at`, `commit_sha`, `active = 1`).
3. On any error: rollback. The old generation stays active and queryable.
4. Vacuum is manual: `devdocs db vacuum` after large replacements.

## Refresh policy

Velocity-stratified, config-driven (research: architecture.md §L1). Defaults:

| Source class | refresh_days default | Trigger |
|---|---|---|
| Core stack libraries (in project lockfiles) | 2 | scheduled + on-demand (`devdocs sync`) |
| Actively queried, not in lockfiles | 7 | scheduled |
| Everything else | 30 | on demand only |

- `devdocs sync` checks all approved sources against `refresh_days`, respects ETag/If-None-Match for HTTP sources, and skips anything unchanged by hash.
- Never refresh a source whose license state is below `approved_fetch`. Expired or revoked states block sync and emit `not_available` at query time.
- Every response carries `fetched_at`, so staleness is visible at answer time (research: architecture.md, risk table).
