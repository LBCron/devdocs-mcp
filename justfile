# Hooks, CI, and agent shells run without the user's profile: rustup installs
# cargo, rustup, and the binstall tools under ~/.cargo/bin, so prepend it once
# for every recipe instead of per command.
home_dir := if env_var_or_default("HOME", "") != "" {
    env_var("HOME")
} else if env_var_or_default("USERPROFILE", "") != "" {
    env_var("USERPROFILE")
} else {
    error("HOME and USERPROFILE are not set")
}
export PATH := home_dir / ".cargo" / "bin" + ":" + env_var_or_default("PATH", "")

set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Format the workspace.
fmt:
    cargo fmt --all

# Check formatting without writing.
fmt-check:
    cargo fmt --all --check

# Lint every target. Warnings are errors. --locked: CI tests the committed
# lockfile, not whatever cargo would resolve today.
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Lint and apply machine-applicable fixes.
lint-fix:
    cargo clippy --workspace --all-targets --fix --allow-dirty

# Run tests with nextest. A missing tool fails loudly everywhere: CI installs
# it explicitly, so a missing tool means the install step failed.
test:
    @if command -v cargo-nextest >/dev/null 2>&1; then \
        cargo nextest run --profile ci --workspace --all-targets --no-tests=pass --locked; \
    else \
        echo "cargo-nextest not installed: cargo binstall --no-confirm cargo-nextest"; \
        exit 1; \
    fi

# Run doc tests. (`--all-targets` would exclude them; unit tests run in `test`.)
test-doc:
    cargo test --workspace --doc --locked

# Run the #[ignore]d tests (network, embedding model) on demand. CI never
# runs them; see docs/implementation/04 §Ignored and slow tests.
test-slow:
    cargo test --workspace --all-targets --locked -- --ignored

# Build API docs. Warnings are errors.
docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

# License, advisory, ban, and source checks.
deny:
    @if command -v cargo-deny >/dev/null 2>&1; then \
        cargo deny check; \
    else \
        echo "cargo-deny not installed: cargo binstall --no-confirm cargo-deny"; \
        exit 1; \
    fi

# RustSec advisory scan.
audit:
    @if command -v cargo-audit >/dev/null 2>&1; then \
        cargo audit; \
    else \
        echo "cargo-audit not installed: cargo binstall --no-confirm cargo-audit"; \
        exit 1; \
    fi

# Codacy static analysis locally, against the main-branch diff. Mirrors the
# Opengrep patterns of the "Codacy Static Code Analysis" PR check (Codacy
# runs no clippy for Rust). Optional gate: skipped cleanly when the CLI or
# its repo config is absent, findings fail with exit 1. Not part of `just ci`
# and not a CI job — the cloud app already runs the same patterns on PRs
# (gap named per the agent-ready-repo contract).
codacy:
    @if ! command -v codacy-analysis >/dev/null 2>&1; then \
        echo "codacy-analysis not installed (optional gate, skipped): pnpm add -g @codacy/analysis-cli (fallback if PATH is not refreshed: pnpm exec codacy-analysis)"; \
    elif [ ! -f .codacy/codacy.config.json ]; then \
        echo "codacy-analysis not initialized (optional gate, skipped): codacy-analysis init --remote gh iamwatchdogs devdocs-mcp"; \
    elif [ -z "$(git rev-parse -q --verify origin/main 2>/dev/null)" ]; then \
        echo "codacy gate skipped: origin/main not found to diff against"; \
    else \
        CODACY_DISABLE_UPDATE_CHECK=1 codacy-analysis analyze --no-log --tool Semgrep --diff origin/main; \
    fi

trivy:
    @if ! command -v trivy >/dev/null 2>&1; then \
        echo "trivy not installed (optional gate, skipped): pnpm i -g trivy"; \
    else \
        trivy fs . --scanners vuln,secret,misconfig --skip-dirs target; \
    fi

# Unused dependency scan.
machete:
    @if command -v cargo-machete >/dev/null 2>&1; then \
        cargo machete; \
    else \
        echo "cargo-machete not installed: cargo binstall --no-confirm cargo-machete"; \
        exit 1; \
    fi

# Line coverage (lcov).
coverage:
    @if command -v cargo-llvm-cov >/dev/null 2>&1; then \
        cargo llvm-cov --workspace --lcov --locked --output-path lcov.info; \
    else \
        echo "cargo-llvm-cov not installed: cargo binstall --no-confirm cargo-llvm-cov"; \
        exit 1; \
    fi

# Check the workspace against the MSRV toolchain.
msrv:
    @if rustup toolchain list | grep -q '^1\.90'; then \
        cargo +1.90.0 check --workspace --all-targets --locked; \
    else \
        echo "MSRV toolchain 1.90.0 missing: rustup toolchain install 1.90.0"; \
        exit 1; \
    fi

# Compile the binary and the ingest crate in every feature state CI checks.
# The ingest crate is checked, not built: only its feature resolution is
# at stake, and the grammar-less state must stay compilable.
features:
    cargo build --locked -p devdocs --no-default-features
    cargo build --locked -p devdocs --no-default-features --features exact-tokens
    cargo build --locked -p devdocs --no-default-features --features embeddings
    cargo check --locked -p devdocs-ingest --no-default-features
    cargo check --locked -p devdocs-ingest --features symbols

# Run the default eval suite and print the gate table (docs/implementation/14).
# A gate that is not PASS exits 2 — the honest state until the retrieval
# core lands at M4.
eval:
    cargo build --locked -p devdocs
    cargo run -p devdocs-eval -- run pilot

# Copy-paste detection across non-Rust sources.
#
# Owned by Codacy — the cloud app runs jscpd via a Codacy tool integration.
# No longer run as a local pre-push gate. If a Codacy jscpd tool is registered
# later, its config would live in .codacy/codacy.config.json.

# Trivy filesystem scan for dependency and config findings.
#
# Owned by Codacy — the cloud app runs Trivy as a Codacy tool with six
# configured patterns (Trivy_malicious_packages, Trivy_secret,
# Trivy_vulnerability_critical/high/medium/minor) in
# .codacy/codacy.config.json. No local recipe or CI job.

# Public API break detection between releases.
semver:
    @if command -v cargo-semver-checks >/dev/null 2>&1; then \
        cargo semver-checks --release; \
    else \
        echo "cargo-semver-checks not installed: cargo binstall --no-confirm cargo-semver-checks"; \
        exit 1; \
    fi

# Mutation testing.
mutants:
    @if command -v cargo-mutants >/dev/null 2>&1; then \
        cargo mutants --workspace --exclude devdocs-testkit; \
    else \
        echo "cargo-mutants not installed: cargo binstall --no-confirm cargo-mutants"; \
        exit 1; \
    fi

# Benchmarks.
bench:
    cargo bench --workspace

# Full local gate. Mirrors CI.
ci:
    @just fmt-check
    @just lint
    @just test
    @just test-doc
    @just docs
    @just deny
    @just features
