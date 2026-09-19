#!/usr/bin/env node
// Minimal Chrome DevTools Protocol client for poking a running sta
// (start it with --remote-debugging-port=9333). Node 22+ (global WebSocket).
//
//   node tools/cdp.mjs list
//   node tools/cdp.mjs eval <url-substring> "<js expression>"
//   node tools/cdp.mjs shot <url-substring> out.png
//   node tools/cdp.mjs key <url-substring> <key> [ctrl,shift,alt]
//
// Env: CDP_PORT (default 9333)

import { writeFileSync } from 'node:fs';

const port = process.env.CDP_PORT || '9333';
const [cmd, match, ...rest] = process.argv.slice(2);

async function targets() {
  const res = await fetch(`http://127.0.0.1:${port}/json/list`);
  return res.json();
}

async function connect(sub) {
  const list = (await targets()).filter((t) => t.type === 'page' || t.type === 'other');
  const t = list.find((t) => t.url.includes(sub));
  if (!t) throw new Error(`no target matching "${sub}". targets:\n${list.map((t) => '  ' + t.url).join('\n')}`);
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((ok, err) => { ws.onopen = ok; ws.onerror = err; });
  let id = 0;
  const pending = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) {
      const { ok, err } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? err(new Error(JSON.stringify(msg.error))) : ok(msg.result);
    }
  };
  const send = (method, params = {}) => new Promise((ok, err) => {
    const i = ++id;
    pending.set(i, { ok, err });
    ws.send(JSON.stringify({ id: i, method, params }));
  });
  return { send, close: () => ws.close(), target: t };
}

const MODS = { alt: 1, ctrl: 2, meta: 4, shift: 8 };

try {
  if (cmd === 'list') {
    for (const t of await targets()) console.log(`${t.type.padEnd(10)} ${t.title.slice(0, 40).padEnd(40)} ${t.url}`);
  } else if (cmd === 'eval') {
    const c = await connect(match);
    const r = await c.send('Runtime.evaluate', { expression: rest.join(' '), awaitPromise: true, returnByValue: true });
    console.log(r.exceptionDetails ? 'EXCEPTION ' + JSON.stringify(r.exceptionDetails) : JSON.stringify(r.result.value, null, 2));
    c.close();
  } else if (cmd === 'shot') {
    const c = await connect(match);
    const r = await c.send('Page.captureScreenshot', { format: 'png' });
    writeFileSync(rest[0] || 'shot.png', Buffer.from(r.data, 'base64'));
    console.log('saved', rest[0] || 'shot.png');
    c.close();
  } else if (cmd === 'key') {
    const c = await connect(match);
    const [key, mods = ''] = rest;
    const modifiers = mods.split(',').filter(Boolean).reduce((a, m) => a | (MODS[m] || 0), 0);
    const code = key.length === 1 ? 'Key' + key.toUpperCase() : key;
    const vk = key.length === 1 ? key.toUpperCase().charCodeAt(0) : 0;
    for (const type of ['rawKeyDown', 'keyUp']) {
      await c.send('Input.dispatchKeyEvent', { type, key, code, modifiers, windowsVirtualKeyCode: vk });
    }
    c.close();
  } else {
    console.log('usage: list | eval <url-sub> <expr> | shot <url-sub> <out.png> | key <url-sub> <key> [mods]');
    process.exit(2);
  }
} catch (e) {
  console.error(e.message);
  process.exit(1);
}
