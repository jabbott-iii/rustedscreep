#!/usr/bin/env node
// PreToolUse(Edit|Write|MultiEdit) guard: keeps the pure crates pure.
//  - crates/bot-core:    no wasm-bindgen/js-sys/web-sys, no direct `screeps` crate usage
//                        (game constants come in through `game_consts`).
//  - crates/game-consts: no wasm-bindgen/js-sys/web-sys and no JS-backed screeps modules.
// Only the text being written is checked (comments are stripped first).
// Exit 2 blocks the edit; stderr is fed back to Claude.
import { readFileSync } from 'node:fs';

let input;
try {
  input = JSON.parse(readFileSync(0, 'utf8'));
} catch {
  process.exit(0);
}
const ti = input?.tool_input ?? {};
const path = String(ti.file_path ?? '').replace(/\\/g, '/');
const m = path.match(/(?:^|\/)crates\/(bot-core|game-consts)\/(.+)$/);
if (!m) process.exit(0);
const [, crate, rel] = m;

const pieces = [ti.content, ti.new_string, ...(Array.isArray(ti.edits) ? ti.edits.map((e) => e?.new_string) : [])];
const text = pieces.filter((s) => typeof s === 'string').join('\n');
if (!text) process.exit(0);

function block(reason) {
  process.stderr.write(
    `Blocked by .claude/hooks/guard-purity.mjs (${crate}/${rel}): ${reason}\n` +
      'Pure crates must stay native-testable with no JS. Put JS-facing code in crates/bot-wasm, ' +
      'or route constants through crates/game-consts. Changing this boundary needs an ADR the user accepts.\n',
  );
  process.exit(2);
}

const JS_CRATES = /\b(wasm[-_]bindgen|js[-_]sys|web[-_]sys)\b/;

if (rel === 'Cargo.toml') {
  // Strip TOML comments.
  const toml = text.replace(/#.*$/gm, '');
  if (/^\s*(wasm-bindgen|js-sys|web-sys)\s*=|\[(?:target\.[^\]]*\.)?dependencies\.(wasm-bindgen|js-sys|web-sys)\]/m.test(toml)) {
    block('adds a JS-binding dependency (wasm-bindgen / js-sys / web-sys).');
  }
  if (crate === 'bot-core' && /^\s*screeps-game-api\s*=|dependencies\.screeps-game-api\]/m.test(toml)) {
    block('bot-core must not depend on screeps-game-api directly; depend on game-consts.');
  }
  process.exit(0);
}

if (!rel.endsWith('.rs')) process.exit(0);

// Strip Rust comments (good enough for a guard: block comments, then line comments).
const code = text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '');

if (JS_CRATES.test(code)) {
  block('uses wasm_bindgen / js_sys / web_sys.');
}
if (crate === 'bot-core' && /\bscreeps\s*::|\buse\s+screeps\b|\bextern\s+crate\s+screeps\b/.test(code)) {
  block('names the `screeps` crate directly; import constants and local types from `game_consts` instead.');
}
if (
  crate === 'game-consts' &&
  /\bscreeps\s*::\s*(game|objects|memory|raw_memory|pathfinder|inter_shard_memory|console|prototypes|js_collections|traits|enums)\b/.test(code)
) {
  block('touches a JS-backed screeps module; game-consts may only re-export constants and pure local types.');
}

process.exit(0);
