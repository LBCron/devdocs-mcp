---
type: guide
title: Code Quality Toolchain
description: The pinned toolchain, formatter policy, lint table, MSRV policy, docs policy, and the pull-request checklist for devdocs. Tool versions verified 2026-10-28.
tags: [devdocs, toolchain, rustfmt, clippy, msrv]
timestamp: 2026-11-02T00:00:00Z
resource: docs
derives-from: [docs/research]
---

# Code Quality Toolchain

## Tool versions

All tools verified 2026-10-28. Install dev tools with `cargo binstall` where a prebuilt binary exists.

| Tool | Version | Purpose |
|---|---|---|
| rustc | 1.99.1 (pinned) | Compiler |
| rustfmt | ships with toolchain | Format |
| clippy | ships with toolchain | Lint |
| cargo-nextest | 0.9.150 | Test runner |
| cargo-deny | 0.20.2 | License, advisory, source checks |
| cargo-audit | 0.22.2 | RustSec advisory scan |
| cargo-semver-checks | 0.51.0 | Public-API break detection for the lib crates |
| cargo-mutants | 27.1.0 | Mutation testing |
| cargo-machete | 0.9.2 | Unused dependency scan |
| cargo-llvm-cov | 0.9.1 | Coverage |
| insta | 1.49.0 | Snapshot tests |
| just | 1.59.0 | Task runner |
| lefthook | 2.2.2 | Git hooks |

Maintenance notes from verification:

- criterion moved to the `criterion-rs` organization and is healthy again (0.8.2, commits through 2026-10).
- cargo-husky is unmaintained since 2020. lefthook replaced it here.
- Both cargo-machete and cargo-udeps are alive. machete is faster and stable-channel-only, so this project uses machete.

## Formatter policy

`rustfmt.toml` contains one line:

```toml
style_edition = "2024"
```

All other settings stay at the stable defaults. Two useful options (`imports_granularity`, `group_imports`) are nightly-only and would force a nightly rustfmt in CI. Do not enable them. Import order stays manual but tidy: std, external crates, workspace crates, with a blank line between groups.

Run `just fmt` before every commit. CI runs `just fmt-check` and blocks on drift.

## Lint table

The workspace lint table lives in the root `Cargo.toml` (see [02](02-project-scaffolding.md)). This section explains each choice.

| Lint | Level | Reason |
|---|---|---|
| `unsafe_code` (rust) | forbid | No exceptions in workspace code — `forbid`, not `deny`, because a local `#[allow(unsafe_code)]` overrode `deny` and carried raw libc FFI to the Codacy PR check during M2. Bundled SQLite and rustls cover the FFI surface, and the store's storage controls read `statfs(2)`/`geteuid(2)` through the safe `nix` wrappers (docs/implementation/09 §Local storage controls) |
| `missing_docs` (rust) | warn | Public API of lib crates needs doc comments |
| clippy `all` | warn | Default correctness and style set |
| clippy `pedantic` | warn | High-value extra checks. Noise is handled with targeted allows |
| clippy `unwrap_used`, `expect_used` | deny | A docs tool must not panic on untrusted input. Use typed errors |
| clippy `dbg_macro` | deny | Debug leftovers must not merge |
| clippy `todo` | warn | Visible in review. Convert to an issue before merge |
| `module_name_repetitions` | allow | Crate names repeat by design (`devdocs_store::Store`) |
| `missing_errors_doc`, `missing_panics_doc` | allow | Error docs live in the error enum, not per function |

Test-code exception. `unwrap_used` and `expect_used` stay deny at the workspace level, but each crate adds this to its `tests/` integration modules and `#[cfg(test)]` blocks:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]
```

Place the allow at the top of test files only. Never in library code.

Clippy churn note. Clippy 1.91 through 1.99 renamed and moved several lints (for example `empty_enum` to `empty_enums`). When the pinned toolchain advances, run `cargo clippy --workspace --all-targets` and fix renamed keys in `[workspace.lints]` first. A renamed key produces a warning, not a silent skip.

## Toolchain and MSRV policy

- `rust-toolchain.toml` pins the exact dev toolchain (1.99.1). CI uses the same pin.
- `rust-version = "1.90"` is the MSRV (root `Cargo.toml`), enforced by the `just msrv` recipe against toolchain 1.90.0.
- CI runs one MSRV job: toolchain 1.90.0 with `cargo check --workspace --all-targets --locked`. The MSRV-aware resolver (stable since Cargo 1.84) picks compatible dependency versions.
- MSRV bumps are minor releases, not breaking. Bump only when a required dependency forces it. Record the bump in the release notes.
- The pinned toolchain advances on a schedule: at most once per quarter, with a full CI run and a clippy-key check.

## Doc policy

- `cargo doc --workspace --no-deps -D warnings` must pass. CI runs it.
- Every public item in a lib crate has a doc comment with at least one example or one sentence of intent.
- Every crate root module has a crate-level doc that states the responsibility and the dependency rules from [02](02-project-scaffolding.md).
- Internal docs use `//!` module docs over inline noise. A comment states a constraint the code cannot show. Nothing else.

## Git hooks

`lefthook.yml` (see [02](02-project-scaffolding.md)) runs three gates, and every hook command delegates to a justfile recipe so hook flags (e.g. `--locked`) cannot drift from CI:

- `pre-commit` — fast, per commit: `cargo fmt` with auto-staged fixes, and clippy `-D warnings` via `just lint`. Both are gated to staged Rust and TOML files so docs-only commits skip them.
- `commit-msg` — the Conventional Commits regex (also enforced on PR titles by CI, see the `commits` job).
- `pre-push` — the CI-facing gate, once per push before workflows trigger: `fmt-check`, `lint`, `test`, `test-doc`, `docs`, plus `deny` only when `Cargo.toml`/`Cargo.lock` changed against the push target, plus `codacy` (see below). This is the backstop that catches doc warnings, test failures, and shared-code lints before CI runs.
- `pre-merge-commit` — the full `just ci` gate, because a merge commit lands on the target branch directly and per-commit checks cannot cover its combined state.

`just codacy` runs the Codacy analysis CLI locally (`@codacy/analysis-cli`) with Opengrep over the main-branch diff — the same pattern engine behind the "Codacy Static Code Analysis" PR check, which for Rust is Opengrep only (Codacy runs no clippy). It is an optional gate, unlike the required tools: when `codacy-analysis` or its repo config (`.codacy/codacy.config.json`, generated once by `codacy-analysis init`) is absent, the check skips cleanly; findings fail the push. It is not part of `just ci` and not a CI job — the cloud app already analyzes every PR, so a CI job would duplicate it (gap named per the agent-ready-repo contract). The gate mirrors the local exclusion policy in `.codacy.yml`, including the `.github/**` exclusion: workflow security is owned by zizmor and CodeQL, and Codacy's pinning pattern misreads local `$`-prefixed action refs.

What hooks deliberately do not cover: code behind Linux-only `#[cfg]` gates is invisible to a macOS host, the msrv and coverage jobs are CI-only, and load-sensitive test races only reproduce on loaded CI runners. Review-tool classes also stay out of reach: CodeRabbit's semantic findings (edge-case regressions, mount-lookup shadowing, fail-open branches) are AI review, Codacy's Lizard lexer bug and Duplication/Complexity advisories are that platform's own non-gating metrics, and none of them have a mechanical local equivalent. Those stay with the review bots and CI.

## Pull-request checklist

The author confirms each item before requesting review. The CI gates make most items mechanical.

1. `just ci` passes locally.
2. New public API has doc comments and appears in the crate docs.
3. New behavior has at least one test that fails without the change.
4. No new dependency without the checklist in [10](10-dependency-audit.md).
5. No lint allow without a comment that states the reason and the issue link.
6. Commit messages follow Conventional Commits.
7. If the change touches the CLI surface or the result shape, update [08](08-retrieval-interfaces.md) and add a snapshot test.
8. If the change touches the schema, add a migration (see [06](06-data-model.md)). Never edit an applied migration.

## Editor setup

rust-analyzer with default settings is sufficient. Recommended workspace settings (`.vscode/settings.json`):

```json
{
  "rust-analyzer.check.command": "clippy",
  "rust-analyzer.cargo.features": "all",
  "editor.formatOnSave": true
}
```

`check.command = "clippy"` surfaces lint findings inside the editor at the same level CI uses.
