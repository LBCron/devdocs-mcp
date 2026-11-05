# PR Babysitting

Run only when the user explicitly asks to review the comments on a PR or to babysit the PR.

Review bots (CodeRabbit, Codacy, and similar) add comments on recently pushed changes. The orchestrator works the comments through subagents; it judges and fixes nothing by itself without a subagent report.

1. **Read all bot comments, one by one.**
2. **Validate.** Dispatch parallel validation subagents ([pr-claim-validator.md](../subagents/pr-claim-validator.md)) — one per claim — to judge each claim: legit, false positive, or does not apply here. Validators dispatch their own `/agent-reach` research subagents for external facts. The orchestrator spot-checks each verdict against the cited evidence before it acts.
3. **Fix stage.** Implement a fix for every legit concern through fix subagents ([fix-implementer.md](../subagents/fix-implementer.md)). Run fix subagents in parallel only when they touch disjoint parts of the repo — no file may receive writes from two subagents at once; sequence everything else. Split a concern into logical-step commits per AGENTS.md §Git commits; each commit stays green.
4. **Major-refactor route.** When a legit fix needs a major refactor that reaches across the repo, hand it to one independent subagent. That subagent must not commit to the PR branch: it creates a new branch off the PR branch, implements the refactor there, and raises a stacked PR that targets the existing PR. The orchestrator replies on the bot conversation with the stacked-PR link and marks the conversation resolved only when the stacked PR is up. The orchestrator keeps the main PR moving on the remaining concerns.
5. **Review the fixes** with the `code-review` skill — or the closest review process the harness offers — before you push.
6. **Push the fixes.**
7. **Reply on each bot conversation.** Start the reply with "This is a legit claim…", "This is a false positive…", or "This does not apply here…". Write the reply in ASD-STE100. Then mark the conversation as resolved.

**Done means**: every bot conversation carries a verdict reply and is resolved, every legit claim has a pushed fix commit, and every major-refactor concern carries a stacked-PR link.
