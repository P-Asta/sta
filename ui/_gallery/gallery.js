// Visual gallery of ui/common: every component and glyph rendered with the fixture theme.
// Serve ui/ and open /_gallery/ (mock mode is implied outside sta://); add ?dark=1 for dark.
// Screenshot: powershell -File tools/ui-shot.ps1 -Path /_gallery/ -Width 1280 -Height 3000 -Out g.png [-Dark]

import { html, render, useRef, useState } from '/common/vendor/htm-preact.js';
import { dispatch, isMock, startSurface } from '/common/ipc.js';
import { ICON_NAMES, Icon } from '/common/icons.js';
import { THEME_VARS, activeSpaceOf, themeStyle } from '/common/theme.js';
import { ANIMATION_GROUPS } from '/common/motion-catalog.js';
import * as motion from '/common/motion.js';
import {
  AudioBars,
  Button,
  Checkbox,
  EmojiPicker,
  Favicon,
  IconButton,
  Kbd,
  Menu,
  Popover,
  ProgressBar,
  ProgressRing,
  Select,
  Spinner,
  TextField,
  Toggle,
} from '/common/components.js';
import {
  allTabs,
  describeDownload,
  downloadFraction,
  formatBytes,
  formatDuration,
  formatSpeed,
  hostLetterColor,
  relativeTime,
  dayLabel,
} from '/common/util.js';

const Card = ({ title, span, children }) => html`<section class=${span ? 'card span-2' : 'card'}>
  <h2>${title}</h2>
  ${children}
</section>`;

// ------------------------------------------------------------------------------------ theme

function ThemeCard({ state }) {
  const space = activeSpaceOf(state);
  return html`<${Card} title="Theme · active space colors" span>
    <div class="swatches">
      ${Object.entries(THEME_VARS).map(
        ([field, prop]) => html`<div class="swatch" key=${field}>
          <div class="swatch-color" style=${{ '--c': `var(${prop})` }} />
          <span class="mono">${prop}</span>
          <span class="muted ellipsis mono">${space?.colors[field]}</span>
        </div>`,
      )}
    </div>
    <h3>Spaces (click to switch; theme.js themeStyle previews)</h3>
    <div class="space-previews">
      ${state.spaces.map(
        (s) => html`<button
          key=${s.id}
          class="space-preview theme-scope"
          data-theme=${state.dark ? 'dark' : 'light'}
          style=${themeStyle(s.colors)}
          onClick=${() => dispatch({ type: 'switchSpace', id: s.id })}
        >
          <span class="row"><span class="emoji">${s.icon}</span><strong class="grow">${s.name}</strong><span class="accent-dot" /></span>
          <span class="row row-sample is-active">Active row</span>
          <span class="row row-sample">Row</span>
        </button>`,
      )}
    </div>
    <h3>Theme presets (state.themePresets)</h3>
    <div class="preset-strip">
      ${state.themePresets.map(
        (p) => html`<span key=${p.name} class="preset theme-scope" data-theme=${state.dark ? 'dark' : 'light'} style=${themeStyle(p.colors)}>
          <span class="preset-dot" />${p.name}
        </span>`,
      )}
    </div>
  </${Card}>`;
}

// ------------------------------------------------------------------------------------ icons

function IconsCard() {
  return html`<${Card} title=${`Icons · ${ICON_NAMES.length} glyphs (16px · 24px)`} span>
    <div class="icon-grid">
      ${ICON_NAMES.map(
        (name) => html`<div class="icon-cell" key=${name} title=${name}>
          <span class="sizes"><${Icon} name=${name} size=${16} /><${Icon} name=${name} size=${24} /></span>
          <span class="name">${name}</span>
        </div>`,
      )}
    </div>
    <h3>Detail at 48px (drawing grid)</h3>
    <div class="icon-big">
      ${['settings', 'boost', 'reload', 'history', 'pin', 'space', 'folder-open', 'star', 'speaker', 'restore-window', 'palette', 'find'].map(
        (name) => html`<div key=${name} title=${name}><${Icon} name=${name} size=${48} /></div>`,
      )}
    </div>
  </${Card}>`;
}

// ------------------------------------------------------------------------------------ buttons

function ButtonsCard() {
  const [pressed, setPressed] = useState(true);
  return html`<${Card} title="Buttons · Kbd · chips">
    <div class="wrap">
      <${Button} variant="primary">Primary<//>
      <${Button}>Default<//>
      <${Button} variant="ghost">Ghost<//>
      <${Button} variant="danger" icon="trash">Delete<//>
      <${Button} disabled>Disabled<//>
    </div>
    <h3>Small</h3>
    <div class="wrap">
      <${Button} size="sm" variant="primary" icon="plus">New Space<//>
      <${Button} size="sm" icon="restore">Restore<//>
      <${Button} size="sm" variant="ghost" iconEnd="chevron-down">More<//>
    </div>
    <h3>Icon buttons (sm · md · lg · pressed · muted · disabled)</h3>
    <div class="wrap">
      <${IconButton} icon="close" label="Close" size="sm" />
      <${IconButton} icon="back" label="Back" />
      <${IconButton} icon="forward" label="Forward" disabled />
      <${IconButton} icon="reload" label="Reload" />
      <${IconButton} icon="sidebar" label="Toggle sidebar" pressed=${pressed} onClick=${() => setPressed(!pressed)} />
      <${IconButton} icon="more" label="More" muted />
      <${IconButton} icon="split" label="Split view" size="lg" />
    </div>
    <h3>Kbd and hint chips</h3>
    <div class="wrap">
      <${Kbd} keys="Ctrl+T" />
      <${Kbd} keys="Ctrl+Shift+K" />
      <${Kbd} keys="Ctrl++" />
      <${Kbd} keys=${['Alt', '1…9']} />
      <span class="chip">Switch to Tab</span>
      <span class="chip">↵</span>
    </div>
  </${Card}>`;
}

// ------------------------------------------------------------------------------------ forms

function FormsCard({ state }) {
  const [name, setName] = useState('Work');
  const [search, setSearch] = useState('cef views');
  const [notes, setNotes] = useState('Multiline text area\nwith two lines.');
  const [toggles, setToggles] = useState({ peek: true, suggestions: false });
  const [remember, setRemember] = useState(true);
  const engines = state.searchEngines.map((e) => ({ value: e.id, label: e.name }));
  return html`<${Card} title="Form controls">
    <div class="stack" style=${{ gap: '12px' }}>
      <${TextField} label="Space name" value=${name} onInput=${setName} hint="Shown in the sidebar and command bar" />
      <${TextField} value=${search} onInput=${setSearch} icon="search" clearable placeholder="Search history" aria-label="Search history" />
      <${TextField} label="Pinned URL" value="htp:/example" error="Enter a valid URL" onInput=${() => {}} />
      <${TextField} label="Notes" multiline value=${notes} onInput=${setNotes} />
      <${Select}
        label="Search engine"
        value=${state.settings.searchEngine}
        options=${engines}
        onChange=${(v) => dispatch({ type: 'updateSettings', patch: { searchEngine: v } })}
      />
      <div class="stack" style=${{ gap: '4px' }}>
        <${Toggle}
          label="Open links from pinned tabs in Peek"
          description="Cross-site links from Favorites and Pinned tabs open in a preview"
          checked=${toggles.peek}
          onChange=${(v) => setToggles({ ...toggles, peek: v })}
        />
        <${Toggle} label="Search suggestions" checked=${toggles.suggestions} onChange=${(v) => setToggles({ ...toggles, suggestions: v })} />
        <${Toggle} label="Disabled toggle" checked=${true} disabled onChange=${() => {}} />
      </div>
      <${Checkbox} label="Remember this decision" checked=${remember} onChange=${setRemember} />
    </div>
  </${Card}>`;
}

// ------------------------------------------------------------------------------------ feedback

function FeedbackCard() {
  return html`<${Card} title="Spinner · progress · audio">
    <div class="wrap" style=${{ gap: '16px' }}>
      <${Spinner} />
      <${Spinner} size=${20} />
      <${ProgressRing} value=${0.3} label="30%" />
      <${ProgressRing} value=${0.72} size=${28} stroke=${2.5} label="Downloading">
        <${Icon} name="download" size=${14} />
      <//>
      <${ProgressRing} value=${null} label="Working" />
      <span class="row" style=${{ gap: '6px' }} title="indicators.audio (moves only while the window is focused)">
        <${AudioBars} size=${14} label="Playing audio" />
        <span class="muted" style=${{ fontSize: '12px' }}>audio</span>
      </span>
    </div>
    <h3>Progress bars (35% · 80% · indeterminate · 2px bare)</h3>
    <div class="stack">
      <${ProgressBar} value=${0.35} label="35%" />
      <${ProgressBar} value=${0.8} label="80%" />
      <${ProgressBar} value=${null} label="Loading" />
      <${ProgressBar} value=${0.55} height=${2} bare label="Page load" />
    </div>
  </${Card}>`;
}

// ------------------------------------------------------------------------------------ favicons & rows

function FaviconsCard({ state }) {
  const tabs = allTabs(state);
  const withIcons = tabs.filter((t) => t.favicon).slice(0, 10);
  const hosts = ['notion.so', 'bank.example.com', 'weather.example.com', 'intranet.corp.example', 'cooking.example.org', 'ünïcode.example', ''];
  const space = activeSpaceOf(state);
  const rows = space.today.filter((n) => n.kind === 'tab').slice(0, 4);
  return html`<${Card} title="Favicons · sample rows">
    <h3>Fixture favicons (16 · 20 · 32)</h3>
    <div class="wrap">
      ${withIcons.map((t) => html`<${Favicon} key=${t.id} src=${t.favicon} host=${t.host} title=${t.host} />`)}
    </div>
    <div class="wrap" style=${{ marginTop: '8px' }}>
      ${withIcons.slice(0, 5).map((t) => html`<${Favicon} key=${t.id} src=${t.favicon} host=${t.host} size=${20} />`)}
      ${withIcons.slice(0, 3).map((t) => html`<${Favicon} key=${`b${t.id}`} src=${t.favicon} host=${t.host} size=${32} />`)}
    </div>
    <h3>Letter tiles (no favicon · broken src · dim)</h3>
    <div class="wrap">
      ${hosts.map((host) => html`<${Favicon} key=${host} host=${host} title=${`${host} ${hostLetterColor(host)}`} />`)}
      <${Favicon} src="data:image/png;base64,AAAA" host="broken.example" />
      <${Favicon} host="github.com" size=${20} />
      <${Favicon} src=${withIcons[0]?.favicon} host="dim.example" dim />
      <${Favicon} host="unloaded.example" dim size=${20} />
    </div>
    <h3>Rows on the sidebar gradient (active · hover · normal · unloaded)</h3>
    <div class="sidebar-sample">
      ${rows.map(
        (t, i) => html`<div key=${t.id} class=${['tab-row', t.active && 'is-active', i === 1 && 'is-hover', !t.loaded && 'is-unloaded'].filter(Boolean).join(' ')}>
          ${t.loading ? html`<${Spinner} label=${null} />` : html`<${Favicon} src=${t.favicon} host=${t.host} dim=${!t.loaded} />`}
          <span class="grow">${t.title}</span>
          ${t.audible && html`<${Icon} name="speaker" size=${14} />`}
          ${t.failed && html`<${Icon} name="warning" size=${14} label="Load failed" />`}
        </div>`,
      )}
    </div>
  </${Card}>`;
}

// ------------------------------------------------------------------------------------ menus & popovers

const tabMenu = (onPick) => [
  { type: 'header', label: 'Today tab' },
  { label: 'Copy URL', icon: 'copy', hint: 'Ctrl+Shift+C', onSelect: () => onPick('Copy URL') },
  { label: 'Rename', icon: 'edit', hint: 'F2', onSelect: () => onPick('Rename') },
  { label: 'Pin', icon: 'pin', hint: 'Ctrl+D', onSelect: () => onPick('Pin') },
  { label: 'Add to Favorites', icon: 'star', disabled: true },
  {
    label: 'Move to Space',
    icon: 'space',
    submenu: [
      { label: '🏠 Personal', onSelect: () => onPick('Move to Personal') },
      { label: '🎮 Play', onSelect: () => onPick('Move to Play') },
    ],
  },
  { label: 'Open in Split View', icon: 'split', onSelect: () => onPick('Split') },
  { label: 'Mute Tab', checked: true, onSelect: () => onPick('Mute') },
  { type: 'separator' },
  { label: 'Archive Tab', icon: 'archive', hint: 'Ctrl+W', danger: true, onSelect: () => onPick('Archive') },
];

function MenusCard({ state }) {
  const [picked, setPicked] = useState('—');
  const [menu, setMenu] = useState(null);
  const [anchored, setAnchored] = useState(false);
  const [popover, setPopover] = useState(false);
  const [emoji, setEmoji] = useState(activeSpaceOf(state)?.icon ?? '🚀');
  const menuButton = useRef(null);
  const popoverButton = useRef(null);
  const items = tabMenu(setPicked);
  return html`<${Card} title="Menus · popovers · emoji picker" span>
    <div class="row" style=${{ alignItems: 'flex-start', gap: '16px', flexWrap: 'wrap' }}>
      <div class="stack">
        <h3 style=${{ margin: 0 }}>Menu (inline render)</h3>
        <${Menu} inline label="Tab actions" items=${items} initialIndex=${2} onClose=${() => {}} />
      </div>
      <div class="stack">
        <h3 style=${{ margin: 0 }}>Submenu (inline)</h3>
        <${Menu}
          inline
          label="Move to Space"
          items=${[
            { type: 'header', label: 'Move to Space' },
            ...state.spaces.map((s) => ({ label: `${s.icon} ${s.name}`, checked: s.id === state.activeSpace })),
          ]}
          onClose=${() => {}}
        />
        <h3 style=${{ margin: '8px 0 0' }}>Popover (inline)</h3>
        <${Popover} open inline label="Downloads" style=${{ width: '300px' }}>
          <div class="stack" style=${{ gap: '0px' }}>
            ${state.downloads.slice(0, 2).map(
              (d) => html`<div class="download-row" key=${d.id}>
                <span class="row"><${Icon} name="download" size=${14} /><span class="grow ellipsis">${d.fileName}</span></span>
                <${ProgressBar} value=${downloadFraction(d)} height=${3} label=${d.fileName} />
                <span class="muted" style=${{ fontSize: '12px' }}>${describeDownload(d)}</span>
              </div>`,
            )}
          </div>
        <//>
      </div>
      <div class="stack">
        <h3 style=${{ margin: 0 }}>Live (click / right-click)</h3>
        <div class="wrap">
          <${Button} buttonRef=${menuButton} iconEnd="chevron-down" aria-expanded=${String(anchored)} onClick=${() => setAnchored(!anchored)}>Anchored menu<//>
          <${Button} buttonRef=${popoverButton} icon="palette" onClick=${() => setPopover(!popover)}>Popover<//>
        </div>
        <div
          class="context-area"
          onContextMenu=${(e) => {
            e.preventDefault();
            setMenu({ x: e.clientX, y: e.clientY });
          }}
        >Right-click here · last pick: ${picked}</div>
        <h3 style=${{ margin: '8px 0 0' }}>Emoji picker · selected ${emoji}</h3>
        <div class="card" style=${{ padding: '10px' }}>
          <${EmojiPicker} value=${emoji} onSelect=${setEmoji} label="Space icon" />
        </div>
      </div>
    </div>
    ${menu && html`<${Menu} x=${menu.x} y=${menu.y} items=${items} label="Tab actions" onClose=${() => setMenu(null)} />`}
    ${anchored &&
    html`<${Menu} anchor=${menuButton.current} items=${items} label="Tab actions" onClose=${() => setAnchored(false)} />`}
    <${Popover}
      open=${popover}
      anchor=${popoverButton.current}
      label="Theme"
      onClose=${() => setPopover(false)}
      style=${{ width: '280px' }}
    >
      <div class="stack">
        <strong>Popover</strong>
        <span class="muted">Closes on Esc or outside click; focus returns to the button.</span>
        <${TextField} value="Focus lands here" onInput=${() => {}} aria-label="Example" />
        <div class="row" style=${{ justifyContent: 'flex-end' }}>
          <${Button} size="sm" variant="primary" onClick=${() => setPopover(false)}>Done<//>
        </div>
      </div>
    <//>
  </${Card}>`;
}

// ------------------------------------------------------------------------------------ utilities

function UtilCard({ state }) {
  const now = Date.now();
  const rows = [
    ['formatBytes(0)', formatBytes(0)],
    ['formatBytes(1536)', formatBytes(1536)],
    ['formatBytes(134217728)', formatBytes(134217728)],
    ['formatBytes(432013312)', formatBytes(432013312)],
    ['formatBytes(5.5e9)', formatBytes(5.5e9)],
    ['formatSpeed(8805000)', formatSpeed(8805000)],
    ['formatDuration(3725)', formatDuration(3725)],
    ['relativeTime(now-20s)', relativeTime(now - 20_000)],
    ['relativeTime(now-5min)', relativeTime(now - 5 * 60_000)],
    ['relativeTime(now-26h)', relativeTime(now - 26 * 3_600_000)],
    ['relativeTime(now-12d)', relativeTime(now - 12 * 86_400_000)],
    ['dayLabel(now-3d)', dayLabel(now - 3 * 86_400_000)],
  ];
  return html`<${Card} title="util.js">
    <table class="util-table">
      <tbody>
        ${rows.map(([k, v]) => html`<tr key=${k}><td>${k}</td><td>${v}</td></tr>`)}
      </tbody>
    </table>
    <h3>describeDownload (fixture downloads)</h3>
    <div>
      ${state.downloads.map(
        (d) => html`<div class="download-row" key=${d.id}>
          <span class="row"><span class="grow ellipsis">${d.fileName}</span><span class="chip">${d.state}</span></span>
          <${ProgressBar} value=${d.state === 'inProgress' ? downloadFraction(d) : downloadFraction(d) ?? 0} height=${3} label=${d.fileName} />
          <span class="muted" style=${{ fontSize: '12px' }}>${describeDownload(d)}</span>
        </div>`,
      )}
    </div>
  </${Card}>`;
}

// ------------------------------------------------------------------------------------ motion

/**
 * Motion (`crates/sta-core/src/motion.rs`, FINAL PLAN §5): the level and off-keys this page is
 * running at, and a replay of the primitives in `motion.js` — which is the developer-facing
 * substitute for the per-key live previews the settings page deliberately does not have.
 */
function MotionCard() {
  const boxRef = useRef(null);
  const listRef = useRef(null);
  const [tick, setTick] = useState(0);
  const [order, setOrder] = useState([1, 2, 3, 4]);
  const stats = motion.stats();
  const replay = (key, frames, opts) => {
    motion.animate(boxRef.current, key, frames, opts);
    setTick((t) => t + 1);
  };
  const shuffle = () => {
    const capture = motion.flip.capture(listRef.current, 'sidebar.reorder', '[data-flip]');
    setOrder((o) => [...o.slice(1), o[0]]);
    // The DOM has to be the new one before `play` measures it.
    requestAnimationFrame(() => capture?.play());
    setTick((t) => t + 1);
  };
  return html`<${Card} title="motion.js">
    <div class="row" style=${{ flexWrap: 'wrap' }}>
      <span class="chip">level ${stats.level}</span>
      <span class="chip">${stats.off.length} of ${ANIMATION_GROUPS.reduce((n, g) => n + g.keys.length, 0)} keys off</span>
      <span class="chip">presented ${String(stats.presented)}</span>
      <span class="chip">started ${stats.started}</span>
      <span class="chip">flips ${stats.flips}</span>
      <span class="chip">ghosts ${stats.ghosts}</span>
    </div>
    <div class="row" style=${{ flexWrap: 'wrap', marginTop: '10px' }}>
      <${Button}
        size="sm"
        onClick=${() =>
          replay('overlays.toast', [{ opacity: 0, translate: `0 ${motion.distance(4)}px` }, { opacity: 1, translate: 'none' }], {
            duration: motion.duration('overlays.toast', 180),
          })}
        >rise<//
      >
      <${Button}
        size="sm"
        onClick=${() =>
          replay('sidebar.favorites', [{ scale: 0.92 }, { scale: 1 }], {
            duration: motion.duration('sidebar.favorites', 140),
            easing: motion.EASE_SPRING,
          })}
        >spring<//
      >
      <${Button} size="sm" onClick=${shuffle}>FLIP<//>
      <${Button}
        size="sm"
        onClick=${() => {
          const clone = motion.ghost(boxRef.current);
          motion.fadeGhost(clone, 'sidebar.tabInsertRemove', [{ opacity: 1 }, { opacity: 0, translate: `0 ${motion.distance(8)}px` }], {
            duration: motion.duration('sidebar.tabInsertRemove', 180),
          });
          setTick((t) => t + 1);
        }}
        >ghost out<//
      >
      <${Button} size="sm" variant="ghost" onClick=${() => { motion.finishAll(); setTick((t) => t + 1); }}>finishAll<//>
    </div>
    <div ref=${boxRef} class="motion-demo" aria-hidden="true">replay #${tick}</div>
    <div ref=${listRef} class="motion-flip">
      ${order.map((n) => html`<span key=${n} data-flip=${n} class="chip">${n}</span>`)}
    </div>
  </${Card}>`;
}

// ------------------------------------------------------------------------------------ page

function Gallery({ state }) {
  const space = activeSpaceOf(state);
  return html`<main class="gallery">
    <header class="gallery-header">
      <${Icon} name="space" size=${24} />
      <h1>sta UI gallery</h1>
      <span class="chip">${isMock ? 'mock' : 'live'} · rev ${state.revision}</span>
      <span class="grow" />
      <span class="muted">${space?.icon} ${space?.name}</span>
      <${IconButton}
        icon=${state.dark ? 'sun' : 'moon'}
        label=${state.dark ? 'Switch to light' : 'Switch to dark'}
        onClick=${() => dispatch({ type: 'updateSettings', patch: { appearance: state.dark ? 'light' : 'dark' } })}
      />
    </header>
    <div class="gallery-grid">
      <${ThemeCard} state=${state} />
      <${IconsCard} />
      <${ButtonsCard} />
      <${FormsCard} state=${state} />
      <${FeedbackCard} />
      <${FaviconsCard} state=${state} />
      <${MenusCard} state=${state} />
      <${UtilCard} state=${state} />
      <${MotionCard} />
    </div>
  </main>`;
}

startSurface({ render: (state) => render(html`<${Gallery} state=${state} />`, document.body) });
