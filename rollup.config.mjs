// Bundles the tsc output (js/build) and the wasm-bindgen glue (js/pkg) into one CommonJS dist/main.js,
// the module Screeps loads (ADR-008). No plugins besides the build-ID substitution below.
const buildId = process.env.RUSTEDSCREEP_BUILD_ID;
if (!buildId) {
  throw new Error('RUSTEDSCREEP_BUILD_ID is not set; run `npm run build` instead of rollup directly');
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
      name: 'rustedscreep-build-id',
      transform(code) {
        const token = '__RUSTEDSCREEP_BUILD_ID__';
        return code.includes(token) ? { code: code.replaceAll(token, JSON.stringify(buildId)), map: null } : null;
      },
    },
  ],
  // Any warning (an unresolved import, a rewritten `this`, a circular import) fails the build.
  onwarn(warning) {
    throw new Error(`rollup warning treated as an error: ${warning.message}`);
  },
};
