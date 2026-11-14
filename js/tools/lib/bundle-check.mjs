// Checks a built dist/main.js for code that cannot run in Screeps, on its syntax tree, so comments and
// strings (for example a Rust doc comment that wasm-bindgen copies into the glue) cannot trigger it.
// TextDecoder and TextEncoder are undefined on the MMO (VERIFICATION_LOG V72); fetch, document, URL and
// the `url` module are assumed absent (V95, unverified), so the build bans them as a safe default.
import { parseAst } from 'rollup/parseAst';

const BANNED_IDENTIFIERS = new Set(['fetch', 'document', 'pathToFileURL', 'URL']);

/** Visits every node except the names of non-computed properties (`a.fetch`, `{ URL: 1 }`), which are not globals. */
function walk(node, visit) {
  if (!node || typeof node.type !== 'string') return;
  visit(node);
  for (const key of Object.keys(node)) {
    if (!node.computed && ((node.type === 'MemberExpression' && key === 'property') ||
      ((node.type === 'Property' || node.type === 'MethodDefinition' || node.type === 'PropertyDefinition') && key === 'key'))) {
      continue;
    }
    const value = node[key];
    if (Array.isArray(value)) for (const v of value) walk(v, visit);
    else if (value && typeof value === 'object' && typeof value.type === 'string') walk(value, visit);
  }
}

/** Problems in `code` that would break it in Screeps; an empty list means none were found. */
export function bundleProblems(code, wasmModule) {
  let ast;
  try {
    ast = parseAst(code);
  } catch (e) {
    return [`does not parse: ${e.message}`];
  }
  const problems = new Set();
  walk(ast, (n) => {
    if (n.type === 'ImportDeclaration' || n.type === 'ImportExpression' || n.type.startsWith('Export')) {
      problems.add(`ES module syntax (${n.type})`);
    } else if (n.type === 'MetaProperty') {
      problems.add('import.meta');
    } else if (n.type === 'Identifier' && BANNED_IDENTIFIERS.has(n.name)) {
      problems.add(n.name);
    } else if (n.type === 'CallExpression' && n.callee.type === 'Identifier' && n.callee.name === 'require') {
      // Only the wasm module is bundled. Rollup's CJS form of import.meta.url is require('u' + 'rl').
      const arg = n.arguments[0];
      const literal = arg && arg.type === 'Literal' ? arg.value : undefined;
      if (arg && arg.type !== 'Identifier' && literal !== wasmModule) {
        problems.add(`require(${literal !== undefined ? JSON.stringify(literal) : arg.type})`);
      }
    }
  });
  return [...problems];
}
