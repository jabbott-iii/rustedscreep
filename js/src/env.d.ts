// Globals of the Screeps runtime that the loader uses. This replaces lib "DOM" on purpose: TextDecoder
// and TextEncoder are undefined on the MMO (VERIFICATION_LOG V72), fetch and document are assumed absent
// (V95), and DOM types would hide that. Extend this file only with globals the runtime is known to provide.

/**
 * CommonJS require: for a binary module it returns the module's bytes (V31); for a text module, its module.exports
 * (the wasm text module exports a base64 string, ADR-015); a missing one throws (V96).
 */
declare function require(module: string): unknown;

/** Rebuilt before every tick, with only log among the methods the loader uses (V96). */
declare const console: {
  log(...args: unknown[]): void;
};

declare const Game: {
  readonly time: number;
  readonly cpu: {
    readonly bucket: number;
    readonly limit: number;
    readonly tickLimit: number;
    getUsed(): number;
    /** Resets the global; discards the current tick's intents and Memory writes (V96). Not on every server. */
    halt?: () => void;
  };
  notify(message: string, groupInterval?: number): void;
};

/** Parsed on first access in a tick and saved at its end; mutate its fields, never reassign it (V96). */
declare const Memory: Record<string, unknown>;

type BufferSource = ArrayBufferView | ArrayBuffer;

declare namespace WebAssembly {
  class Module {
    constructor(bytes: BufferSource);
  }
}
