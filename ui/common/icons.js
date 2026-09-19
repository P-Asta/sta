// Inline SVG glyphs for sta UI surfaces. Original drawings on a 24×24 grid, 1.5px strokes in
// `currentColor` with round caps/joins, so they inherit text color and scale crisply (at 16px a
// stroke renders ~1 device pixel at 100% zoom).
//
//   import { Icon } from '/common/icons.js';
//   html`<${Icon} name="search" size=${16} />`                 // decorative (aria-hidden)
//   html`<${Icon} name="lock" label="Secure connection" />`    // meaningful (role="img")
//
// Core refers to glyphs by name in `ResultIcon::Glyph { name }` (omnibox.rs); keep those names.

import { h } from './vendor/htm-preact.js';

const p = (d) => ['path', { d }];
const c = (cx, cy, r) => ['circle', { cx, cy, r }];
/** Filled dot (for "more", drag handle). */
const dot = (cx, cy, r = 1.35) => ['circle', { cx, cy, r, fill: 'currentColor', stroke: 'none' }];
const rect = (x, y, width, height, rx = 0) => ['rect', { x, y, width, height, rx }];

/** @type {Record<string, Array<[string, Record<string, string|number>]>>} */
export const GLYPHS = {
  // ---------------------------------------------------------------- navigation & page
  search: [c(10.5, 10.5, 6.25), p('M15.25 15.25 20 20')],
  globe: [
    c(12, 12, 8.75),
    p('M3.5 12h17'),
    p('M12 3.25c-2.3 2.4-3.5 5.3-3.5 8.75s1.2 6.35 3.5 8.75c2.3-2.4 3.5-5.3 3.5-8.75S14.3 5.65 12 3.25Z'),
  ],
  back: [p('M19.5 12h-15'), p('M10.5 6 4.5 12l6 6')],
  forward: [p('M4.5 12h15'), p('M13.5 6l6 6-6 6')],
  reload: [p('M19.25 12.5a7.25 7.25 0 1 1-2.12-5.63'), p('M17.75 3.75v3.5h-3.5')],
  stop: [p('M7 7l10 10'), p('M17 7 7 17')],
  'chevron-right': [p('M9.5 5.5 16 12l-6.5 6.5')],
  'chevron-left': [p('M14.5 5.5 8 12l6.5 6.5')],
  'chevron-down': [p('M5.5 9.5 12 16l6.5-6.5')],
  'chevron-up': [p('M5.5 14.5 12 8l6.5 6.5')],
  'arrow-up': [p('M12 19.5v-15'), p('M6 10.5l6-6 6 6')],
  'arrow-down': [p('M12 4.5v15'), p('M6 13.5l6 6 6-6')],
  external: [p('M14 4.5h5.5V10'), p('M19.5 4.5 11 13'), p('M17.5 14v3.75a1.75 1.75 0 0 1-1.75 1.75h-9.5a1.75 1.75 0 0 1-1.75-1.75v-9.5A1.75 1.75 0 0 1 6.25 6.5H10')],
  link: [
    p('M10.5 13.5a3.75 3.75 0 0 0 5.3 0l2.9-2.9a3.75 3.75 0 0 0-5.3-5.3l-.9.9'),
    p('M13.5 10.5a3.75 3.75 0 0 0-5.3 0l-2.9 2.9a3.75 3.75 0 0 0 5.3 5.3l.9-.9'),
  ],
  home: [p('M4.5 10.25 12 4l7.5 6.25v8.25a1.5 1.5 0 0 1-1.5 1.5h-3.5v-5.5h-5V20H6a1.5 1.5 0 0 1-1.5-1.5Z')],
  lock: [rect(5.25, 10.25, 13.5, 10, 2.25), p('M8.5 10.25V7.5a3.5 3.5 0 0 1 7 0v2.75'), p('M12 14.25v2')],
  info: [c(12, 12, 8.75), p('M12 11v5.25'), dot(12, 7.9, 1.05)],
  warning: [
    p('M10.27 4.5 3.2 16.75A2 2 0 0 0 4.93 19.75h14.14a2 2 0 0 0 1.73-3L13.73 4.5a2 2 0 0 0-3.46 0Z'),
    p('M12 9.5v4'),
    dot(12, 16.6, 1.05),
  ],
  check: [p('M5 12.5l4.5 4.5L19 7.5')],
  close: [p('M6.5 6.5l11 11'), p('M17.5 6.5l-11 11')],
  plus: [p('M12 5v14'), p('M5 12h14')],
  minus: [p('M5 12h14')],
  more: [dot(6, 12), dot(12, 12), dot(18, 12)],
  menu: [p('M4.5 7h15'), p('M4.5 12h15'), p('M4.5 17h15')],
  'drag-handle': [dot(9, 6), dot(15, 6), dot(9, 12), dot(15, 12), dot(9, 18), dot(15, 18)],
  undo: [p('M9 14.5 4.5 10 9 5.5'), p('M4.5 10h10a5 5 0 0 1 0 10H11')],

  // ---------------------------------------------------------------- sidebar & items
  sidebar: [rect(3.5, 4.5, 17, 15, 2.5), p('M9.5 4.5v15')],
  split: [rect(3.5, 4.5, 17, 15, 2.5), p('M12 4.5v15')],
  pin: [p('M9 3.75h6'), p('M10.25 3.75v5L7.5 12.25v1.5h9v-1.5l-2.75-3.5v-5'), p('M12 13.75v6.5')],
  star: [p('M12 4.1l2.29 5.64 6.08.44-4.66 3.93 1.46 5.91L12 16.8l-5.17 3.22 1.46-5.91-4.66-3.93 6.08-.44Z')],
  folder: [p('M3.5 7.25a2 2 0 0 1 2-2h3.75l2.25 2.5h7a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2Z')],
  'folder-open': [
    p('M3.5 16.5V7.25a2 2 0 0 1 2-2h3.75l2.25 2.5h6a2 2 0 0 1 2 2v1'),
    p('M3.5 16.75 5.9 11.5a1.75 1.75 0 0 1 1.6-1h12.1a1 1 0 0 1 .92 1.39l-2.35 5.6a2 2 0 0 1-1.84 1.26H5.5a2 2 0 0 1-2-2Z'),
  ],
  'folder-plus': [p('M11 18.75H5.5a2 2 0 0 1-2-2v-9.5a2 2 0 0 1 2-2h3.75l2.25 2.5h7a2 2 0 0 1 2 2V12'), p('M17.5 14.5v5'), p('M15 17h5')],
  space: [rect(8, 3.5, 12.5, 12.5, 2.5), p('M16 16v1.5a3 3 0 0 1-3 3H6.5a3 3 0 0 1-3-3V11a3 3 0 0 1 3-3H8')],
  archive: [rect(3.25, 4.5, 17.5, 4, 1.25), p('M5 8.5v9a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-9'), p('M10 12.5h4')],
  restore: [rect(3.25, 4.5, 17.5, 4, 1.25), p('M5 8.5v9a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-9'), p('M12 17v-5'), p('M9.75 14.25 12 12l2.25 2.25')],
  history: [p('M4.75 12a7.25 7.25 0 1 0 2.12-5.13'), p('M4.5 3.75v3.5H8'), p('M12 8v4.25l2.75 2')],
  copy: [rect(8.5, 8.5, 11.5, 11.5, 2), p('M15.5 8.5V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7.5a2 2 0 0 0 2 2h2.5')],
  trash: [
    p('M4.5 7h15'),
    p('M9.5 7V5.25a1.5 1.5 0 0 1 1.5-1.5h2a1.5 1.5 0 0 1 1.5 1.5V7'),
    p('M6.5 7l.85 11.2a2 2 0 0 0 2 1.8h5.3a2 2 0 0 0 2-1.8L17.5 7'),
    p('M10.25 11v5'),
    p('M13.75 11v5'),
  ],
  edit: [p('M14.5 6.5l3 3'), p('M16.3 4.7a2.1 2.1 0 0 1 3 3L8.5 18.5 4.25 19.75 5.5 15.5Z')],
  download: [p('M12 4v11'), p('M7 10.5l5 5 5-5'), p('M5 19.5h14')],
  // Extensions (Ctrl+E): a puzzle piece — one square with a tab on top and a socket on the right.
  puzzle: [
    p('M10 4.75a1.75 1.75 0 0 1 3.5 0V6h4.25a1.5 1.5 0 0 1 1.5 1.5v3.25h-1.5a1.75 1.75 0 0 0 0 3.5h1.5v3.25a1.5 1.5 0 0 1-1.5 1.5H6.25a1.5 1.5 0 0 1-1.5-1.5V7.5A1.5 1.5 0 0 1 6.25 6H10V4.75Z'),
  ],
  boost: [
    p('M19.6 4.4a1.4 1.4 0 0 1 0 2L13 13l-2-2 6.6-6.6a1.4 1.4 0 0 1 2 0Z'),
    p('M11 13c-1.7-1.1-3.9-.6-4.9 1.1-.9 1.5-.4 3.6-2.6 4.9 2.8 1.3 5.9.9 7.6-.9 1.2-1.3 1.2-3.5-.1-5.1Z'),
  ],
  palette: [
    p('M12 3.5a8.5 8.5 0 0 0 0 17c1 0 1.6-.7 1.6-1.5 0-.45-.2-.8-.45-1.1-.25-.3-.45-.65-.45-1.1 0-.85.7-1.55 1.55-1.55H16a4.5 4.5 0 0 0 4.5-4.5c0-4-3.8-7.25-8.5-7.25Z'),
    dot(7.75, 12, 1.2),
    dot(9.75, 7.75, 1.2),
    dot(14.25, 7.75, 1.2),
  ],
  emoji: [c(12, 12, 8.75), p('M8.5 14a4.25 4.25 0 0 0 7 0'), dot(9.25, 9.75, 1.1), dot(14.75, 9.75, 1.1)],

  // ---------------------------------------------------------------- page tools
  zoom: [c(10.5, 10.5, 6.25), p('M15.25 15.25 20 20'), p('M8 10.5h5'), p('M10.5 8v5')],
  'zoom-out': [c(10.5, 10.5, 6.25), p('M15.25 15.25 20 20'), p('M8 10.5h5')],
  find: [p('M11.5 20.25H6.75a2 2 0 0 1-2-2V5.75a2 2 0 0 1 2-2h6.5l4.5 4.5v2'), p('M13.25 3.75v4.5h4.5'), c(15.25, 15.25, 2.75), p('M17.25 17.25l2.5 2.5')],
  case: [p('M3.5 17.5 8 6.5l4.5 11'), p('M5.25 13.5h5.5'), c(17, 14.75, 2.75), p('M19.75 11.75v5.75')],
  code: [p('M8.5 7.5 4 12l4.5 4.5'), p('M15.5 7.5 20 12l-4.5 4.5'), p('M13.25 5.5l-2.5 13')],
  print: [p('M7 9V4.5h10V9'), p('M7 17H5.5a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2h13a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H17'), rect(7, 14, 10, 6, 1)],
  settings: [
    p('M10.38 4.98 10.68 2.59h2.64l.3 2.39a7.2 7.2 0 0 1 2.2.91l1.9-1.48 1.87 1.87-1.48 1.9a7.2 7.2 0 0 1 .91 2.2l2.39.3v2.64l-2.39.3a7.2 7.2 0 0 1-.91 2.2l1.48 1.9-1.87 1.87-1.9-1.48a7.2 7.2 0 0 1-2.2.91l-.3 2.39h-2.64l-.3-2.39a7.2 7.2 0 0 1-2.2-.91l-1.9 1.48-1.87-1.87 1.48-1.9a7.2 7.2 0 0 1-.91-2.2l-2.39-.3v-2.64l2.39-.3a7.2 7.2 0 0 1 .91-2.2l-1.48-1.9 1.87-1.87 1.9 1.48a7.2 7.2 0 0 1 2.2-.91Z'),
    c(12, 12, 3),
  ],
  pause: [p('M9 6v12'), p('M15 6v12')],
  play: [p('M8 5.5v13l10.5-6.5Z')],

  // ---------------------------------------------------------------- media & permissions
  speaker: [p('M4 9.5v5h3.5l4.5 4v-13l-4.5 4Z'), p('M15.5 9.25a3.75 3.75 0 0 1 0 5.5'), p('M18 6.75a7.25 7.25 0 0 1 0 10.5')],
  'speaker-muted': [p('M4 9.5v5h3.5l4.5 4v-13l-4.5 4Z'), p('M16 9.5l5 5'), p('M21 9.5l-5 5')],
  camera: [rect(3, 6.5, 12.5, 11, 2.5), p('M15.5 10.5l5-3v9l-5-3')],
  mic: [rect(9, 3.5, 6, 10.5, 3), p('M6 11a6 6 0 0 0 12 0'), p('M12 17v3.5'), p('M9 20.5h6')],
  screen: [rect(3.5, 4.5, 17, 12, 2), p('M9 20h6'), p('M12 16.5V20')],
  location: [p('M12 21s-6.5-5.6-6.5-11a6.5 6.5 0 0 1 13 0c0 5.4-6.5 11-6.5 11Z'), c(12, 10, 2.25)],
  bell: [p('M6.5 16.5v-5.25a5.5 5.5 0 0 1 11 0v5.25l1.5 1.5H5Z'), p('M10 20.25a2.1 2.1 0 0 0 4 0')],
  clipboard: [rect(8.5, 3.25, 7, 3.5, 1.25), p('M8.5 5H7a2 2 0 0 0-2 2v11.75a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-1.5')],

  // ---------------------------------------------------------------- app & window
  moon: [p('M19.5 14.25A8 8 0 1 1 9.75 4.5a6.5 6.5 0 0 0 9.75 9.75Z')],
  sun: [
    c(12, 12, 3.75),
    p('M12 2.75v2'),
    p('M12 19.25v2'),
    p('M2.75 12h2'),
    p('M19.25 12h2'),
    p('M5.46 5.46l1.41 1.41'),
    p('M17.13 17.13l1.41 1.41'),
    p('M5.46 18.54l1.41-1.41'),
    p('M17.13 6.87l1.41-1.41'),
  ],
  quit: [p('M12 3.5v8'), p('M7.05 6.55a7.5 7.5 0 1 0 9.9 0')],
  // Caption buttons: thin, Windows-11-like proportions (use at 16px inside 46×40 buttons).
  minimize: [p('M6.5 12h11')],
  maximize: [rect(6.5, 6.5, 11, 11, 1.5)],
  'restore-window': [rect(6.5, 9, 8.5, 8.5, 1.25), p('M9 9V7.75A1.25 1.25 0 0 1 10.25 6.5h6A1.25 1.25 0 0 1 17.5 7.75v6A1.25 1.25 0 0 1 16.25 15H15')],
  'close-window': [p('M6.75 6.75l10.5 10.5'), p('M17.25 6.75 6.75 17.25')],
  // ---------------------------------------------------------------- AI agents (a sparkle; agent-ui.js AgentGlyph)
  agent: [p('M11 3.5c.5 3.9 2.6 6 6.5 6.5-3.9.5-6 2.6-6.5 6.5-.5-3.9-2.6-6-6.5-6.5 3.9-.5 6-2.6 6.5-6.5Z'), p('M18 14.5c.25 1.8 1.2 2.75 3 3-1.8.25-2.75 1.2-3 3-.25-1.8-1.2-2.75-3-3 1.8-.25 2.75-1.2 3-3Z')],
};

/** All glyph names, sorted. */
export const ICON_NAMES = Object.freeze(Object.keys(GLYPHS).sort());

/** Whether `name` is a known glyph. */
export const hasIcon = (name) => Object.prototype.hasOwnProperty.call(GLYPHS, name);

const warned = new Set();
function glyphFor(name) {
  if (hasIcon(name)) return GLYPHS[name];
  if (!warned.has(name)) {
    warned.add(name);
    console.warn(`[icons] unknown glyph "${name}"`);
  }
  return [];
}

const svgAttrs = (size, strokeWidth) => ({
  xmlns: 'http://www.w3.org/2000/svg',
  viewBox: '0 0 24 24',
  width: size,
  height: size,
  fill: 'none',
  stroke: 'currentColor',
  'stroke-width': strokeWidth,
  'stroke-linecap': 'round',
  'stroke-linejoin': 'round',
  focusable: 'false',
});

/**
 * Preact icon component.
 * @param {object} props
 * @param {string} props.name glyph name (see `ICON_NAMES`)
 * @param {number} [props.size=16] rendered width/height in CSS px
 * @param {string} [props.label] accessible name; omitted → decorative (`aria-hidden`)
 * @param {number} [props.strokeWidth=1.5] in 24-unit grid space
 * @param {string} [props.class] extra class names
 * @param {object} [props.style]
 */
export function Icon({ name, size = 16, label, strokeWidth = 1.5, class: className, style }) {
  const a11y = label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': 'true' };
  return h(
    'svg',
    { ...svgAttrs(size, strokeWidth), ...a11y, class: className ? `icon ${className}` : 'icon', style, 'data-icon': name },
    glyphFor(name).map(([tag, attrs], i) => h(tag, { key: i, ...attrs })),
  );
}

/**
 * The application mark: the two paths of `crates/sta/res/icon.svg`, verbatim, in its own 64-unit
 * box — an eight-pointed star with a smaller one cut out of its middle (`evenodd` is what makes
 * the hole). Filled, not stroked, and in `currentColor`, so a surface only has to set a size and a
 * colour. The same drawing is what `crates/sta/res/make_icon.py` rasterises into the app icon:
 * change one and change the others, or sta ends up with two different faces.
 */
export const APP_MARK_PATH =
  'M31.8126 8.23438 34.0767 26.7215 48.7501 15.2501 37.2787 29.9235 55.7658 32.1876 37.2787 34.4517' +
  ' 48.7501 49.1251 34.0767 37.6537 31.8126 56.1408 29.5485 37.6537 14.8751 49.1251 26.3465 34.4517' +
  ' 7.85938 32.1876 26.3465 29.9235 14.8751 15.2501 29.5485 26.7215Z' +
  'M27.2037 19.9451 32.2586 28.0671 37.3135 19.9451 35.1448 29.2626 44.4623 27.0938 36.3402 32.1487' +
  ' 44.4623 37.2037 35.1448 35.0349 37.3135 44.3524 32.2586 36.2304 27.2037 44.3524 29.3725 35.0349' +
  ' 20.0549 37.2037 28.177 32.1487 20.0549 27.0938 29.3725 29.2626Z';

/**
 * The application mark as an `<svg>` (see [`APP_MARK_PATH`]).
 * @param {object} props
 * @param {number} [props.size=64] rendered width/height in CSS px
 * @param {string} [props.label] accessible name; omitted → decorative (`aria-hidden`)
 * @param {string} [props.class] extra class names
 */
export function AppMark({ size = 64, label, class: className }) {
  const a11y = label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': 'true' };
  return h(
    'svg',
    {
      xmlns: 'http://www.w3.org/2000/svg',
      viewBox: '0 0 64 64',
      width: size,
      height: size,
      fill: 'currentColor',
      ...a11y,
      class: className,
    },
    h('path', { d: APP_MARK_PATH, 'fill-rule': 'evenodd', 'clip-rule': 'evenodd' }),
  );
}

/**
 * Plain-DOM variant for code outside Preact (never uses innerHTML).
 * @returns {SVGSVGElement}
 */
export function createIconElement(name, { size = 16, label, strokeWidth = 1.5 } = {}) {
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg');
  for (const [k, v] of Object.entries(svgAttrs(size, strokeWidth))) if (k !== 'xmlns') svg.setAttribute(k, String(v));
  if (label) {
    svg.setAttribute('role', 'img');
    svg.setAttribute('aria-label', label);
  } else {
    svg.setAttribute('aria-hidden', 'true');
  }
  svg.setAttribute('class', 'icon');
  for (const [tag, attrs] of glyphFor(name)) {
    const el = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
    svg.appendChild(el);
  }
  return svg;
}
