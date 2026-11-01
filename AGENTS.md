# AGENTS.md

This project is a local, read-only documentation MCP server for coding agents. It exposes a documentation index (SQLite-backed) with hybrid retrieval over MCP, so agents can query one source instead of crawling docs sites.

> Note that this project is at its very beginning stage, so you are allowed to make major refactoring and changes across the source code.
>
> **Repo state:** scaffold stubs at milestone M0; the build order lives in `docs/implementation/12` and `13`. The `.agents/instructions/` files state contracts in "must" form — they bind code as it lands, and describe current behavior only where the code exists.

## Tooling

### Environment

- `cargo` is the build/test runner; the toolchain is pinned by `rust-toolchain.toml`.
- `just` is the task runner. All primary commands are aggregated within the justfile; `just` with no target prints the full list.
- `lefthook` runs the git hooks; they delegate to justfile recipes, so a hook failure means the same command CI runs failed. Run `lefthook install` after changing `lefthook.yml`.
- Its commit-msg hook enforces Conventional Commits: `type(scope): subject`, scope optional, subject ≤ 72 chars.
- `just test` runs nextest with `.config/nextest.toml` (`retries = 0`, `fail-fast = false`): a failing test surfaces immediately and is never retried.
- Use `bash` as your primary terminal and `rg` instead of plain `grep`.
- Optional cargo tools (`nextest`, `cargo-deny`, `cargo-audit`, `cargo-machete`, `cargo-llvm-cov`, `cargo-mutants`, `cargo-semver-checks`) are installed via `cargo binstall`.
- `just test` and `just deny` hard-fail when nextest/cargo-deny are missing (with a binstall hint). CI installs the tools explicitly, so a missing tool there means the install step failed and fails the job loudly; there is no fallback.
- The dev toolchain is pinned to 1.99.1, but code must compile on MSRV 1.90 (`just msrv`): no std APIs newer than 1.90.

### Commands

Run everything through the justfile. `just ci` mirrors CI except two CI-only jobs, `msrv` and `coverage` — run `just msrv` after touching dependencies or using newer std APIs.

Here're the commands that you need for every task,

| Task | Command |
| --- | --- |
| Formatting | `just fmt` |
| Format check | `just fmt-check` |
| Lint (warnings are errors) | `just lint` |
| Tests (nextest) | `just test` |
| Doc tests | `just test-doc` |
| API docs (warnings are errors) | `just docs` |
| Dependency checks (deny) | `just deny` |
| Full local gate (CI adds msrv + coverage) | `just ci` |

Style config lives in `rustfmt.toml` and `Cargo.toml` — read them, do not restate them.

## Workspace Layout

Crate sources live under `crates/`: `devdocs` (CLI/MCP binary), `devdocs-model`, `devdocs-ingest`, `devdocs-store`, `devdocs-retrieve`, `devdocs-mcp`, `devdocs-eval`, `devdocs-testkit`, plus `context-*` helpers. Design and testing strategy docs live under `docs/implementation/`.

## Agent conduct

These apply to every task:

- Study official sources first — library docs, wikis, changelogs. Use the Context7 MCP tools for official documentation when they are available; add other relevant sources on top.
- Dispatch subagents for leg work. Every task gets at least one subagent for research, exploration, or verification before the main work.
- Write all communication and documentation in ASD-STE100: replies to the user, commit messages, design docs, and everything you publish on GitHub — PRs, issues, and review comments.
- Label new issues with the ticket-set pattern: `ready-for-agent`, one `phase-*`, and the relevant `area:*` labels.

## Instructions

`.agents/instructions/` holds per-concept guidelines. Read only the file matching the task at hand:

| When the task involves… | Read `.agents/instructions/…` |
| --- | --- |
| Adding a crate, placing code, or crate dependency rules | `cargo-workspaces.instruction.md` |
| Modeling error types, handling failures, or exit codes | `error-handling.instruction.md` |
| Logging, `--json` output, or MCP stream correctness | `observability.instruction.md` |
| Async/Tokio code, SQLite access, or blocking work | `async-and-performance.instruction.md` |
| Adding or upgrading a dependency, or feature gating | `dependency-discipline.instruction.md` |
| Changing AGENTS.md, hooks, or the instruction layer | `agent-ready-repo.instruction.md` |
| Creating a pull request | `pr-creation.instruction.md` |
| Reviewing review-bot comments on a PR (the user says "babysit the PR") | `pr-babysitting.instruction.md` |
| The user says to implement a ticket or a GitHub issue | `ticket-workflow.instruction.md` |

Some tasks point into the design docs instead — they are the deeper source of truth:

| When the task involves… | Read |
| --- | --- |
| Writing or changing tests (offline rule, insta snapshots, proptest) | `docs/implementation/04-testing-strategy.md` |
| Lint policy or lint churn on toolchain bumps | `docs/implementation/03-code-quality-toolchain.md` |
| Changing the schema, migrations, or hashing policy | `docs/implementation/06-data-model.md` |
| Changing ingest, fetching, or content filtering | `docs/implementation/07-ingest-pipeline.md` (points into 09 for security) |
| Changing retrieval behavior, CLI flags, or MCP tool shapes | `docs/implementation/08-retrieval-interfaces.md` |
| Cutting a release, targets, or publishing | `docs/implementation/11-ci-cd-release.md` |
| Network access, the fetch guard, or security boundaries | `docs/implementation/09-security-implementation.md` |
| Eval suites, golden questions, or gate thresholds | `docs/implementation/14-benchmark-eval.md` |

## Git commits

Apply to every commit you make, always — read this before committing anything:

- Never squash a whole feature, fix, ticket, or GitHub issue into a single big commit. Split the work into meaningful commits — one per logical step (e.g. model/types, store/schema, retrieval, CLI/MCP wiring, docs).
- Each commit must keep the application in a working state: it compiles and `just test` stays green at that commit. Never leave a commit half-migrated or mid-refactor.
- Commit messages follow one unified structure:

  ```text
  <type>(<scope>): <imperative subject, ≤ 72 chars>

  <optional body: what changed and why, wrapped ~ 72 chars>

  Co-authored-by: ZCode (GLM-5.3-Flash) <noreply@z.ai>
  ```

- `<type>(<scope>)` is the Conventional Commits shape enforced by the commit-msg hook (§Environment). Omit the scope when the change spans multiple crates.
- When an agent harness produced the change, credit the harness and the driving model as a `Co-authored-by:` trailer, in the `Harness (model) <email>` form above; keep one trailer per harness/model pair.

### Using the design docs and keeping them in sync

`docs/implementation/` holds the decided design — why the repo is shaped the way it is. `00-implementation-guide.md` is its index: the file map, the reading order, and the change procedure. When you build in an area for the first time, read the doc that owns it before writing code.

The docs are not a historical record; they must not drift from the code:

- When your change invalidates what a doc says — a new crate, a changed schema, a modified exit-code table, a replaced tool — update that doc in the same change as the code.
- Follow the change procedure in `00-implementation-guide.md` §Change procedure: bump the changed file's `timestamp` header, and record architecture reversals in `01-tech-stack-decision.md`.
- Do not restate doc content in code comments, instruction files, or here. Point at the doc section instead.
