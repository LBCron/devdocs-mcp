# Agent-Ready Repo Contract

For changes to AGENTS.md, hooks, or the instruction layer itself.

- `AGENTS.md` stays short (<200 lines): commands, layout, pointers. Instructions are how to decide; `docs/implementation/` is the decided design; neither duplicates the other or AGENTS.md.
- A new check lands in the justfile AND CI in the same change. `just ci` covers everything except the two CI-only jobs (`msrv`, `coverage`); name any further gap in the same change.
- Commits follow the Conventional Commits shape that the commit-msg hook enforces (AGENTS.md §Environment carries the definition — keep it there).
- Hooks delegate to justfile recipes, so hook flags match CI exactly. `pre-commit` runs `cargo fmt` (auto-staging fixes — expect your files reformatted mid-commit) and clippy `-D warnings`. `pre-push` runs the CI-facing gate: `fmt-check`, `lint`, `test`, `test-doc`, `docs`, and `deny` when dependencies changed. `pre-merge-commit` runs `just ci`. A hook failure is fixed (`just lint-fix` for machine-applicable clippy fixes), never skipped.
- An intentional divergence between `docs/implementation/` and the repo gets exactly one row in the README "Scaffold deviations" table, marked current-state or target-state. When the deviation resolves — code catches up or the doc changes — the row goes in the same change.
- Milestone references use the M-numbers from `docs/implementation/12`. Translate "block N" comments to their M-numbers when you touch the line.
- The change leaves the gate no weaker than it found it. Know which checks are hollow: `just features` and `just eval` prove compilation only until their wiring lands (README deviations).

**Done means**: a fresh agent can clone the repo and get `just ci` green from AGENTS.md alone.
