---
paths:
  - "js/**"
  - "package.json"
  - "rollup.config.*"
  - "babel.config.*"
  - "tsconfig*.json"
---

# TypeScript loader and tools (`js/`)

## Scope
- The loader may only: gate boot on bucket, compile and instantiate the wasm across ticks if needed, call the exported
  `loop`, detect and recover from traps, route `console.error` and notifications, and expose debug console commands.
  It contains **no game logic**.
- Tools (Node, local only): `deploy`, `console` (stream the server console to `logs/`), `stats` (fetch the profiler/telemetry segment).

## Recovery requirements (ADR-003)
- Handle three failure modes:
  1. A JS exception from a Rust trap.
  2. A tick aborted by the CPU limit. Detect it with a "running" flag left set at the start of the next tick.
  3. A compile or instantiate failure.
- Compare two strategies and choose one by measured cost:
  1. Drop the poisoned instance and its wasm-bindgen glue, then re-instantiate from the cached `WebAssembly.Module` on the next tick.
  2. `Game.cpu.halt()`, which costs a full global reset and a recompile.
  The starter uses option 2 because of wasm-bindgen issue #3130.
- Never crash-loop. Back off after repeated failures, send one notification per incident, and keep the bot recoverable without human action.
- Measure the cold-start cost (compile + instantiate + first tick) on each target server. Set the bucket boot gate from that
  measurement and record it in TUNING.md. The starter's 1500 was only a starting point; the MMO gate is 480 since the P1 gate (V150).

## Bundle
- Transpile to the JS level the probe measured on the server (ADR-008): `tsc` to ES2022 on the MMO (VERIFICATION_LOG V72), with no Babel. The optional tracks re-probe their own servers (PS1, SE1).
- Bundle nothing at runtime besides the generated wasm-bindgen glue. Keep the bundle small and record its size and the
  wasm size in STATUS at each phase gate.
- If the probe shows `TextDecoder`/`TextEncoder` are missing, add the smallest polyfill the glue needs.

## Secrets
- Deploy tools read `.screeps.yaml` at runtime. Never log, echo, or print its contents, and never write or commit it. Prefer
  auth tokens over passwords for official servers. `.screeps.yaml` stays in `.gitignore`.
