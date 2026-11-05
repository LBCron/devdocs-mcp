# devdocs-mcp

Local, read-only documentation server for coding agents, exposed over MCP with a SQLite-backed index and hybrid retrieval.

> **Status: M3b (llms.txt hardening).** The CLI shell with config commands, the store layer, and the llms.txt ingest path (fetch guard, license gate, parser, segmentation, v2 link relations, subpath scoping, the llms-full probe, chrome strip, exact dedup, `devdocs add --fixture`) are real; retrieval, symbols, and the MCP server land in later milestones (docs/implementation/12). See "Scaffold deviations" for where the repo intentionally differs from the docs.

## Prerequisites

| Tool | Version | Install |
|---|---|---|
| rustup | latest | <https://rustup.rs> |
| Rust | 1.99.1 (pinned by `rust-toolchain.toml`) | `rustup toolchain install 1.99.1` |
| git | 2.40+ | system package |
| just | 1.59.0 | `brew install just` |
| lefthook | 2.2.2 | `brew install lefthook` |
| cargo-binstall | 1.26.1 | `brew install cargo-binstall` |
| `~/.agents/skills` | latest | Global skills directory carrying `/caveman`, `/ponytail`, and `/agent-reach`, which the repo's agent instructions route subagents through |

Dev tools used by optional recipes:

```sh
cargo binstall --no-confirm cargo-nextest cargo-deny cargo-audit \
  cargo-machete cargo-llvm-cov cargo-mutants cargo-semver-checks
```

## Install

Build from source. The binary lands at `target/release/devdocs`.

```sh
cargo build --release --locked
```

Install into `~/.cargo/bin`:

```sh
cargo install --path crates/devdocs --locked
```

Activate git hooks once per clone:

```sh
lefthook install
```

## Quickstart

```sh
devdocs --version
devdocs config init
```

The license gate is deny-by-default, so register the source in the config
file that `config init` wrote, then run the rest:

```toml
[sources.example]
kind = "llms_txt"
url = "https://example.test/llms.txt"
status = "approved_index"
```

```sh
devdocs add https://example.test/llms.txt --fixture <fixture-dir>
devdocs list
```

Config commands and `devdocs add --fixture` are available. Retrieval, symbols, and the MCP server land in later milestones (docs/implementation/12). Crate-level docs: `just docs`.

## Workspace layout

| Path | Role |
|---|---|
| `crates/devdocs-model` | Core types, config, errors. No workspace dependencies |
| `crates/devdocs-store` | SQLite schema, migrations, queries. Owns `fastembed` behind `embeddings` |
| `crates/devdocs-ingest` | Source adapters, fetch guard, normalization, symbols |
| `crates/devdocs-retrieve` | Hybrid search, fusion, budget packing |
| `crates/devdocs-mcp` | MCP server adapter over `devdocs-retrieve` |
| `crates/devdocs` | `devdocs` binary: CLI plus MCP entry point |
| `crates/devdocs-eval` | Golden-set evaluation harness |
| `crates/devdocs-testkit` | Shared test helpers and fixtures |
| `crates/context-chunker` | Internal helper: chunking utilities (outside the devdocs graph) |
| `crates/context-file` | Internal helper: file utilities (outside the devdocs graph) |
| `migrations/` | Embedded SQL migrations |
| `eval/golden/` | Golden question files (JSONL) |
| `docs/` | Implementation guide and research bundle |

## Dev commands

| Command | What it does |
|---|---|
| `just` | List recipes |
| `just fmt` | `cargo fmt --all` |
| `just fmt-check` | Check formatting without writing |
| `just lint` | Clippy, all targets, warnings denied |
| `just lint-fix` | Clippy with machine-applicable fixes |
| `just test` | `cargo nextest run --workspace --all-targets` |
| `just test-doc` | `cargo test --workspace --all-targets` (includes doc tests) |
| `just docs` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` |
| `just deny` | Licenses, advisories, bans, sources (`deny.toml`) |
| `just audit` | RustSec advisory scan |
| `just machete` | Unused dependency scan |
| `just coverage` | `cargo llvm-cov` to `lcov.info` |
| `just msrv` | `cargo +1.90.0 check --workspace --all-targets --locked` |
| `just features` | Build `devdocs` with no features, `exact-tokens`, `embeddings` |
| `just eval` | Eval harness smoke run |
| `just semver` | Public API break detection |
| `just mutants` | Mutation testing |
| `just bench` | `cargo bench --workspace` |
| `just ci` | `fmt-check` + `lint` + `test` + `docs` + `deny` + `features` |

Optional recipes print an install hint and exit non-zero when the external tool is missing — locally and inside `just ci` alike. A missing tool is never silently downgraded: CI installs its tools explicitly, so a missing tool there means the install step failed.

## Scaffold deviations

Intentional deviations between the implementation docs and the repository. Resolved conflicts live in the owning docs, not here.

| Doc says | Repo does | Why |
|---|---|---|
| Structured logs via `tracing` and `tracing-subscriber` ([01](docs/implementation/01-tech-stack-decision.md)) | `Reporter` writes plain lines to stderr under `--verbose` | The subscriber crate is not in the lock yet; add it when the ingest layer emits structured events |
| Help groups subcommands under headings ([08](docs/implementation/08-retrieval-interfaces.md)) | Only `Global options` gets a heading | clap 4.6.7 `next_help_heading` applies to arguments, not subcommands |

## License

MIT. See `LICENSE`.
