// Applies core-derived space colors (`ThemeColors`) as CSS custom properties (PROTOCOL.md §4).
// Colors always come from core (`UiState.spaces[].colors`, `themePresets[].colors`,
// `invoke('theme.colors')`); UI code never derives them itself.

/** ThemeColors field → CSS custom property. */
export const THEME_VARS = Object.freeze({
  frame: '--frame',
  gradientStart: '--grad-start',
  gradientEnd: '--grad-end',
  accent: '--accent',
  text: '--text',
  textMuted: '--text-muted',
  hover: '--hover',
  pressed: '--pressed',
  activeRow: '--active-row',
  divider: '--divider',
  surface: '--surface',
  border: '--border',
});

/** Last applied key per element, so 30 Hz state pushes don't restyle unchanged themes. */
const applied = new WeakMap();
/** Same, for `applyMotion`. */
const appliedMotion = new WeakMap();
/** Same, for `applyWindowState`. */
const appliedWindow = new WeakMap();

/** The three `UiState.motion.level` values (`crates/sta-core/src/motion.rs`). */
const MOTION_LEVELS = ['full', 'reduced', 'off'];

/**
 * Set the ThemeColors custom properties on `target` (default `<html>`). Unknown or missing
 * fields are skipped, so a partial object only overrides what it has.
 * @param {Record<string, string>} colors
 * @param {HTMLElement} [target]
 */
export function applyColors(colors, target = document.documentElement) {
  if (!colors) return;
  for (const [field, prop] of Object.entries(THEME_VARS)) {
    const value = colors[field];
    if (typeof value === 'string' && value) target.style.setProperty(prop, value);
  }
}

/**
 * Inline-style object with the custom properties for `colors`, for scoped previews
 * (theme swatches, space sheet live preview): `<div style=${themeStyle(preset.colors)}>`.
 * Pair with `data-theme` on the same element when previewing the other mode.
 */
export function themeStyle(colors) {
  const style = {};
  if (!colors) return style;
  for (const [field, prop] of Object.entries(THEME_VARS)) {
    if (typeof colors[field] === 'string') style[prop] = colors[field];
  }
  return style;
}

/** The active `SpaceView` of a UiState (first space as fallback). */
export function activeSpaceOf(state) {
  if (!state?.spaces?.length) return null;
  return state.spaces.find((s) => s.id === state.activeSpace) ?? state.spaces[0];
}

/**
 * Apply a UiState's appearance to `target` (default `<html>`): `data-theme="light|dark"`,
 * `color-scheme`, and the active space's colors. Cheap to call on every state push.
 * @param {any} state UiState
 * @param {HTMLElement} [target]
 */
export function applyTheme(state, target = document.documentElement) {
  if (!state) return;
  const dark = Boolean(state.dark);
  const colors = activeSpaceOf(state)?.colors ?? null;
  const key = `${dark}|${colors ? JSON.stringify(colors) : ''}`;
  if (applied.get(target) === key) return;
  applied.set(target, key);
  target.dataset.theme = dark ? 'dark' : 'light';
  target.style.colorScheme = dark ? 'dark' : 'light';
  applyColors(colors, target);
}

/**
 * Apply a UiState's `motion` (`crates/sta-core/src/motion.rs`, PROTOCOL §3) to `target`
 * (default `<html>`): `data-motion="full|reduced|off"` and `data-anim-off="<key> <key> …"`.
 * `tokens.css` turns those into zeroed durations and static indicators; `motion.js` reads the same
 * two attributes to gate WAAPI animations. Cheap to call on every state push (key-compared).
 *
 * A state without `motion` (an older shell, a fixture that predates the field) means "full motion,
 * nothing off" — and, because `data-motion` is then set all the same, the `prefers-reduced-motion`
 * bootstrap block in `tokens.css` stops applying: once core speaks about motion, core decides.
 * @param {any} state UiState
 * @param {HTMLElement} [target]
 */
export function applyMotion(state, target = document.documentElement) {
  if (!state) return;
  const motion = state.motion ?? null;
  const level = MOTION_LEVELS.includes(motion?.level) ? motion.level : 'full';
  // Keys are `group.name` with no whitespace, so `data-anim-off` can be a space-separated list a
  // CSS `[data-anim-off~="key"]` selector matches. Anything else is dropped rather than trusted.
  const off = Array.isArray(motion?.off)
    ? motion.off.filter((k) => typeof k === 'string' && k !== '' && !/\s/.test(k))
    : [];
  const key = `${level}|${off.join(' ')}`;
  if (appliedMotion.get(target) === key) return;
  appliedMotion.set(target, key);
  target.dataset.motion = level;
  if (off.length) target.dataset.animOff = off.join(' ');
  else delete target.dataset.animOff;
  applyThemeFade(level, off, target);
}

/**
 * `theme.crossFade`: the theme custom properties are registered with `@property`, so a page can
 * cross-fade them (`tokens.css :root.theme-fade`). Only pages that paint their own background may:
 * inside a native card (`html.surface-overlay`) the fill, the border and the corner tiles are the
 * shell's, and they snap at the fade's midpoint — a page fading there would show a ring of
 * mismatched color for the whole fade. Called from `applyMotion`, so the class follows the switch.
 *
 * A card page's own snap has to **meet** that midpoint rather than run ahead of it: the shell delays
 * `SetChrome` by half the fade (`crates/sta/src/motion.rs chrome_delay_ms`), so a page that took the
 * new colors the moment the state arrived would be a white pill inside a black card for those 150 ms.
 * `theme-snap` holds the previous colors for exactly that long (`tokens.css`), under exactly the
 * condition the shell delays under, so both sides change in the same frame. It is only ever added
 * *after* `applyTheme` has set this state's colors (`ipc.js renderState` calls them in that order),
 * so a page's first colors still arrive instantly.
 */
function applyThemeFade(level, off, target) {
  const card = target.classList.contains('surface-overlay');
  const on = level !== 'off' && !off.includes('theme.crossFade');
  target.classList.toggle('theme-fade', !card && on);
  target.classList.toggle('theme-snap', card && on);
}

/**
 * Apply the parts of `UiState.window` that CSS needs: `<html data-focused>` while the window has
 * focus. Infinite indicators in a docked surface (the audio equalizer) run only while it is set, so
 * hours of music in an unfocused window cost no compositing. Cheap to call on every state push.
 * @param {any} state UiState
 * @param {HTMLElement} [target]
 */
export function applyWindowState(state, target = document.documentElement) {
  if (!state) return;
  const focused = state.window?.focused !== false;
  if (appliedWindow.get(target) === focused) return;
  appliedWindow.set(target, focused);
  if (focused) target.dataset.focused = '';
  else delete target.dataset.focused;
}
