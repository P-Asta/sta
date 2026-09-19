// The MCP client library of the end-to-end suites (Windows, Node 22+).
//
// Everything a suite does to a *running* browser goes through here: JSON-RPC over stdio to
// target/debug/sta-mcp.exe, which forwards to the browser over its named pipe. That is the path a
// real MCP client takes, so running the suites through it keeps rmcp's framing, the tool catalog
// and schemas, the `isError` text shape, `_meta["sta/structured"]` and the pipe-security checks in
// sta-mcp/src/win.rs under continuous test.
//
// Two kinds of tools reach the browser this way:
//   - the 23 shipped tools, under the real agent policy (what agent-e2e asserts on);
//   - the debug-only `test_*` surface (docs/TESTING.md), which needs a browser built with
//     `--features test-hooks` and started with `--sta-test-hooks` + `STA_E2E=1` + its own data
//     directory. `client.test('test_info')` returns the structured answer or throws.
//
// No console window is ever opened: the bridge and every helper are spawned with
// `windowsHide: true`, and nothing goes through cmd.exe.
//
//   import { McpClient, PipeClient, attach, waitForEndpoint, TEST_ARGS, TEST_ENV } from './mcp.mjs';

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
export const repo = path.resolve(here, '../../..');
export const BRIDGE = process.env.E2E_BRIDGE ? path.resolve(process.env.E2E_BRIDGE) : path.join(repo, 'target/debug/sta-mcp.exe');

/** Command-line switches a browser needs so the test surface arms (with TEST_ENV). */
export const TEST_ARGS = ['--sta-test-hooks'];
/** Same, but leaving approvals and agent access exactly as the profile has them. */
export const TEST_ARGS_NO_APPROVE = ['--sta-test-hooks-no-approve'];
/** Environment a browser needs so the test surface arms (with TEST_ARGS). */
export const TEST_ENV = { STA_E2E: '1' };

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** `<dataDir>/sta/agent-endpoint.json` once the browser has written it. */
export async function waitForEndpoint(dataDir, timeoutMs = 25000) {
  const file = path.join(dataDir, 'sta', 'agent-endpoint.json');
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    try {
      const endpoint = JSON.parse(readFileSync(file, 'utf8'));
      if (endpoint.pipe && endpoint.pid) return endpoint;
    } catch {
      // not written yet
    }
    await sleep(100);
  }
  throw new Error(`no ${file} within ${timeoutMs} ms (is agent access on, or the test surface armed?)`);
}

export const REBUILD_ARMED = 'rebuild with: cargo build -p sta -p sta-mcp --features test-hooks';
/**
 * The hint for `unknown tool: test_*`. That error has two very different causes and the message
 * alone does not separate them: lock 4 makes an *un-armed browser* answer exactly this so a client
 * cannot even learn the surface exists — but the far more common cause is that a plain
 * `cargo test` / `cargo clippy` (no `--features test-hooks`) rebuilt target/debug/sta-mcp.exe and
 * sta.exe without the surface compiled in at all. `requireArmedBridge()` below catches the bridge
 * half before a suite launches anything; this covers the browser half.
 */
const DISARMED_HINT =
  '\n  The test surface is not there. Either the binaries were rebuilt without it — a plain ' +
  '`cargo test`/`cargo clippy` does that — or the browser refused to arm (docs/TESTING.md §5 locks). ' +
  REBUILD_ARMED;

export class TestToolError extends Error {
  constructor(name, code, text) {
    const unknown = /unknown tool: test_/.test(String(text || ''));
    super(`${name} failed [${code || 'error'}]: ${text}${unknown ? DISARMED_HINT : ''}`);
    this.name = 'TestToolError';
    this.code = code;
    this.text = text;
  }
}

/** Cached result of the bridge preflight: `true`, or the reason it is not armed. */
let bridgeArmed = null;

/**
 * Refuses to run a suite against a bridge built without `test-hooks`.
 *
 * Any cargo command that leaves `--features test-hooks` off rebuilds target/debug/sta-mcp.exe
 * without the test catalog, and every later suite then dies on its first call with
 * `unknown tool: test_info` — a message that reads like a *browser* arming failure (lock 4). One
 * `--test-tools` spawn up front turns that into a sentence that names the cause.
 */
export function requireArmedBridge(bridge = BRIDGE) {
  if (bridgeArmed === true) return;
  if (typeof bridgeArmed === 'string') throw new Error(bridgeArmed);
  if (!existsSync(bridge)) {
    bridgeArmed = `${bridge} not found: ${REBUILD_ARMED}`;
    throw new Error(bridgeArmed);
  }
  const r = spawnSync(bridge, ['--test-tools'], { encoding: 'utf8', windowsHide: true, timeout: 30000 });
  const ok = r.status === 0 && /"test_info"/.test(r.stdout || '');
  if (!ok) {
    bridgeArmed =
      `${bridge} was built without the test surface, so every test_* call would fail with ` +
      `"unknown tool: test_info". A cargo command without --features test-hooks (a plain ` +
      `cargo test or cargo clippy) does this. ${REBUILD_ARMED}`;
    throw new Error(bridgeArmed);
  }
  bridgeArmed = true;
}

const errorCode = (text) => (/^Error \[([a-z_]+)\]/.exec(text || '') || [])[1];

/**
 * One MCP session: a `sta-mcp.exe` child speaking JSON-RPC over stdio.
 *
 * `--no-launch` is always passed: the suite starts the browser itself, and the bridge must never
 * start one behind its back (least of all against the user's default profile).
 */
export class McpClient {
  constructor({ bridge = BRIDGE, dataDir, clientInfo = { name: 'sta-e2e', title: 'sta e2e', version: '1.0' }, timeoutMs = 30000, env = {} } = {}) {
    if (!dataDir) throw new Error('McpClient needs the instance dataDir');
    this.bridge = bridge;
    this.dataDir = dataDir;
    this.clientInfo = clientInfo;
    this.timeoutMs = timeoutMs;
    this.env = env;
    this.stderr = '';
    this.notifications = [];
    this.tools = [];
    /** A call timed out: its id is still in flight in the browser, so reconnect before reusing. */
    this.dirty = false;
  }

  start() {
    if (!existsSync(this.bridge)) throw new Error(`${this.bridge} not found: cargo build -p sta-mcp --features test-hooks`);
    this.child = spawn(this.bridge, ['--data-dir', this.dataDir, '--no-launch'], {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: { ...process.env, ...this.env },
      windowsHide: true, // a console window must never appear during a test run
    });
    this.pending = new Map();
    this.nextId = 1;
    this.exited = null;
    let buf = '';
    this.child.stdout.setEncoding('utf8');
    this.child.stdout.on('data', (chunk) => {
      buf += chunk;
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i).trim();
        buf = buf.slice(i + 1);
        if (!line) continue;
        let msg;
        try {
          msg = JSON.parse(line);
        } catch {
          this.badStdout = line;
          continue;
        }
        if (msg.id !== undefined && this.pending.has(msg.id)) {
          const resolve = this.pending.get(msg.id);
          this.pending.delete(msg.id);
          resolve(msg);
        } else if (msg.method) {
          this.notifications.push(msg);
        }
      }
    });
    this.child.stderr.setEncoding('utf8');
    this.child.stderr.on('data', (d) => (this.stderr += d));
    this.child.on('exit', (code) => (this.exited = code));
    return this;
  }

  request(method, params, timeoutMs = this.timeoutMs) {
    const id = this.nextId++;
    return new Promise((ok, fail) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        this.dirty = true;
        fail(new Error(`${method} timed out after ${timeoutMs} ms`));
      }, timeoutMs);
      this.pending.set(id, (msg) => {
        clearTimeout(timer);
        ok(msg);
      });
      this.child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    });
  }

  notify(method, params) {
    this.child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method, params }) + '\n');
  }

  async initialize() {
    const r = await this.request('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: this.clientInfo });
    this.notify('notifications/initialized', {});
    this.serverInfo = r.result?.serverInfo;
    return r.result;
  }

  /** `tools/list` → the array of tool definitions (also kept in `this.tools`). */
  async listTools() {
    const r = await this.request('tools/list', {});
    if (r.error) throw new Error(`tools/list: ${JSON.stringify(r.error)}`);
    this.tools = r.result.tools;
    this.toolsResult = r.result;
    return this.tools;
  }

  /** `true` once this session has seen a `notifications/tools/list_changed`. */
  sawToolListChanged() {
    return this.notifications.some((n) => n.method === 'notifications/tools/list_changed');
  }

  /** `{text, images, structured, isError, code, raw}` of a tools/call. */
  async call(name, args = {}, { timeoutMs = this.timeoutMs } = {}) {
    if (this.dirty) await this.reconnect();
    return McpClient.parse(await this.request('tools/call', { name, arguments: args }, timeoutMs));
  }

  /**
   * A test tool: the structured answer, or a `TestToolError`. Tools whose answer is `null` (the
   * channel drops a null `structured`) return `null`, never `undefined`.
   */
  async test(name, args = {}, opts = {}) {
    const r = await this.call(name, args, opts);
    if (r.protocolError) throw new TestToolError(name, 'protocol', JSON.stringify(r.protocolError));
    if (r.isError) throw new TestToolError(name, errorCode(r.text), r.text);
    return r.structured ?? null;
  }

  static parse(r) {
    if (r.error) return { protocolError: r.error, text: '', images: [], isError: true, raw: r };
    const content = r.result.content || [];
    const text = content
      .filter((c) => c.type === 'text')
      .map((c) => c.text)
      .join('\n');
    return {
      text,
      images: content.filter((c) => c.type === 'image'),
      structured: r.result._meta?.['sta/structured'],
      isError: !!r.result.isError,
      code: r.result.isError ? errorCode(text) : undefined,
      raw: r,
    };
  }

  /** Starts a call without waiting: `{id, done}` (`done` resolves to the raw message). */
  startCall(name, args = {}) {
    const id = this.nextId++;
    const done = new Promise((ok) => this.pending.set(id, ok));
    this.child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } }) + '\n');
    return { id, done };
  }

  /** MCP cancellation of a started call. */
  cancel(id) {
    this.pending.delete(id);
    this.notify('notifications/cancelled', { requestId: id, reason: 'sta-e2e' });
  }

  /**
   * A fresh bridge process and session — after the browser restarted (the pipe name and the
   * endpoint file change), or after a call timed out with its id still in flight.
   */
  async reconnect({ waitForEndpointMs = 25000 } = {}) {
    await this.close();
    if (waitForEndpointMs) await waitForEndpoint(this.dataDir, waitForEndpointMs);
    this.dirty = false;
    this.start();
    let last;
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        await this.initialize();
        return this;
      } catch (e) {
        last = e;
        await sleep(500);
      }
    }
    throw last;
  }

  async close() {
    if (!this.child) return;
    try {
      this.child.stdin.end();
    } catch {
      // already gone
    }
    for (let i = 0; i < 20 && this.exited === null; i++) await sleep(100);
    if (this.exited === null) this.child.kill();
    this.child = null;
  }
}

/**
 * The raw NDJSON side of the channel, for the ~10 checks that are *about* the pipe (a client that
 * never says hello, a wrong protocol version, more than two sessions, the 8 MiB line limit). A
 * conformant bridge cannot misbehave on purpose, so those checks need this.
 */
export class PipeClient {
  constructor({ dataDir, endpoint = null }) {
    this.dataDir = dataDir;
    this.endpoint = endpoint;
    this.lines = [];
    this.waiters = [];
    this.closed = false;
  }

  async connect() {
    this.endpoint = this.endpoint || (await waitForEndpoint(this.dataDir));
    await new Promise((ok, fail) => {
      this.socket = net.connect(this.endpoint.pipe, ok);
      this.socket.once('error', fail);
    });
    this.socket.setEncoding('utf8');
    let buf = '';
    this.socket.on('data', (chunk) => {
      buf += chunk;
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, i).trim();
        buf = buf.slice(i + 1);
        if (!line) continue;
        const msg = JSON.parse(line);
        this.lines.push(msg);
        const waiter = this.waiters.shift();
        if (waiter) waiter(msg);
      }
    });
    this.socket.on('close', () => {
      this.closed = true;
      for (const w of this.waiters.splice(0)) w(null);
    });
    return this;
  }

  send(message) {
    this.socket.write(typeof message === 'string' ? message : JSON.stringify(message) + '\n');
  }

  /** The next line the browser sends (or null when the pipe closes first). */
  next(timeoutMs = 10000) {
    if (this.lines.length) return Promise.resolve(this.lines.shift());
    return new Promise((ok) => {
      const timer = setTimeout(() => ok(null), timeoutMs);
      this.waiters.push((msg) => {
        clearTimeout(timer);
        ok(msg);
      });
    });
  }

  hello({ v = 1, build = '0.1.0', name = 'sta-e2e-pipe' } = {}) {
    this.send({ t: 'hello', v, build, bridge: { version: '0.1.0', pid: process.pid }, client: { name } });
  }

  close() {
    if (this.socket) this.socket.destroy();
  }
}

/**
 * The MCP session of a started `Instance` (lib.mjs): waits for its endpoint file, spawns a bridge
 * against its data directory and initializes.
 */
export async function attach(instance, { clientInfo, timeoutMs, waitForEndpointMs = 25000 } = {}) {
  const dataDir = instance.data ?? instance;
  await waitForEndpoint(dataDir, waitForEndpointMs);
  const client = new McpClient({ dataDir, clientInfo, timeoutMs }).start();
  await client.initialize();
  return client;
}
