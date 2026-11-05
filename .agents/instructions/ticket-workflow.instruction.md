# Ticket Workflow

Run only when the user explicitly says to implement a ticket or a GitHub issue.

The orchestrator runs this pipeline. Each stage is a subagent dispatch; the orchestrator explores the repo itself only where the pipeline names it. Role definitions live in `.agents/subagents/`; use the linked file or the closest role the harness offers.

1. **Ticket intake.** Dispatch a details subagent ([ticket-details-fetcher.md](../subagents/ticket-details-fetcher.md)) to read the ticket or issue and report back in agent-friendly form: goal, checkable acceptance criteria, constraints, touched areas, and open questions.
2. **Repo exploration.** The orchestrator explores the parts of the repo relevant to the ticket — crates, `docs/implementation/` sections, related `.agents/instructions/` files — and keeps its findings tight enough to paste into subagent prompts.
3. **Research.** Dispatch parallel research subagents ([ticket-researcher.md](../subagents/ticket-researcher.md)), one per question:
   - related changes already in the repo's history,
   - the design docs that own the touched areas,
   - existing solutions in widely used, actively maintained open-source projects (`/agent-reach`).
4. **Planning.** Dispatch a planner subagent ([ticket-planner.md](../subagents/ticket-planner.md)) with the requirements, the repo context, and the research. It must return a step-by-step implementation plan: steps with files, tests, and commit split.
5. Critical concerns or conflicts? Stop here and use the `grill-me` skill — or the closest conflict-resolution process the harness offers — to reach a common understanding with the user before anyone writes code.
6. **Implementation.** Create a branch from `main` and dispatch one or more implementer subagents ([ticket-implementer.md](../subagents/ticket-implementer.md)) to make surgical writes per the plan. Run implementers in parallel only when they touch disjoint files — no file may receive writes from two subagents at once. Parallel implementers share the checkout's Git index — give each parallel implementer its own worktree (`wt switch --create <branch>`, AGENTS.md §Worktrees) and return its commits to the ticket branch with `wt merge --no-squash`. Sequence everything else. Split the work into commits per AGENTS.md §Git commits; each commit stays green.
7. **Plan check.** Dispatch a checker subagent ([ticket-plan-checker.md](../subagents/ticket-plan-checker.md)) with the plan and the branch diff. It verifies every plan requirement is addressed and reports `SUCCESS` or a misalignment list.
8. **Adversary pass.** Dispatch an adversary subagent ([ticket-adversary-reviewer.md](../subagents/ticket-adversary-reviewer.md)) with the same plan. It reports `SUCCESS` or a misalignment list.
9. **Iterate.** On any `MISALIGNED` report, dispatch fix subagents ([fix-implementer.md](../subagents/fix-implementer.md)), then re-run stages 7 and 8. Repeat until both report `SUCCESS`.
10. **Code review.** Dispatch a review subagent ([ticket-code-reviewer.md](../subagents/ticket-code-reviewer.md)) running the `code-review` skill to compare the branch with `main`. From its report, dispatch one or more fix subagents ([fix-implementer.md](../subagents/fix-implementer.md)) for the issues found. After each fix batch, re-run stages 7 and 8 on the changed branch, then dispatch the reviewer again. Iterate until the review is clean and stages 7 and 8 both report `SUCCESS` on the final branch state.
11. **Hand-off.** Stop and hand the changes to the user. The user reviews and decides what to do next.
12. When the user confirms they are happy with the changes, do all four: push the branch, link it to the ticket or issue, apply the `in-progress` label to that ticket or issue, and raise the PR per `pr-creation.instruction.md`.

**Done means**: the plan check and the adversary pass both returned `SUCCESS`, the code-review issues are fixed, and the branch is ready for the user's review. After the user confirms: the branch is pushed, the ticket or issue carries the PR link and the `in-progress` label, and the PR follows the PR creation contract.
