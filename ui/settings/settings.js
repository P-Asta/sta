// Settings page (sta://settings/). Every control reads `state.settings` and writes a
// `updateSettings {patch}` with only the changed field; the next state push re-renders it.

import { html, useEffect, useRef, useState } from '/common/vendor/htm-preact.js';
import { dispatch, invoke } from '/common/ipc.js';
import { useRequest } from '/common/ipc-hooks.js';
import { AppMark, Icon } from '/common/icons.js';
import { Button, IconButton, Select, TextField, Toggle } from '/common/components.js';
import { Segmented, mountPage, useNavIndicator } from '/common/internal-page.js';
import * as motion from '/common/motion.js';
import { classNames, IS_MAC } from '/common/util.js';
import { AgentsSection } from './agents.js';
import { AnimationsSection } from './animations.js';
import { ExtensionsSection } from './extensions.js';

const report = (e) => console.error('[settings]', e);
const send = (command) => dispatch(command).catch(report);
const update = (patch) => send({ type: 'updateSettings', patch });

const SECTIONS = [
  { id: 'general', label: 'General', icon: 'home' },
  { id: 'appearance', label: 'Appearance', icon: 'palette' },
  { id: 'animations', label: 'Animations', icon: 'play' },
  { id: 'search', label: 'Search', icon: 'search' },
  { id: 'tabs', label: 'Tabs', icon: 'archive' },
  { id: 'downloads', label: 'Downloads', icon: 'download' },
  { id: 'boosts', label: 'Boosts', icon: 'boost' },
  { id: 'extensions', label: 'Extensions', icon: 'puzzle' },
  { id: 'agents', label: 'AI agents', icon: 'agent' },
  { id: 'about', label: 'About', icon: 'info' },
];

const ARCHIVE_OPTIONS = [
  { value: 12, label: '12 hours' },
  { value: 24, label: '24 hours' },
  { value: 168, label: '7 days' },
  { value: 720, label: '30 days' },
];

/**
 * Engines without a suggestion service (core's `omnibox::suggest_url` returns `None`): the command
 * bar shows no remote suggestions while one of them is selected.
 */
const ENGINES_WITHOUT_SUGGESTIONS = new Set(['kagi', 'perplexity', 'custom']);

const STARTUP_OPTIONS = [
  { value: 'restoreSession', label: 'Restore previous session' },
  { value: 'newTab', label: 'Open a new tab' },
];

/** A custom search template must be an http(s) URL with a `{q}` (or `%s`) placeholder. */
export function validateSearchTemplate(template) {
  const t = String(template ?? '').trim();
  if (!t) return 'Enter a URL';
  if (!/^https?:\/\/[^\s/?#]+[^\s]*$/i.test(t)) return 'Must be an http:// or https:// URL';
  if (!t.includes('{q}') && !t.includes('%s')) return 'Add {q} where the search terms go';
  return null;
}

// ------------------------------------------------------------------------------------ building blocks

function Section({ id, title, children }) {
  return html`<section id=${id} class="set-section" aria-labelledby=${`${id}-title`}>
    <h2 class="set-section-title" id=${`${id}-title`}>${title}</h2>
    <div class="ip-card">${children}</div>
  </section>`;
}

function Setting({ label, desc, children, stacked = false, labelId }) {
  return html`<div class=${classNames('ip-setting', stacked && 'is-stacked')}>
    <div class="ip-setting-text">
      <span class="ip-setting-label" id=${labelId}>${label}</span>
      ${desc && html`<span class="ip-setting-desc">${desc}</span>`}
    </div>
    ${children && html`<div class="ip-setting-control">${children}</div>`}
  </div>`;
}

// ------------------------------------------------------------------------------------ appearance

const APPEARANCES = [
  { value: 'system', label: 'System' },
  { value: 'light', label: 'Light' },
  { value: 'dark', label: 'Dark' },
];

function Preview({ kind }) {
  return html`<span class=${`set-preview is-${kind}`} aria-hidden="true">
    <span class="set-preview-pane is-light">
      <span class="pv-side"><i class="pv-pill" /><i class="pv-row is-active" /><i class="pv-row" /><i class="pv-row" /></span>
      <span class="pv-page"><i class="pv-line is-wide" /><i class="pv-line" /><i class="pv-line is-short" /></span>
    </span>
    <span class="set-preview-pane is-dark">
      <span class="pv-side"><i class="pv-pill" /><i class="pv-row is-active" /><i class="pv-row" /><i class="pv-row" /></span>
      <span class="pv-page"><i class="pv-line is-wide" /><i class="pv-line" /><i class="pv-line is-short" /></span>
    </span>
  </span>`;
}

function AppearancePicker({ value }) {
  const onKeyDown = (e) => {
    const i = APPEARANCES.findIndex((a) => a.value === value);
    const delta = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0;
    if (!delta) return;
    e.preventDefault();
    const next = APPEARANCES[Math.min(APPEARANCES.length - 1, Math.max(0, i + delta))];
    if (next.value !== value) update({ appearance: next.value });
    e.currentTarget.querySelectorAll('[role="radio"]')[APPEARANCES.indexOf(next)]?.focus();
  };
  return html`<div class="set-appearance" role="radiogroup" aria-label="Appearance" onKeyDown=${onKeyDown}>
    ${APPEARANCES.map(
      (a) => html`<button
        key=${a.value}
        type="button"
        role="radio"
        class="set-appearance-option"
        aria-checked=${String(value === a.value)}
        tabindex=${value === a.value ? 0 : -1}
        onClick=${() => value !== a.value && update({ appearance: a.value })}
      >
        <${Preview} kind=${a.value} />
        <span class="set-appearance-label">
          <span class="set-radio" aria-hidden="true" />
          ${a.label}
        </span>
      </button>`,
    )}
  </div>`;
}

/* The sidebar's width, 200–440 px (`sta-core/src/model.rs SIDEBAR_*_WIDTH`). The sidebar has a drag
 * handle on its right edge too; this is the control people can *find*. It follows a drag made there
 * (the value comes from `state.window`), and while it is being dragged itself it sends at most one
 * command per frame — every one of them lays the whole window out again. */
const SIDEBAR_WIDTH = { min: 200, max: 440, def: 248 };

function SidebarWidth({ value }) {
  const [draft, setDraft] = useState(value);
  const dragging = useRef(false);
  const frame = useRef(0);
  const pending = useRef(null);
  useEffect(() => {
    if (!dragging.current) setDraft(value);
  }, [value]);
  useEffect(() => () => cancelAnimationFrame(frame.current), []);
  const queue = (width) => {
    pending.current = width;
    if (frame.current) return;
    frame.current = requestAnimationFrame(() => {
      frame.current = 0;
      if (pending.current != null) send({ type: 'setSidebarWidth', width: pending.current });
      pending.current = null;
    });
  };
  const release = () => {
    dragging.current = false;
  };
  return html`<div class="set-range">
    <input
      type="range"
      min=${SIDEBAR_WIDTH.min}
      max=${SIDEBAR_WIDTH.max}
      step="4"
      value=${draft}
      aria-label="Sidebar width"
      aria-valuetext=${`${draft} pixels`}
      onPointerDown=${() => {
        dragging.current = true;
      }}
      onPointerUp=${release}
      onPointerCancel=${release}
      onBlur=${release}
      onInput=${(e) => {
        const width = Number(e.currentTarget.value);
        setDraft(width);
        queue(width);
      }}
    />
    <span class="set-range-value">${draft} px</span>
    <${Button} size="sm" variant="ghost" disabled=${draft === SIDEBAR_WIDTH.def} onClick=${() => {
      setDraft(SIDEBAR_WIDTH.def);
      queue(SIDEBAR_WIDTH.def);
    }}>Reset<//>
  </div>`;
}

// ------------------------------------------------------------------------------------ search

function CustomSearchUrl({ value }) {
  const [draft, setDraft] = useState(value);
  const [touched, setTouched] = useState(false);
  const focused = useRef(false);
  useEffect(() => {
    if (!focused.current) setDraft(value);
  }, [value]);
  const error = validateSearchTemplate(draft);
  const commit = (text) => {
    const t = text.trim();
    setTouched(true);
    if (!validateSearchTemplate(t) && t !== value) update({ customSearchUrl: t });
  };
  return html`<${TextField}
    class="set-custom-url"
    label="Search URL"
    value=${draft}
    placeholder="https://search.example.com/?q={q}"
    spellcheck=${false}
    error=${touched && draft.trim() !== '' && error ? error : undefined}
    hint=${!value ? 'Until a valid URL is saved, searches use Google.' : 'Use {q} where the search terms go.'}
    onFocus=${() => {
      focused.current = true;
    }}
    onBlur=${(e) => {
      focused.current = false;
      commit(e.currentTarget.value);
    }}
    onInput=${(v) => {
      setDraft(v);
      if (!validateSearchTemplate(v)) setTouched(false);
    }}
    onCommit=${commit}
  />`;
}

function SearchSection({ settings, engines }) {
  const current = engines.find((e) => e.id === settings.searchEngine);
  const options = engines.map((e) => ({ value: e.id, label: e.name }));
  return html`<${Section} id="search" title="Search">
    <${Setting}
      label="Search engine"
      labelId="engine-label"
      desc=${settings.searchEngine === 'custom' ? 'Your own search URL' : current?.url && html`<span class="set-template">${current.url}</span>`}
    >
      <div class="set-select">
        <${Select} value=${settings.searchEngine} options=${options} ariaLabel="Search engine" onChange=${(v) => update({ searchEngine: v })} />
      </div>
    <//>
    ${settings.searchEngine === 'custom' &&
    html`<div class="ip-setting is-stacked set-custom">
      <${CustomSearchUrl} value=${settings.customSearchUrl ?? ''} />
    </div>`}
    <${SuggestionsSetting} settings=${settings} engine=${current} />
  <//>`;
}

function SuggestionsSetting({ settings, engine }) {
  const custom = settings.searchEngine === 'custom';
  const name = custom ? 'your search engine' : (engine?.name ?? 'your search engine');
  const unsupported = ENGINES_WITHOUT_SUGGESTIONS.has(settings.searchEngine);
  const desc = unsupported
    ? html`<span class="set-suggest-desc">Complete searches and suggest queries from your search engine while you type in the command bar.</span>
        <span class="set-note" role="note">
          <${Icon} name="info" size=${14} />
          <span>${custom ? 'A custom search engine' : name} doesn't offer suggestions: none are shown, and nothing you type is sent.</span>
        </span>`
    : html`<span class="set-suggest-desc"
        >Complete searches and suggest queries while you type in the command bar. To get them, what you type is sent
        to ${name} (not addresses like <span class="set-code">https://…</span> or file paths).</span
      >`;
  return html`<${Setting} label="Search suggestions" desc=${desc}>
    <${Toggle} checked=${settings.searchSuggestions === true} ariaLabel="Search suggestions" onChange=${(v) => update({ searchSuggestions: v })} />
  <//>`;
}

// ------------------------------------------------------------------------------------ downloads

function DownloadsSection({ settings, info }) {
  const custom = settings.downloadDir != null && settings.downloadDir !== '';
  const path = custom ? settings.downloadDir : (info?.downloadDir ?? '');
  const pick = async () => {
    try {
      const dir = await invoke('dialog.pickFolder');
      if (typeof dir === 'string' && dir) update({ downloadDir: dir });
    } catch (e) {
      report(e);
    }
  };
  return html`<${Section} id="downloads" title="Downloads">
    <${Setting}
      label="Download location"
      desc=${html`<span class="set-path" title=${path}>${path ? html`<${Icon} name="folder" size=${13} /><span class="mono-path">${path}</span>` : 'Your Downloads folder'}</span>
        ${!custom && path && html`<span class="set-default">Default</span>`}`}
    >
      ${custom && html`<${Button} variant="ghost" size="sm" onClick=${() => update({ downloadDir: '' })}>Reset<//>`}
      <${Button} size="sm" icon="folder-open" onClick=${pick}>Change…<//>
    <//>
    <${Setting} label="Ask where to save each file" desc="Choose a folder and file name every time a download starts.">
      <${Toggle} checked=${settings.askDownloadLocation} ariaLabel="Ask where to save each file" onChange=${(v) => update({ askDownloadLocation: v })} />
    <//>
  <//>`;
}

// ------------------------------------------------------------------------------------ boosts

function BoostsSection({ boosts }) {
  const edit = (id) => send({ type: 'openUrl', url: `sta://boosts/?id=${id}`, target: 'newTab' });
  return html`<section id="boosts" class="set-section" aria-labelledby="boosts-title">
    <div class="set-section-head">
      <h2 class="set-section-title" id="boosts-title">Boosts</h2>
      <${Button} variant="ghost" size="sm" iconEnd="chevron-right" onClick=${() => send({ type: 'openInternalPage', page: 'boosts' })}>Manage boosts<//>
    </div>
    <div class="ip-card">
      ${boosts.length === 0
        ? html`<div class="set-boosts-empty">
            <${Icon} name="boost" size=${18} />
            <span>No boosts yet. Open a site and choose <b>New Boost for this Site</b> in the command bar to restyle it with your own CSS and JavaScript.</span>
          </div>`
        : boosts.map(
            (b) => html`<div key=${b.id} class="ip-setting set-boost">
              <span class=${classNames('set-boost-icon', !b.enabled && 'is-off')} aria-hidden="true"><${Icon} name="boost" size=${16} /></span>
              <div class="ip-setting-text">
                <span class="ip-setting-label">${b.name || b.host}</span>
                <span class="ip-setting-desc">${b.host}</span>
              </div>
              <div class="ip-setting-control">
                <${IconButton} icon="edit" label=${`Edit ${b.name || b.host}`} muted onClick=${() => edit(b.id)} />
                <${Toggle} checked=${b.enabled} ariaLabel=${`${b.name || b.host} enabled`} onChange=${() => send({ type: 'toggleBoost', id: b.id })} />
              </div>
            </div>`,
          )}
    </div>
  </section>`;
}

// ------------------------------------------------------------------------------------ about

/**
 * The update line in About: what the shell last reported (`UiState.update`, `sta_core::update`)
 * and the one thing that can be done about it. The shell checks by itself a few seconds after
 * startup; everything here is the follow-up.
 */
function UpdateSetting({ update }) {
  const stage = update?.stage ?? 'idle';
  const percent = stage === 'downloading' && update.total > 0 ? Math.round((update.received / update.total) * 100) : null;
  const mb = (bytes) => `${(bytes / 1024 / 1024).toFixed(0)} MB`;
  const desc = {
    idle: 'sta checks for a new version shortly after it starts.',
    checking: 'Checking for updates…',
    upToDate: 'sta is up to date.',
    available: `sta ${update?.version} is available${update?.size ? ` (${mb(update.size)})` : ''}.`,
    downloading: percent === null ? 'Downloading…' : `Downloading ${update.version}… ${percent}%`,
    ready: `sta ${update?.version} is ready. It is installed when you restart.`,
    failed: update?.message || 'The update could not be fetched.',
  }[stage];
  const button = {
    idle: { label: 'Check now', command: 'checkForUpdate' },
    upToDate: { label: 'Check again', command: 'checkForUpdate' },
    available: { label: 'Download', command: 'downloadUpdate' },
    ready: { label: 'Restart to update', command: 'installUpdate' },
    failed: { label: 'Try again', command: 'checkForUpdate' },
  }[stage];
  return html`<${Setting} label="Updates" desc=${desc}>
    ${stage === 'available' && update?.notes
      ? html`<span class="set-update-notes" title=${update.notes}>What's new</span>`
      : null}
    ${button && html`<${Button} onClick=${() => send({ type: button.command })}>${button.label}<//>`}
  <//>`;
}

function AboutSection({ info, error, update }) {
  const copy = (text) => send({ type: 'copyText', text });
  return html`<${Section} id="about" title="About">
    <div class="ip-setting set-about-head">
      <span class="set-logo" aria-hidden="true">
        <${AppMark} />
      </span>
      <div class="ip-setting-text">
        <span class="set-app-name">sta</span>
        <span class="ip-setting-desc">${info ? `Version ${info.version}` : error ? 'Version information unavailable' : 'Loading…'}</span>
      </div>
    </div>
    <${UpdateSetting} update=${update} />
    ${info &&
    html`<${Setting} label="Chromium" desc=${html`<span class="mono-path selectable">${info.chromiumVersion}</span>`} />
      <${Setting} label="CEF" desc=${html`<span class="mono-path selectable">${info.cefVersion}</span>`} />
      <${Setting} label="Profile folder" desc=${html`<span class="mono-path selectable" title=${info.dataDir ?? ''}>${info.dataDir ?? '—'}</span>`}>
        ${info.dataDir && html`<${IconButton} icon="copy" label="Copy path" muted onClick=${() => copy(info.dataDir)} />`}
      <//>`}
  <//>`;
}

// ------------------------------------------------------------------------------------ page

function useActiveSection() {
  const [active, setActive] = useState(SECTIONS[0].id);
  useEffect(() => {
    let frame = 0;
    const measure = () => {
      frame = 0;
      let current = SECTIONS[0].id;
      for (const s of SECTIONS) {
        const el = document.getElementById(s.id);
        if (el && el.getBoundingClientRect().top <= 120) current = s.id;
      }
      if (window.scrollY > 0 && window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 2) current = SECTIONS[SECTIONS.length - 1].id;
      setActive((a) => (a === current ? a : current));
    };
    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(measure);
    };
    measure();
    window.addEventListener('scroll', onScroll, { passive: true });
    window.addEventListener('resize', onScroll);
    return () => {
      window.removeEventListener('scroll', onScroll);
      window.removeEventListener('resize', onScroll);
      cancelAnimationFrame(frame);
    };
  }, []);
  return [active, setActive];
}

function jumpTo(id, setActive) {
  const el = document.getElementById(id);
  if (!el) return;
  setActive(id);
  history.replaceState(null, '', `#${id}`);
  // `controls.smoothScroll`: `motion.scrollBehavior()` answers for the level *and* the key, where
  // this used to ask `prefers-reduced-motion` directly (once core speaks about motion, core decides).
  el.scrollIntoView({ behavior: motion.scrollBehavior(), block: 'start' });
}

let initialHashHandled = false;

function Settings({ state }) {
  const settings = state.settings;
  const [active, setActive] = useActiveSection();
  const info = useRequest('app.info', null, [settings.downloadDir ?? '']);
  const navRef = useRef(null);
  const navIndicator = useRef(null);
  // `pages.navIndicator`: the tint behind the current section glides between links as the page
  // scrolls past a heading, instead of jumping from one to the next.
  useNavIndicator(navRef, navIndicator, '.set-nav-link.is-active');

  useEffect(() => {
    if (initialHashHandled) return;
    initialHashHandled = true;
    // `#<section>`, or `?section=<section>` — which is how the extensions picker links here
    // (`sta://settings/?section=extensions&ext=<id>`), because the query also carries the row.
    let id = decodeURIComponent(location.hash.slice(1));
    if (!id) {
      try {
        id = new URLSearchParams(location.search).get('section') ?? '';
      } catch {
        id = '';
      }
    }
    if (SECTIONS.some((s) => s.id === id)) requestAnimationFrame(() => document.getElementById(id)?.scrollIntoView({ block: 'start' }));
  }, []);

  return html`<div class="set">
    <nav class="set-nav" aria-label="Settings sections">
      <div class="set-nav-title">
        <span class="ip-header-icon" aria-hidden="true"><${Icon} name="settings" size=${20} /></span>
        <h1 class="ip-title">Settings</h1>
      </div>
      <ul class="set-nav-list" ref=${navRef}>
        <li key="indicator" class="ip-nav-indicator" ref=${navIndicator} aria-hidden="true" />
        ${SECTIONS.map(
          (s) => html`<li key=${s.id}>
            <a
              href=${`#${s.id}`}
              class=${classNames('set-nav-link', active === s.id && 'is-active')}
              aria-current=${active === s.id ? 'true' : undefined}
              onClick=${(e) => {
                e.preventDefault();
                jumpTo(s.id, setActive);
              }}
            ><${Icon} name=${s.icon} size=${16} />${s.label}</a>
          </li>`,
        )}
      </ul>
    </nav>

    <main class="set-main">
      <${Section} id="general" title="General">
        <${Setting} label="On startup" desc="What sta shows when it starts. Your sidebar is always restored.">
          <div class="set-select is-wide">
            <${Select} value=${settings.startup} options=${STARTUP_OPTIONS} ariaLabel="On startup" onChange=${(v) => update({ startup: v })} />
          </div>
        <//>
      <//>

      <${Section} id="appearance" title="Appearance">
        <div class="ip-setting is-stacked">
          <div class="ip-setting-text">
            <span class="ip-setting-label">Theme</span>
            <span class="ip-setting-desc"
              >System follows the ${IS_MAC ? 'macOS' : 'Windows'} light or dark mode setting. Applies to every space.</span
            >
          </div>
          <${AppearancePicker} value=${settings.appearance} />
        </div>
        <div class="ip-setting is-stacked">
          <div class="ip-setting-text">
            <span class="ip-setting-label">Sidebar width</span>
            <span class="ip-setting-desc">You can also drag the sidebar's right edge; double-click it to reset.</span>
          </div>
          <${SidebarWidth} value=${state.window?.sidebarWidth ?? SIDEBAR_WIDTH.def} />
        </div>
      <//>

      <${AnimationsSection} state=${state} />

      <${SearchSection} settings=${settings} engines=${state.searchEngines ?? []} />

      <${Section} id="tabs" title="Tabs">
        <div class="ip-setting is-stacked">
          <div class="ip-setting-text">
            <span class="ip-setting-label">Archive Today tabs after</span>
            <span class="ip-setting-desc">Tabs in Today you haven't viewed for this long move to the Archive. Pinned tabs, Favorites and tabs playing audio are never archived.</span>
          </div>
          <div>
            <${Segmented}
              label="Archive Today tabs after"
              value=${settings.archiveAfterHours}
              options=${ARCHIVE_OPTIONS}
              onChange=${(v) => update({ archiveAfterHours: v })}
            />
          </div>
        </div>
        <${Setting} label="Open links in Peek" desc="Links from Pinned tabs and Favorites to other sites, and sign-in popups, open in a Peek preview.">
          <${Toggle} checked=${settings.peekEnabled} ariaLabel="Open links in Peek" onChange=${(v) => update({ peekEnabled: v })} />
        <//>
        <${Setting} label="Archive" desc=${`${state.archiveCount ?? 0} archived tab${state.archiveCount === 1 ? '' : 's'} from the last 30 days.`}>
          <${Button} size="sm" icon="archive" onClick=${() => send({ type: 'openInternalPage', page: 'archive' })}>View Archive<//>
        <//>
      <//>

      <${DownloadsSection} settings=${settings} info=${info.data} />

      <${BoostsSection} boosts=${state.boosts ?? []} />

      <${ExtensionsSection} state=${state} />

      <${AgentsSection} state=${state} />

      <${AboutSection} info=${info.data} error=${info.error} update=${state.update} />
    </main>
  </div>`;
}

mountPage(Settings);
