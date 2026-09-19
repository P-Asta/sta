#!/usr/bin/env node
// Mock-mode motion harness (FINAL PLAN §7). Runs the UI in headless Edge against fixture data and
// checks the motion runtime's behaviour — the half of the motion work a static check cannot see:
//
//   node tools/motion-check.mjs [--edge <msedge.exe>] [--only <section,…>] [--verbose]
//
//   (level)   the level attribute, `--motion-distance`, and identity transforms at `reduced`
//   (keys)    a key that is off zeroes its own token only, and refuses WAAPI
//   (start)   a keyed animation starts within two frames and is replaced, not stacked
//   (off)     no finite animation runs, indicators are static (a ring and a *visible* bar), and the
//             scoped off selectors do not leak into pseudo-element motion on other pages
//   (toggle)  turning animations off mid-run finishes what is running, master switch and one key
//   (stagger) a staggered entrance holds its first keyframe while it waits for its delay
//   (ghost)   ghosts are inert clones with no id, data-*, role, aria-* or tabindex
//   (size)    `trackSurfaceSize` stays correct while the surface is transformed
//   (storm)   a burst of state pushes starts nothing
//   (sidebar) the sidebar's own 14 keys: insert, removal ghosts, the followers' FLIP (and its bulk,
//             drag and pointer-close opt-outs), folder collapse, the Clear Today sweep, the space
//             switch, the active-row crossfade, the split glider, the hover reveal's hide, the drag
//             lift and drop-line glide, the favorites pop and grid FLIP, a panel's exit ghost, the
//             download card and its check, and the URL pill's copy check
//   (cmdbar)  the command bar's 4: the selection glider (keyboard glides, hover snaps), the first
//             results' stagger, the mode crossfade, and the input and card staying untransformed
//   (topbar)  `topbar.navFade` after the resize, and the URL pill's host crossfade
//   (overlays) the toast's rise and text-only replacement, the switcher's one fade and gliding ring,
//             the find bar's fade and its shake (only on a repeat ask, never while composing), the
//             permission prompt's fade-only entrance on an inner wrapper, and Peek's header crossfade
//   (menus)   the pop-in's origin and direction, the drill-down slide, and the inert exit ghost
//   (pages)   the page enter stagger, the settings nav indicator, the disclosure height, archive row
//             removals and their bulk opt-out, the boosts editor's View Transition (fetched first),
//             and the empty state's hero
//   (theme)   `theme.crossFade` on pages that paint their own background and never inside a card
//   (controls) hover and press durations, the toggle thumb, and `controls.smoothScroll`
//   (indicators) one loading period for three indicators, and the audio bars stopping when the
//             window loses focus
//   (exit)    the page half of an acknowledged exit, and the activatable overlays' instant close:
//             a surface blanks when the shell asks, stays
//             blank, answers `surface.exited` with the generation it was given (even with the key
//             off, where it blanks instantly), ignores a request without one, leaves the tracked
//             root alone, and comes back when there is something to show again
//
// Exit codes: 0 all checks passed, 1 the harness could not run, 2 a check failed.
// Everything goes to stdout, and Edge is always killed with its whole tree.
//
// This cannot see stale frames, restacks, the SetChrome midpoint, ack timing, the native card or
// IME placement: those are the in-app `m.motion` checks (docs/TESTING.md).

import { spawn, spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { startStaticServer } from './ui-serve.mjs';

const DEFAULT_EDGE = 'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe';
const toolsDir = path.dirname(fileURLToPath(import.meta.url));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const timeout = (ms) => new Promise((r) => setTimeout(r, ms).unref());
const log = (...a) => console.log(...a);

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 && args[i + 1] && !args[i + 1].startsWith('--') ? args[i + 1] : fallback;
};
const edgePath = opt('edge', DEFAULT_EDGE);
const only = new Set((opt('only', '') || '').split(',').map((s) => s.trim()).filter(Boolean));
const verbose = args.includes('--verbose');

let passed = 0;
const failures = [];

function check(section, name, ok, detail) {
  if (ok) {
    passed++;
    if (verbose) log(`PASS [${section}] ${name}`);
  } else {
    failures.push(`[${section}] ${name}${detail ? ` — ${detail}` : ''}`);
    log(`FAIL [${section}] ${name}${detail ? ` — ${detail}` : ''}`);
  }
}

// ------------------------------------------------------------------------------------ Edge driver

function killTree(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32') {
    spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
  } else {
    try {
      child.kill('SIGKILL');
    } catch {}
  }
}

async function devToolsPort(profileDir, edge, timeoutMs) {
  const file = path.join(profileDir, 'DevToolsActivePort');
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (edge.exitCode !== null) throw new Error(`Edge exited early (code ${edge.exitCode})`);
    const text = await readFile(file, 'utf8').catch(() => '');
    const port = Number(text.split(/\r?\n/)[0]);
    if (Number.isInteger(port) && port > 0) return port;
    await sleep(100);
  }
  throw new Error('Edge did not open its DevTools port');
}

/** One DevTools page session with `eval` and `open`, plus the console problems it saw. */
async function session(port, origin) {
  let page;
  for (let i = 0; i < 100 && !page; i++) {
    page = await fetch(`http://127.0.0.1:${port}/json/list`)
      .then((r) => r.json())
      .then((targets) => targets.find((t) => t.type === 'page'))
      .catch(() => null);
    if (!page) await sleep(100);
  }
  if (!page) throw new Error('no DevTools page target');
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((ok, err) => {
    ws.onopen = ok;
    ws.onerror = () => err(new Error('DevTools WebSocket failed'));
  });
  let nextId = 0;
  const pending = new Map();
  const problems = [];
  let onLoad = () => {};
  ws.onmessage = (event) => {
    const msg = JSON.parse(event.data);
    if (msg.id && pending.has(msg.id)) {
      const p = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) p.reject(new Error(JSON.stringify(msg.error)));
      else p.resolve(msg.result);
      return;
    }
    const params = msg.params ?? {};
    if (msg.method === 'Page.loadEventFired') onLoad();
    else if (msg.method === 'Runtime.consoleAPICalled' && (params.type === 'error' || params.type === 'assert')) {
      problems.push(params.args.map((a) => (a.value !== undefined ? String(a.value) : a.description || a.type)).join(' '));
    } else if (msg.method === 'Runtime.exceptionThrown') {
      problems.push(params.exceptionDetails?.exception?.description ?? params.exceptionDetails?.text ?? 'exception');
    }
  };
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = ++nextId;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Emulation.setDeviceMetricsOverride', { width: 1100, height: 900, deviceScaleFactor: 1, mobile: false });

  /** Evaluate an expression (awaiting a promise) and return its value. */
  const evaluate = async (expression) => {
    const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
    return r.result.value;
  };
  /** Navigate to a mock page and wait for its first render. */
  const open = async (rel) => {
    problems.length = 0;
    const loaded = new Promise((r) => (onLoad = r));
    await send('Page.navigate', { url: `${origin}${rel}` });
    await Promise.race([loaded, timeout(15000)]);
    const deadline = Date.now() + 10000;
    while (Date.now() < deadline && !(await evaluate('window.__mockReady === true'))) await sleep(50);
    if (!(await evaluate('window.__mockReady === true'))) {
      throw new Error(`${rel}: never sent ui.ready${problems.length ? ` — ${problems.join(' | ')}` : ''}`);
    }
    // Two frames, so first-render animations have started and settled into a steady state.
    await evaluate('new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))');
  };
  return { evaluate, open, problems, close: () => ws.close() };
}

// ------------------------------------------------------------------------------------ the checks

/** A probe element plus a `::before` animation, so pseudo-element motion can be measured. */
const PROBE = `
  (() => {
    document.getElementById('motion-probe-style')?.remove();
    document.getElementById('motion-probe')?.remove();
    const style = document.createElement('style');
    style.id = 'motion-probe-style';
    style.textContent = \`
      @keyframes motion-probe-spin { to { rotate: 360deg } }
      #motion-probe { position: fixed; left: 0; top: 0; width: 40px; height: 20px; }
      #motion-probe::before { content: ''; display: block; width: 4px; height: 4px;
        animation: motion-probe-spin 1s linear infinite; }
      #motion-probe.probe-finite { animation: motion-probe-spin 5s linear; }
    \`;
    document.head.appendChild(style);
    const el = document.createElement('div');
    el.id = 'motion-probe';
    document.body.appendChild(el);
    return true;
  })()`;

async function checkLevel(s) {
  await s.open('/_gallery/?mock&motion=full');
  check('level', 'full: level attribute and distance', (await s.evaluate('[document.documentElement.dataset.motion, __motion.level(), __motion.distance(8)].join("|")')) === 'full|full|8');
  check(
    'level',
    'full: --motion-distance is 1',
    (await s.evaluate('getComputedStyle(document.documentElement).getPropertyValue("--motion-distance").trim()')) === '1',
  );

  await s.open('/_gallery/?mock&motion=reduced');
  check('level', 'reduced: level attribute follows the Windows setting', (await s.evaluate('document.documentElement.dataset.motion')) === 'reduced');
  check('level', 'reduced: distance() collapses to 0', (await s.evaluate('__motion.distance(24)')) === 0);
  check(
    'level',
    'reduced: --motion-distance is 0',
    (await s.evaluate('getComputedStyle(document.documentElement).getPropertyValue("--motion-distance").trim()')) === '0',
  );
  // "Identity transforms at 0 distance", not "no transform keyframes": a caller keeps its keyframes
  // and multiplies the travel, so both ends of the animation are the same place.
  const identity = await s.evaluate(`(() => {
    ${PROBE};
    const el = document.getElementById('motion-probe');
    const frames = [{ opacity: 0, translate: '0 ' + __motion.distance(6) + 'px' }, { opacity: 1, translate: 'none' }];
    const anim = __motion.animate(el, 'overlays.toast', frames, { duration: 300 });
    const kf = anim.effect.getKeyframes().map((k) => k.translate);
    anim.cancel();
    return JSON.stringify(kf);
  })()`);
  // Chromium normalises `translate: '0 0px'` to `0px`, so accept either spelling of "no movement".
  const identityOk = JSON.parse(identity).every((v) => /^(none|0px|0 0px|0px 0px|0)$/.test(String(v)));
  check('level', 'reduced: an animation built with distance() has identity translates', identityOk, identity);
  check('level', 'reduced: opacity still animates (fades are what reduced keeps)', (await s.evaluate('__motion.enabled("overlays.toast")')) === true);
  check('level', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkKeys(s) {
  await s.open('/_gallery/?mock&motion=full&animOff=overlays.toast,menus.popIn');
  const attr = await s.evaluate('document.documentElement.dataset.animOff');
  check('keys', 'data-anim-off lists the keys that are off', attr === 'overlays.toast menus.popIn', attr);
  check('keys', 'enabled() refuses an off key', (await s.evaluate('__motion.enabled("overlays.toast")')) === false);
  check('keys', 'enabled() allows the others', (await s.evaluate('__motion.enabled("overlays.find")')) === true);
  const tokens = await s.evaluate(`(() => {
    const cs = getComputedStyle(document.documentElement);
    return JSON.stringify([
      cs.getPropertyValue('--t-overlays-toast').trim(),
      cs.getPropertyValue('--t-menus-pop-in').trim(),
      cs.getPropertyValue('--t-overlays-find').trim(),
    ]);
  })()`);
  check('keys', 'an off key zeroes its own token and no other', tokens === '["0ms","0ms","120ms"]', tokens);
  check('keys', 'duration() reads the zeroed token', (await s.evaluate('__motion.duration("overlays.toast", 180)')) === 0);
  const refused = await s.evaluate(`(() => {
    ${PROBE};
    const el = document.getElementById('motion-probe');
    const a = __motion.animate(el, 'overlays.toast', [{ opacity: 0 }, { opacity: 1 }], { duration: 300 });
    const b = __motion.animate(el, 'overlays.find', [{ opacity: 0 }, { opacity: 1 }], { duration: 300 });
    const ok = a === null && b !== null;
    b?.cancel();
    return ok;
  })()`);
  check('keys', 'animate() returns null for an off key and works for an on one', refused === true);
  check('keys', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkStart(s) {
  await s.open('/_gallery/?mock&motion=full');
  // Counted in *frames*, not against a wall clock: a headless renderer's pacing is not a timing
  // guarantee, and what this check is about is that a keyed animation starts by itself — it never
  // waits for a visibility event, a reveal or anything else.
  const started = await s.evaluate(`(async () => {
    ${PROBE};
    const el = document.getElementById('motion-probe');
    const anim = __motion.animate(el, 'overlays.toast', [{ opacity: 0 }, { opacity: 1 }], { duration: 600 });
    let frames = 0;
    while (anim.startTime === null && frames < 20) {
      await new Promise((r) => requestAnimationFrame(r));
      frames++;
    }
    const info = { frames, playState: anim.playState, started: anim.startTime !== null, current: Number(anim.currentTime), id: anim.id };
    anim.cancel();
    return JSON.stringify(info);
  })()`);
  const info = JSON.parse(started);
  check('start', 'a keyed animation starts by itself, waiting for nothing', info.started === true && info.playState === 'running', started);
  check('start', 'it starts within a few frames', info.frames <= 4, started);
  check('start', 'and it has not skipped ahead by the time it does', info.current >= 0 && info.current <= 60, started);
  check('start', 'the animation carries the key as its id', info.id === 'overlays.toast');
  const replaced = await s.evaluate(`(() => {
    ${PROBE};
    const el = document.getElementById('motion-probe');
    const a = __motion.animate(el, 'overlays.toast', [{ opacity: 0 }, { opacity: 1 }], { duration: 600 });
    const b = __motion.animate(el, 'overlays.toast', [{ opacity: 0 }, { opacity: 1 }], { duration: 600 });
    const n = el.getAnimations().filter((x) => x.id === 'overlays.toast').length;
    const gone = a.playState === 'idle';
    b.cancel();
    return JSON.stringify({ n, gone });
  })()`);
  check('start', 'a second animation for the same key replaces the first', replaced === '{"n":1,"gone":true}', replaced);

  // A surface that answers for itself is believed over `visibilityState`, in both directions: an
  // overlay's view is still hidden when the state that fills it arrives (and the switcher's is
  // hidden for another 250 ms), so an entrance keyed on that state must be allowed to start and be
  // left *pending* until the first frame. A page that never declares presence keeps the flag.
  const presence = await s.evaluate(`(() => {
    const root = document.documentElement;
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
    const undeclared = __motion.enabled('overlays.toast');
    __motion.setPresented(true);
    const declared = __motion.enabled('overlays.toast');
    __motion.setPresented(false);
    const away = __motion.enabled('overlays.toast');
    __motion.setPresented(true);
    delete root.dataset.focused;
    const unfocused = __motion.enabled('overlays.toast');
    root.dataset.focused = '';
    delete document.visibilityState;
    return JSON.stringify({ undeclared, declared, away, unfocused, visible: __motion.enabled('overlays.toast') });
  })()`);
  const p = JSON.parse(presence);
  check('start', 'a hidden document refuses a surface that never declared its presence', p.undeclared === false, presence);
  check('start', 'and believes one that did, so an overlay can animate in before it is shown', p.declared === true, presence);
  check('start', 'a surface that says it is away is still refused', p.away === false, presence);
  check('start', 'and so is one in a window that is not in front (a minimized window hides pages)', p.unfocused === false, presence);
  check('start', 'a visible surface is allowed either way', p.visible === true, presence);
  check('start', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkOff(s) {
  await s.open('/_gallery/?mock&motion=off');
  check('off', 'level is off', (await s.evaluate('__motion.level()')) === 'off');
  check('off', 'enabled() refuses every key', (await s.evaluate('__motion.enabled("overlays.toast") || __motion.enabled("menus.popIn")')) === false);
  const running = await s.evaluate(`(() => {
    ${PROBE};
    document.getElementById('motion-probe').classList.add('probe-finite');
    return document.getAnimations().filter((a) => a.effect?.getComputedTiming?.().iterations !== Infinity)
      .filter((a) => Number(a.effect?.getComputedTiming?.().activeDuration) > 0).length;
  })()`);
  check('off', 'no finite animation has a duration', running === 0, String(running));
  const indicators = await s.evaluate(`(() => {
    const spinner = document.querySelector('.spinner');
    const bar = document.querySelector('.progress.is-indeterminate .progress-fill');
    if (!spinner || !bar) return 'missing: ' + (!spinner ? 'spinner ' : '') + (!bar ? 'bar' : '');
    const s1 = getComputedStyle(spinner);
    const s2 = getComputedStyle(bar);
    return JSON.stringify({
      spinDuration: s1.animationDuration,
      spinVisible: spinner.getBoundingClientRect().width > 0 && Number(s1.opacity) > 0,
      barAnimation: s2.animationName,
      barWidth: s2.width,
      barVisible: bar.getBoundingClientRect().width > 4 && Number(s2.opacity) > 0,
    });
  })()`);
  const ind = indicators.startsWith('{') ? JSON.parse(indicators) : null;
  check('off', 'the spinner is a static ring, not gone', ind?.spinDuration === '0s' && ind?.spinVisible === true, indicators);
  check('off', 'the indeterminate bar is a static, visible bar', ind?.barAnimation === 'none' && ind?.barWidth !== '0px' && ind?.barVisible === true, indicators);
  // The off rule's `::before` / `::after` selectors are scoped: a *page at another level* must keep
  // its pseudo-element motion. Measured on the same page by turning motion back on.
  const pseudo = await s.evaluate(`(() => {
    ${PROBE};
    const el = document.getElementById('motion-probe');
    const at = () => getComputedStyle(el, '::before').animationDuration;
    const off = at();
    document.documentElement.dataset.motion = 'full';
    const full = at();
    document.documentElement.dataset.motion = 'off';
    return JSON.stringify({ off, full });
  })()`);
  check('off', 'the off rule scopes ::before / ::after (no leak to other levels)', pseudo === '{"off":"0s","full":"1s"}', pseudo);
  check('off', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkToggle(s) {
  await s.open('/_gallery/?mock&motion=full');
  const result = await s.evaluate(`(async () => {
    ${PROBE};
    const el = document.getElementById('motion-probe');
    const anim = __motion.animate(el, 'overlays.toast', [{ opacity: 0 }, { opacity: 1 }], { duration: 4000, fill: 'both' });
    await new Promise((r) => requestAnimationFrame(r));
    const before = anim.playState;
    await window.sta.dispatch({ type: 'updateSettings', patch: { animations: { enabled: false } } });
    // The next state push writes data-motion, which motion.js observes.
    for (let i = 0; i < 60 && document.documentElement.dataset.motion !== 'off'; i++) {
      await new Promise((r) => setTimeout(r, 16));
    }
    await new Promise((r) => requestAnimationFrame(r));
    return JSON.stringify({ before, level: document.documentElement.dataset.motion, after: anim.playState, opacity: getComputedStyle(el).opacity });
  })()`);
  const r = JSON.parse(result);
  check('toggle', 'the animation was running before the switch', r.before === 'running', result);
  check('toggle', 'turning animations off reaches the page', r.level === 'off', result);
  check('toggle', 'a running animation is finished, not cancelled', r.after === 'finished', result);
  check('toggle', 'it is left at its end state', r.opacity === '1', result);

  // One key switched off mid-run has to mean the same thing as switching it off beforehand: what is
  // playing under that key settles, and nothing else is touched.
  await s.open('/_gallery/?mock&motion=full');
  const one = await s.evaluate(`(async () => {
    ${PROBE};
    const el = document.getElementById('motion-probe');
    const a = __motion.animate(el, 'overlays.toast', [{ opacity: 0 }, { opacity: 1 }], { duration: 4000, fill: 'both' });
    const b = __motion.animate(el, 'overlays.find', [{ scale: 0.9 }, { scale: 1 }], { duration: 4000, fill: 'both' });
    await new Promise((r) => requestAnimationFrame(r));
    const before = [a.playState, b.playState];
    await window.sta.dispatch({ type: 'updateSettings', patch: { animations: { set: { 'overlays.toast': false } } } });
    const off = () => (document.documentElement.dataset.animOff || '').split(' ');
    for (let i = 0; i < 60 && !off().includes('overlays.toast'); i++) await new Promise((r) => setTimeout(r, 16));
    await new Promise((r) => requestAnimationFrame(r));
    const out = {
      before,
      level: document.documentElement.dataset.motion,
      off: off(),
      a: a.playState,
      b: b.playState,
      opacity: getComputedStyle(el).opacity,
    };
    b.cancel();
    return JSON.stringify(out);
  })()`);
  const k = JSON.parse(one);
  check('toggle', 'both animations were running before the switch', k.before.join() === 'running,running', one);
  check('toggle', 'one key off reaches the page and leaves the level alone', k.off.includes('overlays.toast') && k.level === 'full', one);
  check('toggle', "the switched-off key's animation is finished at its end state", k.a === 'finished' && k.opacity === '1', one);
  check('toggle', 'and another key keeps playing', k.b === 'running', one);
  check('toggle', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkStagger(s) {
  await s.open('/_gallery/?mock&motion=full');
  // FINAL PLAN rule 8: a staggered *entrance* must not paint its later elements at their base style
  // while they wait for their delay (`fill: 'none'` contributes nothing during a delay, so an
  // element whose first keyframe is `opacity: 0` would stand fully opaque and then blink out).
  const result = await s.evaluate(`(async () => {
    document.getElementById('stagger-probe')?.remove();
    const host = document.createElement('div');
    host.id = 'stagger-probe';
    host.style.cssText = 'position:fixed;left:0;top:0;width:30px;height:30px';
    host.innerHTML = '<b></b><b></b><b></b>';
    document.body.appendChild(host);
    const els = [...host.children];
    const anims = __motion.stagger(els, 'overlays.toast', [{ opacity: 0 }, { opacity: 1 }], {
      duration: 400,
      total: 600,
      step: 200,
    });
    const samples = [];
    for (let i = 0; i < 8; i++) {
      await new Promise((r) => requestAnimationFrame(r));
      samples.push(els.map((el) => Math.round(Number(getComputedStyle(el).opacity) * 1000) / 1000));
    }
    const fills = anims.map((a) => a.effect.getTiming().fill);
    const delays = anims.map((a) => a.effect.getTiming().delay);
    for (const a of anims) a.cancel();
    host.remove();
    const flashes = samples.filter((row) => row.some((v, i) => v > 0.99 && row.slice(0, i).some((earlier) => earlier < 0.99)));
    return JSON.stringify({ fills, delays, samples, flashes: flashes.length });
  })()`);
  const r = JSON.parse(result);
  check('stagger', 'the elements are offset in time', r.delays.join() === '0,200,400', result);
  check('stagger', 'every one of them fills backwards', r.fills.every((f) => f === 'backwards'), result);
  check('stagger', 'so none paints opaque while an earlier one is still fading in', r.flashes === 0, result);
  check('stagger', 'the first one does fade in', r.samples.some((row) => row[0] > 0 && row[0] < 1), result);
  check('stagger', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkGhost(s) {
  await s.open('/_gallery/?mock&motion=full');
  const result = await s.evaluate(`(() => {
    ${PROBE};
    const el = document.getElementById('motion-probe');
    el.setAttribute('role', 'treeitem');
    el.setAttribute('aria-selected', 'true');
    el.setAttribute('data-id', '4242');
    el.setAttribute('data-nav', '');
    el.setAttribute('tabindex', '0');
    el.setAttribute('title', 'a row');
    const child = document.createElement('span');
    child.id = 'motion-probe-child';
    child.setAttribute('data-pane', '7');
    child.setAttribute('aria-label', 'x');
    el.appendChild(child);
    const clone = __motion.ghost(el);
    const attrs = clone ? [...clone.attributes].map((a) => a.name).sort() : null;
    const childAttrs = clone ? [...clone.querySelectorAll('*')].flatMap((n) => [...n.attributes].map((a) => a.name)) : null;
    const out = {
      made: Boolean(clone),
      attrs,
      childAttrs,
      inert: clone?.inert === true,
      hidden: clone?.getAttribute('aria-hidden') === 'true',
      inLayer: clone?.parentElement?.classList.contains('motion-ghosts') === true,
      layerAtBody: document.querySelector('body > .motion-ghosts') !== null,
      layerInert: getComputedStyle(document.querySelector('body > .motion-ghosts')).pointerEvents === 'none',
      byId: document.querySelectorAll('#motion-probe').length,
      byData: document.querySelectorAll('[data-id="4242"]').length,
      byNav: document.querySelectorAll('[data-nav]').length,
      byRole: document.querySelectorAll('[role="treeitem"]').length,
      childById: document.querySelectorAll('#motion-probe-child').length,
    };
    __motion.clearGhosts();
    out.cleared = document.querySelectorAll('.motion-ghost').length;
    return JSON.stringify(out);
  })()`);
  const g = JSON.parse(result);
  check('ghost', 'a ghost is made', g.made === true, result);
  check('ghost', 'it keeps only class, style and aria-hidden', JSON.stringify(g.attrs) === '["aria-hidden","class","inert","style"]', JSON.stringify(g.attrs));
  check('ghost', 'its children lose every id, data-* and aria-*', JSON.stringify(g.childAttrs) === '[]', JSON.stringify(g.childAttrs));
  check('ghost', 'it is inert and aria-hidden', g.inert === true && g.hidden === true, result);
  check('ghost', 'it lives in a fixed layer at body level, outside every scroller', g.inLayer === true && g.layerAtBody === true, result);
  check('ghost', 'the layer never takes pointer events', g.layerInert === true, result);
  check(
    'ghost',
    'no lookup by id, data-id, [data-nav] or role can find it',
    g.byId === 1 && g.byData === 1 && g.byNav === 1 && g.byRole === 1 && g.childById === 1,
    result,
  );
  check('ghost', 'clearGhosts() removes them', g.cleared === 0, result);
  check('ghost', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkSize(s) {
  await s.open('/toast/?mock&toast=Something%20happened&motion=full');
  const result = await s.evaluate(`(async () => {
    const frame = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    const root = document.querySelector('.toast');
    const inner = document.querySelector('.toast-inner');
    const layout = () => ({ w: root.offsetWidth, h: root.offsetHeight });
    await frame();
    const before = { ...window.__mockSurfaceSize };
    const size = layout();
    // A scale on the page, and on the tracked root itself: neither may change the size the shell is
    // told, because the shell keeps it.
    inner.style.scale = '2';
    root.style.scale = '0.5';
    root.style.translate = '30px 12px';
    await frame();
    await frame();
    const after = { ...window.__mockSurfaceSize };
    const rect = root.getBoundingClientRect();
    inner.style.scale = '';
    root.style.scale = '';
    root.style.translate = '';
    return JSON.stringify({ before, after, size, rectW: Math.round(rect.width) });
  })()`);
  const r = JSON.parse(result);
  check('size', 'the toast reported a size', Number(r.before?.width) > 0 && Number(r.before?.height) > 0, result);
  // The reported size is the layout box rounded up (`setSurfaceSize` ceils a fractional
  // `borderBoxSize`), so it is within a pixel of `offsetWidth`/`offsetHeight` — and nowhere near the
  // transformed rect, which is what this check exists to catch.
  check(
    'size',
    'it is the layout box, not the transformed rect',
    Math.abs(r.before.width - r.size.w) <= 1 && Math.abs(r.before.height - r.size.h) <= 1,
    result,
  );
  check('size', 'a transform on the tracked root or inside it never changes the reported size', JSON.stringify(r.before) === JSON.stringify(r.after), result);
  check('size', 'the transform really was applied (the check is not vacuous)', r.rectW !== r.size.w, result);
  check('size', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkStorm(s) {
  await s.open('/sidebar/?mock&motion=full');
  const result = await s.evaluate(`(async () => {
    // Let the sidebar's own "don't animate the first render" delay expire first.
    await new Promise((r) => setTimeout(r, 400));
    const before = __motion.stats().started;
    for (let i = 0; i < 30; i++) {
      window.__mock.setState((s) => {
        // Nothing keyed changes: no ids, no order, no seq — only a value a render reads.
        s.window.sidebarWidth = 248 + (i % 2);
      });
      await new Promise((r) => setTimeout(r, 8));
    }
    await new Promise((r) => setTimeout(r, 120));
    const after = __motion.stats().started;
    const running = document.getAnimations().filter((a) => typeof a.id === 'string' && a.id.includes('.')).length;
    return JSON.stringify({ before, after, running, rev: window.__mock.state.revision });
  })()`);
  const r = JSON.parse(result);
  check('storm', '30 state pushes in a row start no animation', r.after === r.before, result);
  check('storm', 'and leave none running', r.running === 0, result);
  check('storm', 'the pushes really happened', r.rev > 0, result);
  check('storm', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

// ------------------------------------------------------------------- shared probe helpers
//
// Pasted into the page by the area sections below. Nothing here reaches into a surface's private
// state: the scenarios drive the mock through `window.sta.dispatch` (the real IPC path) or through
// `__mock.setState`, exactly as the app's own pushes arrive, so every animation they observe was
// started by a keyed diff and not by the harness.
const HELP = `
  const frame = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const until = async (fn, ms = 3000) => {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      if (fn()) return true;
      await frame();
    }
    return false;
  };
  const ids = () => document.getAnimations().map((a) => a.id).filter(Boolean);
  const anims = (id) => document.getAnimations().filter((a) => a.id === id);
  /** The class (or tag) of every element animating under \`id\`: which element actually moves. */
  const owners = (id) =>
    anims(id).map((a) => {
      const el = a.effect && a.effect.target;
      return el ? String(el.className || el.tagName) : '?';
    });
  /** The properties \`id\`'s keyframes animate, so "opacity only" can be asserted. */
  const kprops = (id) => {
    const a = anims(id)[0];
    if (!a) return [];
    const meta = new Set(['offset', 'computedOffset', 'easing', 'composite']);
    const out = new Set();
    for (const frame of a.effect.getKeyframes()) for (const k of Object.keys(frame)) if (!meta.has(k)) out.add(k);
    return [...out].sort();
  };
  const dur = (el, pseudo) => Number.parseFloat(getComputedStyle(el, pseudo).animationDuration) || 0;
  /**
   * Stretch one duration token, so a probe can still see a short animation running however slowly
   * this headless renderer happens to produce frames. Re-setting \`data-motion\` clears motion.js's
   * duration cache (it observes the attribute), even when the value does not change.
   */
  const slow = (token, ms) => {
    const el = document.createElement('style');
    el.textContent = ':root{' + token + ':' + ms + 'ms;}';
    document.head.appendChild(el);
    document.documentElement.dataset.motion = document.documentElement.dataset.motion || 'full';
  };
  const shift = (el) => Number.parseFloat(getComputedStyle(el).translate) || 0;
  const fade = (el, pseudo) => Number.parseFloat(getComputedStyle(el, pseudo).transitionDuration) || 0;
  const rowsOf = (list) => [...list.getElementsByClassName('cmd-row')];
  const gliderTop = (el) => Number.parseFloat((el.style.translate || '0 0px').split(' ')[1]) || 0;
`;

async function checkSidebarRows(s) {
  await s.open('/sidebar/?mock&motion=full');
  // (a) a row inserted into Today fades in, and the rows below it FLIP down.
  const insert = await s.evaluate(`(async () => {
    ${HELP}
    await wait(400); // the sidebar refuses to animate its own first render
    const before = __motion.stats();
    __mock.setState((st) => {
      const space = st.spaces.find((x) => x.id === 15);
      space.today.splice(1, 0, { ...space.today[2], id: 900 });
    });
    const arrived = await until(() => document.querySelector('.today-list [data-id="900"]'));
    const running = ids();
    const after = __motion.stats();
    return JSON.stringify({
      arrived,
      running,
      started: after.started - before.started,
      flips: after.flips - before.flips,
      ghosts: after.ghosts - before.ghosts,
    });
  })()`);
  const ins = JSON.parse(insert);
  check('sidebar', 'a row inserted into Today arrives', ins.arrived === true, insert);
  check('sidebar', 'it fades in under its own key', ins.running.includes('sidebar.tabInsertRemove'), insert);
  check('sidebar', 'the rows below it FLIP down', ins.flips === 1, insert);
  check('sidebar', 'an insert leaves no ghost behind', ins.ghosts === 0, insert);

  // (b) a close made with the pointer: the row leaves a ghost, the followers stay put.
  const pointerClose = await s.evaluate(`(async () => {
    ${HELP}
    const before = __motion.stats();
    document.querySelector('.today-list [data-id="34"] .row-close').click();
    const gone = await until(() => !document.querySelector('.today-list [data-id="34"]'));
    const after = __motion.stats();
    const ghost = document.querySelector('.motion-ghost');
    return JSON.stringify({
      gone,
      ghosts: after.ghosts - before.ghosts,
      flips: after.flips - before.flips,
      inert: ghost ? ghost.inert === true : false,
      hidden: ghost ? ghost.getAttribute('aria-hidden') === 'true' : false,
      findable: document.querySelectorAll('[data-id="34"]').length,
      running: ids(),
    });
  })()`);
  const pc = JSON.parse(pointerClose);
  check('sidebar', 'a closed row leaves exactly one ghost', pc.gone === true && pc.ghosts === 1, pointerClose);
  check(
    'sidebar',
    'the ghost is inert and aria-hidden, and no [data-id] lookup finds it',
    pc.inert === true && pc.hidden === true && pc.findable === 0,
    pointerClose,
  );
  check('sidebar', 'a close made with the pointer never pulls the followers up under the cursor', pc.flips === 0, pointerClose);
  check('sidebar', 'the removal itself still animates', pc.running.includes('sidebar.tabInsertRemove'), pointerClose);
  check('sidebar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkSidebarOrder(s) {
  await s.open('/sidebar/?mock&motion=full');
  const result = await s.evaluate(`(async () => {
    ${HELP}
    await wait(400);
    const swap = () =>
      __mock.setState((st) => {
        const today = st.spaces.find((x) => x.id === 15).today;
        const first = today[1];
        today[1] = today[2];
        today[2] = first;
      });
    const topId = () => document.querySelectorAll('.today-list [data-row]')[1]?.dataset.id ?? null;

    // (a) a reorder glides under its own key and makes no ghosts.
    let was = topId();
    let before = __motion.stats();
    swap();
    await until(() => topId() !== was);
    await frame();
    let after = __motion.stats();
    const reorder = { flips: after.flips - before.flips, ghosts: after.ghosts - before.ghosts, running: ids() };

    // (b) FLIP is suspended while a drag is running: DnD reads row rects live.
    await wait(300);
    document.documentElement.classList.add('is-dragging');
    was = topId();
    before = __motion.stats();
    swap();
    await until(() => topId() !== was);
    await frame();
    after = __motion.stats();
    document.documentElement.classList.remove('is-dragging');
    const dragging = { flips: after.flips - before.flips };

    // (c) a bulk change is not a rearrangement the eye can follow: no FLIP, ghosts capped.
    await wait(300);
    before = __motion.stats();
    __mock.setState((st) => {
      const space = st.spaces.find((x) => x.id === 15);
      const template = space.today[2];
      space.today = [space.today[0]].concat(Array.from({ length: 14 }, (_, i) => ({ ...template, id: 800 + i })));
    });
    await until(() => document.querySelectorAll('.today-list [data-row]').length === 15);
    await frame();
    after = __motion.stats();
    const bulk = { flips: after.flips - before.flips, ghosts: after.ghosts - before.ghosts };
    return JSON.stringify({ reorder, dragging, bulk });
  })()`);
  const r = JSON.parse(result);
  check('sidebar', 'a reorder FLIPs the rows that moved', r.reorder.flips === 1, result);
  check('sidebar', 'it runs under sidebar.reorder', r.reorder.running.includes('sidebar.reorder'), result);
  check('sidebar', 'a reorder leaves no ghosts', r.reorder.ghosts === 0, result);
  check('sidebar', 'FLIP is suspended while a drag is running', r.dragging.flips === 0, result);
  check('sidebar', 'a bulk change does not FLIP', r.bulk.flips === 0, result);
  check('sidebar', 'and never leaves more than 8 ghosts', r.bulk.ghosts <= 8, result);
  check('sidebar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkSidebarSweeps(s) {
  await s.open('/sidebar/?mock&motion=full');
  const result = await s.evaluate(`(async () => {
    ${HELP}
    await wait(400);
    // (a) Clear Today: one staggered sweep, capped at 12 ghosts. It runs first, because it leaves a
    // list short enough for the collapse below to move eight rows or fewer - past that, rule 7 skips
    // the FLIP on purpose, and (b) would be measuring that instead.
    let before = __motion.stats();
    const rows = document.querySelectorAll('.today-list [data-row]').length;
    await window.sta.dispatch({ type: 'clearToday', space: 15 });
    await until(() => document.querySelectorAll('.today-list [data-row]').length < rows);
    await frame();
    let after = __motion.stats();
    const delays = anims('sidebar.clearToday').map((a) => a.effect.getTiming().delay);
    const sweep = {
      ghosts: after.ghosts - before.ghosts,
      spread: delays.length ? Math.max(...delays) - Math.min(...delays) : -1,
      steps: new Set(delays).size,
      running: ids(),
    };
    __motion.clearGhosts();
    await wait(300);

    // (b) a folder collapse: the children leave under sidebar.folderExpand, the followers FLIP up.
    before = __motion.stats();
    await window.sta.dispatch({ type: 'toggleFolder', id: 22 });
    await until(() => !document.querySelector('[data-id="23"]'));
    await frame();
    after = __motion.stats();
    const collapse = {
      running: ids(),
      ghosts: after.ghosts - before.ghosts,
      flips: after.flips - before.flips,
      chevron: fade(document.querySelector('.folder-row .folder-chevron')),
    };
    __motion.clearGhosts();
    await wait(300);

    // (c) a space switch: the old pane leaves as one ghost, the new one slides in at mount.
    before = __motion.stats();
    await window.sta.dispatch({ type: 'switchSpace', id: 1 });
    await until(() => document.querySelector('.space-pane[aria-label^="Personal"]'));
    await frame();
    after = __motion.stats();
    const pane = document.querySelector('.space-pane');
    const switched = {
      ghosts: after.ghosts - before.ghosts,
      entering: pane ? pane.classList.contains('is-entering') : false,
      animation: pane ? getComputedStyle(pane).animationName : null,
    };
    // (d) switching faster than a pane ghost can fade never stacks whole panes in the layer: a
    // pane farewell is a deep clone of everything on screen, so there is one slot for it.
    __motion.clearGhosts();
    await wait(300);
    before = __motion.stats();
    await window.sta.dispatch({ type: 'switchSpace', id: 15 });
    await frame();
    await window.sta.dispatch({ type: 'switchSpace', id: 1 });
    await frame();
    const spam = {
      taken: __motion.stats().ghosts - before.ghosts,
      live: document.querySelectorAll('.motion-ghosts .motion-ghost').length,
    };
    return JSON.stringify({
      collapse,
      sweep,
      switched,
      spam,
      divider: getComputedStyle(document.querySelector('.today-divider-line')).transitionProperty,
    });
  })()`);
  const r = JSON.parse(result);
  check('sidebar', 'collapsing a folder fades its children out under sidebar.folderExpand', r.collapse.running.includes('sidebar.folderExpand'), result);
  check('sidebar', 'the children leave ghosts, at most 20', r.collapse.ghosts > 0 && r.collapse.ghosts <= 20, result);
  check('sidebar', 'the followers FLIP up', r.collapse.flips === 1, result);
  check('sidebar', "the chevron turns on the folder key's own token (160 ms)", Math.abs(r.collapse.chevron - 0.16) < 0.001, result);
  check('sidebar', 'Clear Today sweeps at most 12 ghosts away', r.sweep.ghosts > 1 && r.sweep.ghosts <= 12, result);
  check('sidebar', 'under sidebar.clearToday, staggered', r.sweep.running.includes('sidebar.clearToday') && r.sweep.steps > 1, result);
  check('sidebar', 'and the stagger spans no more than 150 ms in total', r.sweep.spread >= 0 && r.sweep.spread <= 150, result);
  check('sidebar', 'a space switch slides the new pane in', r.switched.entering === true && r.switched.animation === 'space-in', result);
  check('sidebar', 'and leaves exactly one ghost: the pane, not its rows', r.switched.ghosts === 1, result);
  check('sidebar', 'two fast switches take two pane ghosts', r.spam.taken >= 2, result);
  check('sidebar', 'and never leave more than one alive at a time', r.spam.live <= 1, result);
  check('sidebar', 'the Today divider clips instead of animating a margin', r.divider.includes('clip-path') && !r.divider.includes('margin'), result);
  check('sidebar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkSidebarFlipGates(s) {
  await s.open('/sidebar/?mock&motion=full');
  // Clear Today and its Undo (the toast's button, Ctrl+Shift+T): the list empties to the rows that
  // are kept and then refills. Dozens of ids change while only the survivors move — and one of them
  // moves the length of the list, which is the opposite of a rearrangement the eye can follow. What
  // makes a change bulk is the ids it changes, not the rows that happened to move.
  const result = await s.evaluate(`(async () => {
    ${HELP}
    await wait(400);
    const rows = () => document.querySelectorAll('.today-list [data-row]').length;
    const fill = (n, keep) =>
      __mock.setState((st) => {
        const sp = st.spaces.find((x) => x.id === 15);
        const t = sp.today[0];
        const fresh = Array.from({ length: n }, (_, i) => ({ ...t, id: 800 + i }));
        sp.today = keep ? fresh.concat([{ ...t, id: 700 }]) : [{ ...t, id: 700 }].concat(fresh);
      });
    fill(59, false);
    await until(() => rows() === 60);
    await wait(300);
    __motion.clearGhosts();
    // Clear Today: only the kept row is left.
    __mock.setState((st) => {
      const sp = st.spaces.find((x) => x.id === 15);
      sp.today = sp.today.filter((n) => n.id === 700);
    });
    await until(() => rows() === 1);
    await wait(400);
    __motion.clearGhosts();
    // Undo: the rows come back *above* the survivor, which would slide the whole list's length.
    const before = __motion.stats();
    fill(59, true);
    await until(() => rows() === 60);
    await frame();
    const after = __motion.stats();
    const travel = anims('sidebar.tabInsertRemove')
      .map((a) => a.effect.getKeyframes().map((f) => String(f.translate ?? '')).join(' -> '))
      .filter((t) => /px/.test(t));
    const far = travel.filter((t) => t.split(/[^-\d.]+/).some((n) => Math.abs(Number(n) || 0) > window.innerHeight));
    return JSON.stringify({
      flips: after.flips - before.flips,
      started: after.started - before.started,
      far: far.slice(0, 3),
      viewport: window.innerHeight,
    });
  })()`);
  const r = JSON.parse(result);
  check('sidebar', 'the Undo of Clear Today runs no FLIP: 59 ids changed, whichever rows moved', r.flips === 0, result);
  check('sidebar', 'the rows that came back still animate', r.started > 0, result);
  check('sidebar', 'and nothing travels further than the viewport is tall', r.far.length === 0, result);

  // The same rule where nothing else could catch it: a short list, every row on screen, one survivor
  // that moves two rows' worth — only the *count of changed ids* says this is a bulk change.
  const near = await s.evaluate(`(async () => {
    ${HELP}
    const list = () => [...document.querySelectorAll('.today-list [data-row]')];
    const set = (n, base) =>
      __mock.setState((st) => {
        const sp = st.spaces.find((x) => x.id === 15);
        const t = sp.today[0];
        sp.today = Array.from({ length: n }, (_, i) => ({ ...t, id: base + i })).concat([{ ...t, id: 700 }]);
      });
    set(4, 900);
    await until(() => list().length === 5);
    await wait(300);
    __motion.clearGhosts();
    const survivor = () => list().find((el) => el.dataset.id === '700');
    const was = survivor().getBoundingClientRect().top;
    const before = __motion.stats();
    // 4 ids out, 6 in (all different): 10 changed, and the survivor moves two rows down.
    set(6, 950);
    await until(() => list().length === 7);
    await frame();
    const after = __motion.stats();
    const now = survivor().getBoundingClientRect().top;
    return JSON.stringify({
      flips: after.flips - before.flips,
      moved: Math.round(now - was),
      top: Math.round(now),
      viewport: window.innerHeight,
      visible: now > 0 && now < window.innerHeight,
    });
  })()`);
  const n = JSON.parse(near);
  check('sidebar', 'the survivor really did move, on screen and by a short hop', n.moved > 8 && n.moved < n.viewport && n.visible === true, near);
  check('sidebar', 'and a 10-id change still runs no FLIP, however few rows moved', n.flips === 0, near);
  check('sidebar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // FLIP is a *position* animation, so `reduced` snaps it like the gliders: travelling
  // `--motion-distance × the distance` would leave rows in the wrong place.
  await s.open('/sidebar/?mock&motion=reduced');
  const reduced = await s.evaluate(`(async () => {
    ${HELP}
    await wait(400);
    const topId = () => document.querySelectorAll('.today-list [data-row]')[1]?.dataset.id ?? null;
    const was = topId();
    const before = __motion.stats();
    __mock.setState((st) => {
      const today = st.spaces.find((x) => x.id === 15).today;
      const first = today[1];
      today[1] = today[2];
      today[2] = first;
    });
    await until(() => topId() !== was);
    await frame();
    const after = __motion.stats();
    return JSON.stringify({
      level: document.documentElement.dataset.motion,
      flips: after.flips - before.flips,
      running: ids().filter((id) => id === 'sidebar.reorder').length,
    });
  })()`);
  const rd = JSON.parse(reduced);
  check('sidebar', 'at the reduced level a reorder snaps instead of FLIPping', rd.level === 'reduced' && rd.flips === 0, reduced);
  check('sidebar', 'and leaves nothing running under the reorder key', rd.running === 0, reduced);
  check('sidebar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkSidebarStates(s) {
  await s.open('/sidebar/?mock&motion=full');
  const on = await s.evaluate(`(async () => {
    ${HELP}
    const active = document.querySelector('.row.is-active');
    const glider = document.querySelector('.split-glider');
    const out = {
      activeFill: getComputedStyle(active, '::before').opacity,
      activeFade: fade(active, '::before'),
      gliderIndex: getComputedStyle(glider).getPropertyValue('--seg-index').trim(),
      gliderShift: glider.getBoundingClientRect().left - document.querySelector('.split-row').getBoundingClientRect().left,
      gliderWidth: glider.getBoundingClientRect().width,
      gliderHits: getComputedStyle(glider).pointerEvents,
      gliderFade: fade(glider),
      gliderOpacity: getComputedStyle(glider).opacity,
    };
    // The hover reveal's hide: a fade no longer than the shell's own correctness delay, and no clicks.
    __mock.emit('sidebar.hover', { visible: false });
    await until(() => document.querySelector('.sidebar.is-hover-hidden'));
    out.hiddenHits = getComputedStyle(document.querySelector('.sidebar')).pointerEvents;
    out.hideFade = fade(document.querySelector('.sidebar.is-hover-hidden > .top-row'));
    __mock.emit('sidebar.hover', { visible: true });
    await until(() => document.querySelector('.sidebar.is-hover-entering'));
    out.enter = getComputedStyle(document.querySelector('.sidebar.is-hover-entering > .top-row')).animationName;
    return JSON.stringify(out);
  })()`);
  const o = JSON.parse(on);
  check('sidebar', 'the active row fill is a layer of its own, at full opacity', o.activeFill === '1', on);
  check('sidebar', 'it crossfades on the active-row key (120 ms)', Math.abs(o.activeFade - 0.12) < 0.001, on);
  check('sidebar', 'the split glider marks the focused pane', o.gliderIndex === '1' && o.gliderOpacity !== '0', on);
  check('sidebar', "it stands one segment width in, past the row's own 3px inset", Math.abs(o.gliderShift - o.gliderWidth - 3) <= 1, on);
  check('sidebar', 'it never takes pointer events and glides on its own key (140 ms)', o.gliderHits === 'none' && Math.abs(o.gliderFade - 0.14) < 0.001, on);
  check('sidebar', 'a hiding sidebar stops taking clicks', o.hiddenHits === 'none', on);
  check('sidebar', 'its fade never outlasts the shell hide delay (<= 60 ms)', o.hideFade > 0 && o.hideFade <= 0.06, on);
  check('sidebar', 'a reveal slides the contents back in', o.enter === 'sidebar-hover-in', on);
  check('sidebar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // The floating card is the one that travels (the shell slides its host in from outside the
  // window): its contents must not slide inside it as well. At `reduced` nothing travels, so the
  // page's own fade is all there is and it comes back.
  const revealEnter = async (level) => {
    await s.open(`/sidebar/?mock&motion=${level}&sidebar=0`);
    return s.evaluate(`(async () => {
      ${HELP}
      await wait(400);
      __mock.emit('sidebar.hover', { visible: false });
      await until(() => document.querySelector('.sidebar.is-hover-hidden'));
      __mock.emit('sidebar.hover', { visible: true });
      await until(() => document.querySelector('.sidebar.is-hover-entering'));
      const row = document.querySelector('.sidebar.is-floating.is-hover-entering > .top-row');
      return JSON.stringify({ floating: !!row, name: row && getComputedStyle(row).animationName });
    })()`);
  };
  const enterFull = JSON.parse(await revealEnter('full'));
  const enterReduced = JSON.parse(await revealEnter('reduced'));
  check('sidebar', 'a floating reveal leaves its contents alone: the card itself slides', enterFull.floating && enterFull.name === 'none', enterFull);
  check('sidebar', 'at reduced the shell does not slide it, so the page fades its contents in', enterReduced.name === 'sidebar-hover-in', enterReduced);

  // Per-key switches, one key at a time: an off key takes its own motion away and nothing else.
  await s.open('/sidebar/?mock&motion=full&animOff=sidebar.tabInsertRemove,sidebar.activeRow,sidebar.splitRow');
  const off = await s.evaluate(`(async () => {
    ${HELP}
    await wait(400);
    const before = __motion.stats();
    __mock.setState((st) => {
      const space = st.spaces.find((x) => x.id === 15);
      space.today.splice(1, 0, { ...space.today[2], id: 901 });
    });
    await until(() => document.querySelector('.today-list [data-id="901"]'));
    await frame();
    const after = __motion.stats();
    const cs = getComputedStyle(document.documentElement);
    const active = document.querySelector('.row.is-active');
    return JSON.stringify({
      started: after.started - before.started,
      skipped: after.skipped - before.skipped,
      insertToken: cs.getPropertyValue('--t-sidebar-tab-insert-remove').trim(),
      reorderToken: cs.getPropertyValue('--t-sidebar-reorder').trim(),
      activeFade: fade(active, '::before'),
      activeFill: getComputedStyle(active, '::before').opacity,
      gliderFade: fade(document.querySelector('.split-glider')),
      gliderOpacity: getComputedStyle(document.querySelector('.split-glider')).opacity,
      running: ids(),
    });
  })()`);
  const f = JSON.parse(off);
  check('sidebar', 'sidebar.tabInsertRemove off: an inserted row starts nothing', f.started === 0 && f.skipped > 0, off);
  check('sidebar', 'and only its own token is zeroed', f.insertToken === '0ms' && f.reorderToken === '200ms', off);
  check('sidebar', 'sidebar.activeRow off: the fill is still there, it just does not fade', f.activeFade === 0 && f.activeFill === '1', off);
  check('sidebar', 'sidebar.splitRow off: the glider still marks the pane, it just does not glide', f.gliderFade === 0 && f.gliderOpacity !== '0', off);
  check('sidebar', 'nothing from this area is running', f.running.filter((id) => id.startsWith('sidebar.')).length === 0, off);
  check('sidebar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkCommandBar(s) {
  await s.open('/command/?mock&commandBar=newTab&motion=full');
  const result = await s.evaluate(`(async () => {
    ${HELP}
    const input = document.getElementById('input');
    const list = document.getElementById('cmd-list');
    const glider = document.querySelector('.cmd-glider');
    const out = {
      glider: Boolean(glider),
      gliderHits: getComputedStyle(glider).pointerEvents,
      at: gliderTop(glider),
      firstTop: rowsOf(list)[0].offsetTop,
      rowFill: getComputedStyle(rowsOf(list)[0]).backgroundColor,
    };
    // (a) a keyboard move glides.
    let before = __motion.stats();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }));
    await frame();
    out.keyGlides = anims('commandBar.selection').length;
    out.keyStarted = __motion.stats().started - before.started;
    out.afterKey = gliderTop(glider);
    out.secondTop = rowsOf(list)[1].offsetTop;
    await wait(160);
    // (b) a hover snaps: the glider moves, nothing animates.
    before = __motion.stats();
    const target = rowsOf(list)[3];
    const box = target.getBoundingClientRect();
    target.dispatchEvent(new PointerEvent('pointermove', { clientX: box.left + 8, clientY: box.top + 8, bubbles: true }));
    await frame();
    out.hoverStarted = __motion.stats().started - before.started;
    out.afterHover = gliderTop(glider);
    out.hoverTop = target.offsetTop;
    await wait(160);
    // (c) the mode chip and the placeholder.
    const wasPh = input.dataset.ph;
    __commandBar.toggleActions();
    await frame();
    out.modeAnims = anims('commandBar.modeToggle').length;
    out.phFlipped = input.dataset.ph !== wasPh;
    out.placeholder = getComputedStyle(input, '::placeholder').animationName;
    out.inputTransform = getComputedStyle(input).transform;
    await wait(240);
    // (d) the first results of a *fresh* open stagger in.
    await window.sta.dispatch({ type: 'closeCommandBar' });
    await wait(240);
    __mock.setState((st) => {
      st.commandBar = { mode: 'newTab', text: '', splitSide: null, seq: Date.now() };
    });
    await until(() => anims('commandBar.results').length > 1, 3000);
    out.resultDelays = anims('commandBar.results').map((a) => a.effect.getTiming().delay);
    out.openAnims = anims('commandBar.open').length;
    out.cardTransform = getComputedStyle(document.querySelector('.cmd')).transform;
    return JSON.stringify(out);
  })()`);
  const r = JSON.parse(result);
  check('cmdbar', 'the selection is one glider layer, not a row background', r.glider === true && r.rowFill === 'rgba(0, 0, 0, 0)', result);
  check('cmdbar', 'it never takes pointer events', r.gliderHits === 'none', result);
  check('cmdbar', 'it starts on the selected row', r.at === r.firstTop, result);
  check('cmdbar', 'a keyboard move glides it', r.keyGlides === 1 && r.keyStarted === 1 && r.afterKey === r.secondTop, result);
  check('cmdbar', 'a hover snaps it: the glider moves, nothing animates', r.hoverStarted === 0 && r.afterHover === r.hoverTop, result);
  check('cmdbar', 'a mode change crossfades the chip and the leading glyph', r.modeAnims === 2, result);
  check('cmdbar', 'and restarts the placeholder fade through its own attribute', r.phFlipped === true && /^cmd-placeholder-[ab]$/.test(r.placeholder), result);
  check('cmdbar', 'the input is never transformed (the IME candidate window must stay at the caret)', r.inputTransform === 'none', result);
  check('cmdbar', 'the first results of a fresh open fade in', r.resultDelays.length > 1, result);
  check('cmdbar', 'their stagger spans no more than 100 ms', Math.max(...r.resultDelays, 0) <= 100, result);
  check('cmdbar', 'the card fades without ever being transformed (the shell keeps the size it reports)', r.openAnims > 0 && r.cardTransform === 'none', result);
  check('cmdbar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  await s.open('/command/?mock&commandBar=newTab&motion=full&animOff=commandBar.selection');
  const off = await s.evaluate(`(async () => {
    ${HELP}
    const input = document.getElementById('input');
    const list = document.getElementById('cmd-list');
    const glider = document.querySelector('.cmd-glider');
    const before = __motion.stats();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }));
    await frame();
    return JSON.stringify({
      started: __motion.stats().started - before.started,
      at: gliderTop(glider),
      want: rowsOf(list)[1].offsetTop,
      visible: getComputedStyle(glider).opacity,
    });
  })()`);
  const o = JSON.parse(off);
  check('cmdbar', 'commandBar.selection off: the highlight still moves, it just does not glide', o.started === 0 && o.at === o.want, off);
  check('cmdbar', 'and the selection is still visible', o.visible === '1', off);
  check('cmdbar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkTopBar(s) {
  await s.open('/topbar/?mock&motion=full');
  const result = await s.evaluate(`(async () => {
    ${HELP}
    // (a) the nav buttons appear only after the top bar has resized, so they fade rather than slide.
    let before = __motion.stats();
    __mock.setState((st) => {
      st.window.sidebarVisible = false;
    });
    await until(() => document.querySelector('.tb-center .url-pill'));
    await frame();
    const fades = anims('topbar.navFade');
    const out = {
      fades: fades.length,
      delay: fades.length ? fades[0].effect.getTiming().delay : -1,
      started: __motion.stats().started - before.started,
    };
    await wait(260);

    // (b) the same tab moving to another site crossfades the host.
    const setSite = (host) =>
      __mock.setState((st) => {
        const id = st.current.tab;
        const stack = [...st.favorites, ...st.spaces.flatMap((sp) => [...sp.pinned, ...sp.today])];
        while (stack.length) {
          const node = stack.pop();
          if (node.kind === 'folder') stack.push(...node.children);
          else if (node.kind === 'split') stack.push(...node.panes);
          if (node.id === id) {
            node.host = host;
            node.url = 'https://' + host + '/';
          }
        }
      });
    before = __motion.stats();
    setSite('example.test');
    await until(() => (document.querySelector('.url-pill-text').textContent || '').includes('example.test'));
    await frame();
    let after = __motion.stats();
    out.sameTab = { running: ids(), ghosts: after.ghosts - before.ghosts };
    await wait(260);

    // (c) another tab's host in the pill changes instantly (Ctrl+Tab must not crossfade).
    before = __motion.stats();
    await window.sta.dispatch({ type: 'activateItem', id: 34 });
    await until(() => document.querySelector('.url-pill-text').textContent !== 'example.test');
    await frame();
    after = __motion.stats();
    out.otherTab = { started: after.started - before.started, ghosts: after.ghosts - before.ghosts };
    return JSON.stringify(out);
  })()`);
  const r = JSON.parse(result);
  check('topbar', 'the nav buttons and the pill fade in when the sidebar hides', r.fades === 2, result);
  check('topbar', "and only after the shell's own resize delay", r.delay >= 60, result);
  check('topbar', 'the same tab changing site crossfades the host', r.sameTab.running.includes('sidebar.urlPill') && r.sameTab.ghosts === 1, result);
  check('topbar', 'another tab in the pill changes instantly', r.otherTab.started === 0 && r.otherTab.ghosts === 0, result);
  check('topbar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

async function checkSidebarExtras(s) {
  await s.open('/sidebar/?mock&motion=full');
  // (a) drag & drop: the ghost lifts with the individual `scale` property (its inline `transform`
  // follows the pointer), the drop line glides between insertion points, and a cancelled drag gets
  // nothing at all.
  const drag = await s.evaluate(`(async () => {
    ${HELP}
    await wait(400);
    const row = document.querySelector('.today-list [data-id="33"]');
    const r = row.getBoundingClientRect();
    const at = (type, x, y, target) =>
      (target ?? window).dispatchEvent(
        new PointerEvent(type, { pointerId: 7, isPrimary: true, button: 0, buttons: 1, clientX: x, clientY: y, bubbles: true, cancelable: true }),
      );
    at('pointerdown', r.left + 40, r.top + 18, row);
    at('pointermove', r.left + 48, r.top + 28);
    // Aim at a row's top edge: the middle band of a Today row is a *split* target, which
    // highlights the row instead of showing an insertion line.
    at('pointermove', r.left + 60, r.top + 77);
    await frame();
    const ghost = document.querySelector('.drag-ghost');
    const line = document.querySelector('.drop-line');
    const out = {
      lift: ghost ? getComputedStyle(ghost).animationName : null,
      liftMs: ghost ? getComputedStyle(ghost).animationDuration : null,
    };
    await wait(240); // the lift is 120ms: read the scale it settles at, not a frame of the way there
    out.lifted = ghost ? getComputedStyle(ghost).scale : null;
    out.firstLine = line && !line.hidden ? line.style.translate : null;
    const before = __motion.stats();
    at('pointermove', r.left + 60, r.top + 153);
    await frame();
    out.glides = anims('sidebar.dragDrop').length;
    out.started = __motion.stats().started - before.started;
    out.secondLine = line && !line.hidden ? line.style.translate : null;
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await frame();
    out.cancelled = document.querySelectorAll('.drag-ghost').length;
    out.lineGone = document.querySelector('.drop-line').hidden;
    return JSON.stringify(out);
  })()`);
  const d = JSON.parse(drag);
  check('sidebar', 'the drag ghost lifts with the `scale` property, not a transform', Math.abs(Number.parseFloat(d.lifted) - 1.03) < 0.001, drag);
  check('sidebar', "and on the drag key's own token (120 ms)", d.lift === 'ghost-lift' && d.liftMs === '0.12s', drag);
  check('sidebar', 'the drop line glides to the next insertion point', d.glides === 1 && d.started === 1 && d.firstLine !== d.secondLine, drag);
  check('sidebar', 'a cancelled drag leaves nothing behind', d.cancelled === 0 && d.lineGone === true, drag);

  // (b) favorites: a tile pops in, the grid FLIPs on a reorder, and the press is a `scale` spring.
  const favorites = await s.evaluate(`(async () => {
    ${HELP}
    await wait(300);
    const tile = document.querySelector('.fav-tile');
    const press = getComputedStyle(tile);
    let before = __motion.stats();
    __mock.setState((st) => {
      const f = st.favorites;
      const first = f[0];
      f[0] = f[1];
      f[1] = first;
    });
    await until(() => document.querySelectorAll('.favorites [data-row]')[0].dataset.id !== tile.dataset.id);
    await frame();
    let after = __motion.stats();
    const reorder = { flips: after.flips - before.flips, running: ids() };
    await wait(260);
    before = __motion.stats();
    __mock.setState((st) => {
      st.favorites.splice(2, 0, { ...st.favorites[0], id: 910 });
    });
    await until(() => document.querySelector('.favorites [data-id="910"]'));
    await frame();
    after = __motion.stats();
    const pop = anims('sidebar.favorites')[0];
    return JSON.stringify({
      transition: press.transitionProperty + '|' + press.transitionDuration,
      reorder,
      added: after.started - before.started,
      keyframes: pop ? pop.effect.getKeyframes().map((k) => k.scale ?? null) : null,
    });
  })()`);
  const f = JSON.parse(favorites);
  check('sidebar', 'a favorites reorder FLIPs the grid on its own key', f.reorder.flips === 1 && f.reorder.running.includes('sidebar.favorites'), favorites);
  check('sidebar', 'an added tile pops in with `scale`, not a transform', f.added > 0 && JSON.stringify(f.keyframes) === '["0.86","1"]', favorites);
  check('sidebar', 'the press springs the tile with the `scale` property on the favorites token', f.transition.includes('scale') && f.transition.endsWith('0.14s'), favorites);

  // (c) panels, downloads and the URL pill's copy check.
  const rest = await s.evaluate(`(async () => {
    ${HELP}
    await wait(300);
    // A panel that a shared component renders into a portal leaves an exit ghost of its own.
    let before = __motion.stats();
    await window.sta.dispatch({ type: 'openSidebarPanel', panel: { type: 'downloads' } });
    await until(() => document.querySelector('.portal-host .downloads-popover'));
    await wait(200);
    before = __motion.stats();
    await window.sta.dispatch({ type: 'closeSidebarPanel' });
    await until(() => !document.querySelector('.portal-host .downloads-popover'));
    await frame();
    let after = __motion.stats();
    const panel = { ghosts: after.ghosts - before.ghosts, running: ids() };
    __motion.clearGhosts();
    await wait(260);

    // The in-progress card rises on the downloads token; a finished download hands the ring over to
    // a check that pops in.
    const card = document.querySelector('.dl-card');
    const cardStyle = card ? getComputedStyle(card).animationName + '|' + getComputedStyle(card).animationDuration : null;
    before = __motion.stats();
    __mock.setState((st) => {
      st.downloads = [{ ...st.downloads[0], id: 981, state: 'complete', receivedBytes: 100, totalBytes: 100 }];
    });
    await until(() => document.querySelector('.bb-done'));
    await frame();
    after = __motion.stats();
    const done = { running: ids(), started: after.started - before.started };
    await wait(260);

    // The copy button check pops in, keyed on the copied flag flipping.
    before = __motion.stats();
    document.querySelector('.url-pill-copy').click();
    await until(() => document.querySelector('.url-pill-copy.is-done'));
    await frame();
    after = __motion.stats();
    const copy = { running: ids(), started: after.started - before.started };
    return JSON.stringify({ panel, cardStyle, done, copy });
  })()`);
  const r = JSON.parse(rest);
  check('sidebar', 'a closing panel leaves an exit ghost under sidebar.panels', r.panel.ghosts === 1 && r.panel.running.includes('sidebar.panels'), rest);
  check('sidebar', 'the download card rises on the downloads token (180 ms)', r.cardStyle === 'card-up|0.18s', rest);
  check('sidebar', 'a finished download pops a check in where the ring was', r.done.started === 1 && r.done.running.includes('sidebar.downloads'), rest);
  check('sidebar', "the URL pill's copy check pops in", r.copy.started === 1 && r.copy.running.includes('sidebar.urlPill'), rest);
  check('sidebar', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

// ------------------------------------------------------------------------------------ overlays

async function checkOverlays(s) {
  // (a) the toast: the pill rises, a *replacement* only crosses the text over, and the tracked root
  // is never touched (the shell keeps whatever size it reports).
  await s.open('/toast/?mock&motion=full');
  const toast = await s.evaluate(`(async () => {
    ${HELP}
    let before = __motion.stats();
    __mock.setState((st) => { st.toast = { id: 91, message: 'Cleared 4 tabs', action: null, durationMs: 60000 }; });
    await until(() => document.querySelector('.toast-msg'));
    const enter = { started: __motion.stats().started - before.started, on: owners('overlays.toast'), props: kprops('overlays.toast') };
    await wait(250); // let the rise finish, so what runs next is only what the replacement started
    before = __motion.stats();
    __mock.setState((st) => { st.toast = { id: 92, message: 'Cleared 9 tabs', action: null, durationMs: 60000 }; });
    await until(() => (document.querySelector('.toast-msg') || {}).textContent === 'Cleared 9 tabs');
    const swap = { started: __motion.stats().started - before.started, on: owners('overlays.toast') };
    const root = document.querySelector('.toast');
    return JSON.stringify({ enter, swap, rootTransform: getComputedStyle(root).transform, rootAnims: root.getAnimations().length });
  })()`);
  const t = JSON.parse(toast);
  check('overlays', 'a toast rises in under overlays.toast', t.enter.started === 1 && t.enter.on.includes('toast-inner'), toast);
  check('overlays', 'it rises by 4 px at most, in transform and opacity only', JSON.stringify(t.enter.props) === '["opacity","translate"]', toast);
  check('overlays', 'a replacement crosses only the text over, and never re-plays the rise', t.swap.started === 1 && t.swap.on.join() === 'toast-msg', toast);
  check('overlays', 'the tracked root is never animated or transformed', t.rootAnims === 0 && t.rootTransform === 'none', toast);

  // Inside the shell's card the page is 28 DIP high and `html, body { overflow: hidden }` clips it:
  // the pill's own controls have ~1 px of slack, so a rise would slice the action button and the ×
  // flat against the card's rounded edge. There the entrance is a fade.
  const card = await s.evaluate(`(async () => {
    ${HELP}
    document.documentElement.classList.add('native-card');
    __mock.setState((st) => { st.toast = null; });
    await until(() => !document.querySelector('.toast-msg'));
    await wait(100);
    __mock.setState((st) => { st.toast = { id: 93, message: 'Cleared 4 tabs', action: { label: 'Undo', command: null }, durationMs: 60000 }; });
    await until(() => document.querySelector('.toast-action'));
    const props = kprops('overlays.toast');
    const frames = anims('overlays.toast').map((a) => a.effect.getKeyframes().map((f) => String(f.translate ?? 'none')));
    const page = document.documentElement.clientHeight;
    const inside = [...document.querySelectorAll('.toast-inner *')].every((el) => el.getBoundingClientRect().bottom <= page + 0.5);
    document.documentElement.classList.remove('native-card');
    return JSON.stringify({ props, frames, page, inside, started: anims('overlays.toast').length });
  })()`);
  const c = JSON.parse(card);
  check('overlays', 'inside the native card the pill still animates in', c.started === 1, card);
  check('overlays', 'it fades and never moves there', JSON.stringify(c.props) === '["opacity","translate"]' && c.frames.every((f) => f.every((v) => /^(none|0px|0 0px|0px 0px|0)$/.test(v))), card);
  check('overlays', "and nothing it draws leaves the card page's box", c.inside === true, card);
  check('overlays', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // (b) the switcher: one fade for all the cards, and a ring that glides to the selection.
  await s.open('/switcher/?mock&switcher=1&motion=full');
  const sw = await s.evaluate(`(async () => {
    ${HELP}
    const ring = document.querySelector('.sw-ring');
    const cards = document.querySelectorAll('.sw-card').length;
    const enter = { cards, fading: anims('overlays.switcher').length, props: kprops('overlays.switcher') };
    await wait(250);
    const placed = ring.style.translate;
    const before = __motion.stats();
    __mock.setState((st) => { st.switcher.selected = 3; });
    await until(() => ring.style.translate !== placed);
    const glide = { started: __motion.stats().started - before.started, on: owners('overlays.switcher'), props: kprops('overlays.switcher'), to: ring.style.translate };
    const root = document.querySelector('.sw');
    return JSON.stringify({ enter, placed, glide, size: [ring.style.width, ring.style.height], rootTransform: getComputedStyle(root).transform });
  })()`);
  const w = JSON.parse(sw);
  check('overlays', 'every switcher card fades in together (no stagger)', w.enter.cards > 1 && w.enter.fading === w.enter.cards, sw);
  check('overlays', 'the cards fade and nothing else', JSON.stringify(w.enter.props) === '["opacity"]', sw);
  check('overlays', "the ring is placed on the selected card's own box", /px/.test(w.placed) && /px/.test(w.size[0]), sw);
  check('overlays', 'moving the selection glides the ring alone', w.glide.started === 1 && w.glide.on.join() === 'sw-ring', sw);
  check('overlays', 'the glide animates translate only, and the cards do not re-fade', JSON.stringify(w.glide.props) === '["translate"]', sw);
  check('overlays', 'the tracked root is never transformed', w.rootTransform === 'none', sw);
  check('overlays', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  await s.open('/switcher/?mock&switcher=1&motion=reduced');
  const swReduced = await s.evaluate(`(async () => {
    ${HELP}
    const ring = document.querySelector('.sw-ring');
    await wait(200);
    const placed = ring.style.translate;
    const before = __motion.stats();
    __mock.setState((st) => { st.switcher.selected = 3; });
    await until(() => ring.style.translate !== placed);
    return JSON.stringify({ moved: ring.style.translate !== placed, started: __motion.stats().started - before.started });
  })()`);
  check('overlays', 'at reduced the ring snaps to the new card instead of gliding', swReduced === '{"moved":true,"started":0}', swReduced);

  // (c) the find bar: a fade with no transform (it holds a focused input), and a shake only when the
  // user asks *again* for a search already known to be empty.
  await s.open('/find/?mock&find=1&text=zzz&motion=full');
  const find = await s.evaluate(`(async () => {
    ${HELP}
    const enterProps = kprops('overlays.find');
    await wait(200);
    const tab = __mock.state.find.tab;
    const result = (count) => __mock.emit('find.result', { tab, count, active: count ? 1 : 0, final: true });
    let before = __motion.stats();
    result(3);
    await frame();
    const found = __motion.stats().started - before.started;
    before = __motion.stats();
    result(0);
    await frame();
    const firstEmpty = __motion.stats().started - before.started;
    before = __motion.stats();
    result(0);
    await frame();
    const askedAgain = { started: __motion.stats().started - before.started, props: kprops('overlays.find'), on: owners('overlays.find') };
    return JSON.stringify({ enterProps, found, firstEmpty, askedAgain, empty: document.querySelector('.find.is-empty') !== null });
  })()`);
  const f = JSON.parse(find);
  check('overlays', 'the find bar fades in with no transform (the caret and the IME stay put)', JSON.stringify(f.enterProps) === '["opacity"]', find);
  check('overlays', 'matches found: nothing animates', f.found === 0, find);
  check('overlays', 'the first "no matches" only records it — typing must not shake the bar', f.firstEmpty === 0, find);
  check('overlays', 'asking again for an empty search shakes the bar', f.askedAgain.started === 1 && JSON.stringify(f.askedAgain.props) === '["translate"]', find);
  check('overlays', 'and it says so in the counter', f.empty === true, find);
  check('overlays', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  const findComposing = await s.evaluate(`(async () => {
    ${HELP}
    const input = document.querySelector('.find-input');
    input.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
    const tab = __mock.state.find.tab;
    const before = __motion.stats();
    __mock.emit('find.result', { tab, count: 0, active: 0, final: true });
    await frame();
    const started = __motion.stats().started - before.started;
    input.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true }));
    return String(started);
  })()`);
  check('overlays', 'nothing moves the bar while the IME is composing', findComposing === '0', findComposing);

  // (d) the permission prompt: a fade, and only a fade, on an inner wrapper.
  await s.open('/permission/?mock&permission=camera,microphone&motion=full');
  const perm = await s.evaluate(`(async () => {
    ${HELP}
    const enter = { props: kprops('overlays.permission'), on: owners('overlays.permission') };
    const root = document.querySelector('.perm');
    const rootState = { anims: root.getAnimations().length, transform: getComputedStyle(root).transform };
    await wait(200);
    const before = __motion.stats();
    __mock.setState((st) => { st.permissionPrompts = st.permissionPrompts.concat([{ ...st.permissionPrompts[0], id: 2 }]); });
    await until(() => document.querySelector('.perm-queue'));
    await frame();
    return JSON.stringify({ enter, rootState, queue: { started: __motion.stats().started - before.started, on: owners('indicators.badges'), props: kprops('indicators.badges') } });
  })()`);
  const p = JSON.parse(perm);
  check('overlays', 'the permission prompt only fades (no spring in front of Allow)', JSON.stringify(p.enter.props) === '["opacity"]' && p.enter.on.join() === 'perm-inner', perm);
  check('overlays', 'its tracked root is untouched', p.rootState.anims === 0 && p.rootState.transform === 'none', perm);
  check('overlays', 'the queue count pops when another request lines up', p.queue.started === 1 && p.queue.on.join() === 'perm-queue' && JSON.stringify(p.queue.props) === '["scale"]', perm);
  check('overlays', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // (e) Peek: only what the strip *says* about the page crosses over, keyed on the peeked tab.
  await s.open('/peek/?mock&peek=1&motion=full');
  const peek = await s.evaluate(`(async () => {
    ${HELP}
    const enter = { props: kprops('overlays.peek'), on: owners('overlays.peek') };
    await wait(200);
    let before = __motion.stats();
    __mock.setState((st) => { st.peek.tab = { ...st.peek.tab, id: st.peek.tab.id + 1, title: 'Another page', host: 'other.example.com' }; });
    await until(() => (document.querySelector('.peek-title') || {}).textContent === 'Another page');
    const swap = __motion.stats().started - before.started;
    before = __motion.stats();
    __mock.setState((st) => { st.peek.tab = { ...st.peek.tab, loading: true }; });
    await frame();
    await frame();
    const sameTab = __motion.stats().started - before.started;
    return JSON.stringify({ enter, swap, sameTab });
  })()`);
  const pk = JSON.parse(peek);
  check('overlays', "Peek's header content fades in", JSON.stringify(pk.enter.props) === '["opacity"]' && pk.enter.on.join() === 'peek-center', peek);
  check('overlays', 'peeking another page crosses the header over', pk.swap === 1, peek);
  check('overlays', 'an update to the same tab does not', pk.sameTab === 0, peek);
  check('overlays', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

// ------------------------------------------------------------------------------------ menus

async function checkMenus(s) {
  await s.open('/_gallery/?mock&motion=full');
  const open = await s.evaluate(`(async () => {
    ${HELP}
    const button = [...document.querySelectorAll('.btn')].find((b) => b.textContent.includes('Anchored menu'));
    button.click();
    await until(() => document.querySelector('.portal-host .menu'));
    const menu = document.querySelector('.portal-host .menu');
    const cs = getComputedStyle(menu);
    const pop = {
      name: cs.animationName,
      duration: cs.animationDuration,
      origin: menu.style.getPropertyValue('--pop-origin'),
      dy: menu.style.getPropertyValue('--pop-dy'),
      transformOrigin: cs.transformOrigin,
    };
    // Drill down with the keyboard (the hover path waits 140 ms on purpose). One ArrowDown per
    // frame: the highlight is component state, so it lands on the next render, not on the dispatch.
    menu.focus();
    const active = () => menu.querySelector('.menu-item.is-active');
    let found = false;
    for (let i = 0; i < 12 && !found; i++) {
      found = Boolean(active() && active().textContent.includes('Move to Space'));
      if (found) break;
      menu.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
      await frame();
    }
    menu.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    await until(() => document.querySelector('.menu.is-submenu'));
    const subEl = document.querySelector('.menu.is-submenu');
    return JSON.stringify({ pop, drilled: found, sub: subEl ? getComputedStyle(subEl).animationName : null });
  })()`);
  const m = JSON.parse(open);
  check('menus', 'a menu pops in on its own key (140 ms)', m.pop.name === 'sta-pop-in' && m.pop.duration === '0.14s', open);
  check('menus', 'it grows out of the side it was placed on', /^(top|bottom) (left|right)$/.test(m.pop.origin) && m.pop.transformOrigin !== '', open);
  check('menus', 'the travel has a direction', m.pop.dy === '-1' || m.pop.dy === '1', open);
  check('menus', 'a drill-down submenu slides out of its parent instead', m.sub === 'sta-drill-in', open);

  const close = await s.evaluate(`(async () => {
    ${HELP}
    const before = __motion.stats();
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    document.querySelector('.portal-host .menu')?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await until(() => !document.querySelector('.portal-host .menu'));
    await frame();
    const g = document.querySelector('.motion-ghosts .motion-ghost');
    return JSON.stringify({
      ghosts: __motion.stats().ghosts - before.ghosts,
      running: ids(),
      inert: g ? g.inert === true : false,
      hidden: g ? g.getAttribute('aria-hidden') === 'true' : false,
      attrs: g ? [...g.attributes].map((a) => a.name).sort() : [],
      items: document.querySelectorAll('.motion-ghosts [role="menuitem"], .motion-ghosts [id]').length,
    });
  })()`);
  const c = JSON.parse(close);
  check('menus', 'a closing menu leaves one inert ghost per panel', c.ghosts >= 1 && c.inert === true && c.hidden === true, close);
  check('menus', 'it sinks back under menus.popIn', c.running.includes('menus.popIn'), close);
  check('menus', 'the ghost keeps only class, style and aria-hidden', JSON.stringify(c.attrs) === '["aria-hidden","class","inert","style"]', close);
  check('menus', 'nothing inside a ghost has a role or an id any more', c.items === 0, close);
  check('menus', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  await s.open('/_gallery/?mock&motion=full&animOff=menus.popIn');
  const offKey = await s.evaluate(`(async () => {
    ${HELP}
    const button = [...document.querySelectorAll('.btn')].find((b) => b.textContent.includes('Anchored menu'));
    const before = __motion.stats();
    button.click();
    await until(() => document.querySelector('.portal-host .menu'));
    const duration = getComputedStyle(document.querySelector('.portal-host .menu')).animationDuration;
    button.click();
    await until(() => !document.querySelector('.portal-host .menu'));
    await frame();
    return JSON.stringify({ duration, ghosts: __motion.stats().ghosts - before.ghosts });
  })()`);
  check('menus', 'with the key off the pop-in is 0s and no ghost is left behind', offKey === '{"duration":"0s","ghosts":0}', offKey);

  await s.open('/_gallery/?mock&motion=reduced');
  const reduced = await s.evaluate(`(async () => {
    ${HELP}
    const button = [...document.querySelectorAll('.btn')].find((b) => b.textContent.includes('Anchored menu'));
    button.click();
    await until(() => document.querySelector('.portal-host .menu'));
    const menu = document.querySelector('.portal-host .menu');
    const cs = getComputedStyle(menu);
    return JSON.stringify({ duration: cs.animationDuration, transform: cs.transform });
  })()`);
  const r = JSON.parse(reduced);
  check('menus', 'at reduced the menu still fades over its own duration', r.duration === '0.14s', reduced);
  check('menus', 'but it does not move: the travel is an identity transform', r.transform === 'none' || r.transform === 'matrix(1, 0, 0, 1, 0, 0)', reduced);
  check('menus', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

// ------------------------------------------------------------------------------------ pages

async function checkPages(s) {
  // (a) the page's own enter, and the settings nav indicator.
  await s.open('/settings/?mock&motion=full');
  const enter = await s.evaluate(`(async () => {
    ${HELP}
    const running = anims('pages.enter');
    const delays = running.map((a) => Number(a.effect.getComputedTiming().delay));
    return JSON.stringify({ n: running.length, maxDelay: Math.max(0, ...delays), props: kprops('pages.enter'), staggers: __motion.stats().staggers });
  })()`);
  const e = JSON.parse(enter);
  check('pages', 'the page staggers at most six cards in', e.n > 1 && e.n <= 6, enter);
  check('pages', 'the whole stagger fits the 200 ms page-enter budget', e.maxDelay <= 200, enter);
  check('pages', 'the cards rise and fade, nothing else', JSON.stringify(e.props) === '["opacity","translate"]', enter);

  const nav = await s.evaluate(`(async () => {
    ${HELP}
    await wait(300);
    slow('--t-pages-nav-indicator', 4000);
    const bar = document.querySelector('.set-nav-list .ip-nav-indicator');
    const placed = bar.style.translate;
    const before = __motion.stats();
    [...document.querySelectorAll('.set-nav-link')][4].click();
    await until(() => bar.style.translate !== placed);
    return JSON.stringify({
      placed,
      size: [bar.style.width, bar.style.height],
      started: __motion.stats().started - before.started,
      props: kprops('pages.navIndicator'),
      on: owners('pages.navIndicator'),
    });
  })()`);
  const n = JSON.parse(nav);
  check('pages', "the nav indicator sits on the active link's own box", /px/.test(n.placed) && n.size[0] !== '' && n.size[1] !== '', nav);
  check('pages', 'picking another section glides it there', n.started === 1 && JSON.stringify(n.props) === '["translate"]', nav);
  check('pages', 'and only the indicator moves', n.on.join() === 'ip-nav-indicator', nav);

  // (b) the disclosure in Settings › Animations: a height animation, which only an internal page may do.
  const disclose = await s.evaluate(`(async () => {
    ${HELP}
    const head = document.querySelector('.set-anim-disclosure');
    const list = document.getElementById(head.getAttribute('aria-controls'));
    const before = __motion.stats();
    head.click();
    await frame();
    const a = anims('controls.toggles')[0];
    const props = kprops('controls.toggles');
    const openState = { started: __motion.stats().started - before.started, props, hidden: list.hidden, overflow: list.style.overflow };
    await wait(300);
    return JSON.stringify({ openState, settled: { hidden: list.hidden, overflow: list.style.overflow, height: list.style.height } });
  })()`);
  const d = JSON.parse(disclose);
  check('pages', 'a disclosure grows its height open (internal pages may animate layout)', d.openState.started === 1 && JSON.stringify(d.openState.props) === '["height"]', disclose);
  check('pages', 'it is visible while it grows, and clipped', d.openState.hidden === false && d.openState.overflow === 'hidden', disclose);
  check('pages', 'and it settles back to its natural height', d.settled.hidden === false && d.settled.overflow === '' && d.settled.height === '', disclose);
  check('pages', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // (c) list rows: a removed row leaves a ghost and the rest glide up.
  await s.open('/archive/?mock&motion=full');
  const rows = await s.evaluate(`(async () => {
    ${HELP}
    await until(() => document.querySelectorAll('.ip-list [data-row]').length > 2);
    await wait(400);
    const id = document.querySelector('.ip-list [data-row]').dataset.id;
    const before = __motion.stats();
    await window.sta.dispatch({ type: 'deleteArchived', id: Number(id) });
    await until(() => !document.querySelector('[data-row][data-id="' + id + '"]'));
    await frame();
    const after = __motion.stats();
    const g = document.querySelector('.motion-ghosts .motion-ghost');
    return JSON.stringify({
      ghosts: after.ghosts - before.ghosts,
      flips: after.flips - before.flips,
      running: ids(),
      findable: document.querySelectorAll('[data-row][data-id="' + id + '"]').length,
      inert: g ? g.inert === true : false,
    });
  })()`);
  const rw = JSON.parse(rows);
  check('pages', 'a removed row leaves exactly one inert ghost', rw.ghosts === 1 && rw.inert === true && rw.findable === 0, rows);
  check('pages', 'the rows below it glide up under pages.listRows', rw.flips === 1 && rw.running.includes('pages.listRows'), rows);

  const bulk = await s.evaluate(`(async () => {
    ${HELP}
    await wait(400);
    const input = document.querySelector('.ip-search input');
    const before = __motion.stats();
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, 'zzzzz');
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await until(() => !document.querySelector('.ip-list [data-row]'));
    await frame();
    const after = __motion.stats();
    return JSON.stringify({ ghosts: after.ghosts - before.ghosts, flips: after.flips - before.flips });
  })()`);
  const b = JSON.parse(bulk);
  check('pages', 'a search that empties the list is not a row change anyone can follow', b.flips === 0, bulk);
  check('pages', 'and it leaves at most the change limit in ghosts', b.ghosts <= 20, bulk);
  check('pages', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // (d) the boosts editor: a View Transition, with the boost already fetched.
  await s.open('/boosts/?mock&motion=full');
  const boosts = await s.evaluate(`(async () => {
    ${HELP}
    await wait(400);
    __mock.setState((st) => { st.boosts = st.boosts.concat([{ id: 777, name: 'Second boost', host: 'example.com', enabled: true }]); });
    await until(() => document.querySelectorAll('.bst-item').length === 2);
    const before = __motion.stats();
    const supported = typeof document.startViewTransition === 'function';
    // What the new snapshot will contain: the DOM as it stands when the update callback is done.
    let atCapture = null;
    const started = new Promise((resolve) => {
      const real = document.startViewTransition;
      if (!real) return resolve();
      document.startViewTransition = function (cb) {
        document.startViewTransition = real;
        const t = real.call(this, cb);
        t.updateCallbackDone.then(() => {
          const el = document.querySelector('.bst-editor input');
          atCapture = el ? el.value : null;
          resolve();
        }, resolve);
        return t;
      };
    });
    [...document.querySelectorAll('.bst-item')][1].click();
    await until(() => (document.querySelector('.bst-item.is-selected .bst-item-name') || {}).textContent === 'Second boost', 5000);
    await Promise.race([started, wait(3000)]);
    const name = document.querySelector('.bst-name input, .bst-editor input');
    return JSON.stringify({
      supported,
      transitions: __motion.stats().viewTransitions - before.viewTransitions,
      filled: name ? name.value : null,
      atCapture,
    });
  })()`);
  const bo = JSON.parse(boosts);
  check('pages', 'this renderer has View Transitions, so the two checks below mean something', bo.supported === true, boosts);
  check('pages', 'switching boosts runs a View Transition', !bo.supported || bo.transitions === 1, boosts);
  check('pages', 'the boost was fetched first, so the new frame is not an empty editor', bo.filled === 'Second boost', boosts);
  check('pages', 'the new snapshot is the new editor, not the old one', !bo.supported || bo.atCapture === 'Second boost', boosts);
  check('pages', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // (e) the empty state's hero.
  await s.open('/empty/?mock&empty=1&motion=full');
  const hero = await s.evaluate(`(() => {
    const cs = getComputedStyle(document.querySelector('.empty-inner'));
    return JSON.stringify({ name: cs.animationName, duration: cs.animationDuration });
  })()`);
  check('pages', "the empty state's hero rises on its own key (320 ms)", hero === '{"name":"empty-in","duration":"0.32s"}', hero);

  await s.open('/empty/?mock&empty=1&motion=full&animOff=pages.emptyHero');
  const heroOff = await s.evaluate(`getComputedStyle(document.querySelector('.empty-inner')).animationDuration`);
  check('pages', 'and its own switch turns it off', heroOff === '0s', heroOff);
  check('pages', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

// ------------------------------------------------------------------------------------ theme

async function checkTheme(s) {
  const fade = async (rel) => {
    await s.open(rel);
    return s.evaluate(`JSON.stringify({
      fade: document.documentElement.classList.contains('theme-fade'),
      card: document.documentElement.classList.contains('surface-overlay'),
      duration: getComputedStyle(document.documentElement).transitionDuration,
    })`).then(JSON.parse);
  };
  const sidebar = await fade('/sidebar/?mock&motion=full');
  check('theme', 'a page that paints its own background cross-fades the theme', sidebar.fade === true && sidebar.duration === '0.3s', JSON.stringify(sidebar));
  const toast = await fade('/toast/?mock&toast=Hi&motion=full');
  check('theme', 'a page inside a native card snaps instead (the shell draws its fill and border)', toast.card === true && toast.fade === false, JSON.stringify(toast));
  const off = await fade('/sidebar/?mock&motion=full&animOff=theme.crossFade');
  check('theme', 'its own switch turns the cross-fade off', off.fade === false, JSON.stringify(off));
  const master = await fade('/sidebar/?mock&motion=off');
  check('theme', 'so does the master switch', master.fade === false, JSON.stringify(master));
  const reduced = await fade('/sidebar/?mock&motion=reduced');
  check('theme', 'reduced keeps it: a colour fade is not motion', reduced.fade === true && reduced.duration === '0.3s', JSON.stringify(reduced));
  check('theme', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

// ------------------------------------------------------------------------------------ controls

async function checkControls(s) {
  await s.open('/_gallery/?mock&motion=full');
  const full = await s.evaluate(`(() => {
    ${HELP}
    const toggle = document.querySelector('.toggle');
    const thumb = document.querySelector('.toggle-thumb');
    const btn = document.querySelector('.btn');
    const seg = document.querySelector('.ip-segment');
    const read = (el) => getComputedStyle(el);
    return JSON.stringify({
      track: read(toggle).transitionDuration,
      thumb: read(thumb).transitionDuration,
      btn: read(btn).transitionDuration,
      btnProps: read(btn).transitionProperty,
      segment: seg ? read(seg).transitionDuration : null,
      scroll: __motion.scrollBehavior(),
    });
  })()`);
  const c = JSON.parse(full);
  check('controls', 'a toggle moves its thumb over controls.toggles (140 ms)', c.thumb.includes('0.14s') && c.track === '0.14s', full);
  check('controls', 'a button has a hover duration and a shorter press scale', c.btn === '0.09s, 0.09s, 0.06s' && c.btnProps.includes('scale'), full);
  check('controls', 'controls.smoothScroll answers for scrolling', c.scroll === 'smooth', full);

  await s.open('/_gallery/?mock&motion=full&animOff=controls.hoverPress');
  const hoverOff = await s.evaluate(`getComputedStyle(document.querySelector('.btn')).transitionDuration`);
  check('controls', 'its switch zeroes both of the key durations at once', hoverOff === '0s, 0s, 0s', hoverOff);
  const togglesStillOn = await s.evaluate(`getComputedStyle(document.querySelector('.toggle-thumb')).transitionDuration`);
  check('controls', 'and leaves controls.toggles alone', togglesStillOn.includes('0.14s'), togglesStillOn);

  await s.open('/_gallery/?mock&motion=full&animOff=controls.toggles');
  const togglesOff = await s.evaluate(`JSON.stringify([
    getComputedStyle(document.querySelector('.toggle-thumb')).transitionDuration,
    getComputedStyle(document.querySelector('.btn')).transitionDuration,
  ])`);
  check('controls', 'controls.toggles off stops the thumb and nothing else', togglesOff === '["0s, 0s","0.09s, 0.09s, 0.06s"]', togglesOff);

  await s.open('/_gallery/?mock&motion=reduced');
  const reduced = await s.evaluate(`(() => {
    ${HELP}
    return JSON.stringify({
      thumb: getComputedStyle(document.querySelector('.toggle-thumb')).transitionDuration,
      btn: getComputedStyle(document.querySelector('.btn')).transitionDuration,
      scroll: __motion.scrollBehavior(),
    });
  })()`);
  const r = JSON.parse(reduced);
  check('controls', 'at reduced a thumb arrives at once (a position cannot travel 0 px)', r.thumb === '0s, 0s', reduced);
  check('controls', 'the hover tint still fades, the press scale does not', r.btn === '0.09s, 0.09s, 0s', reduced);
  check('controls', 'and there is no smooth scrolling', r.scroll === 'auto', reduced);

  await s.open('/_gallery/?mock&motion=full&animOff=controls.smoothScroll');
  check('controls', 'its own switch turns smooth scrolling off too', (await s.evaluate('__motion.scrollBehavior()')) === 'auto');
  check('controls', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

// ------------------------------------------------------------------------------------ indicators

async function checkIndicators(s) {
  const read = () =>
    s.evaluate(`(() => {
      ${HELP}
      const bar = document.querySelector('.progress.is-indeterminate .progress-fill');
      const ring = document.querySelector('.progress-ring.is-indeterminate > .progress-ring-svg');
      const rect = document.querySelector('.audio-bars > rect');
      const second = document.querySelectorAll('.audio-bars > rect')[1];
      return JSON.stringify({
        spinner: dur(document.querySelector('.spinner')),
        bar: dur(bar),
        barName: getComputedStyle(bar).animationName,
        barWidth: getComputedStyle(bar).width,
        ring: dur(ring),
        audio: dur(rect),
        audioName: getComputedStyle(rect).animationName,
        audioDelay: getComputedStyle(second).animationDelay,
        focused: document.documentElement.dataset.focused !== undefined,
      });
    })()`).then(JSON.parse);

  await s.open('/_gallery/?mock&motion=full');
  const full = await read();
  check('indicators', 'one period drives the spinner, the bar (1.5×) and the ring (1.25×)', full.spinner === 0.8 && full.bar === 1.2 && full.ring === 1, JSON.stringify(full));
  check('indicators', 'the audio bars run while the window has focus', full.focused === true && full.audio === 0.9 && full.audioName === 'sta-audio-bar', JSON.stringify(full));
  check('indicators', 'they are offset from one another', full.audioDelay.startsWith('-0.31'), JSON.stringify(full));

  const unfocused = await s.evaluate(`(async () => {
    ${HELP}
    __mock.setState((st) => { st.window.focused = false; });
    await until(() => document.documentElement.dataset.focused === undefined);
    await frame();
    const rect = document.querySelector('.audio-bars > rect');
    return JSON.stringify({ name: getComputedStyle(rect).animationName, visible: rect.getBoundingClientRect().height > 0 });
  })()`);
  check('indicators', 'an unfocused window shows a still glyph instead (no compositing for hours of music)', unfocused === '{"name":"none","visible":true}', unfocused);

  await s.open('/_gallery/?mock&motion=reduced');
  const reduced = await read();
  check('indicators', 'reduced only slows the loading indicators down', reduced.spinner === 1.6 && reduced.bar === 2.4 && reduced.ring === 2, JSON.stringify(reduced));
  check('indicators', 'and stops the audio bars', reduced.audioName === 'none', JSON.stringify(reduced));

  await s.open('/_gallery/?mock&motion=full&animOff=indicators.loading');
  const loadingOff = await read();
  check('indicators', 'indicators.loading off leaves a static ring', loadingOff.spinner === 0, JSON.stringify(loadingOff));
  check('indicators', 'and a still, *visible* bar rather than one parked off-screen', loadingOff.barName === 'none' && loadingOff.barWidth !== '0px', JSON.stringify(loadingOff));
  check('indicators', 'while the audio bars keep their own switch', loadingOff.audio === 0.9, JSON.stringify(loadingOff));

  await s.open('/_gallery/?mock&motion=full&animOff=indicators.audio');
  const audioOff = await read();
  check('indicators', 'indicators.audio off stops only the bars', audioOff.audioName === 'none' && audioOff.spinner === 0.8, JSON.stringify(audioOff));

  await s.open('/sidebar/?mock&motion=full&animOff=indicators.badges');
  const badgesOff = await s.evaluate(`(async () => {
    ${HELP}
    await wait(300);
    const id = Number(document.querySelector('.today-list .row[data-row][data-id]:not(.split-row)').dataset.id);
    __mock.setState((st) => {
      st.permissionPrompts = [{ id: 1, tab: id, origin: 'https://meet.example.com', host: 'meet.example.com', kinds: ['camera'] }];
    });
    const shown = await until(() => document.querySelector('.row-permission'), 4000);
    const dot = document.querySelector('.row-permission');
    if (!dot) return JSON.stringify({ shown, id });
    const cs = getComputedStyle(dot);
    return JSON.stringify({ name: cs.animationName, shadow: cs.boxShadow !== 'none' });
  })()`);
  check('indicators', 'a pending-permission dot is a static double ring when its key is off', badgesOff === '{"name":"none","shadow":true}', badgesOff);
  check('indicators', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

// ------------------------------------------------------------------------ acknowledged exits

/**
 * The page half of an acknowledged exit (PROTOCOL §14, FINAL PLAN §4): the shell asks a surface to
 * present a blank frame before it hides the widget, and waits for `surface.exited {gen}`. What the
 * mock can see is exactly the page's contract — it blanks *and stays blank*, it answers with the
 * generation it was given, it answers even when the animation is switched off (then instantly, and
 * the shell's own 50/60 ms floor is what keeps the frame), it never answers a request it cannot
 * understand, and it comes back when the surface has something to show again.
 */
async function checkExit(s) {
  /** `{fade, blank, acked, props, root}` after asking `host` to exit. */
  const ask = (host, sel, key, gen) => `(async () => {
    ${HELP}
    await wait(250);                                  // the entrance is over
    const els = [...document.querySelectorAll(${JSON.stringify(sel)})];
    const before = __motion.stats();
    __mock.emit('surface.exit', { gen: ${gen} });
    await until(() => __motion.isBlanked(), 2000);
    const running = anims(${JSON.stringify(key)});
    const fadeMs = running.length ? running[0].effect.getComputedTiming().duration : 0;
    const props = kprops(${JSON.stringify(key)});
    const started = __motion.stats().started - before.started;
    const acked = await until(() => window.__mockExited.length > 0, 3000);
    await wait(120);                                  // …and the fade is over by now
    const opacity = els.map((el) => getComputedStyle(el).opacity);
    const inline = els.map((el) => el.style.opacity);
    const clicks = els.map((el) => getComputedStyle(el).pointerEvents);
    return JSON.stringify({ fade: fadeMs, props, started, acked, exited: window.__mockExited, opacity, inline, clicks, blanked: __motion.isBlanked() });
  })()`;

  // (a) the toast: the pill blanks, the ack carries the generation, and the tracked root is untouched.
  await s.open('/toast/?mock&toast=1&motion=full');
  const toast = JSON.parse(await s.evaluate(ask('toast', '.toast-inner', 'overlays.toast', 41)));
  check('exit', 'asking the toast to exit blanks the pill', toast.blanked && toast.opacity.join() === '0', JSON.stringify(toast));
  check('exit', 'the fade is capped at --t-surface-exit (60 ms), not the key\'s own 180 ms', toast.fade === 60, JSON.stringify(toast));
  check('exit', 'and it is an opacity fade, nothing else', JSON.stringify(toast.props) === '["opacity"]', JSON.stringify(toast));
  check('exit', 'the page acknowledges the blank frame with the generation it was given', toast.acked && toast.exited.length === 1 && toast.exited[0].gen === 41, JSON.stringify(toast));
  check(
    'exit',
    'the end state is the page\'s own, so a settled animation cannot bring the pill back',
    toast.inline.join() === '0',
    JSON.stringify(toast),
  );
  check('exit', 'a blanked surface stops taking clicks (the widget is still up)', toast.clicks.join() === 'none', JSON.stringify(toast));
  const rootAnims = await s.evaluate(`JSON.stringify({ anims: document.querySelector('.toast').getAnimations().length, opacity: document.querySelector('.toast').style.opacity })`);
  check('exit', 'the tracked root is never part of it', rootAnims === '{"anims":0,"opacity":""}', rootAnims);
  check('exit', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // (b) a toast shown again while the shell was still waiting: the pill has to come back.
  const back = await s.evaluate(`(async () => {
    ${HELP}
    __mock.setState((st) => { st.toast = { id: 77, message: 'Back again', action: null, durationMs: 60000 }; });
    await until(() => (document.querySelector('.toast-msg') || {}).textContent === 'Back again');
    await wait(250);
    const inner = document.querySelector('.toast-inner');
    return JSON.stringify({ blanked: __motion.isBlanked(), inline: inner.style.opacity, opacity: getComputedStyle(inner).opacity, clicks: getComputedStyle(inner).pointerEvents });
  })()`);
  check('exit', 'a new toast during the linger un-blanks the pill completely, clicks included', back === '{"blanked":false,"inline":"","opacity":"1","clicks":"auto"}', back);

  // (c) the same request with the key off: no fade at all, and still an ack (the shell's floor is
  // what gives the page its frame, and it is never 0).
  await s.open('/toast/?mock&toast=1&motion=full&animOff=overlays.toast');
  const offKey = JSON.parse(await s.evaluate(ask('toast', '.toast-inner', 'overlays.toast', 42)));
  check('exit', 'with the key off the pill blanks instantly', offKey.fade === 0 && offKey.started === 0 && offKey.opacity.join() === '0', JSON.stringify(offKey));
  check('exit', 'and the exit is still acknowledged', offKey.acked && offKey.exited[0].gen === 42, JSON.stringify(offKey));

  // (d) a request the page cannot understand is never acknowledged.
  await s.open('/toast/?mock&toast=1&motion=full');
  const bad = await s.evaluate(`(async () => {
    ${HELP}
    await wait(250);
    __mock.emit('surface.exit', {});
    __mock.emit('surface.exit', { gen: 'soon' });
    await wait(300);
    return JSON.stringify({ exited: window.__mockExited, blanked: __motion.isBlanked() });
  })()`);
  check('exit', 'a surface.exit without a usable generation is ignored, not acked', bad === '{"exited":[],"blanked":false}', bad);

  // (e) the switcher: the cards and the ring blank, the root (which the shell measures) does not.
  await s.open('/switcher/?mock&switcher=1&motion=full');
  const sw = JSON.parse(await s.evaluate(ask('switcher', '.sw-card, .sw-ring', 'overlays.switcher', 43)));
  check('exit', 'the switcher blanks every card and its ring', sw.blanked && sw.opacity.every((o) => o === '0') && sw.opacity.length > 1, JSON.stringify(sw));
  check('exit', 'over the same 60 ms cap, in opacity only', sw.fade === 60 && JSON.stringify(sw.props) === '["opacity"]', JSON.stringify(sw));
  check('exit', 'and acknowledges it', sw.acked && sw.exited[0].gen === 43, JSON.stringify(sw));
  const swRoot = await s.evaluate(`JSON.stringify({ anims: document.querySelector('.sw').getAnimations().length, opacity: document.querySelector('.sw').style.opacity })`);
  check('exit', 'the tracked .sw root is neither animated nor blanked itself', swRoot === '{"anims":0,"opacity":""}', swRoot);
  check('exit', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // (f) the floating sidebar is asked through `sidebar.hover {gen}` (it already blanks there), and
  // only when a generation is present: an ordinary hide is not an acknowledged exit.
  await s.open('/sidebar/?mock&sidebar=0&hover=1&motion=full');
  const sidebar = await s.evaluate(`(async () => {
    ${HELP}
    await until(() => document.querySelector('.sidebar.is-floating'), 4000);
    await wait(300);
    __mock.emit('sidebar.hover', { visible: false, dismiss: true, gen: 44 });
    const hidden = await until(() => document.querySelector('.sidebar.is-hover-hidden'), 2000);
    const fadeSec = fade(document.querySelector('.sidebar > .sidebar-main'));
    const acked = await until(() => window.__mockExited.length > 0, 3000);
    const first = window.__mockExited[0] || null;
    __mock.emit('sidebar.hover', { visible: true, dismiss: false });
    await wait(200);
    __mock.emit('sidebar.hover', { visible: false, dismiss: true });
    await wait(400);
    return JSON.stringify({ hidden, fadeSec, acked, first, exited: window.__mockExited.length, clicks: getComputedStyle(document.querySelector('.sidebar')).pointerEvents });
  })()`);
  const sb = JSON.parse(sidebar);
  check('exit', 'the floating sidebar hides its contents when asked', sb.hidden, sidebar);
  check('exit', 'their fade is capped at the surface-exit cap (<= 0.06 s)', sb.fadeSec > 0 && sb.fadeSec <= 0.06, sidebar);
  check('exit', 'and it acknowledges the blank frame with that generation', sb.acked && sb.first?.gen === 44, sidebar);
  check('exit', 'a hide without a generation is not acknowledged (nothing is waiting for it)', sb.exited === 1, sidebar);
  check('exit', 'a hiding sidebar also stops taking clicks', sb.clicks === 'none', sidebar);
  check('exit', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  // (g) the activatable overlays are the other half of the rule: nothing waits for them, so they
  // blank *synchronously* on a page-initiated close and dispatch a frame later (FINAL PLAN §1.3).
  // The frame their renderer produced last is the frame the next open shows.
  await s.open('/command/?mock&commandBar=newTab&motion=full');
  const cmdClose = await s.evaluate(`(async () => {
    ${HELP}
    await wait(300);
    const card = document.querySelector('.cmd');
    const rows = document.querySelectorAll('.cmd-row').length;
    const sent = () => __mock.dispatched().filter((c) => c && c.type === 'closeCommandBar');
    document.getElementById('input').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    // Still the same task as the key: the blank frame has to be on its way before the command is.
    const atOnce = { inline: card.style.opacity, clicks: card.style.pointerEvents, blanked: __motion.isBlanked(), sent: sent().length };
    const closed = await until(() => sent().length > 0, 2000);
    const onSend = { inline: card.style.opacity, seq: sent()[0] ? sent()[0].seq : undefined };
    await until(() => !__mock.state.commandBar, 2000);
    await window.sta.dispatch({ type: 'openCommandBar', mode: 'newTab' });
    await until(() => document.querySelectorAll('.cmd-row').length > 0, 3000);
    await wait(300); // the open's own fade is over, so this reads the card's settled opacity
    const reopened = { inline: card.style.opacity, blanked: __motion.isBlanked(), opacity: getComputedStyle(card).opacity, clicks: getComputedStyle(card).pointerEvents };
    return JSON.stringify({ rows, atOnce, closed, onSend, reopened });
  })()`);
  const cc = JSON.parse(cmdClose);
  check('exit', 'the command bar had something on screen to leave behind', cc.rows > 0, cmdClose);
  check('exit', 'Esc blanks the card in the same task as the key, clicks included', cc.atOnce.inline === '0' && cc.atOnce.clicks === 'none' && cc.atOnce.blanked === true, cmdClose);
  check('exit', 'and the close is dispatched only after that', cc.atOnce.sent === 0 && cc.closed === true && cc.onSend.inline === '0', cmdClose);
  check('exit', "it still carries the bar's own seq", cc.onSend.seq !== undefined, cmdClose);
  check('exit', 'the next open un-blanks the card completely', cc.reopened.inline === '' && cc.reopened.blanked === false && cc.reopened.opacity === '1' && cc.reopened.clicks !== 'none', cmdClose);
  check('exit', 'no console errors', s.problems.length === 0, s.problems.join(' | '));

  await s.open('/find/?mock&find=1&motion=full');
  const findClose = await s.evaluate(`(async () => {
    ${HELP}
    await wait(300);
    const bar = document.querySelector('.find');
    const sent = () => __mock.dispatched().filter((c) => c && c.type === 'closeFind');
    document.getElementById('input').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    const atOnce = { inline: bar.style.opacity, blanked: __motion.isBlanked(), sent: sent().length };
    const closed = await until(() => sent().length > 0, 2000);
    return JSON.stringify({ atOnce, closed });
  })()`);
  const fc = JSON.parse(findClose);
  check('exit', 'the find bar blanks on Esc before it dispatches (the next Ctrl+F is a fresh search)', fc.atOnce.inline === '0' && fc.atOnce.blanked === true && fc.atOnce.sent === 0, findClose);
  check('exit', 'and the close does go out', fc.closed === true, findClose);
  check('exit', 'no console errors', s.problems.length === 0, s.problems.join(' | '));
}

const SECTIONS = [
  ['level', checkLevel],
  ['keys', checkKeys],
  ['start', checkStart],
  ['off', checkOff],
  ['toggle', checkToggle],
  ['stagger', checkStagger],
  ['ghost', checkGhost],
  ['size', checkSize],
  ['storm', checkStorm],
  ['sidebar', checkSidebarRows],
  ['sidebar', checkSidebarOrder],
  ['sidebar', checkSidebarSweeps],
  ['sidebar', checkSidebarStates],
  ['sidebar', checkSidebarFlipGates],
  ['sidebar', checkSidebarExtras],
  ['cmdbar', checkCommandBar],
  ['topbar', checkTopBar],
  ['overlays', checkOverlays],
  ['menus', checkMenus],
  ['pages', checkPages],
  ['theme', checkTheme],
  ['controls', checkControls],
  ['indicators', checkIndicators],
  ['exit', checkExit],
];

// ------------------------------------------------------------------------------------ main

async function main() {
  let server;
  let edge;
  let tmp;
  let s;
  const cleanup = async () => {
    s?.close();
    killTree(edge);
    await server?.close();
    if (tmp) await rm(tmp, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 }).catch(() => {});
  };
  const onSignal = () => cleanup().finally(() => process.exit(130));
  process.once('SIGINT', onSignal);
  process.once('SIGTERM', onSignal);
  try {
    server = await startStaticServer({ root: path.join(toolsDir, '..', 'ui') });
    tmp = await mkdtemp(path.join(tmpdir(), 'sta-motion-check-'));
    const profileDir = path.join(tmp, 'edge-profile');
    edge = spawn(
      edgePath,
      [
        '--headless=new',
        '--disable-gpu',
        '--hide-scrollbars',
        '--no-first-run',
        '--no-default-browser-check',
        '--disable-extensions',
        '--disable-sync',
        '--disable-background-networking',
        `--user-data-dir=${profileDir}`,
        '--remote-debugging-port=0',
        'about:blank',
      ],
      { stdio: 'ignore', windowsHide: true },
    );
    const port = await devToolsPort(profileDir, edge, 20000);
    s = await session(port, server.origin);
    for (const [name, fn] of SECTIONS) {
      if (only.size && !only.has(name)) continue;
      await fn(s);
    }
  } catch (e) {
    log(`motion-check: ${e.message}`);
    await cleanup();
    return 1;
  } finally {
    process.off('SIGINT', onSignal);
    process.off('SIGTERM', onSignal);
  }
  await cleanup();
  log(`motion-check: ${passed} passed, ${failures.length} failed`);
  return failures.length ? 2 : 0;
}

process.exitCode = await main();
