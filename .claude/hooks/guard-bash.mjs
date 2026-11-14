#!/usr/bin/env node
// PreToolUse(Bash) guard for the Screeps bot repo.
// Blocks: git history/publish operations, commands that discard uncommitted work,
// package publishing, and any command that touches deployment credentials.
// Exit 2 blocks the call; stderr is fed back to Claude so it can adjust.
// This duplicates permission deny rules on purpose: it also catches compound
// commands (`a && git commit`), `git -C dir commit`, and env-prefixed forms.
import { readFileSync } from 'node:fs';

let input;
try {
  input = JSON.parse(readFileSync(0, 'utf8'));
} catch {
  process.exit(0); // malformed input: let the normal permission flow decide
}
const cmd = String(input?.tool_input?.command ?? '');
if (!cmd.trim()) process.exit(0);

function block(reason) {
  process.stderr.write(
    `Blocked by .claude/hooks/guard-bash.mjs: ${reason}\n` +
      'Project rule: leave changes uncommitted for user review and never touch credentials. ' +
      'If the user explicitly wants this, ask them to run it themselves.\n',
  );
  process.exit(2);
}

// 1. Credentials: the deploy config and token variables.
if (/(^|[\s"'=/\\])\.screeps\.ya?ml\b/.test(cmd)) {
  block('command references .screeps.yaml (deployment credentials). Deploy via `npm run deploy`, which reads it internally.');
}
if (/\$\{?SCREEPS_(TOKEN|PASSWORD|AUTH)\w*/.test(cmd)) {
  block('command expands a Screeps credential environment variable.');
}

// 2. Split into simple commands (naive on purpose: a false positive is safer than a miss).
const segments = cmd.split(/&&|\|\||;|\||\n|`|\$\(|\)/).map((s) => s.trim()).filter(Boolean);

const GIT_BLOCKED = new Set([
  'commit', 'push', 'reset', 'rebase', 'merge', 'tag', 'cherry-pick', 'revert', 'am',
  'filter-branch', 'filter-repo', 'update-ref', 'replace', 'clean', 'restore', 'checkout',
  'switch', 'pull', 'gc', 'prune', 'reflog',
]);
// Global options that take a separate value argument.
const GIT_OPTS_WITH_VALUE = new Set(['-C', '-c', '--git-dir', '--work-tree', '--namespace', '--exec-path']);

for (const seg of segments) {
  const tokens = seg.split(/\s+/);
  // Skip leading env assignments and wrappers (FOO=1 sudo command nice git ...).
  let i = 0;
  while (i < tokens.length && (/^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[i]) || ['sudo', 'command', 'nice', 'env', 'time', 'exec'].includes(tokens[i]))) i++;
  const head = (tokens[i] ?? '').replace(/^.*[\\/]/, ''); // allow /usr/bin/git

  if (head === 'git') {
    let j = i + 1;
    while (j < tokens.length && tokens[j].startsWith('-')) {
      if (GIT_OPTS_WITH_VALUE.has(tokens[j])) j += 2;
      else j += 1;
    }
    const sub = tokens[j] ?? '';
    const rest = tokens.slice(j + 1);
    if (GIT_BLOCKED.has(sub)) {
      // Read-only forms that are fine.
      if (sub === 'reflog' && (rest.length === 0 || rest[0] === 'show')) continue;
      block(`\`git ${sub}\` changes history, publishes, or discards working-tree changes.`);
    }
    if (sub === 'stash' && !['list', 'show'].includes(rest[0] ?? '')) {
      block('`git stash` hides or drops the user\'s uncommitted work.');
    }
    if (sub === 'branch' && rest.some((t) => /^-(d|D|m|M|f|c|C)$|^--(delete|move|force|copy)$/.test(t))) {
      block('`git branch` with a delete/move/force flag.');
    }
    if (sub === 'worktree' && ['remove', 'prune'].includes(rest[0] ?? '')) {
      block('`git worktree remove/prune` can discard work.');
    }
    if (sub === 'config' && !rest.some((t) => ['--get', '--list', '-l', '--get-all'].includes(t))) {
      block('`git config` writes are the user\'s call.');
    }
  }

  if (head === 'gh') {
    const sub = `${tokens[i + 1] ?? ''} ${tokens[i + 2] ?? ''}`;
    if (/^(pr (create|merge|close|edit)|release (create|delete|edit)|repo (create|delete|edit))/.test(sub)) {
      block(`\`gh ${sub.trim()}\` publishes or changes remote state.`);
    }
  }

  if ((head === 'cargo' || head === 'npm' || head === 'pnpm' || head === 'yarn') && tokens[i + 1] === 'publish') {
    block(`\`${head} publish\` publishes a package.`);
  }
}

process.exit(0);
