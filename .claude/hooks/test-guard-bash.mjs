#!/usr/bin/env node
// Regression test for guard-bash.mjs: feeds commands to the hook and checks exit codes (2 = block, 0 = allow).
// The block list covers every route to GitHub found in the 2026-11-13 review (ADR-012: a push to main is a live MMO deploy).
// Usage: node .claude/hooks/test-guard-bash.mjs
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HOOK = join(dirname(fileURLToPath(import.meta.url)), 'guard-bash.mjs');
const BLOCK = [
  // Existing coverage (must stay blocked).
  'git push', 'git commit -m x', 'git -C /x push', 'cd a && git push origin main', 'FOO=1 git push', '/usr/bin/git push',
  'git reset --hard', 'git stash', 'git branch -D x', 'git config user.name x', 'gh pr create', 'gh repo delete x',
  'npm publish', 'cargo publish', 'cat .screeps.yaml', 'echo $SCREEPS_TOKEN', 'git worktree remove x',
  // Quoted or escaped command names.
  '"git" push', "g'i't push", '\\git push', "'git' 'push'",
  // Nested shells and eval.
  'bash -c "git push"', "sh -c 'git commit -m x'", 'bash -lc "cd a && git push"', 'zsh -c "git push"', '/bin/bash -c "git push"',
  'bash -c git push', 'eval git push', 'eval "git push origin main"', 'bash -c "bash -c \'git push\'"',
  // Wrappers and their options.
  'xargs git push', 'echo x | xargs -I{} git push', 'env -i git push', 'env -u HOME git push', 'sudo -u me git push', 'sudo -E git push',
  'timeout 10 git push', 'timeout -k 5 10 git push', 'nohup git push', 'nice -n 5 git push', 'stdbuf -o L git push', 'env -S "git push"',
  // Git plumbing that publishes or writes history.
  'git send-pack origin main', 'git http-push x', 'git commit-tree HEAD^{tree}', 'git subtree push --prefix deploy origin main',
  // GitHub writes (a write to main is a live MMO deploy).
  'gh api -X PUT repos/o/r/contents/deploy/main.js -f message=x', 'gh api --method=POST repos/o/r/git/refs', 'gh api -XPATCH repos/o/r',
  'gh api repos/o/r/contents/x -F content=@f', 'gh api repos/o/r/merges --input body.json', 'gh api -X DELETE repos/o/r/git/refs/heads/x',
  'gh api repos/o/r -f name=x', 'gh api --raw-field=x=y repos/o/r', 'bash -c "gh api -X PUT repos/o/r/contents/x"',
  'gh pr merge 1', 'gh repo sync', 'gh workflow run deploy.yml', 'gh gist create f', 'gh release upload v1 f',
];
const ALLOW = [
  'git status', 'git status --short', 'git diff --stat', 'git log --oneline -3', 'git ls-files', 'git show HEAD:docs/STATUS.md',
  'git -C /x status', 'git reflog', 'git config --get user.name', 'git ls-remote --symref origin HEAD', 'git check-ignore -v deploy/x',
  'gh api repos/o/r', 'gh api -X GET repos/o/r/commits', 'gh api --method GET repos/o/r', 'gh api -XGET repos/o/r -f per_page=1',
  'gh pr view 1', 'gh pr list', 'gh repo view',
  'npm run stage', 'npm run build', 'npm run check', 'node js/probe/check.mjs', 'cargo test -p rustedscreep-core',
  'bash -c "ls -la"', 'sh -c \'echo hi\'', 'echo "git push is blocked"', 'grep -n "git push" CLAUDE.md', 'cat docs/STATUS.md | grep push',
  'env FOO=1 node x.mjs', 'sudo -u me ls', 'timeout 5 node x.mjs', 'xargs -n 1 echo',
];

let failures = 0;
const run = (cmd) => spawnSync('node', [HOOK], { input: JSON.stringify({ tool_input: { command: cmd } }), encoding: 'utf8' }).status;
for (const c of BLOCK) { const s = run(c); if (s !== 2) { failures++; console.log(`MISS (exit ${s}) should block: ${c}`); } }
for (const c of ALLOW) { const s = run(c); if (s !== 0) { failures++; console.log(`FALSE POSITIVE (exit ${s}) should allow: ${c}`); } }
console.log(`${BLOCK.length} block cases, ${ALLOW.length} allow cases, ${failures} failures`);
process.exit(failures ? 1 : 0);
