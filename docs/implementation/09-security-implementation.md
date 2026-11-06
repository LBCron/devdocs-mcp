---
type: guide
title: Security Implementation
description: The mapping from the research threat model to concrete controls in code, the fetch-guard invariants and tests, injection-filter operations, storage controls, and the security checklist.
tags: [devdocs, security, ssrf, injection, licensing]
timestamp: 2026-11-07T03:05:11Z
resource: docs
derives-from: [docs/research]
---

# Security Implementation

Documentation content is an untrusted input channel (research: security-licensing.md §1). The domain is narrow and read-only, which makes the mitigation set tractable. This file maps each mitigation to a module and states the invariants that tests enforce.

## Control map

| Threat (research: security-licensing.md §1) | Control | Module |
|---|---|---|
| Indirect prompt injection through crawled content | Injection filter, quarantine-on-flag | devdocs-ingest |
| Corpus poisoning (PoisonedRAG class) | License gate + deny-by-default registry + operator-controlled sources | devdocs-ingest |
| Registry metadata poisoning | Config is local and operator-owned. No remote registries in v1 | devdocs-model |
| SSRF, DNS rebinding, redirects | Fetch guard (below) | devdocs-ingest |
| Parser attacks (XML entities, YAML tags, `$ref` cycles) | oas3 parser limits, serde_yaml_ng safe loading, cycle detection with warnings | devdocs-ingest |
| Git hook / filter execution during clone | Clone with hooks and filters disabled (below) | devdocs-ingest |
| Secret leakage through docs | Redaction pass at ingest and output | devdocs-ingest, devdocs |
| Read-only scope breach | The tool never writes outside `DEVDOCS_HOME` and temp dirs. No shell execution of fetched content | whole binary |

## Read-only and data-not-instructions

- The tool never executes fetched content. Code examples render as data. No auto-run, no shell-out with doc content, ever (research: security-licensing.md §2 item 5).
- Retrieved text is data. The MCP envelope marks it: results are content, and the server instructions tell the model that doc text cannot change tool policy. The CLI result envelope includes no model-facing directives at all (research: security-licensing.md §2 item 2).
- ANSI and control sequences are stripped from chunk text at ingest. Terminal output re-escapes anything that looks like an escape sequence (research: security-licensing.md §2 item 3).

## Fetch guard invariants

The guard is the only network code. Enforced properties, each with a property test ([04](04-testing-strategy.md)):

1. Scheme is HTTPS. Everything else fails before DNS.
2. URL has no userinfo. URL normalizes through the `url` crate before checks.
3. DNS resolves through the pinned resolver. The connection opens only to the resolved address, with SNI intact (reqwest 0.13 `dns_resolver` hook, verified 2026-10-28). Ambient proxies are disabled on the guard client (`no_proxy`): a proxy would connect to its own address and resolve hostnames itself, which bypasses the pinned resolver and the peer check.
4. The resolved IP passes the blocklist check at connect time: private, loopback, link-local, multicast, unspecified, documentation (`2001:db8::/32`, `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`), and cloud metadata ranges (`169.254.169.254/32` and equivalents). Beyond the listed set, the blocklist also refuses fail-closed: shared address space (`100.64/10`), the benchmarking range (`198.18.0.0/15`), `0/8`, and the reserved block (`240/4`).
5. Redirects re-run checks 1 to 4 on every hop. Limit 3. A redirect hop whose host resolves to only refused addresses reports the same refusal (`FETCH_BLOCKED`) as a refused initial URL.
6. Byte cap 20 MiB applies to decompressed bytes, enforced by a wrapping reader, not by trusting Content-Length.
7. Timeout 15 s per request. The initial DNS lookup shares the same budget, so a stalled resolver surfaces as a per-request timeout, not as a total-cap trip. Total ingest job wall time caps at 600 s by config; the cap bounds the fetch-and-segment work, and the publish runs after it and reports its own result, so a timed-out run never reports failure while a generation turns active.
8. Fail closed: any guard error aborts the whole source ingest, not just the failing request.

The pinned resolver returns one address per host lookup. DNS rebinding fails because the connect-time IP is the checked IP.

The blocklist has no config escape. The one exception is in test code: the guard constructor behind the wiremock doubles admits local HTTP test doubles (loopback host, http or https scheme, any port) so offline tests can exercise the live request path on `127.0.0.1`. Production paths never enable it, and a unit test pins the default off.

## Git subprocess safety

- argv arrays only. No shell, no string concatenation into commands.
- `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1` on every invocation.
- Clone with hooks and filters disabled: `git clone --config core.hooksPath=/dev/null --config filter.lfs.process= --filter=blob:none` where supported, and a docs-only sparse checkout for large repos (deferred; recorded in [07](07-ingest-pipeline.md) §Git adapter). A malicious repo cannot run checkout hooks through this path.
- Remote URLs come from config or CLI, never from fetched content. A source URL that changes between plan and fetch aborts the run. Transport URLs are passed to git with the operator's own privilege and no scheme is restricted — `file://` stays allowed for fixtures and tests, so an `ext::` URL would run a command with operator privilege; the operator's own config and CLI are the trust boundary.
- The temp clone directory sits under `DEVDOCS_HOME/tmp` with mode 0700 and is removed after the run.

## Injection filter operations

The filter flags. It does not silently rewrite (research: security-licensing.md §2 item 3. [07](07-ingest-pipeline.md) carries the rule table):

- Quarantined chunks store a `quarantine` flag and stay out of all serving paths. The exclusion is a store-layer guarantee, not a query-layer filter.
- `devdocs db quarantine list` shows flagged chunks with rule names. `devdocs db quarantine release <chunk_id>` moves the chunk back after operator review, with an audit line in `meta`.
- Rule recall beats precision in v0. A false positive costs one manual review. A false negative costs one poisoned context. Tune toward recall, log everything.

## Secrets and redaction

- A redaction pass runs at ingest over chunk text: common token shapes (`ghp_…`, `sk-…`, AWS key shapes, PEM blocks, Bearer headers) are replaced with `[REDACTED:<kind>]` before persistence.
- The same pass runs over output rendering as a second gate.
- Logs carry provenance, not user queries (research: security-licensing.md §2 item 8). `--verbose` prints queries locally. Nothing leaves the machine in v1 because there is no telemetry.

## Local storage controls

From the research local-profile controls (research: security-licensing.md §2):

- `DEVDOCS_HOME` created with mode 0700. `docs.db` with 0600. The store verifies modes at open and warns on drift.
- Symlink check: the store refuses a DB path that is a symlink into another user's territory (ownership check on the resolved path).
- Secure temp cleanup: `tmp/` contents removed on process exit, including the SIGINT/SIGTERM path.
- Network filesystems: the store refuses a DB path on a known network filesystem — matched by `statfs` filesystem type (NFS, SMB/CIFS, AFS, NCP, CODA, 9P, OCFS2) plus network-backed FUSE subtypes such as sshfs, whose FUSE magic hides them from the type list — unless `--allow-network-fs` is set, because SQLite locking over network FS is unreliable.

## License gate enforcement points

The state machine is defined in [07](07-ingest-pipeline.md). The SQLite store is authoritative for the mutable license state; the config `[sources.<name>]` entry is declarative only (docs/implementation/07 §License gate). Enforcement is layered so a bug in one place cannot serve a denied source:

1. The ingest planner refuses sources below `approved_fetch`.
2. The store refuses to flip a source to `active` below `approved_index`, and publishing a terminal license state (`denied`, `expired`, `revoked`) deactivates the source, withdrawing it from search. The publish transaction re-reads the stored license states inside its own commit: a terminal state on any row of the library deactivates every row of the library, and that deactivation commits before the refusal returns, so a revocation that lands while a re-sync is in flight wins over the stale record the caller holds.
3. The retrieve layer filters on the stored license state at query time. A source whose stored state is terminal drops out of results at the next query: `search` drops it silently, and a direct-name query or `open` answers `NOT_AVAILABLE` with `miss_reason: license_blocked`. No re-sync is needed — the check reads the store, so `devdocs source deny` reaches the query at once. The same read paths run the expiry sweep: a passed `sources.license_expires_at` flips the rows to `expired` and appends an audit line with the operator `auto:expiry` (docs/implementation/07 §Expiry sweep). An `add` run for a URL whose gate refusal fires withdraws the published generation before the error returns; the withdrawal is mandatory, and when the store cannot record it the command returns `UPSTREAM_ERROR` instead of `FETCH_BLOCKED`. The discovery surfaces (`list`, `versions`) report the indexed state and do not refuse a terminal library — only the content paths (`search`, `open`, `verify`) filter — so terminal content never serves while an operator can still discover what is blocked.
4. Output rendering enforces the snippet-vs-full rule per source state. Only `approved_snippet` serves verbatim text; no state grants a full-document render.

All four points are implemented, each with its own tests (docs/implementation/13 §M7).

## Code Mode sandbox (deferred)

Code Mode ships only after the full sandbox contract passes (research: security-licensing.md §4): fail-closed feature detection, rootless process with seccomp and namespaces, brokered capability tokens, network disabled by default, hard resource limits, redacted telemetry, kill switch. The core exposes no code-mode surface today. Any future work starts from that contract as acceptance criteria, not as hardening TODOs.

## Security checklist for releases

Run before each tagged release:

1. `cargo audit` clean, or every advisory triaged with a note.
2. `cargo deny check` passes with the allow lists.
3. The fetch-guard property tests pass on all targets.
4. No new dependency entered the tree without the checklist in [10](10-dependency-audit.md).
5. The injection-filter rule set has a review note in the release PR: rules added, removed, or tuned.
6. `grep -r "unwrap(" crates/ --include="*.rs" -l` returns only test files (the lint denies it. Verify manually).
7. The release binary runs the CLI end-to-end suite from a clean `DEVDOCS_HOME` in a temp dir.
