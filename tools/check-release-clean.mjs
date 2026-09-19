#!/usr/bin/env node
// Proves that a **release** binary contains no trace of the debug-only MCP test surface
// (docs/TESTING.md): no `test_*` tool name, no arming switch, no test-hooks marker.
//
// The compile-time half is `crates/sta/src/test_hooks/mod.rs`, whose `compile_error!` makes
// `cargo build --release --features test-hooks` fail instead of silently producing an un-armed
// binary. This is the other half: whatever the feature flags were, the shipped bytes have no test
// surface in them.
//
//   cargo build --release -p sta -p sta-mcp
//   node tools/check-release-clean.mjs [path...]     exit 0 = clean, 1 = found, 2 = not built
//
// It also scans the *debug* binaries when `--debug` is passed, which must be clean too unless they
// were built with `--features test-hooks`.

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(join(dirname(fileURLToPath(import.meta.url)), '..'));
const profile = process.argv.includes('--debug') ? 'debug' : 'release';
const explicit = process.argv.slice(2).filter((a) => !a.startsWith('--'));
const exe = process.platform === 'win32' ? '.exe' : '';
const binaries = explicit.length ? explicit.map((p) => resolve(p)) : [`sta${exe}`, `sta-mcp${exe}`].map((n) => join(root, 'target', profile, n));

/** Tool names, straight from the catalog source: the check can't drift from what exists. */
function toolNames() {
  const file = join(root, 'crates', 'sta-core', 'src', 'agent', 'test_tools.rs');
  const text = readFileSync(file, 'utf8');
  const names = [...text.matchAll(/name:\s*"(test_[a-z_]+)"/g)].map((m) => m[1]);
  if (names.length < 20) {
    console.error(`check-release-clean: only ${names.length} tool names parsed from test_tools.rs (parser out of date?)`);
    process.exit(2);
  }
  return names;
}

/** Other strings that only an armed build can contain. */
const MARKERS = ['--sta-test-hooks', 'TEST HOOKS ARMED', 'test_hooks_off', 'sta/test'];

const missing = binaries.filter((b) => !existsSync(b));
if (missing.length) {
  console.error(`check-release-clean: not built: ${missing.map((m) => relative(root, m)).join(', ')} (cargo build --${profile} -p sta -p sta-mcp)`);
  process.exit(2);
}

const needles = [...toolNames(), ...MARKERS];
let found = 0;
for (const binary of binaries) {
  const bytes = readFileSync(binary);
  const hits = needles.filter((n) => bytes.includes(Buffer.from(n, 'latin1')) || bytes.includes(Buffer.from(n, 'utf16le')));
  const name = relative(root, binary).replace(/\\/g, '/');
  if (hits.length) {
    console.log(`FAIL ${name} contains ${hits.length} test-surface string(s): ${hits.slice(0, 8).join(', ')}`);
    found += hits.length;
  } else {
    console.log(`OK   ${name} (${(bytes.length / 1048576).toFixed(1)} MB) has none of the ${needles.length} test-surface strings`);
  }
}

if (found) {
  console.log(`\ncheck-release-clean: the ${profile} build carries the test surface`);
  process.exit(1);
}
console.log(`check-release-clean: the ${profile} build has no test surface (${binaries.length} binaries, ${needles.length} strings)`);
