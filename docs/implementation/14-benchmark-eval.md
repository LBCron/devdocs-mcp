---
type: guide
title: Benchmark and Eval Harness
description: The golden-set format, the eval harness implementation, the gate metrics with thresholds, run artifacts, and the regression policy that wires the research benchmark protocol into devdocs.
tags: [devdocs, eval, benchmark, golden-set, gates]
timestamp: 2026-11-03T00:00:00Z
resource: docs
derives-from: [docs/research]
---

# Benchmark and Eval Harness

The research bundle states that no controlled docs-domain benchmark exists, and it prescribes the protocol that closes that gap (research: benchmark-protocol.md). This file wires that protocol into the repository as a runnable harness. The harness measures the retrieval core. It does not replace the unit tests in [04](04-testing-strategy.md).

## Components

- `crates/devdocs-eval` — the harness binary. Reads a suite, runs the retrieve crate, scores results, writes artifacts.
- `eval/golden/*.jsonl` — golden question files, one suite per file. A `<suite>.meta.json` sidecar next to a suite records the curation minutes.
- `eval/runs/<date>-<suite>/` — run artifacts. Gitignored.
- `eval/baseline/*.json` — the standing Phase 0 baseline records (schema rent, per-query cost of the current docs workflow) that each run report carries.
- `just eval` — runs the default pilot suite through the harness binary and prints the gate table. A gate that is not PASS exits 2, so the recipe reports the honest state until the retrieval core lands at M4 ([13](13-implementation-plan.md)).

## Golden-set format

One JSON object per line:

```json
{
  "id": "nextjs-proxy-rewrite-001",
  "library": "next.js",
  "version": "16.2.9",
  "task_type": "exact_api",
  "question": "How do I rewrite an incoming request path in Next.js middleware?",
  "expect_refs": ["docs://next.js/v16.2.9/proxy#rewrite"],
  "expect_symbols": ["middleware"],
  "grader": "ref_match",
  "notes": "accepts the Proxy section; the middleware page alone is a partial"
}
```

Field rules:

- `task_type` comes from the research 8-category union (research: benchmark-protocol.md Step 3): `exact_api`, `how_to`, `discovery`, `migration`, `multi_library`, `agent_task`, `unanswerable`, `poisoned`.
- `expect_refs` pins the acceptable `docs://` refs. `expect_symbols` pins required symbols for symbol-recall scoring. One of the two is required.
- `grader` is `ref_match` (default), `symbol_match`, or `manual`. `unanswerable` questions expect an empty result set. A hit is a failure.
- Version is exact. A question without a pinned version is invalid input for the harness.

## Pilot (Phase 0)

Build a 20-question suite across 3 libraries before writing retrieval code. This calibrates curation effort and produces the baseline the gates need (research: architecture.md Phase 0). Suggested start: Next.js, Zod, and one library the project's own lockfiles use. Include at least 2 `unanswerable` questions. Record curation minutes in the run report. The research protocol treats that cost as a measurement, not trivia.

## Harness flow

```text
devdocs-eval [--json] run <suite> [--db PATH]
  1. load suite, validate every question (version pinned, refs well-formed)
  2. for each question: run the retrieve pipeline, record hits, ranks, timings
  3. score per question, aggregate per task_type and overall
  4. write eval/runs/<date>-<suite>/results.json + report.md
  5. print the gate table; exit 0 when all gates PASS, 2 on any FAIL
```

`--json` is a top-level flag and precedes the subcommand. The harness binary is `devdocs-eval`. When the CLI subcommand wiring lands at M10 ([12](12-roadmap-milestones.md)), `devdocs eval run` proxies to it with the same flags. Exit codes: 0 every gate PASS; 1 invalid input (a broken suite, an unknown flag); 2 any gate FAIL or INCONCLUSIVE; 3 operational failure (a missing `devdocs` binary, I/O, a `devdocs search` call that exits 1, 3, or 4, or overshoots its 30-second deadline). Exit 2 is a valid timed miss (docs/implementation/08 §Error codes).

Timing note: the harness measures the in-process pipeline plus, in a second pass, the CLI subprocess end-to-end. The p95 gate is judged on the subprocess pass, cold-start excluded after one warmup call. Until M4, the CLI `search` path is a placeholder. The p95 row then times process start and dispatch only, not retrieval. Every timed call must exit 0; a failed or stalled call fails the run with exit 3 instead of contributing its wall time to the gate.

## Metrics

| Metric | Definition | Phase 1 gate |
|---|---|---|
| hit@5 | Fraction of questions with at least one expected ref in the top 5 results | ≥ 0.8 |
| symbol recall | Fraction of `expect_symbols` present in the symbol rung results for exact-lookup questions | ≥ 0.95 |
| p95 latency | 95th percentile of CLI subprocess wall time across the suite. Until M4, the CLI `search` path is a placeholder, so the row times process start and dispatch only | < 400 ms |
| cost per query | Model cost of the retrieval path (always $0 locally. Ingest embedding cost counted in Phase 3) | $0 |
| failure rate | Fraction of questions with a wrong or partial result under the binary grader | baseline for the −49% Phase 2 delta |
| version correctness | Fraction of results with `version_status = verified` where an exact version was requested | ≥ 0.99 |

The binary grader: partial counts as wrong. The research protocol is explicit (research: benchmark-protocol.md Step 6).

Denominators: hit@5 counts only questions with non-empty `expect_refs`; symbol recall counts pinned symbols; failure rate and version correctness count every question — an abstention with no results is not version-correct. The harness grades `ref_match` and `symbol_match`; a suite that carries the `manual` grader makes a run exit 1 until manual grading lands.

## Gate table

The harness prints this table and writes it into `report.md`:

| Gate | Threshold | Phase |
|---|---|---|
| Symbol recall | ≥ 0.95 | 1 |
| hit@5 | ≥ 0.8 | 1 |
| p95 latency | < 400 ms | 1 |
| Cost per 1,000 queries | ≤ $0.20 frontier-class | 3 |
| Retrieval failures | −49% vs the M4 baseline | 2 |
| Version correctness | ≥ 0.99 | 1 |

A FAIL blocks the phase transition ([12](12-roadmap-milestones.md)). An INCONCLUSIVE (suite too small for the margin) is also a block. The protocol forbids widening the margin to fit the data (research: benchmark-protocol.md Step 2).

## Run artifacts

Each run writes:

- `results.json` — one row per question per run: id, rank list, timings, per-metric score, index build hash (library versions and commit shas of the corpus used).
- `report.md` — the gate table plus per-task-type breakdown.
- `manifest.json` — tool version, DB file hash, suite hash, host, date. A third party can reconstruct the denominator from it (research: benchmark-protocol.md Step 4 criterion).

The corpus identity matters: every result names the exact corpus it ran against. A gate claim without the manifest is not a claim.

## Regression gate

Before any re-indexed content ships from an ingest change:

1. Run the affected libraries' suites. The new index must not regress hit@5 or symbol recall on them.
2. Run the full default suite on a schedule (weekly in CI, on a self-hosted runner with the fixture DB rebuilt from recorded fixtures. Real-network rebuilds stay manual `#[ignore]`-style, run per release).
3. A regression blocks the release PR, same as a failing test.

This reproduces Context7's benchmark loop locally: questions score the index, and bad content fails the gate before it serves (research: architecture.md §L3, context7-internals.md).

## Scaling up

The pilot and Phase 1 suites are small. Before any external adoption claim, grow to the hardened sizing from the research protocol: 50 tasks per category across 6 categories, 300 tasks, pre-registered margins, paired cluster bootstrap (research: benchmark-protocol.md Steps 2 and 7). Also add the interface comparison arms (CLI versus MCP versus a hosted incumbent) from Step 5, each against the shared retrieval core, with the incumbent run in its own labeled comparison.

## Standing re-measurement

Re-run the Phase 0 probe quarterly and on the reversal conditions in [01](01-tech-stack-decision.md). The quarterly run records: baseline cost per query of the current workflow, schema-rent bands, and any movement in the competitive boundary (research: verdict.md, reversal conditions).
