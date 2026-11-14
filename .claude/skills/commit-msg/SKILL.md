---
name: commit-msg
description: Propose a professional commit message for the current uncommitted work. Never runs git commit and adds no Co-Authored-By or other trailers. Use at module or phase checkpoints.
allowed-tools: Bash(git status *), Bash(git diff *)
---

# /commit-msg

1. Run `git status --short` and `git diff --stat`. Run `git diff` on files you need to understand. These are read-only commands.
   If this isn't a git repository, say so and draft the message from this session's work.
2. Draft one Conventional Commits message:
   - Subject: `type(scope): summary`. Use imperative mood and at most 72 characters. Types: feat, fix, perf, refactor, test, docs, build, chore.
     Scopes follow crate or module names (`core/planner`, `wasm/snapshot`, `loader`, `docs`).
   - Body: what changed and why, wrapped at 72 characters. Mention measured CPU or size effects only if they were measured this session.
     Note breaking changes to persisted formats or config with a `BREAKING CHANGE:` paragraph.
   - **No** `Co-Authored-By`, sign-off, or tool-attribution lines.
3. If the diff mixes unrelated work, propose a split with one message per group, and list the files for each.
4. Print the message in a fenced block. Do **not** run `git add` or `git commit`. Leave the changes uncommitted.
