#!/usr/bin/env node
// Checks that the MCP documentation matches the bridge's real tool list (docs/MCP.md "Tool
// reference", docs/MCP.ko.md):
//
// - runs `sta-mcp.exe` (target/debug, or MCP_BRIDGE=<path>) and asks it for `tools/list` over
//   stdio, exactly like an MCP client (the list is static: no browser is needed or started);
// - every tool has one `### \`name\`` heading in docs/MCP.md, and no heading there names a tool
//   that doesn't exist;
// - each tool's section names every input property (nested `fields[]` properties too) as
//   `` `prop` ``, every enum value as `` `value` ``, every numeric limit (minimum > 1, maximum,
//   maxItems, maxLength) as a number, and its access (`**Access:** read-only` for tools with
//   `readOnlyHint`, `**Access:** full` otherwise);
// - every error code of crates/sta-core/src/agent/errors.rs appears in docs/MCP.md;
// - docs/MCP.ko.md names every tool.
//
//   node tools/check-mcp-docs.mjs            exit 0 = in sync, 1 = mismatch, 2 = no bridge
//   node tools/check-mcp-docs.mjs --dump     print the tools/list result as JSON
//   node tools/check-mcp-docs.mjs --armed    check docs/TESTING.md against the debug-only test
//                                            surface (`sta-mcp --test-tools`, a test-hooks build)
//   MCP_DOCS_DIR=<dir>                       check MCP.md / MCP.ko.md in another folder

import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const bridge = process.env.MCP_BRIDGE ? resolve(process.env.MCP_BRIDGE) : join(root, 'target', 'debug', 'sta-mcp.exe');

function listTools() {
  return new Promise((ok, fail) => {
    const dataDir = mkdtempSync(join(tmpdir(), 'mcp-docs-'));
    const child = spawn(bridge, ['--data-dir', dataDir, '--no-launch'], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    let buf = '';
    let stderr = '';
    const timer = setTimeout(() => {
      child.kill();
      fail(new Error(`no tools/list answer within 15 s. stderr: ${stderr.slice(-400)}`));
    }, 15000);
    const done = (fn) => {
      clearTimeout(timer);
      child.stdin.end();
      setTimeout(() => {
        child.kill();
        rmSync(dataDir, { recursive: true, force: true });
      }, 500);
      fn();
    };
    child.on('error', (e) => done(() => fail(e)));
    child.stderr.setEncoding('utf8').on('data', (d) => (stderr += d));
    child.stdout.setEncoding('utf8').on('data', (chunk) => {
      buf += chunk;
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i).trim();
        buf = buf.slice(i + 1);
        if (!line) continue;
        const msg = JSON.parse(line);
        if (msg.id === 1) {
          child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized', params: {} }) + '\n');
          child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} }) + '\n');
        } else if (msg.id === 2) {
          done(() => (msg.error ? fail(new Error(JSON.stringify(msg.error))) : ok(msg.result.tools)));
        }
      }
    });
    child.stdin.write(
      JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'check-mcp-docs', version: '1' } } }) + '\n',
    );
  });
}

/** The debug-only test catalog, printed by a `--features test-hooks` bridge without connecting. */
function listTestTools() {
  return new Promise((ok, fail) => {
    const child = spawn(bridge, ['--test-tools'], { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    let out = '';
    let stderr = '';
    child.stdout.setEncoding('utf8').on('data', (d) => (out += d));
    child.stderr.setEncoding('utf8').on('data', (d) => (stderr += d));
    child.on('error', fail);
    child.on('exit', (code) => {
      if (code !== 0 || !out.trim().startsWith('[')) {
        return fail(new Error(`sta-mcp --test-tools failed (exit ${code}); build it with --features test-hooks. ${stderr.slice(-200)}`));
      }
      ok(JSON.parse(out));
    });
  });
}

if (!existsSync(bridge)) {
  console.error(`check-mcp-docs: ${bridge} not found (cargo build -p sta-mcp, or set MCP_BRIDGE)`);
  process.exit(2);
}
const armed = process.argv.includes('--armed');
let tools;
try {
  tools = armed ? await listTestTools() : await listTools();
} catch (e) {
  console.error(`check-mcp-docs: ${e.message}`);
  process.exit(2);
}
if (process.argv.includes('--dump')) {
  console.log(JSON.stringify(tools, null, 2));
  process.exit(0);
}

const errors = [];
const fail = (msg) => errors.push(msg);
const docsDir = process.env.MCP_DOCS_DIR ? resolve(process.env.MCP_DOCS_DIR) : join(root, 'docs');
const docName = armed ? 'TESTING.md' : 'MCP.md';
const doc = readFileSync(join(docsDir, docName), 'utf8').replace(/\r\n/g, '\n');
const koPath = join(docsDir, 'MCP.ko.md');
const ko = armed ? '' : existsSync(koPath) ? readFileSync(koPath, 'utf8') : '';
if (!armed && !ko) fail('docs/MCP.ko.md is missing');

/** `### \`name\`` sections: name → body (until the next ### or ## heading). */
const sections = new Map();
const headingRe = /^### `([a-z_]+)`\s*$/gm;
const headings = [...doc.matchAll(headingRe)];
for (let i = 0; i < headings.length; i++) {
  const name = headings[i][1];
  const start = headings[i].index + headings[i][0].length;
  const rest = doc.slice(start);
  const next = rest.search(/^#{2,3} /m);
  const body = next < 0 ? rest : rest.slice(0, next);
  if (sections.has(name)) fail(`docs/${docName}: tool heading \`${name}\` appears twice`);
  sections.set(name, body);
}

const names = new Set(tools.map((t) => t.name));
for (const heading of sections.keys()) {
  if (!names.has(heading)) fail(`docs/${docName}: "### \`${heading}\`" is not a tool in the catalog`);
}

const mentions = (body, word) => body.includes('`' + word + '`');
const number = (body, n) => new RegExp(`(^|[^0-9])${String(n).replace(/\B(?=(\d{3})+(?!\d))/g, '[ ,]?')}([^0-9]|$)`).test(body);

function checkProps(tool, body, schema, path) {
  for (const [prop, spec] of Object.entries(schema.properties ?? {})) {
    const where = `${tool.name}.${path}${prop}`;
    if (!mentions(body, prop) && !mentions(body, path + prop)) fail(`docs/${docName} ${tool.name}: input \`${path}${prop}\` is not documented`);
    for (const value of spec.enum ?? []) {
      if (!mentions(body, value)) fail(`docs/${docName} ${where}: enum value \`${value}\` is not documented`);
    }
    for (const key of ['maximum', 'maxItems', 'maxLength']) {
      if (typeof spec[key] === 'number' && !number(body, spec[key])) fail(`docs/${docName} ${where}: ${key} ${spec[key]} is not documented`);
    }
    if (typeof spec.minimum === 'number' && spec.minimum > 1 && !number(body, spec.minimum)) fail(`docs/${docName} ${where}: minimum ${spec.minimum} is not documented`);
    if (spec.items?.maxLength && !number(body, spec.items.maxLength)) fail(`docs/${docName} ${where}[]: maxLength ${spec.items.maxLength} is not documented`);
    if (spec.items?.type === 'object') checkProps(tool, body, spec.items, `${prop}[].`);
  }
}

for (const tool of tools) {
  const body = sections.get(tool.name);
  if (!body) {
    fail(`docs/${docName}: no "### \`${tool.name}\`" section`);
    continue;
  }
  checkProps(tool, body, tool.inputSchema ?? {}, '');
  if (armed) {
    // The test surface has no access levels (it bypasses them) and no Korean page.
    if (tool.annotations?.readOnlyHint !== false) fail(`docs/TESTING.md ${tool.name}: a test tool must not be readOnlyHint`);
    if (tool._meta?.['sta/test'] !== true) fail(`docs/TESTING.md ${tool.name}: not tagged _meta["sta/test"]`);
    continue;
  }
  const readOnly = tool.annotations?.readOnlyHint === true;
  const access = /\*\*Access:\*\*\s*(read-only|full)/.exec(body)?.[1];
  if (!access) fail(`docs/MCP.md ${tool.name}: no "**Access:** read-only|full" line`);
  else if ((access === 'read-only') !== readOnly) fail(`docs/MCP.md ${tool.name}: documented as ${access}, but readOnlyHint is ${readOnly}`);
  if (!ko.includes('`' + tool.name + '`')) fail(`docs/MCP.ko.md: tool \`${tool.name}\` is not listed`);
}

const errorsRs = armed ? '' : readFileSync(join(root, 'crates', 'sta-core', 'src', 'agent', 'errors.rs'), 'utf8');
const codes = [...errorsRs.matchAll(/ErrorCode::\w+ => "([a-z_]+)",/g)].map((m) => m[1]);
if (!armed && codes.length < 10) fail(`errors.rs: found only ${codes.length} error codes (parser out of date?)`);
for (const code of new Set(codes)) {
  if (!mentions(doc, code)) fail(`docs/MCP.md: error code \`${code}\` is not documented`);
}

if (errors.length) {
  for (const e of errors) console.log(`FAIL ${e}`);
  console.log(`\ncheck-mcp-docs: ${errors.length} problem(s) (${tools.length} tools, ${new Set(codes).size} error codes)`);
  process.exit(1);
}
if (armed) {
  console.log(`check-mcp-docs --armed: docs/TESTING.md matches the debug-only test surface (${tools.length} tools)`);
} else {
  console.log(`check-mcp-docs: docs/MCP.md and docs/MCP.ko.md match tools/list (${tools.length} tools, ${new Set(codes).size} error codes)`);
}
