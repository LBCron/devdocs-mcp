---
type: guide
title: Architecture
description: The layered design of devdocs as crates and types: layer map, crate graph, data flow, error model, concurrency model, configuration, storage layout, and the latency budget.
tags: [devdocs, architecture, crates, errors, config]
timestamp: 2026-11-07T00:00:00Z
resource: docs
derives-from: [docs/research]
---

# Architecture

## Layer map

Four layers, one core, three thin adapters (research: architecture.md). The crates map onto the layers one-to-one.

```text
┌────────────────────────────────────────────────────────────┐
│ L4 SERVE     devdocs (CLI)      devdocs-mcp (2–3 tools)    │
│              thin wrappers; identical results per query     │
├────────────────────────────────────────────────────────────┤
│ L3 RETRIEVE  devdocs-retrieve                               │
│              version resolve → hybrid search → RRF fusion   │
│              → budget pack → fallback ladder                │
├────────────────────────────────────────────────────────────┤
│ L2+L1 INGEST devdocs-ingest                                 │
│              adapters (llms.txt → git → OpenAPI → HTML)     │
│              fetch guard → license gate → normalize         │
│              → segment → dedup → symbols                    │
├────────────────────────────────────────────────────────────┤
│ STORE        devdocs-store (SQLite, FTS5, migrations)       │
├────────────────────────────────────────────────────────────┤
│ SHARED       devdocs-model (types, errors, config)          │
└────────────────────────────────────────────────────────────┘
```

The corpus is the product. The adapters are swappable (research: architecture.md, "one core, three thin adapters").

## Crate graph

```text
                 devdocs (bin)
                /   |   \      \
               v    v    v      v
        devdocs-mcp  devdocs-retrieve  devdocs-ingest  devdocs-eval (bin)
               \        |             /        |
                v       v            v         v
              devdocs-store ←—— devdocs-model ——→ (external types only)
                                 (devdocs-testkit is a dev-dependency)
```

Dependency rules are in [02](02-project-scaffolding.md). Two rules matter most: `devdocs-model` depends on no workspace crate, and `devdocs-retrieve` never reads through the ingest code.

The `context-chunker` and `context-file` crates are internal helpers outside this graph. No devdocs crate depends on them and they depend on no devdocs crate ([02](02-project-scaffolding.md)).

## Data flow

### Add flow (ingest)

1. The user runs `devdocs add <source> --version <v>`.
2. The CLI resolves the source to an adapter (llms.txt, git, OpenAPI, HTML).
3. The license gate checks the source entry in the registry. A missing or denied entry stops the flow with a structured error (research: security-licensing.md §3).
4. The fetch guard plans and performs the network reads. Every request passes the SSRF rules in [09](09-security-implementation.md).
5. The adapter produces `RawDocument` values with provenance attached at the boundary.
6. The normalizer strips chrome, segments into chunks, adds contextual headers, and extracts symbols.
7. The deduper drops exact duplicates by hash.
8. The store writes chunks and the FTS5 index inside one transaction, then flips the source row to active. The whole update is atomic (research: architecture.md §L3).

### Query flow (retrieve)

1. The CLI or the MCP tool calls `retrieve::search(Query)`.
2. The resolver determines library and version from explicit flags first, then from project lockfiles ([08](08-retrieval-interfaces.md)).
3. The planner picks rungs: exact symbol, FTS5 BM25, and (if the feature is on) vector scan.
4. The fusion step merges rung results with reciprocal rank fusion, k = 60.
5. The budget packer takes results in fusion order until `max_tokens` fills. It sets `truncated` and a cursor when content remains.
6. The result carries full provenance. The CLI renders it. The MCP server returns it as structured JSON.

## Core types

Sketches only. Field names are binding. The types live in `devdocs-model`.

```rust
pub struct SourceId(pub i64);

pub struct SourceRecord {
    pub id: SourceId,
    pub library: String,          // normalized name, e.g. "next.js"
    pub version: String,          // exact version, e.g. "16.2.9"
    pub kind: SourceKind,         // LlmsTxt | Git | OpenApi | Html
    pub origin_url: Url,
    pub commit_sha: Option<String>,
    pub license_status: LicenseStatus,
    pub fetched_at: OffsetDateTime,
}

pub enum LicenseStatus {
    PendingReview, ApprovedFetch, ApprovedIndex, ApprovedEmbed,
    ApprovedCache, ApprovedSnippet, Denied, Expired, Revoked,
}

pub struct Chunk {
    pub id: i64,
    pub source: SourceId,
    pub section_path: Vec<String>,   // breadcrumb, e.g. ["Routing", "Proxy"]
    pub symbol: Option<String>,      // exact symbol this chunk documents
    pub text: String,                // body with contextual header prepended
    pub token_count: u32,
    pub content_hash: [u8; 32],      // sha256 of canonical body
    pub ordinal: u32,
}

pub struct Query {
    pub text: String,
    pub library: Option<String>,
    pub version: Option<VersionRequest>, // Exact | Latest | Alias(String)
    pub limit: u32,
    pub max_tokens: u32,
    pub detail: Detail,              // Name | Summary | Full
}

pub struct SearchResult {
    pub r#ref: String,               // docs://library/version/section#anchor
    pub title: String,
    pub section: String,
    pub snippet: String,
    pub examples: Vec<CodeExample>,
    pub source_url: String,
    pub source_commit: Option<String>,
    pub requested_version: String,
    pub served_version: String,
    pub version_status: VersionStatus, // Verified | Fallback | Alias
    pub content_hash: String,          // hex sha256
    pub license_status: LicenseStatus,
    pub confidence: f32,
    pub token_count: u32,
}
```

The result shape matches the evidence contract in the research bundle (research: retrieval-design.md §3.8). `ref` values are stable identifiers. The server generates them. The model never writes them.

## Error model

- Each lib crate defines one `thiserror` error enum. Variants carry context, never messages for humans.
- `devdocs-model::Error` is the top enum. Crate errors convert into it with `#[from]`.
- The CLI maps errors to exit codes and to structured JSON on stderr.

| Exit code | Meaning | Typical source |
|---|---|---|
| 0 | Success (a hit, or a completed operation) | — |
| 1 | Usage or configuration error | clap parse failure (remapped from clap's default 2), bad config file |
| 2 | No result. Valid query, empty index or no match | retrieve returns `Miss` |
| 3 | Upstream error. Fetch, network, git, or parse failure | ingest fetch guard, adapters |
| 4 | Budget exceeded with no deliverable content | retrieve budget packer |

Deviation note. The research bundle maps exit 2 to "no result" (research: architecture.md §L4, DS §3). clap emits exit code 2 for argument errors by default. To keep the codes unambiguous, the CLI catches the clap error, prints the structured JSON error, and exits 1. The 0/2/3/4 semantics from the research stay intact.

The JSON error shape on stderr:

```json
{"error": "PKG_NOT_INDEXED", "hint": "run: devdocs add https://github.com/vercel/next.js --version 16.2.9", "detail": null}
```

Error codes are stable UPPER_SNAKE strings. A new code is a minor release. The full code table lives in [08](08-retrieval-interfaces.md).

## Concurrency model

- tokio multi-thread runtime. The CLI builds it with `#[tokio::main]`.
- rusqlite is synchronous. All store access runs inside `tokio::task::spawn_blocking` behind `devdocs_store::Store`, which wraps a `Mutex<Connection>`.
- One writer at a time. The DB opens with WAL mode. Readers and the writer share the single connection. Ingest jobs serialize on the store mutex. Concurrency needs stay modest by design (single user, local DB).
- Fetch concurrency defaults to 4 with a semaphore. The config caps it at 16.
- Signals: `SIGINT`/`SIGTERM` cancel ingest jobs. A cancelled ingest leaves the DB untouched because writes are one transaction.

## Configuration

Precedence, highest first:

1. Command-line flags.
2. Environment variables prefixed `DEVDOCS_` (for example `DEVDOCS_HOME`, `DEVDOCS_FETCH_CONCURRENCY`).
3. Project file `devdocs.toml` in the current directory or a parent.
4. User file `$XDG_CONFIG_HOME/devdocs/config.toml` (macOS: `~/Library/Application Support/devdocs/config.toml`).
5. Built-in defaults.

`devdocs config init` writes the user file with comments. `devdocs config show` prints the merged effective configuration as JSON. `devdocs config validate` checks a file against the schema, including every `[sources.*]` URL.

Example `devdocs.toml`:

```toml
[fetch]
concurrency = 4
timeout_ms = 15000
max_bytes = 20971520        # 20 MiB per response
max_redirects = 3
user_agent = "devdocs/0.1 (+https://github.com/OWNER/devdocs-mcp)"

[index]
path = "~/.local/share/devdocs/docs.db"   # DEVDOCS_HOME overrides
embeddings = false

[search]
max_tokens_default = 2000
limit_default = 8
rrf_k = 60

[sources.nextjs]
kind = "llms_txt"
url = "https://nextjs.org/docs/llms.txt"
license = "MIT"
attribution = "docs (c) Vercel"
expires = "2027-11-06"
refresh_days = 2
```

The `[sources]` table is the declarative half of the license registry: it names the origin, the license SPDX id, and the optional `attribution` and `expires` lines. It carries no `status`. A config entry with a `status` key fails validation with the key path `sources.<name>.status`. The mutable license state lives in the SQLite store — `sources.license_status`, `sources.license_expires_at`, and the audit lines in `meta` (docs/implementation/06 §License-gate state). `devdocs source approve` and `deny` move the state; the state machine semantics live in [07](07-ingest-pipeline.md) and [09](09-security-implementation.md).

## Storage layout

| Path | Purpose |
|---|---|
| `$DEVDOCS_HOME/docs.db` | The single SQLite database. WAL mode |
| `$DEVDOCS_HOME/tmp/` | Scratch space for clones and downloads. Cleaned after each run |
| `$XDG_CONFIG_HOME/devdocs/config.toml` | User configuration and the source registry |

`DEVDOCS_HOME` overrides the data root on every platform. The research bundle named `~/.docctl/docs.db`. This design uses the XDG-correct root with the same single-file property (research: architecture.md §L3).

## Latency budget

The Phase 1 gate is p95 under 400 ms for a full query (research: architecture.md Phase 1). The budget table assigns that time. The benchmark suite in [04](04-testing-strategy.md) measures each row.

| Step | Budget (p95) |
|---|---|
| Config + DB open (warm) | 15 ms |
| Version resolution (lockfile parse) | 40 ms |
| Symbol exact lookup | 10 ms |
| FTS5 BM25 query | 50 ms |
| Fusion + budget pack | 20 ms |
| Total retrieve path | under 150 ms |
| Cold DB open (first run) | under 500 ms, outside the query gate |

The 400 ms gate includes process start on a warm page cache. The CLI must stay lean: no network calls on the query path, ever. A query that cannot be served from the local index returns exit code 2 with a `miss_reason`, it does not fetch (research: retrieval-design.md §3.7, cold-start policy).

## Adapter boundaries

- Federation to hosted indexes (Context7 as optional provider) is a future L4 concern behind a `Provider` trait. Not in Phase 1 to 3. The trait seat exists so the core stays unchanged when it arrives (research: architecture.md, optional federation).
- Code Mode is a future adapter behind the sandbox contract. The core exposes no interface for it today (research: interfaces.md §3, gated adapter).
- The MCP adapter shares the retrieve crate with the CLI. The identical-results gate enforces that both produce the same results for the same query (research: architecture.md Phase 2 gate).
