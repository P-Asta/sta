// The UI half of the animation registry in `crates/sta-core/src/motion.rs`: the same eight groups
// in the same order, the same 36 keys in the same order, each key's `defaultOn`, plus the label and
// the one-line description core has no business knowing.
//
// It is plain data with no imports, because three very different callers need it: the Settings page
// (`ui/settings/animations.js`), the mock backend (`mock-reducers.js`, which has to resolve
// `UiState.motion` exactly as core does), and `tools/check-motion.mjs`, which fails when this file,
// `motion.rs`, the `--t-*` tokens in `tokens.css` and the CSS gates drift apart.
//
// The resolution rules mirror `motion.rs` exactly:
// - level: `off` when the master switch is off; `reduced` when it follows Windows and Windows has
//   animation effects off; `full` otherwise;
// - a key is off when its group is off **or** its own choice is off;
// - `choices` holds explicit user choices, never differences from the default.

/** @typedef {{key: string, label: string, desc: string, defaultOn: boolean}} AnimationEntry */

export const ANIMATION_GROUPS = Object.freeze([
  {
    id: 'sidebar',
    label: 'Sidebar & top bar',
    keys: [
      { key: 'sidebar.tabInsertRemove', label: 'Opening and closing tabs', desc: 'A new row fades in, a closed one fades out, and the rows below it slide up.', defaultOn: true },
      { key: 'sidebar.reorder', label: 'Reordering', desc: 'Rows glide to their new place when the order changes.', defaultOn: true },
      { key: 'sidebar.dragDrop', label: 'Drag and drop', desc: 'The row you drag lifts, the drop line glides, and a refused drop shakes.', defaultOn: true },
      { key: 'sidebar.folderExpand', label: 'Folders', desc: 'The chevron turns and the tabs inside a folder fade in and out.', defaultOn: true },
      { key: 'sidebar.favorites', label: 'Favorites', desc: 'Tiles give way when pressed, pop when added and glide when rearranged.', defaultOn: true },
      { key: 'sidebar.spaceSwitch', label: 'Switching spaces', desc: 'The space you switch to slides in from its own side of the list.', defaultOn: true },
      { key: 'sidebar.activeRow', label: 'Active tab', desc: 'The highlight crossfades from the row you left to the one you opened.', defaultOn: true },
      { key: 'sidebar.hoverReveal', label: 'Hover reveal', desc: 'A hidden sidebar slides in from outside the window edge and back out again.', defaultOn: true },
      { key: 'sidebar.panels', label: 'Panels', desc: 'Downloads, the app menu and the space sheets open and close with motion.', defaultOn: true },
      { key: 'sidebar.downloads', label: 'Downloads', desc: 'A download card rises in, and its ring turns into a check mark when it finishes.', defaultOn: true },
      { key: 'sidebar.clearToday', label: 'Clear Today', desc: 'Cleared tabs slide away one after another.', defaultOn: true },
      { key: 'sidebar.urlPill', label: 'URL pill', desc: 'The loading bar, the copy check mark and a changed site name crossfade.', defaultOn: true },
      { key: 'sidebar.splitRow', label: 'Split rows', desc: 'The focused segment glides, and panes fade as they are added or removed.', defaultOn: true },
      { key: 'topbar.navFade', label: 'Top bar buttons', desc: 'Back, forward and reload fade in after the top bar has resized.', defaultOn: true },
    ],
  },
  {
    id: 'commandBar',
    label: 'Command bar',
    keys: [
      { key: 'commandBar.open', label: 'Opening', desc: 'The card fades in and the results rise into place.', defaultOn: true },
      { key: 'commandBar.results', label: 'First results', desc: 'The results of the first search after opening fade in one after another.', defaultOn: true },
      { key: 'commandBar.selection', label: 'Selection', desc: 'The highlight glides between rows when you move with the arrow keys.', defaultOn: true },
      { key: 'commandBar.modeToggle', label: 'Mode chip', desc: 'The mode chip and the placeholder crossfade when the mode changes.', defaultOn: true },
    ],
  },
  {
    id: 'overlays',
    label: 'Overlays',
    keys: [
      { key: 'overlays.toast', label: 'Messages', desc: 'A message rises into view; a message that replaces it crossfades its text.', defaultOn: true },
      { key: 'overlays.switcher', label: 'Tab switcher', desc: 'The Ctrl+Tab cards fade in and the selection ring glides between them.', defaultOn: true },
      { key: 'overlays.find', label: 'Find bar', desc: 'The find bar fades in, and shakes when Enter finds nothing.', defaultOn: true },
      { key: 'overlays.permission', label: 'Permission prompts', desc: "A site's request for your camera, microphone or location fades in.", defaultOn: true },
      { key: 'overlays.peek', label: 'Peek', desc: 'The Peek header crossfades when the page it previews changes.', defaultOn: true },
    ],
  },
  {
    id: 'menus',
    label: 'Menus',
    keys: [
      { key: 'menus.popIn', label: 'Menus and popovers', desc: 'Menus open from the side they are anchored to, and submenus slide in.', defaultOn: true },
    ],
  },
  {
    id: 'pages',
    label: 'Pages',
    keys: [
      { key: 'pages.enter', label: 'Opening a page', desc: 'Settings, Archive, History and Boosts fade their cards in.', defaultOn: true },
      { key: 'pages.listRows', label: 'List rows', desc: 'Rows that change in Archive and History fade as they come and go.', defaultOn: true },
      { key: 'pages.navIndicator', label: 'Section marker', desc: 'The marker beside the section list glides as you scroll.', defaultOn: true },
      { key: 'pages.boostsEditor', label: 'Boosts editor', desc: 'Switching to another boost crossfades the editor.', defaultOn: true },
      { key: 'pages.emptyHero', label: 'Empty state', desc: 'The "Ctrl+T to open a tab" card fades in.', defaultOn: true },
    ],
  },
  {
    id: 'theme',
    label: 'Theme',
    keys: [
      { key: 'theme.crossFade', label: 'Colour crossfade', desc: 'Colours fade when you switch space or theme, or when Windows changes light and dark.', defaultOn: true },
    ],
  },
  {
    id: 'controls',
    label: 'Controls',
    keys: [
      { key: 'controls.hoverPress', label: 'Hover and press', desc: 'Buttons and rows fade their hover background in and give way when pressed.', defaultOn: true },
      { key: 'controls.toggles', label: 'Switches and sections', desc: 'Switch thumbs, checkboxes, segmented controls and expanding sections move between states.', defaultOn: true },
      { key: 'controls.smoothScroll', label: 'Smooth scrolling', desc: 'Jumping to a section or a row scrolls there instead of snapping.', defaultOn: true },
    ],
  },
  {
    id: 'indicators',
    label: 'Indicators',
    keys: [
      { key: 'indicators.loading', label: 'Loading', desc: 'Spinners turn and progress bars move. Off leaves a static ring or bar, never nothing.', defaultOn: true },
      { key: 'indicators.badges', label: 'Badges and dots', desc: 'Counters pop when they change, and a tab waiting for your permission pulses.', defaultOn: true },
      { key: 'indicators.audio', label: 'Audio', desc: 'The equalizer beside a tab playing sound moves while the window is in front.', defaultOn: true },
    ],
  },
]);

/** Every entry, flattened, each with its `group` id. */
export const ANIMATION_KEYS = Object.freeze(ANIMATION_GROUPS.flatMap((g) => g.keys.map((k) => Object.freeze({ ...k, group: g.id }))));

/** At most this many entries are kept in each stored map (`motion.rs MAX_ANIMATION_ENTRIES`). */
export const MAX_ANIMATION_ENTRIES = 128;

/** `settings.animations` with its defaults filled in (an older shell may not send it at all). */
export function animationSettings(settings) {
  const a = settings?.animations ?? {};
  const bool = (v, fallback) => (typeof v === 'boolean' ? v : fallback);
  const map = (v) => (v && typeof v === 'object' && !Array.isArray(v) ? { ...v } : {});
  return { enabled: bool(a.enabled, true), followSystem: bool(a.followSystem, true), groups: map(a.groups), choices: map(a.choices) };
}

/** Whether a group is on (a group nobody touched is on). */
export function groupOn(a, id) {
  return typeof a.groups[id] === 'boolean' ? a.groups[id] : true;
}

/**
 * A key's **own** choice — what its switch shows even while its group is off.
 * @param {ReturnType<typeof animationSettings>} a
 * @param {AnimationEntry} entry
 */
export function keyOwnValue(a, entry) {
  return typeof a.choices[entry.key] === 'boolean' ? a.choices[entry.key] : entry.defaultOn;
}

/** `'full' | 'reduced' | 'off'` for these settings and the Windows setting. */
export function motionLevel(a, systemAnimations) {
  if (!a.enabled) return 'off';
  if (a.followSystem && !systemAnimations) return 'reduced';
  return 'full';
}

/** The keys that are off (group off, or an explicit choice of off), in registry order. */
export function motionOffKeys(a) {
  return ANIMATION_KEYS.filter((e) => !(groupOn(a, e.group) && keyOwnValue(a, e))).map((e) => e.key);
}

/** Nothing has been customised: "Reset to defaults" has nothing to do. */
export function isDefaultAnimations(a) {
  return a.enabled && a.followSystem && Object.keys(a.groups).length === 0 && Object.keys(a.choices).length === 0;
}

/**
 * Apply an `AnimationsPatch` (`crates/sta-core/src/motion.rs`) in the documented order: `reset`,
 * then the scalars, then the maps. `null` clears an entry; unknown keys are ignored.
 * @param {ReturnType<typeof animationSettings>} a mutated in place
 * @param {any} patch
 */
export function applyAnimationsPatch(a, patch) {
  if (!patch || typeof patch !== 'object') return a;
  if (patch.reset === true) {
    a.enabled = true;
    a.followSystem = true;
    a.groups = {};
    a.choices = {};
  }
  if (typeof patch.enabled === 'boolean') a.enabled = patch.enabled;
  if (typeof patch.followSystem === 'boolean') a.followSystem = patch.followSystem;
  const known = (map, id) => (map === 'groups' ? ANIMATION_GROUPS.some((g) => g.id === id) : ANIMATION_KEYS.some((e) => e.key === id));
  for (const [name, from] of [
    ['groups', patch.groups],
    ['choices', patch.set],
  ]) {
    if (!from || typeof from !== 'object') continue;
    for (const [id, value] of Object.entries(from)) {
      if (!known(name, id)) continue;
      if (value === null || value === undefined) delete a[name][id];
      else if (typeof value === 'boolean' && (Object.keys(a[name]).length < MAX_ANIMATION_ENTRIES || id in a[name])) a[name][id] = value;
    }
  }
  return a;
}

/** `UiState.motion` for these settings (what `Store::motion_view` returns). */
export function motionView(settings, systemAnimations = true) {
  const a = animationSettings(settings);
  return { level: motionLevel(a, systemAnimations), off: motionOffKeys(a), systemAnimations };
}
