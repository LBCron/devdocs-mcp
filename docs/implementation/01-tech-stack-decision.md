---
type: decision record
title: Tech Stack Decision
description: The technology decision for devdocs, the score matrix behind it, the rejected alternatives, and the conditions that would reverse it. All versions verified 2026-10-28.
tags: [devdocs, tech-stack, rust, decision]
timestamp: 2026-11-04T00:00:00Z
resource: docs
derives-from: [docs/research]
---

# Tech Stack Decision

Status: accepted, 2026-10-28.

## The decision

Build `devdocs` as one Rust binary. The binary is a CLI first and an MCP server on demand. SQLite with FTS5 is the only storage engine. Git access goes through the system `git` binary. Embeddings stay optional behind a feature flag.

The reasoning comes from the research bundle: the corpus and the retrieval quality are the product. The interface is a thin adapter. A single systems language with a small dependency graph gives the strongest reliability per unit of effort for a read-only local tool (research: verdict.md, architecture.md).

## Chosen stack

All versions verified 2026-10-28 against crates.io, docs.rs, and the release pages.

| Component | Choice | Version | Reason |
|---|---|---|---|
| Language | Rust, edition 2024 | 1.99.1 | One binary, no runtime install, memory safety, smallest risk profile for this domain |
| Async runtime | tokio | 1.54.1 | De-facto standard. MSRV 1.90 matches the workspace floor |
| HTTP client | reqwest | 0.13.5 | rustls default TLS. `dns_resolver` hook stays available for SSRF pinning |
| Storage | rusqlite, `bundled` feature | 0.41.0 | Bundles SQLite 3.54.0 with FTS5 compiled in. No system SQLite dependency |
| CLI parser | clap, `derive` feature | 4.6.7 | De-facto standard for Rust CLIs |
| Serialization | serde + serde_json | 1.0.229 / 1.0.154 | Standard |
| Markdown | comrak | 0.59.0 | CommonMark plus GFM. Reliable `sourcepos` for code-block positions |
| HTML extraction | dom_smoothie | 0.19.0 | Most active Readability port. Markdown output mode |
| HTML extraction fallback | rust-trafilatura | 2.2.8 | Published F1 scores 91.14 / 95.98 / 79.57 on public corpora |
| Chunk sizing helper | text-splitter | 0.33.1 | `MarkdownSplitter` keeps code blocks intact. Token-based lengths |
| Symbols | tree-sitter | 0.25.8 (pinned) | Grammar crates require this line. See the note below |
| Grammars | rust 0.24.2, go 0.25.0, python 0.25.0, javascript 0.25.0 | — | Verified compatible with binding 0.25 |
| MCP server | rmcp | 3.8.0 | Official Rust SDK. Used by Block goose and Apollo. OpenAI keeps a fork |
| Errors | thiserror (libs), anyhow (bin) | 2.0.21 / 1.0.104 | Standard split |
| Logging | tracing + tracing-subscriber | 0.1.44 / 0.3.23 | Structured logs on stderr only |
| Config | toml + serde | toml 0.9 | Config files are TOML |
| YAML (lockfiles) | serde_yaml_ng | 0.10.0 | serde_yaml and serde_yml are archived. This fork is maintained |
| URLs | url | 2.5.8 | Standard |
| Hashing | sha2 (provenance), blake3 (dedup) | 0.11.1 / 1.8.7 | sha256 for provenance interop. blake3 for the fast dedup path |
| Paths | dirs | 7.0.0 | XDG-correct data and config directories |
| OpenAPI | oas3 | 0.22.0 | Most active OpenAPI parser in 2026 |
| Token count | chars-per-4 heuristic. tiktoken-rs optional | 0.12.1 | Heuristic is the default. Exact mode behind a feature flag |
| Embeddings (optional) | fastembed | 7.5.0 | ort backend. Default model bge-small-en-v1.5, 384 dimensions, Apache-2.0 |
| Fetch tests | wiremock | 0.6.5 | Async HTTP doubles without network access |

Tree-sitter note. The TypeScript grammar (`tree-sitter-typescript` 0.23.2) still requires binding 0.24 and conflicts with the other grammars. Phase 1 supports Rust, Go, Python, and JavaScript only. TypeScript support waits for a grammar release that accepts binding 0.25 or later. The JavaScript grammar gives partial TypeScript coverage for indexing, with no recall claim.

Git note. Clone and checkout shell out to the system `git` binary. This matches cargo, uv, and gh. It gives credential helpers, ssh-agent, and shallow clones for free. The `git2` crate stays out of the dependency tree (research: verified 2026-10-28, libgit2 gaps in credential helpers and shallow fetch).

Vector note. The `sqlite-vec` extension is dormant (last release 2026-05-18, pre-v1.0). The optional vector rung uses an in-process scan over binary-quantized vectors in SQLite blobs instead. At 100,000 vectors this is roughly 13 MB and far below the latency budget (research: architecture.md §L3).

## Score matrix

The stack evaluation on 2026-10-28 scored seven candidate stacks. Three research subagents verified the runtime facts first. The weights were fixed before scoring.

| Criterion | Weight |
|---|---|
| Hard requirements fit | 30% |
| Reliability | 20% |
| Development velocity | 15% |
| Performance | 10% |
| Distribution | 10% |
| Maintainability | 10% |
| Extensibility | 5% |

| Rank | Stack | Score |
|---|---|---|
| 1 | Pure Rust | 4.40 |
| 2 | TypeScript on Node 24 LTS (`node:sqlite`) | 4.08 |
| 3 | Rust + embedded WASM plugins | 4.05 |
| 4 | Go | 3.95 |
| 5 | Rust + Python sidecar | 3.35 |
| 6 | Bun | 2.95 |
| 7 | Multi-language single binary | 1.80 |

Node 24 remains a viable fallback. Node compiles its own SQLite with FTS5 and extension loading, so the Bun macOS problem does not apply to Node. Its weak link is binary distribution, because Node single-executable applications are still experimental. This evaluation derives from the 2026-10-28 session. The per-criterion evidence lives in that session log and in the research bundle.

## Rejected alternatives

| Alternative | Reason to reject |
|---|---|
| Bun | `bun:sqlite` on macOS uses the system SQLite without extension loading. AOT with `--trim` is an unmerged PR from 2026-10-17 that blocks native addons. Linux binary floor near 57 MB |
| Go | Strong fit, ranked 4th. Loses on tree-sitter binding maturity and single-binary MCP precedent. Revisit if the team changes |
| Multi-language linked binary | No production tool links 4 to 5 runtimes. Go `c-archive` has known signal-stack hazards. BEAM has no static-lib path. Proven pattern is one systems language plus one embedded runtime plus C libraries |
| Python | Distribution cost (uv downloads a full interpreter) and latency floor. Sidecar pattern scored 3.35 |
| JVM (Java, Kotlin, Scala) | Startup and memory budget conflicts with the p95 latency gate and the single-binary goal |
| Zig | Ecosystem too small for tree-sitter, MCP, and extraction needs in 2026 |
| Elixir | No static-library embedding path. BEAM distribution model does not fit a per-project CLI |
| Turso (database) | Pre-1.0 and lacks a mature FTS5 path. Bundled SQLite is strictly safer here |

## Reversal conditions

The decision stands until one of these conditions holds. Re-run the evaluation when one fires.

1. `node:sqlite` reaches Stability 2 and single-executable applications ship in an LTS line. Then Node ties Rust and wins on velocity.
2. Bun AOT merges with cross-compile, trims native addons, and bundles SQLite on macOS. Then Bun re-enters the list.
3. Grounded Docs (arabold/docs-mcp-server) adds per-span provenance or license gating. Then re-run the differentiation claim before Phase 1 (research: architecture.md, competitive boundary).
4. The tree-sitter grammar conflict blocks a needed language for more than one quarter. Then evaluate an alternative symbol extractor.

## Revised decisions

The scaffold exposed conflicts between this decision record and the committed repository. The M0 reconciliation ticket resolved them. Each entry states the change and the reason.

- **2026-10-31 — License: single MIT.** The first draft planned `MIT OR Apache-2.0` with two license files. The scaffold committed one MIT `LICENSE` and the dual-license path was never used. The workspace `license` field and the docs now state `MIT`. Revisit only if a downstream embedding requirement demands it.
- **2026-10-31 — MSRV 1.90.** The first draft planned 1.85. Dependency MSRVs forced the floor up: rusqlite 0.41 needs 1.89 and text-splitter 0.33 needs 1.90. The workspace `rust-version`, the docs, and `just msrv` name 1.90.
- **2026-10-31 — Release gate: auto-merge, not a human merge.** The publish workflow planned a human merge of the release PR as the release gate. The owner chose zero-touch releases: the `release-plz-pr` job enables auto-merge on every release PR it opens, the merge fires when the repository ruleset's requirements pass, and crates.io publishing uses trusted publishing so no registry token is stored. The trade is explicit: a broken commit that reaches `main` auto-publishes on the next release cycle instead of waiting for a human.
- **2026-11-04 — Release gate: a published GitHub release, not an auto-merged release PR.** The 2026-10-31 entry made the `release-plz-pr` job enable auto-merge on its own release PR. Each merge added a `chore: release vX.Y.Z` commit. release-plz read that commit as a new change and opened the next release PR. Auto-merge made the chain continuous. The owner reversed the decision. `publish.yml` now starts only from a published GitHub release or a manual dispatch that names an existing release tag. release-plz runs the `release` command only. No workflow merges a PR. A human bumps the version, commits, and pushes the tag. The 0.1.0 reset and the deleted 0.2.x and 0.3.x tags and releases remove the output of the loop.

## Out of scope by decision

- Code Mode adapter. Ships only after the sandbox contract passes (research: security-licensing.md §4).
- Hosted service and multi-tenant profiles. The MVP profile is local, read-only, single-user.
- Windows ARM64 release builds. The target became Tier 1 in Rust 1.99, but Phase 1 ships the three main desktop triples first.
- macOS code signing and notarization. Most OSS Rust CLIs ship unsigned. Revisit when there is a distribution partner.
