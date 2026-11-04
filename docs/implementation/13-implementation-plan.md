---
type: guide
title: Implementation Plan
description: The step-by-step build order for devdocs with concrete commands, module sketches, and function signatures, plus the risk register and the do-not-build list.
tags: [devdocs, implementation, plan, risks]
timestamp: 2026-11-04T00:00:00Z
resource: docs
derives-from: [docs/research]
---

# Implementation Plan

This file is the build order for milestones M0 to M12 ([12](12-roadmap-milestones.md)). Each step lists the work, the key signatures, and the tests to write. Sketches are binding for names and shape, not for every line.

## M0 — Scaffold (1 day)

1. Run the commands in [02](02-project-scaffolding.md). Create every config file from that file verbatim.
2. Add the workspace `Cargo.toml` and the member templates. Build once: `cargo build --workspace`.
3. Create `ci.yml` from [11](11-ci-cd-release.md). Push. Verify CI green on the empty workspace.
4. Commit. `lefthook install`.

Done when: CI green, `just ci` passes locally.

## M1 — CLI skeleton (2 days)

Work in `crates/devdocs/src` and `devdocs-model`.

1. Error model first:

```rust
// devdocs-model/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("usage: {0}")]
    Usage(String),
    #[error("no result: {reason:?}")]
    NoResult { reason: MissReason },
    #[error("upstream: {0}")]
    Upstream(#[from] UpstreamError),
    #[error("budget exceeded")]
    BudgetExceeded,
}
```

2. The exit-code mapper is a table test: each `Error` variant maps to 0/1/2/3/4.
3. clap enum with all commands from [08](08-retrieval-interfaces.md), most returning `todo!()` placeholders that exit 0 and print the envelope shape. The `todo` lint warns. Acceptable for one milestone.
4. clap usage errors: catch the parse error, print `{"error":"USAGE",...}` on stderr, exit 1. This implements the remap decision in [05](05-architecture.md).
5. Config load: precedence chain from [05](05-architecture.md). `config show` prints merged JSON. Add the schema struct with serde defaults so unknown keys fail validation.

Tests: one assert_cmd test per exit code. Config precedence test with temp dirs. Snapshot of `config show`.

## M2 — Store layer (2 days)

Work in `devdocs-store`.

1. Migration runner ([06](06-data-model.md)):

```rust
pub fn migrate(conn: &mut Connection) -> Result<Vec<AppliedMigration>, StoreError>;
pub struct Store { /* Mutex<Connection> */ }
impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError>;          // pragmas + mode checks
    pub fn publish_generation(&self, src: &SourceRecord, chunks: &[NewChunk]) -> Result<(), StoreError>;
    pub fn search_fts(&self, match_expr: &str, limit: u32) -> Result<Vec<Hit>, StoreError>;
    pub fn lookup_symbol(&self, symbol: &str, src: SourceId) -> Result<Vec<Hit>, StoreError>;
}
```

2. `publish_generation` implements the atomic swap from [07](07-ingest-pipeline.md) inside one transaction.
3. `search_fts` builds and binds the MATCH expression. The query builder is a separate pure function so proptest can hammer it.

Tests: migration up/down on a temp file. FTS5 bm25-weight and snippet behavior. Publish-then-search round trip. Concurrent open with WAL.

## M3 — llms.txt ingest end-to-end (4 days)

Work in `devdocs-ingest`.

1. Fetch guard first, before any adapter. It is the foundation ([09](09-security-implementation.md)):

```rust
pub struct FetchGuard { /* config */ }
impl FetchGuard {
    pub fn client(&self) -> Result<reqwest::Client, GuardError>;
    pub fn check(&self, url: &Url) -> Result<(), GuardError>;
    pub async fn get(&self, url: &Url) -> Result<Bytes, GuardError>;  // caps + timeouts
}
```

The custom `Resolve` impl returns one pinned address. A connect-time validator re-checks the IP. wiremock tests cover: redirect chains, blocklist hits, byte caps, timeouts.

2. License gate: parse `[sources]`, state machine transitions, deny-by-default error.
3. llms.txt v2 parser: H1, blockquote, H2 sections, link lines, `rel="alternate"` markdown preference, subpath scoping. Feed the three fixture shapes from [04](04-testing-strategy.md).
4. Segmentation: comrak parse, heading walk, text-splitter packing, sourcepos cross-check, contextual headers.
5. Wire `devdocs add <llms.txt url>` end to end behind a `--fixture` flag that replays recorded responses instead of network (keeps CI offline). Real network use stays manual until M6.

Tests: property tests for the guard. Segmentation snapshots. `LLMS_TXT_EMPTY` fallback. Publish atomicity under a forced failure.

The M3b hardening completes the milestone: v2 link relations (index hints, `Link:` headers, HTML `<link>` elements) with markdown-twin preference and an HTML-path fallback, breadth-first subpath scoping where the most specific index governs, the `llms-full.txt` probe with one pre-built export per scope, the markdown chrome strip (pattern rules plus the 60% frequency rule), corpus-wide SHA-256 page dedup, the ingest event report under `--verbose` and `--json`, and parse snapshots over the three fixture shapes (docs/implementation/07 §llms.txt adapter, §Normalization pipeline).

## M4 — Search and result shape (3 days)

Work in `devdocs-retrieve` and the CLI.

1. Resolver: lockfile table from [08](08-retrieval-interfaces.md). Start with Cargo.lock, package-lock.json, pyproject/uv.lock, go.mod.

```rust
pub fn resolve(query: &Query, ctx: &ProjectContext) -> Resolved { /* library, version, status */ }
```

2. Planner and rungs: symbol rung returns empty for now. FTS5 rung live. Fusion with one rung is identity.
3. Budget packer with the properties from [08](08-retrieval-interfaces.md). `truncated` and `next_cursor` logic.
4. `devdocs search|open|verify|list|versions` fully live against the store.
5. Result envelope serialization with serde. Snapshot the exact JSON for one fixture query.

Tests: budget properties (proptest). Resolver precedence. Version-status policy (`verified`/`alias`/`fallback`/`absent`). CLI end-to-end JSON snapshots.

## M5 — Symbol index (3 days)

Work in `devdocs-ingest` (extraction) and `devdocs-retrieve` (rung).

1. Grammar registry with feature-gated grammars (rust, go, python, javascript. TS deferred — see [01](01-tech-stack-decision.md)).
2. Extraction pass: for each fenced code block, parse with the fence-info language, walk for definition nodes, emit `(symbol, kind, position)`. Store to `symbols`. Unknown or missing fence info: skip, count, report in ingest events.
3. Query-side normalization: strip call punctuation (`useRouter()` → `useRouter`), try exact, then case-sensitive prefix, then case-insensitive.
4. Wire the symbol rung into the planner with its rank list for fusion.

Tests: per-grammar extraction fixtures (a known sample file per language with the expected symbol list). Symbol recall measured on the pilot golden set (the first partial gate run).

## M6 — Git adapter (3 days)

1. `GitSource` implementing `IngestAdapter` with the argv rules from [07](07-ingest-pipeline.md).
2. Tag pin, commit pin with SHA-fetch fallback, rev-parse provenance.
3. Docs-only walk with the `ignore` crate and the extension allowlist.
4. `devdocs add https://github.com/OWNER/REPO --version v1.2.3` end to end.
5. Marked `#[ignore]` integration test against one small public repo, run manually.

Tests: fixture repos built by testkit (tag, branch, commit pins). Argv builder unit tests (no shell ever). Fallback path with a local bare repo that rejects SHA fetches.

## M7 — License gate full (2 days)

1. All four enforcement points from [09](09-security-implementation.md), each as a separate store/retrieve check with its own test.
2. `devdocs source approve|deny|list` with audit lines in `meta`.
3. Expiry and revocation: a source with `expired` disappears from results at query time.

Tests: transition table (every legal and illegal move). The revoke-then-query drop. Attribution emission for an attribution-required fixture.

## M8 — MCP adapter (2 days)

1. rmcp server in `devdocs-mcp` with the three tools and annotations from [08](08-retrieval-interfaces.md).
2. Contract tests: in-process rmcp client calls the server. Results must equal the CLI results for the same queries against the same DB (the identical-results gate).
3. `devdocs mcp --transport stdio` runs the server. Test with a spawned process speaking JSON-RPC over stdio.

## M9 — Packaging (2 days)

1. `release.yml` and `publish.yml` from [11](11-ci-cd-release.md). Trusted publishing configured on crates.io.
2. README and CONTRIBUTING with the PR checklist from [03](03-code-quality-toolchain.md).
3. Tag `v0.1.0`. Verify artifacts, checksums, smoke test, publish PR.

## M10 — Phase 1 gate run (3 days)

1. Grow the golden set to 45+ questions across at least 3 libraries and 4 task types ([14](14-benchmark-eval.md)).
2. Run the harness. Produce the gate report.
3. Fix what the gate surfaces. Re-run. The gate table must show PASS or a documented triage before Phase 2 starts.

## M11 — Quality pass (4 days)

1. Injection filter v0 with the rule table from [07](07-ingest-pipeline.md). Quarantine ops.
2. Session dedup (`session_trajectory`) and `--session`.
3. Surface trust metadata (`fetched_at`, extractor identity) in results.
4. Measure the −49% failure delta versus the M4 baseline on the golden set. If the delta is smaller, tune segmentation and headers before adding anything new.

## M12 — Embeddings optional (5 days)

1. Feature `embeddings`: fastembed with bge-small-en-v1.5. Embed at ingest. Store binary-quantized blobs ([06](06-data-model.md)).
2. Query-side embedding and the vector rung. RRF fusion live with k = 60.
3. Gate: hit@3 improves on the golden set at p95 < 400 ms. The feature-off build is byte-identical in behavior to M11.

## Risk register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| tree-sitter grammar churn breaks extraction | Medium | Medium | Pinned binding 0.25.8 + grammar pins. Extraction behind a trait so a regex fallback can serve. TS already deferred |
| FTS5 query builder produces invalid MATCH on odd input | Medium | Low | Property test with an escaper. Every token quoted. Fuzz in CI monthly |
| reqwest DNS hook changes in a future 0.x | Low | High | The guard wraps reqwest behind one type. The connect-time re-check is independent code that works with or without the hook |
| rust-trafilatura or dom_smoothie stalls | Low | Low | Both are interchangeable behind the extractor boundary. Htmd is the noted alternate ([10](10-dependency-audit.md)) |
| rmcp breaking changes (3.x moved fast: 16 releases in 2026) | Medium | Medium | Pin exact version. Contract tests catch behavior drift at bump time |
| Golden set too small to trust the gates | Medium | High | Grow to GPT2-style sizing (300 tasks) before any external adoption claim ([14](14-benchmark-eval.md)) |
| Solo-project bus factor | High | Medium | This guide is the bus insurance: every decision and command is written down. CI is the enforcer |
| SQLite FTS5 edge cases on CJK or mixed-script docs | Medium | Low | unicode61 covers most. Note the limitation in README. Trigram tokenizer is a documented future option |

## Do-not-build list

1. No plugin system. The adapter trait is the extension point.
2. No daemon, no background service. One process per command.
3. No remote registry or sync service. Config is local files.
4. No LLM calls anywhere in the core. Retrieval is deterministic. Cost stays $0 per query.
5. No HTML crawl in Phase 1, and only with robots plus license gates later.
6. No TypeScript symbols until the grammar conflict resolves.
7. No telemetry, ever, in the local profile.
