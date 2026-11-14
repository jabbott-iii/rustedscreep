#!/usr/bin/env node
// PreToolUse(Bash) guard for the Screeps bot repo.
// Blocks: git history/publish operations, commands that discard uncommitted work,
// package publishing, GitHub writes, and any command that touches deployment credentials.
// With the client's GitHub link (ADR-012), anything that reaches GitHub `main` is a live
// MMO deploy, so every route that can push or create a commit on GitHub is blocked here.
// Exit 2 blocks the call; stderr is fed back to Claude so it can adjust.
// This duplicates permission deny rules on purpose: it also catches compound commands
// (`a && git commit`), `git -C dir commit`, env and wrapper prefixes (`sudo`, `env -i`,
// `timeout 5`, `xargs`), quoted or escaped command names (`"git"`, `g'i't`, `\git`),
// and commands nested in `bash -c "..."`, `sh -c '...'` or `eval ...`.
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
      'Project rule: leave changes uncommitted for user review, never publish (a push to GitHub main is a live MMO deploy, ADR-012), ' +
      'and never touch credentials. If the user explicitly wants this, ask them to run it themselves.\n',
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

const GIT_BLOCKED = new Set([
  'commit', 'push', 'reset', 'rebase', 'merge', 'tag', 'cherry-pick', 'revert', 'am',
  'filter-branch', 'filter-repo', 'update-ref', 'replace', 'clean', 'restore', 'checkout',
  'switch', 'pull', 'gc', 'prune', 'reflog', 'send-pack', 'http-push', 'commit-tree',
]);
// Global options that take a separate value argument.
const GIT_OPTS_WITH_VALUE = new Set(['-C', '-c', '--git-dir', '--work-tree', '--namespace', '--exec-path']);
// Wrappers that run the next word as a command, and their options that take a value.
// Each wrapper's options that take a separate value (flags such as `env -i` or `sudo -E` take none).
const WRAPPER_VALUE_OPTS = {
  sudo: new Set(['-u', '-g', '-C', '-h', '-p', '-U', '-r', '-t', '-T']),
  doas: new Set(['-u', '-C']),
  env: new Set(['-u', '-C']),
  nice: new Set(['-n']),
  timeout: new Set(['-k', '-s']),
  xargs: new Set(['-I', '-n', '-P', '-d', '-L', '-s', '-E', '-a']),
  stdbuf: new Set(['-i', '-o', '-e']),
  time: new Set(['-f', '-o']),
  exec: new Set(['-a']),
  command: new Set(),
  nohup: new Set(),
};
const SHELLS = new Set(['bash', 'sh', 'zsh', 'dash', 'ksh', 'fish']);
const GH_READ_METHODS = new Set(['GET', 'HEAD']);

// Strip every quote and backslash from a word, so `"git"`, `g'i't` and `\git` read as `git`.
const norm = (t) => t.replace(/["'\\]/g, '');

function checkGh(tokens, i) {
  const sub = `${tokens[i + 1] ?? ''} ${tokens[i + 2] ?? ''}`;
  if (/^(pr (create|merge|close|edit|reopen)|release (create|delete|edit|upload)|repo (create|delete|edit|sync|rename|archive|fork)|workflow (run|enable)|gist (create|edit))/.test(sub)) {
    block(`\`gh ${sub.trim()}\` publishes or changes remote state.`);
  }
  if (tokens[i + 1] === 'api') {
    const rest = tokens.slice(i + 2);
    for (let k = 0; k < rest.length; k++) {
      const t = rest[k];
      let method = null;
      if (t === '-X' || t === '--method') method = rest[k + 1] ?? '';
      else if (/^-X./.test(t)) method = t.slice(2);
      else if (t.startsWith('--method=')) method = t.slice('--method='.length);
      if (method !== null && !GH_READ_METHODS.has(method.toUpperCase())) {
        block(`\`gh api\` with method ${method || '(missing)'} writes to GitHub; a write to main is a live MMO deploy (ADR-012).`);
      }
      if (/^(-f|-F|--field|--raw-field|--input)$/.test(t) || /^(-f|-F).+|^--(field|raw-field|input)=/.test(t)) {
        // Fields make `gh api` default to POST unless an explicit read method was given.
        const explicitRead = rest.some((r, n) => ((r === '-X' || r === '--method') && GH_READ_METHODS.has((rest[n + 1] ?? '').toUpperCase())) ||
          (/^-X./.test(r) && GH_READ_METHODS.has(r.slice(2).toUpperCase())) ||
          (r.startsWith('--method=') && GH_READ_METHODS.has(r.slice(9).toUpperCase())));
        if (!explicitRead) block('`gh api` with fields or --input sends a POST that can write to GitHub (ADR-012).');
      }
    }
  }
}

function checkGit(tokens, i) {
  let j = i + 1;
  while (j < tokens.length && tokens[j].startsWith('-')) {
    if (GIT_OPTS_WITH_VALUE.has(tokens[j])) j += 2;
    else j += 1;
  }
  const sub = tokens[j] ?? '';
  const rest = tokens.slice(j + 1);
  if (GIT_BLOCKED.has(sub)) {
    // Read-only forms that are fine.
    if (sub === 'reflog' && (rest.length === 0 || rest[0] === 'show')) return;
    block(`\`git ${sub}\` changes history, publishes, or discards working-tree changes.`);
  }
  if (sub === 'subtree' && ['push', 'merge', 'pull', 'add', 'split'].includes(rest[0] ?? '')) {
    block(`\`git subtree ${rest[0]}\` changes history or publishes.`);
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

function analyze(text, depth) {
  if (depth > 4) block('command nests shells or eval too deeply to check.');
  // Nested shells: check the -c string as a command of its own (before splitting breaks the quotes).
  const nested = /(?:^|[\s;&|(`])(?:\S*[\\/])?(?:bash|sh|zsh|dash|ksh|fish)\s+(?:-[A-Za-z]+\s+)*-[A-Za-z]*c[A-Za-z]*\s+(["'])([\s\S]*?)\1/g;
  for (const m of text.matchAll(nested)) analyze(m[2], depth + 1);
  // Split into simple commands (naive on purpose: a false positive is safer than a miss).
  const segments = text.split(/&&|\|\||;|\||\n|`|\$\(|\)/).map((s) => s.trim()).filter(Boolean);
  for (const seg of segments) {
    const tokens = seg.split(/\s+/).map(norm).filter(Boolean);
    // Skip leading env assignments and wrappers, with their options (FOO=1 sudo -u x timeout 5 git ...).
    let i = 0;
    for (;;) {
      if (i < tokens.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[i])) { i++; continue; }
      const w = (tokens[i] ?? '').replace(/^.*[\\/]/, '');
      if (Object.hasOwn(WRAPPER_VALUE_OPTS, w)) {
        i++;
        while (i < tokens.length && tokens[i].startsWith('-')) {
          // `env -S "git push"` runs its value as a command line.
          if (w === 'env' && (tokens[i] === '-S' || tokens[i] === '--split-string')) analyze(tokens.slice(i + 1).join(' '), depth + 1);
          i += WRAPPER_VALUE_OPTS[w].has(tokens[i]) ? 2 : 1;
        }
        if (w === 'timeout' && /^\d/.test(tokens[i] ?? '')) i++; // timeout DURATION command
        continue;
      }
      break;
    }
    const head = (tokens[i] ?? '').replace(/^.*[\\/]/, ''); // allow /usr/bin/git
    if (head === 'eval') {
      analyze(tokens.slice(i + 1).join(' '), depth + 1);
      continue;
    }
    if (SHELLS.has(head)) {
      // Unquoted `bash -c git push`: everything after -c is the command.
      const c = tokens.findIndex((t, n) => n > i && /^-[A-Za-z]*c[A-Za-z]*$/.test(t));
      if (c > 0) analyze(tokens.slice(c + 1).join(' '), depth + 1);
      continue;
    }
    if (head === 'git') checkGit(tokens, i);
    if (head === 'gh') checkGh(tokens, i);
    if ((head === 'cargo' || head === 'npm' || head === 'pnpm' || head === 'yarn') && tokens[i + 1] === 'publish') {
      block(`\`${head} publish\` publishes a package.`);
    }
  }
}

analyze(cmd, 0);
process.exit(0);
