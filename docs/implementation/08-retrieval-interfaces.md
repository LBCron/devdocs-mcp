---
type: guide
title: Retrieval and Interfaces (L3 + L4)
description: The hybrid retrieval pipeline, version resolution, budget enforcement, fallback ladder, the result shape, the full CLI contract, and the MCP adapter.
tags: [devdocs, retrieval, cli, mcp, rrf, max-tokens]
timestamp: 2026-11-07T08:00:00Z
resource: docs
derives-from: [docs/research]
---

# Retrieval and Interfaces (L3 + L4)

## Query pipeline

One operation is the normal path: `search` with optional filters. Explicit library resolution is for ambiguity only (research: architecture.md, one-request fast path).

```text
Query → resolve (library, version) → plan rungs → execute rungs
      → RRF fusion (k=60) → budget pack → result envelope
```

## Version resolution

Order, highest first:

1. Explicit `--library` / `--version` flags (or MCP parameters).
2. Project lockfiles in the current directory or parents. Parsed per this table:

| File | Parser | Notes |
|---|---|---|
| `Cargo.lock` | toml | `[package] name, version` rows |
| `package-lock.json` | serde_json | `packages` map keys strip `node_modules/` |
| `pnpm-lock.yaml` | serde_yaml_ng | `importers` and `packages` sections |
| `uv.lock` | toml | `[[package]]` rows |
| `pyproject.toml` | toml | `[project] dependencies` for the request, lockfile for the pin |
| `go.mod` | internal line parser | `require (...)` blocks |
| `Gemfile.lock` | internal line parser | `GEM specs` section (phase 2) |

3. Query text mentions (`next.js 16`) as a weak hint. A leading `v` on a mention version is stripped, so the hint matches the index's bare version spelling.
4. Fallback: no library filter. Search all approved sources. An explicit version that the index carries narrows the fallback to that version; an absent version follows the fallback ladder (§Version policy). A library the lockfile or a query mention named that is absent from the index degrades to this fallback too — the flag layer is the only source that names a library hard.

The precedence applies per field: a flag beats the lockfile in the field it fills, and a field the flags leave open falls to the lockfile — but a lockfile entry never donates its version to a library a flag named unless the entry is that library.

A pinned scope serves every active origin of one exact `(library, version)` pair — the schema allows several origins per pair (docs/implementation/06-data-model.md). `open` and `verify` resolve refs through the lowest source id, so one ref always serves one source.

Policy (research: retrieval-design.md §3.4): never serve `latest` silently when a version was requested. The result carries `requested_version`, `served_version`, and `version_status`:

| Status | Meaning |
|---|---|
| `verified` | Served version equals the requested exact version |
| `alias` | Requested `latest` or a major alias, resolved by the declared rule, stated in the result |
| `fallback` | Requested version absent. Served the nearest available version, named in the result. Counts as a miss for grading |
| `absent` | Library absent from the index and named by an explicit flag. Exit 2 with `miss_reason` |

The declared rules the implementation applies: the alias rule serves the highest active version, and the highest active version within the requested major for a major alias (`16`); a request with no version at all serves the highest active version under the same alias rule. The fallback rule serves the nearest available version — same major first, closest numeric distance `|Δmajor|·10⁶ + |Δminor|·10³ + |Δpatch|`; with no shared major the same distance rule applies across majors, so the closest major serves.

Migration questions: when the query matches a migration pattern ("upgrade", "migrate", "breaking changes"), the planner retrieves both the served version and the nearest older version and labels them (research: retrieval-design.md §3.4).

## Rungs

1. **Symbol exact.** `SELECT ... FROM symbols JOIN chunks` on the normalized identifier. Cheap, decisive for API names. Exact identifiers beat dense retrieval for API names (research: retrieval-design.md §3.1). The rung normalizes the query first: trim it, then strip trailing balanced `(...)`, `[...]`, and `<...>` groups repeatedly (`useRouter()` becomes `useRouter`). Dots stay: `obj.method` keeps its path. The ladder tries, in order: exact, the last dot-segment of a dotted form as a second exact attempt, case-sensitive prefix, case-insensitive. The first non-empty list wins. The rung requests 4× the hit count for fusion headroom, the same shape the FTS rung applies. The case-insensitive rung folds ASCII only — SQLite's NOCASE/LIKE fold ASCII letters, so an identifier cased with non-ASCII letters must match its stored case. When the request resolves to a library (§Version resolution), the rung pins to that library's served version's sources; otherwise it scans the resolved scope.
2. **FTS5 BM25.** Build the MATCH expression from the query: split on whitespace, drop operators, wrap each token as a quoted literal, append `*` to the last token of each run for prefix matching. Example: query `rewrite proxy request` becomes `"rewrite" "proxy" "request" *`. This construction is a property-tested invariant ([04](04-testing-strategy.md)). Ranking: `bm25(chunks_fts, 5.0, 1.0)` with `LIMIT` at 4× the requested hits for fusion headroom. Attach `snippet(chunks_fts, 1, '⟦', '⟧', '…', 12)` for highlight offsets.
3. **Vector scan** (feature `embeddings`, phase 3). Embed the query with the same model that built the index, scan `chunks_vec` in-process by hamming distance, take the top 4× hits.

## Fusion and budget

Reciprocal rank fusion with k = 60: `score(d) = Σ_rungs 1 / (60 + rank_rung(d))`. Ties break by lower chunk id (stable order, property-tested). The Redis production eval measured hybrid hit@3 at 0.96 against vector-only 0.88 with this mechanism (research: retrieval-design.md §3.1).

The budget packer walks results in fusion order, accumulates stored `token_count` values, and stops before `max_tokens`. Properties: rank 1 always fits (a result larger than the whole budget is chunked at open time, never dropped). The envelope sets `truncated: true` and a `next_cursor` when content remains. The packer never silently drops the top result (research: retrieval-design.md §3.6). The same exception governs `open`: the chunk's first line always serves, so an `open` page can exceed `--max-tokens` by one line rather than deliver an empty page.

`max_tokens` default: 2000. The research bundle records a contested spread — DS says 2000 to 4000, GEM's reference uses 1000, GLMP recommends a 1000 to 2000 server-side ceiling. This design takes GLMP's ceiling and DS's floor: 2000, enforced server-side in both the CLI and the MCP path. Per-profile overrides live in config (research: retrieval-design.md §3.6. Recorded, not averaged).

## Fallback ladder

Entered only on low confidence or an explicit `miss_reason` match (research: retrieval-design.md §3.7). The v1 tool implements rungs 1 and 2 in-process and refuses past them:

1. Local exact symbol index
2. Local hybrid docs index
3. (refused: external fetch) Official documentation page fetch — a future, explicitly gated feature
4. (refused) Official GitHub source search
5. (refused) General web search

A cold miss returns `not_indexed` with a sync hint. An abstention is a valid result (research: retrieval-design.md §3.7).

`miss_reason` values: `not_indexed`, `version_absent`, `license_blocked`, `quarantined_only`, `low_confidence`, `symbol_absent` — the last one the exact symbol lookup's miss when the target resolved but no symbol matched (hint names `devdocs search <symbol>`). The JSON error on exit 2 carries `miss_reason` and a `hint`; a cold miss — the library or the requested version absent — carries `error = "PKG_NOT_INDEXED"`, every other valid-query miss `error = "NO_RESULT"` (docs/implementation/05 §Error model).

## Result envelope

The unified shape for CLI JSON and MCP structured output (research: retrieval-design.md §3.8):

```json
{
  "results": [{
    "ref": "docs://next.js/v16.2.9/proxy#rewrite",
    "title": "Proxy rewrite",
    "section": "Routing > Proxy",
    "snippet": "...",
    "examples": [{"language": "tsx", "code": "..."}],
    "source_url": "https://nextjs.org/docs/proxy",
    "source_commit": "a1b2c3...",
    "requested_version": "16",
    "served_version": "16.2.9",
    "version_status": "verified",
    "content_hash": "<hex sha256>",
    "license_status": "approved_snippet",
    "confidence": 0.94,
    "token_count": 142
  }],
  "truncated": false,
  "next_cursor": null,
  "served_at": "2026-10-28T00:00:00Z"
}
```

Session dedup: with `--session <id>` (and session id in MCP calls), served chunk ids land in `session_trajectory`, and later queries in the same session down-rank already-served chunks instead of re-serving identical text (research: retrieval-design.md §3.8. The Ref measurement motivating it: −5,500 tokens average per request).

Ref derivation: a chunk with an empty section path (content before the first heading), and any path segment whose heading has no ASCII alphanumerics (non-ASCII or punctuation-only text slugs to empty), gets the placeholder segment `index` in its ref — `docs://<library>/v<version>/index`. A symbol that slugs to empty leaves the fragment out. A ref that names several chunks (two pages with the same section title, or several chunks under the placeholder) serves the lowest chunk id.

## CLI contract

Commands:

| Command | Purpose |
|---|---|
| `devdocs add <source> [--version V] [--kind K]` | Ingest a source through the ladder |
| `devdocs sync [library]` | Refresh stale approved sources |
| `devdocs list [--json]` | List indexed libraries and versions |
| `devdocs search "query" [--library L] [--version V] [--limit N] [--cursor C] [--max-tokens T] [--json]` | Hybrid search |
| `devdocs symbol <library[@version]> <symbol> [--cursor C] [--json]` | Exact symbol lookup |
| `devdocs open <ref> [--cursor C] [--max-tokens T] [--json]` | Read one section by ref |
| `devdocs verify <ref> [--json]` | Re-derive and compare the content hash |
| `devdocs versions <library> [--json]` | List the active versions a library serves |
| `devdocs source approve|deny|list` | License registry operations |
| `devdocs skill install [--path P]` | Write the SKILL.md companion file |
| `devdocs mcp [--transport stdio]` | Start the MCP server on stdio |
| `devdocs db migrate|vacuum|stats|quarantine list` | Database operations |
| `devdocs config init|show|validate|path` | Configuration operations |
| `devdocs eval run <suite> [--json]` | Run a golden-set suite (see [14](14-benchmark-eval.md)) |

Global flags: `--json` (machine output on stdout), `--db <path>`, `--config <path>`, `--max-tokens <n>`, `--timeout-ms <n>`, `--verbose` (trace logs on stderr), `--quiet`.

Contract properties (research: architecture.md §L4, GEM §5 checklist):

- JSON mode: `--json` prints the result envelope to stdout and nothing else. Logs always go to stderr.
- Non-TTY auto-detection: no spinners, no colors, no tables when stdout is not a terminal. Progress goes to stderr as plain lines.
- Bounded output everywhere: `--max-tokens` caps every read path. `--limit` caps result count (default 8).
- Deterministic ordering: fusion order, ties by chunk id. Identical DB state gives byte-identical `--json` output. Byte-identity excludes the `served_at` wall-clock field (the comparison rule for the CLI, the MCP (#12), and the embeddings (#20) tickets).
- Pagination: `--cursor C` resumes a truncated page — for `search` the cursor is the last served chunk id, for `open` the character offset into the chunk.
- Exit codes 0/1/2/3/4 per [05](05-architecture.md) (usage errors remap clap's default 2 to 1).

`devdocs symbol <library[@version]> <symbol>` resolves the target first. A library the index lacks is `PKG_NOT_INDEXED` (`not_indexed`), a version it lacks is `PKG_NOT_INDEXED` (`version_absent`) — both exit 2. An unpinned target pins to the highest active version under the alias rule (§Version resolution), and the match ladder applies across the pinned target's origins with ladder precedence holding over source order. The target resolved, the symbol rung runs alone through the same fusion-and-pack path as `search`. An empty fusion is the `symbol_absent` miss with the `devdocs search <symbol>` hint. `--cursor C` resumes after the last served chunk id of a truncated page, the same chunk-id cursor `search` uses; pagination walks headroom-sized windows past the cursor, so matches beyond the retrieval head stay reachable and `truncated` stays accurate. The command defines no `--limit` flag; the limit is the search default (8). A malformed or empty target is a usage error, exit 1.
- Progressive disclosure: `devdocs list --json` is the ~20-token discovery surface. `devdocs versions <lib> --json` the per-library surface. `search` and `open` the full read. No prose man page. `--help` stays short.
- No secrets in argv or stdout. The tool holds no credentials in v1 (public sources only). A future private-source feature will read tokens from the environment or an OS keystore, never from config files.
- One static binary. No daemon. `devdocs mcp` reuses the same process.

The skill companion (`devdocs skill install`) writes a file of roughly 500 tokens with this shape (research: architecture.md §L4):

```markdown
---
name: devdocs
description: Search pinned, version-exact official docs for project dependencies.
---
Use `devdocs` for library documentation. Workflow:
1. `devdocs search "<one concept>" --json` — search; add --library/--version if known.
2. `devdocs open <ref> --json` — read one section. Refs come only from search results.
3. One concept per query. Stop after 3 searches; use the best result.
4. Exit 2 = no result; run `devdocs sync <library>` if the library is missing.
Never cite a URL not present in a result. Trust `version_status`; do not mix versions.
```

## MCP adapter

Implemented with rmcp 3.8.0 (verified 2026-10-28. Official Rust SDK, used by Block goose and Apollo). Features: `server`, `transport-io` (stdio), `macros`, `schemars`. Three tools, not more (research: architecture.md §L4):

| Tool | Parameters | Returns |
|---|---|---|
| `search_docs` | `query` (string, required), `library?`, `version?`, `detail?` enum `name\|summary\|full`, `max_tokens?` | Result envelope |
| `open_doc` | `ref` (string, required. Must come from `search_docs`), `max_tokens?` | One section, bounded |
| `verify_doc` | `ref` (string, required) | `{"valid": bool, "content_hash": "..."}` |

Tool annotations (MCP spec 2026-07-28. Untrusted hints, set honestly): `readOnlyHint = true`, `destructiveHint = false`, `idempotentHint = true`, `openWorldHint = false`.

Server instructions text (kept short, discriminative. Description quality is a measured failure source, research: retrieval-design.md §3.6):

```text
devdocs serves version-pinned official docs from a local index.
search_docs: one concept per query. open_doc: read a section by ref from search_docs results only.
Stop after 3 searches per question. verify_doc confirms a citation before you rely on it.
```

Gates:

- The MCP path and the CLI path must produce identical results for the same query against the same DB. The contract test in [04](04-testing-strategy.md) enforces it (research: architecture.md Phase 2 gate).
- The tool surface stays under the 10%-of-context deferral threshold: three tools with compact schemas cost a few hundred tokens (research: interfaces.md §1).

## Error codes

Stable `UPPER_SNAKE` codes in JSON errors and MCP errors:

| Code | Exit | Meaning |
|---|---|---|
| `USAGE` | 1 | Bad flags or config |
| `CONFIG_INVALID` | 1 | Config file failed validation |
| `NO_RESULT` | 2 | Valid query, nothing served |
| `PKG_NOT_INDEXED` | 2 | Library or version absent from an explicitly named request. Hint names the add command. A lockfile- or mention-named absent library falls through to the fallback search instead |
| `FETCH_BLOCKED` | 3 | Fetch guard or license gate refused |
| `UPSTREAM_ERROR` | 3 | Network, git, or parse failure |
| `BUDGET_EXCEEDED` | 4 | Budget consumed with no deliverable content |
| `HASH_MISMATCH` | 3 | Index drifted. Re-sync suggested |
| `NOT_AVAILABLE` | 2 | Source expired or revoked |
