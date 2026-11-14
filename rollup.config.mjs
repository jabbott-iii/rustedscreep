// Bundles the tsc output (js/build) and the wasm-bindgen glue (js/pkg) into one CommonJS dist/main.js,
// the module Screeps loads (ADR-008). No plugins besides the substitution of the build's values below.
const values = {
  __RUSTEDSCREEP_BUILD__: process.env.RUSTEDSCREEP_BUILD,
  __RUSTEDSCREEP_LOADER_CONFIG__: process.env.RUSTEDSCREEP_LOADER_CONFIG,
};
for (const [token, json] of Object.entries(values)) {
  if (!json) throw new Error(`${token}: its environment variable is not set; run \`npm run build\` instead of rollup directly`);
  JSON.parse(json); // must be valid JSON, which is also a valid JS object literal
}

export default {
  input: 'js/build/main.js',
  output: {
    file: 'dist/main.js',
    format: 'cjs',
    exports: 'named',
    // ES2015 helpers, without the Symbol.toStringTag line Screeps does not need.
    generatedCode: { preset: 'es2015', symbols: false },
  },
  plugins: [
    {
      name: 'rustedscreep-build-values',
      transform(code) {
        let out = code;
        for (const [token, json] of Object.entries(values)) out = out.replaceAll(token, `(${json})`);
        return out === code ? null : { code: out, map: null };
      },
    },
  ],
  // Any warning (an unresolved import, a rewritten `this`, a circular import) fails the build.
  onwarn(warning) {
    throw new Error(`rollup warning treated as an error: ${warning.message}`);
  },
};
