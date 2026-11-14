// Globals of the Screeps runtime that the loader uses. This replaces lib "DOM" on purpose: TextDecoder
// and TextEncoder are undefined on the MMO (VERIFICATION_LOG V72), fetch and document are assumed absent
// (V95), and DOM types would hide that. Extend this file only with globals the runtime is known to provide.

/** CommonJS require: for a binary module it returns the module's bytes (V31). */
declare function require(module: string): unknown;

declare const console: {
  log(...args: unknown[]): void;
};

type BufferSource = ArrayBufferView | ArrayBuffer;

declare namespace WebAssembly {
  class Module {
    constructor(bytes: BufferSource);
  }
}
