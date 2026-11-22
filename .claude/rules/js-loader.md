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
- The loader may only: gate boot on bucket (the game's store of unused CPU), compile and instantiate the wasm (WebAssembly)
  across ticks (game turns) if needed, call the exported `loop`, detect and recover from traps (wasm runtime errors that abort
  the call), route `console.error` and notifications, and expose debug console commands.
  It contains **no game logic**.
- Tools (Node, local only, `js/tools/`): the build, `stage`, `size`, `check`, and the sandbox sims (Node scripts that run the build
  in a model of the game). The API tools `deploy`, `console`, and `stats` were dropped (architecture decision record ADR-021, which
  takes live evidence from the user's console output instead).

## Recovery requirements (ADR-003)
- Handle three failure modes:
  1. A JS exception from a Rust trap.
  2. A tick aborted by the CPU limit. Detect it with a "running" flag left set at the start of the next tick.
  3. A compile or instantiate failure.
- Compare two strategies and choose one by measured cost. ADR-003 chose a hybrid before any measurement: option 2 is the default
  (it recovered from 10 forced traps in a row on the MMO, V148), and option 1 stays behind a config flag, off by default. Option 1
  becomes the default only if its own MMO trap-test shows it correct over 10 traps in a row and cheaper than a halt plus a recompile:
  1. Drop the poisoned instance and its wasm-bindgen glue (the generated JS bindings), then re-instantiate from the cached
     `WebAssembly.Module` on the next tick.
  2. `Game.cpu.halt()`, which costs a full global reset (a fresh JS global, with all heap state lost) and a recompile.
  The starter (screeps-starter-rust, the template bot) uses option 2 because of wasm-bindgen issue #3130.
- Never crash-loop. Back off after repeated failures, send one notification per incident, and keep the bot recoverable without human action.
- Measure the cold-start cost (compile + instantiate + first tick) on each target server. Set the bucket boot gate from that
  measurement and record it in TUNING.md. The starter's 1500 was only a starting point; the gate on the MMO (the official persistent
  server) is 480 since the P1 gate, the build-and-loader phase's exit check (V150: from bucket 480 every boot tick already gets the
  full 500-CPU tick limit).

## Bundle
- Transpile to the JS level the probe (a console snippet that tests what the server accepts) measured on the server (ADR-008):
  `tsc` to ES2022 on the MMO (VERIFICATION_LOG V72: all 21 syntax checks through ES2022 compiled), with no Babel. The optional
  tracks re-probe their own servers (PS1, the private server's setup, and SE1, the Seasonal server's verification).
- Bundle nothing at runtime besides the generated wasm-bindgen glue. Keep the bundle small and record its size and the
  wasm size in STATUS at each phase gate.
- If the probe shows `TextDecoder`/`TextEncoder` are missing, add the smallest polyfill the glue needs.

## Secrets
- No tool reads `.screeps.yaml` (ADR-021). Never read, log, echo, or print its contents, and never write or commit it.
  `.screeps.yaml` stays in `.gitignore`.
