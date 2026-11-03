---
type: guide
title: Testing Strategy
description: The test frameworks, the test pyramid, fixture strategy, coverage and mutation policy for devdocs. Framework versions verified 2026-10-28.
tags: [devdocs, testing, nextest, insta, proptest, coverage]
timestamp: 2026-11-03T00:00:00Z
resource: docs
derives-from: [docs/research]
---

# Testing Strategy

## Principles

1. Tests run offline by default. No test talks to the public internet. HTTP doubles come from wiremock. Git fixtures come from local repositories that the test creates.
2. Untrusted-input paths get the most tests. The fetch guard, the injection filter, and the segmentation code are the highest-risk modules.
3. Every bug fix adds the test that would have caught it.
4. The golden-set harness ([14](14-benchmark-eval.md)) is a separate quality lane. It measures retrieval quality. It does not replace unit tests.

## Frameworks

| Framework | Version | Use |
|---|---|---|
| built-in `#[test]` + cargo-nextest 0.9.150 | — | All unit and integration tests. nextest runs each test in its own process |
| insta 1.49.0 | — | Snapshot tests for chunk output, JSON result shapes, and rendered reports |
| proptest 1.12.0 | — | Property tests for parsers, the URL guard, and fusion ordering |
| criterion 0.8.2 | — | Benchmarks for the query hot path and the segmentation pipeline |
| assert_cmd 2.2.2 + predicates 3.1.4 | — | End-to-end CLI tests through the real binary |
| wiremock 0.6.5 | — | HTTP doubles for the fetch guard and adapters |
| cargo-llvm-cov 0.9.1 | — | Coverage measurement |
| cargo-mutants 27.1.0 | — | Mutation testing on schedule |

Install the runners once:

```sh
cargo binstall cargo-nextest cargo-llvm-cov cargo-mutants cargo-deny cargo-audit
```

## Test pyramid

| Level | Location | Count target | What it covers |
|---|---|---|---|
| Unit | `#[cfg(test)]` modules in each crate | hundreds | Pure logic: parsers, guards, fusion, chunk math |
| Integration | `crates/*/tests/` | tens | Crate boundaries: store migrations, ingest adapters, retrieve queries |
| CLI end-to-end | `crates/devdocs/tests/` | ~30 | Real binary, real temp DB, exit codes, JSON output, non-TTY rules |
| Contract | `crates/devdocs-mcp/tests/` | ~10 | MCP protocol through an in-process rmcp client against the server |
| Golden set | `crates/devdocs-eval` + `eval/golden/` | 45+ questions | Retrieval quality gates (see [14](14-benchmark-eval.md)) |
| Benchmarks | `crates/devdocs-retrieve/benches/` | ~8 | p50/p95 of query paths, segmentation throughput |
| Mutation | scheduled, not per-PR | — | Survival rate on parsers and fusion |

## Per-crate test map

| Crate | Required tests |
|---|---|
| devdocs-model | Config parsing round-trips. Error-to-exit-code mapping table test |
| devdocs-store | Migration up-and-down steps on a temp DB. FTS5 query behaviors (bm25 weights, snippet, tokenizer options). WAL and locking behavior |
| devdocs-ingest | URL guard properties. Adapter detection. llms.txt v2 parsing. Segmentation snapshots. Symbol extraction per grammar. Dedup logic. Injection filter rules. License state machine transitions |
| devdocs-retrieve | Version resolution per lockfile format. RRF fusion ordering. Budget packing. Fallback ladder. Result shape snapshots |
| devdocs-mcp | Tool schemas match the documented JSON. Tool calls return the same results as the CLI for identical queries (the identical-results gate) |
| devdocs | Exit codes 0/1/2/3/4. `--json` output. Non-TTY behavior. `--max-tokens` truncation flags |
| devdocs-eval | Metric math (hit at 5, symbol recall, percentiles) against hand-computed fixtures |

## Fixtures

- Location: `crates/devdocs-testkit/src/fixtures/` plus `testdata/` directories per crate. Keep fixtures under 2 MB total.
- Git fixtures: the testkit builds tiny repositories at test time in a temp dir. A helper runs `git init`, writes two files, makes two commits, and tags `v1.0.0`. This gives pinned-ref tests without network access. The helper sets `user.name` and `user.email` locally for the temp repo.
- HTTP fixtures: wiremock servers bound to `127.0.0.1` on an ephemeral port. Each adapter test mounts canned responses, including malformed HTML, giant bodies, redirect chains, and `text/plain` files.
- llms.txt fixtures: three real-world-shaped samples (a Mintlify-style index, an index with no links, a v2 file with `rel="alternate"` markers) plus the spec examples from llmstxt.org.
- Never commit a fixture copied from a source with unclear license. Write fixtures by hand or use public-domain text.

## Snapshot workflow

1. Write the test with `insta::assert_json_snapshot!` for JSON shapes and `insta::assert_snapshot!` for text.
2. Run `cargo insta review` after a failing snapshot run.
3. Accept a changed snapshot only after reading the diff. A silent accept hides a behavior change.
4. CI sets `INSTA_FORCE_UPDATE=0` and fails on any pending snapshot.

## Property tests (required list)

These invariants run as proptest cases. Each has a shrink-friendly design.

| Property | Module |
|---|---|
| A URL that passes the guard keeps passing after zero, one, two, and three redirects that stay in-allowlist | ingest fetch guard |
| A URL with private, loopback, link-local, or metadata IP forms always fails the guard | ingest fetch guard |
| Segmentation never produces a chunk that starts or ends inside a fenced code block | ingest segmentation |
| Chunk spans cover the document with no lost content: every in-section gap is inter-block whitespace or the configured overlap, a section boundary (the next heading) is the only other hole, and chunk bodies are byte-exact page slices, so the non-overlap spans reassemble the body byte-for-byte minus dropped chrome | ingest segmentation |
| RRF fusion output is stable under reordering of the input rungs' ties (stable sort by rank, then chunk id) | retrieve fusion |
| Budget packing never exceeds `max_tokens` and never skips rank 1 | retrieve budget |
| The query escaper produces a valid FTS5 MATCH expression for any input string | store query builder |

One clarification since the 80-token segmentation overlap landed: adjacent chunks share the overlap, so plain concatenation would duplicate it. The concatenation property therefore asserts coverage (no lost content) over byte-exact chunk bodies; the overlap regions are the only duplicated bytes.

## Coverage

- Command: `just coverage` produces `lcov.info` through cargo-llvm-cov.
- Gate: line coverage at or above 80% for `devdocs-store`, `devdocs-retrieve`, and the guard modules of `devdocs-ingest`. No gate on glue code and the CLI layer.
- CI uploads to Codecov. The badge lives in the README at M9. Coverage is a signal, not a target: a mutant-surviving line at 100% coverage is still a bug.

## Mutation testing

- Command: `just mutants`. Scope: `devdocs-ingest` parsers and guards, `devdocs-retrieve` fusion and budget, `devdocs-store` query builder.
- Schedule: monthly, and before each phase gate. Not per pull request.
- Target: mutant kill rate at or above 70% in the scoped modules. Investigate surviving mutants of the fetch guard manually. They take priority.

## Benchmarks

- Location: `crates/devdocs-retrieve/benches/query.rs` and `crates/devdocs-ingest/benches/segment.rs`.
- The query benchmark runs against a fixture DB with 20 synthetic libraries, 100k chunks total. It reports p50 and p95 for symbol lookup, FTS query, fusion, and full pipeline. The Phase 1 gate (p95 under 400 ms) is judged from this benchmark and from the eval harness (see [14](14-benchmark-eval.md)).
- Run benchmarks on the reference machine and record the machine spec in the run output. Cross-machine comparisons are not valid.

## Ignored and slow tests

Tests that need real network access, real `git clone` from a public host, or the embedding model stay behind `#[ignore]` with a reason string. A `just test-slow` recipe runs them on demand. CI never runs them. The eval harness owns all public-internet quality measurements.
