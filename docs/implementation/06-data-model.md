---
type: guide
title: Data Model
description: The complete SQLite schema for devdocs, the FTS5 configuration, the migration system, and the hashing policy. Targets SQLite 3.54 bundled in rusqlite 0.41.
tags: [devdocs, sqlite, fts5, schema, migrations]
timestamp: 2026-10-31T00:00:00Z
resource: docs
derives-from: [docs/research]
---

# Data Model

One SQLite file holds everything (research: architecture.md §L3). The schema below is migration `0001_init.sql`. Later migrations extend it. The metadata tuple `(library, version, source_url, commit_sha, content_hash, section_path, symbol, text)` from the research bundle appears as columns across `sources` and `chunks` (research: architecture.md §L3, GLMC Rec. 1).

## Connection pragmas

The store sets these on every open:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA temp_store = MEMORY;
PRAGMA mmap_size = 134217728;   -- 128 MiB
```

WAL plus `synchronous = NORMAL` fits the single-user local profile. The file mode is 0600 and the directory mode is 0700 ([09](09-security-implementation.md)).

## Schema

```sql
-- meta: tool + schema bookkeeping
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT INTO meta(key, value) VALUES ('schema_created_at', strftime('%Y-%m-%dT%H:%M:%SZ','now'));

-- sources: one row per (library, version, origin)
CREATE TABLE sources (
  source_id      INTEGER PRIMARY KEY,
  library        TEXT NOT NULL,
  version        TEXT NOT NULL,
  kind           TEXT NOT NULL CHECK (kind IN ('llms_txt','git','openapi','html')),
  origin_url     TEXT NOT NULL,
  commit_sha     TEXT,
  license        TEXT,                       -- SPDX id or 'unknown'
  license_status TEXT NOT NULL CHECK (license_status IN (
      'pending_review','approved_fetch','approved_index','approved_embed',
      'approved_cache','approved_snippet','denied','expired','revoked')),
  fetched_at     TEXT NOT NULL,              -- RFC 3339 UTC
  active         INTEGER NOT NULL DEFAULT 0, -- 1 after a successful publish
  UNIQUE (library, version, kind, origin_url)
);
CREATE INDEX idx_sources_library ON sources(library);
CREATE INDEX idx_sources_active  ON sources(active);

-- chunks: normalized retrieval units
CREATE TABLE chunks (
  chunk_id     INTEGER PRIMARY KEY,
  source_id    INTEGER NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
  content_hash BLOB NOT NULL,               -- sha256, 32 raw bytes
  section_path TEXT NOT NULL,               -- JSON array of breadcrumb strings
  symbol       TEXT,                        -- exact symbol or NULL
  text         TEXT NOT NULL,               -- body, contextual header prepended
  token_count  INTEGER NOT NULL,
  ordinal      INTEGER NOT NULL,
  UNIQUE (source_id, ordinal)
);
CREATE INDEX idx_chunks_symbol ON chunks(source_id, symbol) WHERE symbol IS NOT NULL;
CREATE INDEX idx_chunks_hash   ON chunks(content_hash);

-- chunks_content: view over chunks that spells the FTS5 columns by name.
-- FTS5 external-content reads resolve the FTS5 column names (title, body)
-- against the content table, so the view derives them without storing
-- derived columns in chunks.
CREATE VIEW chunks_content AS
  SELECT chunk_id,
         section_path || ' ' || COALESCE(symbol, '') AS title,
         text AS body
  FROM chunks;

-- chunks_fts: FTS5 external-content index over chunks_content
CREATE VIRTUAL TABLE chunks_fts USING fts5(
  title,
  body,
  content='chunks_content',
  content_rowid='chunk_id',
  tokenize="porter unicode61 remove_diacritics 2 tokenchars '-_'"
);

-- keep the FTS index in sync with chunks
CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
  INSERT INTO chunks_fts(rowid, title, body)
  VALUES (new.chunk_id,
          new.section_path || ' ' || COALESCE(new.symbol, ''),
          new.text);
END;
CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, title, body)
  VALUES ('delete', old.chunk_id,
          old.section_path || ' ' || COALESCE(old.symbol, ''),
          old.text);
END;

-- symbols: exact-symbol lookup
CREATE TABLE symbols (
  symbol     TEXT NOT NULL,
  chunk_id   INTEGER NOT NULL REFERENCES chunks(chunk_id) ON DELETE CASCADE,
  kind       TEXT NOT NULL,                 -- 'function','class','method','route','operation', ...
  PRIMARY KEY (symbol, chunk_id)
);
CREATE INDEX idx_symbols_chunk ON symbols(chunk_id);

-- session_trajectory: per-session served-chunk dedup (phase 2)
CREATE TABLE session_trajectory (
  session_id TEXT NOT NULL,
  chunk_id   INTEGER NOT NULL REFERENCES chunks(chunk_id) ON DELETE CASCADE,
  served_at  TEXT NOT NULL,
  PRIMARY KEY (session_id, chunk_id)
);
```

## FTS5 design decisions

Verified against the SQLite 3.54 FTS5 documentation on 2026-10-28.

- External content through the `chunks_content` view over `chunks`. FTS5 content reads resolve the FTS5 column names against the content object (`SELECT <content_rowid>, <cols> FROM <content>`), and `chunks` names its columns `section_path`, `symbol`, and `text`, so a bare `content='chunks'` cannot serve `snippet()`; the view supplies `title` and `body` by name. The external-content form supports `snippet()` for the stored text; contentless tables return NULL from `snippet()` columns.
- The `delete` trigger form shown above is the documented sync method for external-content tables.
- Tokenizer: `porter unicode61 remove_diacritics 2 tokenchars '-_'`. Porter stems English prose. `remove_diacritics 2` handles multi-diacritic codepoints. Code symbols keep `-` and `_` inside tokens, so `use_router` and `use-route` tokenize whole.
- The `title` column carries the breadcrumb plus the symbol. It gets the high weight in ranking. The `body` column carries the text.
- Ranking uses per-column weights: `bm25(chunks_fts, 5.0, 1.0)`. Title matches outrank body matches 5 to 1.
- The ingest path can also use `INSERT INTO chunks_fts(chunks_fts, rank) VALUES ('rank', 'bm25(5.0, 1.0)')` to fix the weights for the connection instead of passing them per query. The retrieve layer passes them per query so the weights stay visible in code.

## Optional vector table (feature `embeddings`, phase 3)

```sql
CREATE TABLE chunks_vec (
  chunk_id INTEGER PRIMARY KEY REFERENCES chunks(chunk_id) ON DELETE CASCADE,
  vec      BLOB NOT NULL,      -- binary-quantized: 1 bit per dimension + fp32 norm
  dims     INTEGER NOT NULL
);
```

The scan is in-process Rust over the blobs (hamming distance on bytes). sqlite-vec stays out of the dependency tree while it is dormant (verified 2026-10-28: last release 2026-05-18, pre-v1.0). At 100,000 vectors of 384 dimensions the table is about 4.8 MB of bits plus overhead, and a full scan stays far under the 400 ms gate.

## Migrations

- Files live in `migrations/` and embed with `include_str!` into `devdocs-store`.
- Names: `NNNN_description.sql`, numbered from `0001`.
- The runner reads `PRAGMA user_version` and applies pending files in order inside one transaction each, then sets `user_version` to the applied number.
- Rules:
  1. Applied migrations never change. A schema change is a new migration.
  2. Every migration is idempotent under failure because it runs in one transaction. SQLite DDL is transactional.
  3. `devdocs db migrate` runs at CLI start and at MCP server start. It is fast (one read of `user_version`) and safe to run concurrently thanks to the write lock.
  4. Destructive migrations (drops) require a `--force` flag on `devdocs db migrate` for that migration only, recorded in the migration header comment.
  5. Migration tests cover the up direction on a temp file, the no-op re-run on an up-to-date database, and rollback of a failed migration inside its transaction. There is no down direction to test: applied migrations never change (rule 1), so a failure rolls back to the previous version's schema and nothing else exists to roll back to.

Runner signature in `devdocs-store`:

```rust
pub fn migrate(conn: &mut Connection) -> Result<Vec<AppliedMigration>, StoreError>;
```

## Hashing policy

- `chunks.content_hash` is SHA-256 of the canonical chunk body (the text without the contextual header, UTF-8 bytes, LF line endings). SHA-256 is the interop choice: provenance verifiers and upstream artifacts use it (research: retrieval-design.md §3.8).
- Exact dedup during ingest compares SHA-256 across the whole corpus. The hot path may use blake3 for an in-memory pre-filter because it is faster, then confirm with SHA-256. blake3 hashes never leave the process.
- `verify_doc` re-derives the hash of a stored chunk and compares it to the stored value. A mismatch means the index drifted. The tool reports `HASH_MISMATCH` and offers a re-sync.

## Metadata mapping

| Research tuple element | Column(s) |
|---|---|
| library | `sources.library` |
| version | `sources.version` |
| source_url | `sources.origin_url` |
| commit_sha | `sources.commit_sha` |
| content_hash | `chunks.content_hash` (hex in results) |
| section_path | `chunks.section_path` (JSON array) |
| symbol | `chunks.symbol` + `symbols` table |
| text | `chunks.text` |
| license_status | `sources.license_status` (research: security-licensing.md §3 state machine) |
| trust / freshness | `sources.fetched_at` + refresh policy ([07](07-ingest-pipeline.md)) |
