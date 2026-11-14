#!/usr/bin/env node
// SessionStart hook (startup / resume / compact): re-injects the project position so
// long multi-phase work survives /clear and compaction. Stdout becomes context.
// Fails silently: a missing file must never break a session.
import { readFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const root = process.env.CLAUDE_PROJECT_DIR || process.cwd();
const out = [];

function head(file, maxLines) {
  const p = join(root, file);
  if (!existsSync(p)) return null;
  try {
    const lines = readFileSync(p, 'utf8').split(/\r?\n/);
    const shown = lines.slice(0, maxLines).join('\n');
    return lines.length > maxLines ? `${shown}\n… (${lines.length - maxLines} more lines in ${file})` : shown;
  } catch {
    return null;
  }
}

const status = head('docs/STATUS.md', 60);
if (status) out.push('## Project position (docs/STATUS.md, head)', status);

try {
  const logPath = join(root, 'docs/VERIFICATION_LOG.md');
  if (existsSync(logPath)) {
    const rows = readFileSync(logPath, 'utf8').split(/\r?\n/).filter((l) => l.startsWith('|'));
    const open = rows.filter((l) => /\|\s*(unverified|VERIFY)\s*\|/i.test(l) && !/superseded/i.test(l)).length;
    out.push(`Verification log: ${open} open (unverified/VERIFY) rows. Verify before coding against them (/verify).`);
  }
} catch {
  /* ignore */
}

out.push(
  'Reminders: smallest complete change; no git commits/history changes (offer /commit-msg); never read .screeps.yaml; ' +
    'verify game rules before use; report only checks you actually ran.',
);

process.stdout.write(out.join('\n\n') + '\n');
process.exit(0);
