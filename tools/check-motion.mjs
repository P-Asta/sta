#!/usr/bin/env node
// Static motion check (FINAL PLAN §7). Keeps the three halves of the animation system in sync and
// enforces the rules that a reviewer cannot see by reading one file:
//
//   registry   crates/sta-core/src/motion.rs      keys, groups, defaults  (the source of truth)
//   catalog    ui/common/motion-catalog.js        the same keys with labels and descriptions
//   gates      ui/common/tokens.css               one `--t-*` duration token per key, zeroed by
//                                                 `:root[data-anim-off~="<key>"]`
//
// Checks:
//  1. the registry and the catalog agree: same groups in the same order with the same labels, same
//     keys in the same order, same `default_on`;
//  2. every key has exactly one duration token, named mechanically from the key, declared in
//     `tokens.css` and zeroed by its own `[data-anim-off~=…]` rule — no token is claimed by two
//     keys, and no single animated property (one entry of a `transition`/`animation` list) reads
//     two keys' tokens;
//  3. `motion.js`'s `tokenOf` derives the same name;
//  4. the `off` level's four selectors are all scoped to `:root[data-motion="off"]` (a bare
//     `::before` would take pseudo-element motion away from every page, always) and zero only
//     *durations*, so delays and end states survive;
//  5. nothing in `ui/` waits on `animationend` (it never fires while a surface renders no frames);
//  6. no tracked overlay root is animated or transformed (`ipc.js trackSurfaceSize` keeps the size
//     it reports, and a transformed root would report the wrong one);
//  7. CSS transitions of layout properties are allowlisted, one selector + property at a time,
//     until each area converts them;
//  8. the keys whose exits the shell waits for (`crates/sta/src/motion.rs`) are registered, and their
//     durations are at least `--t-surface-exit` — the cap the shell computes its wait from.
//
//   node tools/check-motion.mjs        exit 0 = in sync, 1 = problems
//
// The Rust parsing is deliberately simple: it relies on `motion.rs` keeping one `AnimationSpec` /
// `MotionGroup` literal per line, as it does today.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const errors = [];
const warnings = [];
const fail = (msg) => errors.push(msg);
const read = (p) => readFileSync(join(root, p), 'utf8');

// ------------------------------------------------------------------------------------ inputs

const rust = read('crates/sta-core/src/motion.rs');

/** `[{key, group, defaultOn}]` in declaration order (the `ANIMATIONS` table). */
const registry = [...rust.matchAll(/AnimationSpec\s*\{\s*key:\s*"([^"]+)",\s*group:\s*"([^"]+)",\s*default_on:\s*(true|false)\s*\}/g)].map(
  (m) => ({ key: m[1], group: m[2], defaultOn: m[3] === 'true' }),
);
/** `[{id, label}]` in declaration order (the `GROUPS` table). */
const registryGroups = [...rust.matchAll(/MotionGroup\s*\{\s*id:\s*"([^"]+)",\s*label:\s*"([^"]+)"\s*\}/g)].map((m) => ({
  id: m[1],
  label: m[2],
}));

if (registry.length === 0) fail('motion.rs: no AnimationSpec parsed (parser out of date?)');
if (registryGroups.length === 0) fail('motion.rs: no MotionGroup parsed (parser out of date?)');

const catalog = await import(pathToFileURL(join(root, 'ui', 'common', 'motion-catalog.js')).href);

// ------------------------------------------------------------------------------------ CSS helper

/** Strip comments, keeping offsets close enough for line numbers. */
const stripComments = (css) => css.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '));

/**
 * Innermost declaration blocks of a stylesheet: `{selector, body, line}`. `selector` is the chain of
 * preludes joined with ` >> ` (so an `@media` wrapper is visible), which is all these checks need.
 * @returns {{selector: string, body: string, line: number}[]}
 */
function blocks(css) {
  const text = stripComments(css);
  const out = [];
  const stack = [];
  let prelude = '';
  let body = '';
  let line = 1;
  let preludeLine = 1;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === '\n') line++;
    if (ch === '{') {
      if (!prelude.trim()) preludeLine = line;
      stack.push({ prelude: prelude.trim(), line: preludeLine });
      prelude = '';
      body = '';
      preludeLine = line;
    } else if (ch === '}') {
      const frame = stack.pop();
      if (frame && /[:;]/.test(body)) {
        out.push({ selector: stack.map((f) => f.prelude).concat(frame.prelude).join(' >> '), body, line: frame.line });
      }
      body = '';
      prelude = '';
    } else if (stack.length) {
      body += ch;
      prelude += ch;
    } else {
      prelude += ch;
      if (!prelude.trim()) preludeLine = line;
    }
  }
  return out;
}

/**
 * Blank out JS comments and string bodies, keeping every newline, so a line scan cannot be fooled by
 * a comment that mentions the thing it looks for (nor by a message that quotes it).
 */
function stripJs(source) {
  const NL = '\n';
  const BACKSLASH = String.fromCharCode(92);
  let out = '';
  let mode = 'code';
  let quote = '';
  for (let i = 0; i < source.length; i++) {
    const ch = source[i];
    const next = source[i + 1];
    if (mode === 'code') {
      if (ch === '/' && next === '/') {
        mode = 'line';
        out += '  ';
        i++;
      } else if (ch === '/' && next === '*') {
        mode = 'block';
        out += '  ';
        i++;
      } else if (ch === '"' || ch === "'" || ch === '`') {
        mode = 'string';
        quote = ch;
        out += ch;
      } else {
        out += ch;
      }
    } else if (mode === 'line') {
      if (ch === NL) {
        mode = 'code';
        out += ch;
      } else {
        out += ' ';
      }
    } else if (mode === 'block') {
      if (ch === '*' && next === '/') {
        mode = 'code';
        out += '  ';
        i++;
      } else {
        out += ch === NL ? ch : ' ';
      }
    } else if (ch === BACKSLASH) {
      // An escape inside a string: skip both characters so a closing quote is never missed.
      out += '  ';
      i++;
    } else if (ch === quote) {
      mode = 'code';
      out += ch;
    } else {
      // Keep string contents, so `'animationend'` still reads as a literal.
      out += ch;
    }
  }
  return out;
}

/** Every `.css` file under `ui/`. */
function cssFiles(dir = 'ui') {
  const out = [];
  for (const name of readdirSync(join(root, dir))) {
    const rel = `${dir}/${name}`;
    if (statSync(join(root, rel)).isDirectory()) out.push(...cssFiles(rel));
    else if (name.endsWith('.css')) out.push(rel);
  }
  return out;
}

/** Every `.js` file under `ui/`, except the vendored bundle. */
function jsFiles(dir = 'ui') {
  const out = [];
  for (const name of readdirSync(join(root, dir))) {
    const rel = `${dir}/${name}`;
    if (statSync(join(root, rel)).isDirectory()) out.push(...jsFiles(rel));
    else if (name.endsWith('.js') && !rel.includes('/vendor/')) out.push(rel);
  }
  return out;
}

// ------------------------------------------------- 1. registry ⇄ catalog

const catalogGroups = catalog.ANIMATION_GROUPS ?? [];
const catalogKeys = catalog.ANIMATION_KEYS ?? [];

if (catalogGroups.map((g) => g.id).join(',') !== registryGroups.map((g) => g.id).join(',')) {
  fail(
    `motion-catalog.js groups [${catalogGroups.map((g) => g.id).join(', ')}] ≠ motion.rs GROUPS [${registryGroups.map((g) => g.id).join(', ')}] (same ids, same order)`,
  );
}
for (const g of registryGroups) {
  const mine = catalogGroups.find((c) => c.id === g.id);
  if (mine && mine.label !== g.label) fail(`group "${g.id}": motion-catalog.js label "${mine.label}" ≠ motion.rs "${g.label}"`);
}
if (catalogKeys.map((k) => k.key).join(',') !== registry.map((k) => k.key).join(',')) {
  const missing = registry.filter((r) => !catalogKeys.some((c) => c.key === r.key)).map((r) => r.key);
  const extra = catalogKeys.filter((c) => !registry.some((r) => r.key === c.key)).map((c) => c.key);
  fail(
    `motion-catalog.js keys differ from motion.rs ANIMATIONS (same keys, same order)` +
      (missing.length ? `; missing: ${missing.join(', ')}` : '') +
      (extra.length ? `; unknown: ${extra.join(', ')}` : ''),
  );
}
for (const spec of registry) {
  const mine = catalogKeys.find((c) => c.key === spec.key);
  if (!mine) continue;
  if (mine.group !== spec.group) fail(`${spec.key}: motion-catalog.js group "${mine.group}" ≠ motion.rs "${spec.group}"`);
  if (mine.defaultOn !== spec.defaultOn) fail(`${spec.key}: motion-catalog.js defaultOn ${mine.defaultOn} ≠ motion.rs ${spec.defaultOn}`);
  if (!mine.label?.trim()) fail(`${spec.key}: motion-catalog.js has no label`);
  if (!mine.desc?.trim()) fail(`${spec.key}: motion-catalog.js has no one-line description`);
  else if (mine.desc.length > 140) fail(`${spec.key}: the description is ${mine.desc.length} characters (keep it to one line, ≤ 140)`);
  if (!/^[a-z][A-Za-z]*\.[a-z][A-Za-z]*$/.test(spec.key)) fail(`${spec.key}: does not match ^[a-z][A-Za-z]*\\.[a-z][A-Za-z]*$`);
}

// ------------------------------------------------- 2/3. tokens and per-key gates

/** The same derivation `motion.js tokenOf` uses. */
const tokenOf = (key) => `--t-${key.replace(/\./g, '-').replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase()}`;

const motionJs = read('ui/common/motion.js');
if (!motionJs.includes(`return \`--t-\${String(key).replace(/\\./g, '-').replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase()}\`;`)) {
  fail('motion.js tokenOf() no longer derives `--t-<kebab key>`; this checker and tokens.css assume it does');
}

const tokensCss = read('ui/common/tokens.css');
const tokenBlocks = blocks(tokensCss);

/** token → key, for the per-key tokens only. */
const tokenOwner = new Map();
for (const spec of registry) {
  const token = tokenOf(spec.key);
  if (tokenOwner.has(token)) fail(`${spec.key} and ${tokenOwner.get(token)} both map to ${token}`);
  tokenOwner.set(token, spec.key);
}

const declaredTokens = new Set();
/** token → its declared value on `:root`, e.g. `--t-overlays-toast` → `180ms`. */
const tokenValues = new Map();
for (const b of tokenBlocks) {
  if (b.selector !== ':root') continue;
  for (const m of b.body.matchAll(/(--t-[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    declaredTokens.add(m[1]);
    tokenValues.set(m[1], m[2].trim());
  }
}
for (const [token, key] of tokenOwner) {
  if (!declaredTokens.has(token)) fail(`tokens.css: ${key} has no \`${token}\` declaration on :root`);
}

/** `[data-anim-off~="key"]` rules: key → the tokens they zero. */
const gates = new Map();
for (const b of tokenBlocks) {
  const m = /:root\[data-anim-off~="([^"]+)"\]/.exec(b.selector);
  if (!m) continue;
  const zeroed = [...b.body.matchAll(/(--t-[a-z0-9-]+)\s*:\s*([^;]+);/g)].map(([, t, v]) => [t, v.trim()]);
  gates.set(m[1], (gates.get(m[1]) ?? []).concat(zeroed));
}
for (const spec of registry) {
  const zeroed = gates.get(spec.key);
  if (!zeroed) {
    fail(`tokens.css: no \`:root[data-anim-off~="${spec.key}"]\` rule (turning the key off must zero its own token)`);
    continue;
  }
  const token = tokenOf(spec.key);
  for (const [t, value] of zeroed) {
    if (tokenOwner.has(t) && t !== token) fail(`tokens.css: the gate for ${spec.key} zeroes ${t}, which belongs to ${tokenOwner.get(t)}`);
    if (t === token && !/^0m?s$/.test(value)) fail(`tokens.css: the gate for ${spec.key} sets ${t} to "${value}" (expected 0ms)`);
  }
  if (!zeroed.some(([t]) => t === token)) fail(`tokens.css: the gate for ${spec.key} does not zero ${token}`);
}
for (const key of gates.keys()) {
  if (!registry.some((r) => r.key === key)) fail(`tokens.css: \`[data-anim-off~="${key}"]\` is not a registered animation key`);
}

// Every animated *property* must take its duration from exactly one key. A `transition` list whose
// entries belong to different keys is right — a row's hover tint is `controls.hoverPress` and its
// chevron's rotate is `sidebar.folderExpand`, and each must switch off on its own — but one entry,
// or anything outside a transition/animation value, that names two keys' tokens conflates them,
// which is the whole reason the shared `--t-popover` had to be split.
const TIMED_VALUE = /(?:transition|animation)(?:-duration|-property)?\s*:\s*([^;}]+)(?:;|$)/g;
for (const file of cssFiles()) {
  for (const b of blocks(read(file))) {
    if (/data-anim-off/.test(b.selector)) continue;
    const parts = [];
    for (const m of b.body.matchAll(TIMED_VALUE)) parts.push(...m[1].split(','));
    // What is left over counts as one part: a key's token aliased into another custom property (or
    // read by `animation-delay`) would otherwise hide the conflation.
    parts.push(b.body.replace(TIMED_VALUE, ' '));
    for (const part of parts) {
      const used = new Set();
      for (const m of part.matchAll(/var\(\s*(--t-[a-z0-9-]+)/g)) if (tokenOwner.has(m[1])) used.add(tokenOwner.get(m[1]));
      if (used.size > 1) {
        fail(`${file}:${b.line}: one animated property uses the tokens of ${[...used].join(' and ')}; give each key its own property`);
      }
    }
  }
}

// ------------------------------------------------- 4. the `off` level's selectors

const offBlock = tokenBlocks.find((b) => /:root\[data-motion="off"\]\s*\*::after/.test(b.selector.replace(/\s+/g, ' ')));
if (!offBlock) {
  fail('tokens.css: the `off` rule (`:root[data-motion="off"]`, plus its `*`, `*::before` and `*::after`) is missing');
} else {
  const parts = offBlock.selector
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
  const want = [':root[data-motion="off"]', ':root[data-motion="off"] *', ':root[data-motion="off"] *::before', ':root[data-motion="off"] *::after'];
  for (const w of want) if (!parts.includes(w)) fail(`tokens.css: the \`off\` rule is missing the selector \`${w}\``);
  for (const p of parts) {
    if (!p.startsWith(':root[data-motion="off"]')) {
      fail(`tokens.css: the \`off\` rule has the unscoped selector \`${p}\`, which would take motion away from every page`);
    }
  }
  if (!/animation-duration:\s*0s\s*!important/.test(offBlock.body) || !/transition-duration:\s*0s\s*!important/.test(offBlock.body)) {
    fail('tokens.css: the `off` rule must set `animation-duration` and `transition-duration` to `0s !important`');
  }
  if (/\b(animation|transition)\s*:/.test(offBlock.body) || /(animation|transition)-delay\s*:/.test(offBlock.body)) {
    fail('tokens.css: the `off` rule must zero *durations* only — delays and end states carry meaning (the resize hint, the pill fade-out)');
  }
}

// ------------------------------------------------- 5. no animationend

/** `file:line` sites that may still listen for `animationend`, with the reason. */
const ANIMATIONEND_ALLOWLIST = new Map();
for (const file of jsFiles()) {
  const lines = stripJs(read(file)).split(/\r?\n/);
  lines.forEach((text, i) => {
    if (!/animationend|animationEnd|onAnimationEnd/.test(text)) return;
    const site = `${file}:${i + 1}`;
    if (ANIMATIONEND_ALLOWLIST.has(site)) return;
    fail(`${site}: waits on animationend — it never fires while the surface renders no frames; use a motion.js key instead`);
  });
}

// ------------------------------------------------- 6. tracked overlay roots

const TRACKED_ROOT_CSS_SKIP = new Set(['is-dimmed']);
for (const file of jsFiles()) {
  const source = stripJs(read(file));
  const tracked = [...source.matchAll(/trackSurfaceSize\(\s*([A-Za-z_$][\w$]*)\.current/g)].map((m) => m[1]);
  for (const ref of new Set(tracked)) {
    const animated = new RegExp(String.raw`(?:motion\.)?animate\(\s*${ref}\.current`).exec(source);
    if (animated) fail(`${file}: animates ${ref}.current, which is a tracked overlay root — animate an inner wrapper instead`);
    if (new RegExp(String.raw`${ref}\.current\.style\.(transform|scale|translate|rotate)`).exec(source)) {
      fail(`${file}: transforms ${ref}.current, which is a tracked overlay root`);
    }
    // The class on the element that carries the ref, so the stylesheet can be checked too.
    const el =
      new RegExp(String.raw`class="([a-z][a-z0-9-]*)"[^>]{0,200}?ref=\$\{${ref}\}`, 's').exec(source) ??
      new RegExp(String.raw`ref=\$\{${ref}\}[^>]{0,200}?class="([a-z][a-z0-9-]*)"`, 's').exec(source);
    if (!el) {
      warnings.push(`${file}: cannot tell which element carries ${ref} (no literal class beside the ref); its stylesheet is unchecked`);
      continue;
    }
    const cls = el[1];
    if (TRACKED_ROOT_CSS_SKIP.has(cls)) continue;
    const css = file.replace(/\.js$/, '.css');
    let sheet;
    try {
      sheet = read(css);
    } catch {
      continue;
    }
    for (const b of blocks(sheet)) {
      const last = b.selector.split(',').some((s) => new RegExp(String.raw`\.${cls}(\s*(:[a-z-]+(\([^)]*\))?)*)?$`).test(s.trim()));
      if (!last) continue;
      const bad = /(^|[;\s])(transform|scale|translate|rotate|animation)\s*:/.exec(b.body);
      if (bad) fail(`${css}:${b.line}: .${cls} is a tracked overlay root and must not declare \`${bad[2]}\``);
    }
  }
}

// ------------------------------------------------- 7. layout transitions

/** Properties whose transition costs layout (and, in a docked surface, is banned). */
const LAYOUT_PROPS = new Set([
  'width', 'height', 'min-width', 'min-height', 'max-width', 'max-height',
  'margin', 'margin-top', 'margin-right', 'margin-bottom', 'margin-left',
  'padding', 'padding-top', 'padding-right', 'padding-bottom', 'padding-left',
  'top', 'right', 'bottom', 'left', 'inset', 'gap', 'row-gap', 'column-gap',
  'flex', 'flex-basis', 'flex-grow', 'font-size', 'line-height', 'border-width',
]);

/**
 * Layout transitions that are allowed to stay until their area is converted (FINAL PLAN §5: "an
 * allowlist for existing layout transitions until they are converted"). `selector|property`.
 */
const LAYOUT_ALLOWLIST = new Set([
  // The overlay scrollbar thumb thickens on hover: 4px → 6px inside its own absolute box, so no
  // other element moves. Converting it to `scale` would blur its 1px radius.
  '.overlay-scrollbar-thumb|width',
]);

for (const file of cssFiles()) {
  for (const b of blocks(read(file))) {
    const declarations = [...b.body.matchAll(/transition(?:-property)?\s*:\s*([^;]+);/g)].map((m) => m[1]);
    for (const value of declarations) {
      for (const part of value.split(',')) {
        const prop = part.trim().split(/\s+/)[0];
        if (!LAYOUT_PROPS.has(prop)) continue;
        const allowed = b.selector.split(',').some((s) => LAYOUT_ALLOWLIST.has(`${s.trim()}|${prop}`));
        if (!allowed) {
          fail(`${file}:${b.line}: \`${b.selector}\` transitions the layout property \`${prop}\` — animate transform/opacity, or add it to LAYOUT_ALLOWLIST with a reason`);
        }
      }
    }
  }
}

// ------------------------------------------------- 8. acknowledged exits

// The shell waits for a blank frame before it hides the toast, the switcher or the floating sidebar,
// and it computes that wait from `--t-surface-exit` (`crates/sta/src/motion.rs EXIT_FADE_MS`) because
// it cannot read CSS. The page plays `min(the key's own duration, --t-surface-exit)`, so the two only
// agree while every exit key's duration is at least as long as that cap — otherwise the shell would
// wait for a fade that was already over, or hide before a longer one ended.
const shellMotion = read('crates/sta/src/motion.rs');
const ms = (value) => {
  const n = Number.parseFloat(value);
  return !Number.isFinite(n) ? NaN : /ms\s*$/.test(value) ? n : n * 1000;
};
const surfaceExit = ms(tokenValues.get('--t-surface-exit') ?? '');
const exitFade = Number.parseInt(/pub const EXIT_FADE_MS: i64 = (\d+);/.exec(shellMotion)?.[1] ?? '', 10);
const exitKeys = [...shellMotion.matchAll(/pub const [A-Z]+_KEY: &str = "([^"]+)";/g)].map((m) => m[1]);

if (!Number.isFinite(surfaceExit)) fail('tokens.css: no `--t-surface-exit` on :root (the exit-fade cap the shell waits for)');
else if (exitFade !== surfaceExit) fail(`crates/sta/src/motion.rs EXIT_FADE_MS is ${exitFade} ms, tokens.css --t-surface-exit is ${surfaceExit} ms`);
if (exitKeys.length === 0) fail('crates/sta/src/motion.rs: no `*_KEY` constants parsed (parser out of date?)');
for (const key of exitKeys) {
  if (!registry.some((r) => r.key === key)) {
    fail(`crates/sta/src/motion.rs names "${key}", which is not a registered animation key`);
    continue;
  }
  const value = tokenValues.get(tokenOf(key));
  if (Number.isFinite(surfaceExit) && ms(value ?? '') < surfaceExit) {
    fail(`tokens.css: ${tokenOf(key)} is ${value} — the shell waits ${surfaceExit} ms for this key's exit fade, so it must be at least that long`);
  }
}

// ------------------------------------------------------------------------------------ report

for (const w of warnings) console.log(`warning: ${w}`);
for (const e of errors) console.log(`error: ${e}`);
console.log(
  `check-motion: ${registry.length} keys in ${registryGroups.length} groups, ${declaredTokens.size} duration tokens, ` +
    `${gates.size} per-key gates, ${cssFiles().length} stylesheets, ${jsFiles().length} modules: ` +
    (errors.length ? `${errors.length} error(s)` : 'registry, catalog and gates in sync') +
    (warnings.length ? `, ${warnings.length} warning(s)` : ''),
);
process.exit(errors.length ? 1 : 0);
