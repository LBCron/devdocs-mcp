---
type: guide
title: CI/CD and Release
description: The GitHub Actions workflows (CI, release, publish, audits, and the security workflows: CodeQL, Scorecard, dependency review, zizmor), the release build matrix, the crates.io publish path with trusted publishing and its `crates` environment policy, and the versioning policy. Action versions digest-pinned 2026-10-30.
tags: [devdocs, ci, github-actions, release, crates-io, security]
timestamp: 2026-11-04T14:00:00Z
resource: docs
derives-from: [docs/research]
---

# CI/CD and Release

## Workflows

Eight workflows:

| Workflow | Trigger | Purpose |
| --- | --- | --- |
| `ci.yml` | Rust workspace/build/config paths on push to `main` and pull requests; manual dispatch | fmt, lint, tests (3 OS), doc tests, msrv, features, deny, coverage, docs |
| `release.yml` | push of `v*.*.*` tags | binary build matrix, build provenance attestation, GitHub release |
| `publish.yml` | publication of a GitHub release; manual dispatch with an existing release tag | crates.io publish for the released version: no PR, no version bump, no tag |
| `audit.yml` | daily + weekly schedules, manual dispatch | cargo-deny + cargo-audit daily, cargo-machete weekly |
| `codeql.yml` | Rust/Cargo or Actions paths on push/PR, weekly schedule, manual dispatch | CodeQL SAST over Rust and workflow definitions |
| `scorecard.yml` | security/dependency configuration paths on push to `main`, branch-protection rule changes, weekly schedule, manual dispatch | OSSF Scorecard supply-chain posture, published to the OpenSSF API |
| `dependency-review.yml` | Cargo manifest, lockfile, or deny policy paths on pull requests | advisory/license diff gate on dependency changes |
| `zizmor.yml` | workflow or composite-action paths on push/PR; weekly schedule; manual dispatch | zizmor static analysis of every workflow and composite action |

Two invariants hold across all eight workflows:

- **Digest pinning.** Every action is pinned to a full commit SHA with a `# vX.Y.Z` version comment. Renovate (`github-actions.pinDigests: true`) keeps the pins current and opens a single grouped `ci: actions` PR per update batch; action majors are grouped deliberately (cheap to revert). Pinned versions verified 2026-10-30:

- `actions/checkout` v7.0.1, `actions/upload-artifact` v7.0.2, `actions/download-artifact` v8.0.2, `actions/github-script` v9.0.0
- `actions/attest-build-provenance` v4.2.2, `actions/attest` ecosystem for provenance
- `codecov/codecov-action` v7.1.1 — release creation uses the runner-preinstalled `gh` CLI, not `softprops/action-gh-release`
- `dtolnay/rust-toolchain` v1, `Swatinem/rust-cache` v2.9.3 (CI only; release builds deliberately run cacheless), `taiki-e/install-action` v2.88.0, `EmbarkStudios/cargo-deny-action` v2.1.1
- `release-plz/action` v0.5.147 (org renamed from `MarcoIeni`)
- `github/codeql-action` v4.39.1, `ossf/scorecard-action` v2.5.0, `actions/dependency-review-action` v5.0.0, `zizmorcore/zizmor-action` v0.8.0

- **Token hygiene.** Every `checkout` runs with `persist-credentials: false` — the job token is never left in the runner's git config where dependency build scripts (or a compromised step) could read it. The one exception-free rule: no step in any workflow pushes via git credentials (release-plz authenticates through its `GITHUB_TOKEN` env; release upload and attestations go through APIs).

The `msrv` and `coverage` jobs are CI-only; `just ci` covers the rest locally.

## ci.yml

All compiling jobs share one composite action, referenced with GitHub's self-repository syntax `uses: $/.github/actions/setup-rust` — "this repo, at the running commit SHA" (GA 2026-07; the runner resolves the action without checkout, which pins local actions against ref confusion, and zizmor's self-repository audit requires it over the older `./` path form). The action provides toolchain + cache + optional tools, so the preamble exists once. Every job sets `timeout-minutes`; the test matrix keeps `fail-fast: false` so one OS failing does not mask the others.

```yaml
jobs:
  fmt:        # toolchain from rust-toolchain.toml + rustfmt + just, runs `just fmt-check`
  lint:       # toolchain from rust-toolchain.toml + clippy + just, runs `just lint`
  test:       # 3-OS matrix (ubuntu/macos/windows), runs `just test`
  test-doc:   # runs `just test-doc` (cargo test --doc; nextest does not run doctests)
  msrv:       # toolchain 1.90.0, runs `just msrv`
  features:   # runs `just features` (3 feature states of the devdocs binary)
  deny:       # EmbarkStudios/cargo-deny-action
  coverage:   # cargo-llvm-cov + codecov upload (token: secrets.CODECOV_TOKEN)
  docs:       # runs `just docs` (rustdoc -D warnings)
  gate:       # aggregate check: needs every job above; require only "gate"
              # in branch protection (matrix legs publish per-OS check names)
  commits:    # PR-only: validates the PR title against the Conventional
              # Commits rule enforced by the lefthook commit-msg hook, so
              # squash merges keep a parseable commit history
```

Notes:

- All cargo-invoking recipes (`lint`, `test`, `test-doc`, `docs`, `features`) run `--locked`: CI tests the committed lockfile, not whatever cargo would resolve today. A Cargo.toml edit without a matching Cargo.lock fails loudly instead of silently regenerating the dependency set.
- `just test` / `just deny` hard-fail when their tool is missing — everywhere, with no fallback. CI installs tools explicitly via the composite action, so a missing tool is a loud environment failure, not a silent downgrade.
- The composite action's install-action tools (`just`, `cargo-nextest`, `cargo-llvm-cov`) and audit.yml's (`cargo-audit`, `cargo-machete`) deliberately float at latest: Renovate cannot bump install-action inputs, so a pin would go stale silently, and security scanners must see new releases. The release-critical toolchain (`cargo-zigbuild@0.24.0` + `ziglang==0.17.0`) is pinned where it is used; those two pins are human-maintained (no manager covers install-action inputs or inline pip pins) — review them at each release rehearsal. The Rust toolchain has a single source of truth: `rust-toolchain.toml`. The composite action resolves the channel from it when a caller omits `toolchain` (dtolnay/rust-toolchain does not read the file itself), and release.yml/publish.yml resolve the channel from the file with a dedicated step. Only the `msrv` job passes an explicit toolchain (1.90.0, the workspace `rust-version`). Renovate is configured to leave toolchain inputs alone (dep `rust` disabled) — an auto-bumped release/publish compiler desynchronized from the CI-validated one would erode the release gate; a toolchain bump edits `rust-toolchain.toml` only.
- The `msrv` recipe validates the real lock file against toolchain 1.90 (the workspace `rust-version` in `Cargo.toml`; do not restate the literal here — this file references it).
- The coverage job necessarily re-runs the full suite: instrumented builds cannot share the nextest cache profile. `cargo llvm-cov` accepts nextest as its runner if a speed-up is ever needed.
- codecov uploads carry `token: ${{ secrets.CODECOV_TOKEN }}` (optional secret, not in the setup list): codecov-action v4+ supports tokenless uploads only from fork PRs to public repos, so an unset secret degrades main-branch uploads to a logged failure — which does not fail the job (`fail_ci_if_error` is unset).
- The `features` recipe will compile the embeddings backend (ort) once the feature flags forward to the store crate; today they are empty no-ops (README "Scaffold deviations"), so the cache cost is currently bounded by compilation alone.

## release.yml (binaries)

The release builds use native runners for macOS and Windows and `cargo-zigbuild` for Linux, including musl and arm64. Both build steps pin `RUSTUP_TOOLCHAIN` to the channel from rust-toolchain.toml: without the pin, cargo re-syncs the toml's components at build time and rustup dies with `detected conflict: bin/cargo-fmt` on the legs whose matrix target is the host triple. `cross-rs` has not tagged a release since 2023, so it is not used. Cargo-dist exists but moves slowly. The hand-rolled matrix is easier to audit for this project. A workflow-level concurrency group (`release`, no cancellation) serializes tag pushes so the 7-leg matrix never runs twice concurrently.

```yaml
permissions:
  contents: read          # workflow default; only publish-github gets write

jobs:
  build:
    timeout-minutes: 90
    permissions:
      contents: read
      id-token: write      # for build provenance attestation
      attestations: write
    strategy:
      fail-fast: false
      matrix:
        include:
          - { target: x86_64-apple-darwin,        os: macos-15,       zig: false, smoke: false }
          - { target: aarch64-apple-darwin,       os: macos-15,       zig: false, smoke: true  }
          - { target: x86_64-pc-windows-msvc,     os: windows-latest, zig: false, smoke: true  }
          # Windows ARM64 is out of scope (docs 01); do not add a Windows ARM64 target.
          - { target: x86_64-unknown-linux-gnu,   os: ubuntu-latest,  zig: true,  smoke: true  }
          - { target: aarch64-unknown-linux-gnu,  os: ubuntu-latest,  zig: true,  smoke: false }
          - { target: x86_64-unknown-linux-musl,  os: ubuntu-latest,  zig: true,  smoke: true  }
          - { target: aarch64-unknown-linux-musl, os: ubuntu-latest,  zig: true,  smoke: false }
    steps:
      - checkout (fetch-depth: 0, persist-credentials: false), then verify the
        tag commit is reachable from origin/main — branch protection gates main
        pushes, not tag pushes, so without this a manually pushed tag could
        build, attest, and publish binaries from un-gated code
      - toolchain (targets: matrix.target); deliberately no cache restore:
        a release build must not consume cache entries that PR workflows
        can write (docs 09; the release is the cache-poisoning "sink")
      - [zig targets] taiki-e/install-action for cargo-zigbuild@0.24.0
        (checksum-verified); pip install --require-hashes ziglang==0.17.0 for the
        zig backend — ziglang ships only on PyPI, so the install hash-pins
        the exact wheel digests (x86_64 + aarch64) from the PyPI JSON API
        residual trust; cargo-zigbuild itself is checksummed)
      - cargo [zig]build -p devdocs --release --locked --target ${{ matrix.target }}
      - smoke test: run the built binary's `--version` (skipped where the
        runner cannot execute the target natively — x86_64-apple-darwin would
        run only under Rosetta emulation on the arm64 macos-15 runner, which
        is not a meaningful smoke of the native build, and the arm64 Linux
        targets on x86_64 runners)
      - Package: dist/devdocs-<tag>-<target>/ with the binary; .tar.gz for
        POSIX targets, .zip for Windows; per-archive .sha256 next to each
        archive (sha256sum via Git Bash on Windows and coreutils on Linux;
        shasum -a 256 on macOS, which ships no GNU coreutils)
      - actions/attest-build-provenance on dist/* — SLSA build provenance
        stored in the repo's attestation store
      - upload-artifact as dist-<target> (if-no-files-found: error)

  publish-github:
    needs: build
    timeout-minutes: 10
    permissions:
      contents: read   # gh release uses the App token, not the job token
    steps:
      - download-artifact (merge-multiple) into dist/   # no checkout: the job
        only consumes artifacts; the release step takes GH_REPO explicitly
      - smoke test: extract the linux-gnu archive, run `--version`
      - `gh release create`/`gh release upload` (runner-preinstalled CLI,
        GH_TOKEN + GH_REPO env): uploads dist/*.tar.gz, dist/*.zip,
        dist/*.sha256 with generated release notes; an existing release is
        updated in place so a re-run is idempotent
```

Notes:

- macOS runners are pinned to `macos-15` (arm64), not the floating `macos-latest` label: the smoke flags hardcode which targets the runner executes natively, and a label repoint would silently invert them. The x86_64-apple-darwin smoke is skipped deliberately — Rosetta emulation exercises an emulated, not native, binary. The Windows release leg floats on `windows-latest` (x86_64 today; its smoke flag assumes that). ci.yml's test matrix keeps floating labels deliberately: CI tracks the current runner image for coverage breadth, while release pins macOS for smoke-flag determinism. Branch protection should require only ci.yml's aggregate `gate` check — matrix legs publish per-OS check names that silently desync from a hand-maintained required-checks list.
- Windows ARM64 (`aarch64-pc-windows-msvc`) is Tier 1 since Rust 1.99 but is deliberately out of scope for Phase 1 per [01](01-tech-stack-decision.md) — the matrix must not add it.
- musl builds are fully static: bundled SQLite compiles from source, rustls needs no OpenSSL. If musl DNS becomes a problem, switch the resolver to hickory in a follow-up.
- macOS binaries ship unsigned. Gatekeeper requires a right-click open on first run. Notarization is a deliberate skip ([01](01-tech-stack-decision.md)).
- Consumers verify a binary's origin with `gh attestation verify <artifact> -R <owner>/<repo>`; the `.sha256` files alone are self-generated and prove nothing independently.

## Publish to crates.io

release-plz (CLI 0.4.2, action v0.5.147, verified 2026-10-29) publishes the workspace crates. Trusted publishing removes the long-lived crates.io API token. `publish.yml` starts only from a published GitHub release. A manual dispatch is the recovery path and must name an existing release tag. The workflow never starts from a push, never opens a release PR, never bumps a version, and never creates a tag. The 2026-11-04 reversal in [01](01-tech-stack-decision.md) records the loop that made this rule necessary.

`publish.yml` runs one job, `publish-crates`. The job refuses to publish unless all of these conditions hold:

- The ref is a well-formed `vMAJOR.MINOR.PATCH` tag.
- A non-draft GitHub release exists for the tag. A dispatch cannot publish a bare tag.
- The tag equals `v` plus the `[workspace.package] version` in `Cargo.toml`.
- The tag commit is reachable from `origin/main`.
- No crate at the manifest version already exists on crates.io (the registry never reuses a version number; the check is advisory against races — a concurrent publish between the check and release-plz still fails at crates.io).

The job runs in the `crates` environment with `contents: read`, `pull-requests: read`, and `id-token: write`. The `devdocs-testkit` crate stays `publish = false`, so release-plz skips it. The CLI crate carries no `devdocs-testkit` dev-dependency. testkit is `publish = false`, so any dev-dependency naming it must be path-only. Cargo publish keeps version-carrying dev-dependencies in the published manifest, where testkit can never resolve. The `allow-wildcard-paths` setting in deny.toml permits the path-only form without disabling the wildcard lint for version specs. A crate that needs testkit declares it path-only in its own `[dev-dependencies]`, not through the workspace table.

The release flow:

1. A human bumps `[workspace.package] version` and the internal dependency versions in `Cargo.toml`, then commits and tags that commit `vX.Y.Z`.
2. The tag push starts `release.yml`. It builds the binary matrix, then creates the GitHub release with the release App token.
3. The `release: published` event starts `publish.yml`.
4. `publish-crates` verifies the release and publishes the crates in dependency order with trusted publishing.

The App token is necessary in `release.yml`. A release created with a job's `GITHUB_TOKEN` does not start another workflow, so `publish.yml` would never run. `publish.yml` uses the read-only `github.token` and needs no write access. `release-plz.toml` disables git tags and git releases for every package, so release-plz has no path to create a tag, a release, or a PR.

A concurrency group (`publish-crates`, no cancellation) serializes publishes. Trusted publishing is release-plz's built-in OIDC exchange, so `id-token: write` is all it needs. No `rust-lang/crates-io-auth-action` step and no `CARGO_REGISTRY_TOKEN` are used.

One-time setup:

1. On crates.io, open each crate's Settings and add a Trusted Publisher: this repository, the workflow filename `publish.yml`, and the environment `crates`. A brand-new crate needs one manual first publish before it can use trusted publishing.
2. Create a GitHub App for this repository. Leave its webhook inactive. Grant repository permission `Contents: read/write`. Install it on this repository only. Create a private key.
3. Add the App **Client ID** as the repository variable `RELEASE_APP_CLIENT_ID`. Copy it from the app's About section. Client IDs look like `Iv1.xxxxxxxxxxxxxxxx` or start with `Iv23`, and the 40-character hex client secret is not the Client ID. Add the private key as the repository secret `RELEASE_APP_PRIVATE_KEY` from the whole downloaded `.pem` file. The command `gh secret set RELEASE_APP_PRIVATE_KEY < path/to/app.private-key.pem` keeps the newlines where a web-UI paste can lose them. `release.yml` checks that both values exist, then creates a short-lived installation token scoped to this repository. `publish.yml` does not use the App.
4. Create the `crates` GitHub environment (repo Settings → Environments) with a `main` branch policy and a `v*` tag policy. The release event runs on a tag ref, so a main-only policy rejects `publish-crates`. The job declares the environment. The environment claim in the Trusted Publisher entry means crates.io rejects OIDC tokens that any other job in the repo mints. That includes the `release.yml` build legs, which hold `id-token: write` for attestations and execute dependency build scripts.

If the first token mint fails with 401 `A JSON web token could not be decoded`, the two values do not belong to the same app or one of them is misshapen. Re-copy the Client ID from the app's About section. Re-set the secret from the app's current `.pem` file. Rotating a key in the app settings does not update the repository secret. A 404 on the same step means the JWT was valid but the app is not installed on this repository.

The previous design ran `release-pr` on every push to `main`. It enabled auto-merge on the generated PR and published on the merge. Each merge added a `chore: release vX.Y.Z` commit. release-plz parsed that commit as a new change and opened the next release PR. Auto-merge made the chain continuous. The push trigger is gone, the auto-merge step is gone, and the repository `Allow auto-merge` setting is disabled (2026-11-04).

## Scheduled audits

`audit.yml` runs `cargo-deny` and `cargo-audit` daily and `cargo-machete` weekly (the job-level `if:` guards compare against the exact cron strings — change them together). Runs are serialized by a concurrency group (`cancel-in-progress: false`): a manual dispatch queues behind an in-flight scheduled run instead of cancelling it, because a cancelled run reports `cancelled` — not `failure` — and its result would never reach the report job. Only scheduled failures open a `dependencies`-labeled issue; recurrences add comments to the existing issue instead of spawning duplicates (dedup paginates all open issues). Note the platform behavior: GitHub auto-disables scheduled workflows after 60 days without repository activity — Renovate's automated PRs keep this repo active, but if activity ever stops, re-enable the workflow from the Actions tab.

## Security workflows

Four workflows form the supply-chain/SAST layer. They are **advisory, not gating**: only ci.yml's `gate` job is a required branch-protection check, so a new finding can never block a PR that gate would have passed — they are meant to surface regressions, not to hold merges hostage. All four follow the repo-wide invariants (top-level `contents: read` with per-job escalation, SHA-pinned actions, `persist-credentials: false`).

- `codeql.yml` — CodeQL for `rust` and `actions` (the workflows and composite actions are themselves an attack surface). All legs use `build-mode: none`: the Rust analysis is autobuild-free (no compile needed for Rust extraction), and the Actions analysis needs no build at all. The Rust leg reuses the `$/.github/actions/setup-rust` composite action; schedule and manual dispatch exist because a push/PR-only CodeQL config silently stops analysing after the default 300 analysis-minute budget. Findings land in the Security tab via `security-events: write`.
- `scorecard.yml` — OSSF Scorecard, published to the OpenSSF API (`publish_results: true`, which requires a **public** repository and `id-token: write`; make the repo public before relying on this workflow). The job carries the official starter's `if: github.event.repository.default_branch == github.ref_name` guard, because the OpenSSF API only accepts results published from the default branch — an off-branch dispatch skips cleanly instead of failing. Runs on push to `main`, on branch-protection rule changes (`branch_protection_rule` trigger, so the score reflects protection edits promptly), and weekly. Artifacts and SARIF are uploaded for the badge and the Security tab. No `repo_token` PAT: with repository rulesets (not classic branch protection) the default `GITHUB_TOKEN` is sufficient.
- `dependency-review.yml` — PR-only advisory diff gate (`fail-on-severity: moderate`, scopes `runtime, development`, `retry-on-snapshot-warnings: true` to cover the race between the head commit's manifest scan and the check start). No dependency-submission workflow is needed: GitHub's dependency graph parses `Cargo.lock` server-side via static analysis, provided **Dependency graph** is enabled in the repository's code-security settings (it is the same setting Dependabot alerts need). It deliberately declares **no license inputs**: license policy lives in `deny.toml` as the single source of truth (docs 09), and duplicating it here would create two places to keep in sync. It complements rather than duplicates audit.yml: dependency-review catches newly *introduced* advisories and license changes on the diff, while cargo-deny/cargo-audit scan the full graph on a schedule.
- `zizmor.yml` — zizmor static analysis of every workflow and composite action at `persona: regular` (plus a weekly scheduled run that observes rule-pack drift of the floating zizmor binary independently of pushes). It runs in annotations mode (`advanced-security: false`, `annotations: true`), not the default SARIF/Advanced Security mode: with `--format=sarif` zizmor always exits 0 (findings are only communicated through the Security tab), which would make the check a no-op; annotations preserve zizmor's real exit code so findings fail the check and render inline on the PR diff. Its job needs only `contents: read`.

## Versioning policy

- SemVer. Pre-1.0: breaking changes bump the minor, everything else the patch.
- The CLI exit codes, JSON shapes, and error codes in [08](08-retrieval-interfaces.md) are public contract. Any change is a breaking change and needs a changelog note.
- The DB schema has its own compatibility rule: a binary must be able to open a DB written by the same minor series. Schema breaking changes bump the minor and run a migration.
- Workspace crates version together (release-plz single-version mode). They move as one product until any crate is consumed externally.
- crates.io never reuses a version number. Versions 0.1.0 through 0.3.8 are already published and stay on the registry after the 2026-11-04 reset. The first release after the reset must use a version above 0.3.8. Use 0.3.9 for a patch or 0.4.0 for the first deliberate minor.

## Local release rehearsal

Before tagging:

```sh
just ci
cargo build --release
./target/release/devdocs --version
./target/release/devdocs search "test" --json   # against a scratch DEVDOCS_HOME
```

Then bump `[workspace.package] version` and the internal dependency versions, commit, and push the tag:

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

`release.yml` creates the GitHub release from the tag. `publish.yml` then publishes the crates. A tag push always does both.

The first real release doubles as a rehearsal for the paths `just ci` cannot reach: the zig install on the ubuntu runner image, the `cargo-zigbuild@0.24.0` × `ziglang==0.17.0` pairing (no upstream compatibility matrix exists — record the verified pairing in the workflow comment once confirmed), the cold-cache release build (deliberately no rust-cache in release.yml), and the release.yml tag→release chain. Treat its first run as supervised.
