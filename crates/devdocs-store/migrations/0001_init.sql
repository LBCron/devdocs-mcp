-- Schema 0001 for the devdocs store.
--
-- Source of truth: docs/implementation/06-data-model.md §Schema.
-- Applied migrations never change: a schema change is a new migration file
-- (docs/implementation/06 §Migrations).

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
