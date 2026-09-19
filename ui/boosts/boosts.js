// Boosts page (sta://boosts/, arc_spec §2.22). Left: every boost (`state.boosts` summaries)
// with a "New boost" action. Right: the editor for `?id=N`, loaded with `boosts.get {id}`:
// name, site host, enabled toggle (applies at once with `toggleBoost`), CSS and JavaScript code
// fields (Tab indents; Esc then Tab leaves the field). Save (Ctrl+S) sends `upsertBoost`; unsaved
// edits are also saved when switching boosts or leaving the tab. Delete → `deleteBoost`.

import { html, useEffect, useRef, useState } from '/common/vendor/htm-preact.js';
import { dispatch, invoke } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { Button, Popover, TextField, Toggle } from '/common/components.js';
import { useLatest } from '/common/hooks.js';
import { ConfirmButton, EmptyState, PageHeader, mountPage, useNavIndicator } from '/common/internal-page.js';
import * as motion from '/common/motion.js';
import { classNames, shortcut } from '/common/util.js';

const report = (e) => console.error('[boosts]', e);
const send = (command) => dispatch(command).catch(report);
/** `pages.boostsEditor`: the editor crosses over when another boost is selected. */
const EDITOR_KEY = 'pages.boostsEditor';

/**
 * Boosts already fetched for a pending switch, so the editor's **first** render has its content.
 * A View Transition captures the old frame, runs its update callback and captures the new one — all
 * synchronously — so a switch that still had to wait for `boosts.get` would either freeze rendering
 * until the IPC returned or cross-fade into an empty editor. The fetch therefore happens first and
 * the transition only swaps what is already in hand.
 */
const prefetched = new Map();

// ------------------------------------------------------------------------------------ helpers

function readId() {
  const raw = new URLSearchParams(location.search).get('id');
  const n = Number(raw);
  return raw && Number.isSafeInteger(n) && n > 0 ? n : null;
}

function writeId(id, { replace = false } = {}) {
  const params = new URLSearchParams(location.search);
  if (id == null) params.delete('id');
  else params.set('id', String(id));
  const qs = params.toString();
  history[replace ? 'replaceState' : 'pushState'](null, '', `${location.pathname}${qs ? `?${qs}` : ''}${location.hash}`);
}

/** `https://www.Example.com/path` → `example.com` (core normalizes the same way). */
export function normalizeHost(input) {
  return String(input ?? '')
    .trim()
    .toLowerCase()
    .replace(/^[a-z][a-z0-9+.-]*:\/\//, '')
    .split(/[/?#]/)[0]
    .replace(/^www\./, '');
}

export function hostError(input) {
  const host = normalizeHost(input);
  if (!host) return 'Enter a site, like example.com';
  if (/[\s@]/.test(host) || host.startsWith('.') || host.endsWith('.')) return 'Enter a host name, like example.com';
  return null;
}

const editable = (b) => ({ name: b.name ?? '', host: b.host ?? '', css: b.css ?? '', js: b.js ?? '' });
const sameEdit = (a, b) => a.name === b.name && a.host === b.host && a.css === b.css && a.js === b.js;

// ------------------------------------------------------------------------------------ code field

/** Leading whitespace of the line containing `pos`. */
function lineIndent(text, pos) {
  const start = text.lastIndexOf('\n', pos - 1) + 1;
  return /^[ \t]*/.exec(text.slice(start))[0];
}

function insertText(ta, text) {
  // execCommand keeps the textarea's undo stack; fall back to setRangeText.
  if (!document.execCommand('insertText', false, text)) {
    ta.setRangeText(text, ta.selectionStart, ta.selectionEnd, 'end');
    ta.dispatchEvent(new Event('input', { bubbles: true }));
  }
}

function indentLines(ta, outdent) {
  const value = ta.value;
  const selStart = ta.selectionStart;
  const selEnd = ta.selectionEnd;
  const blockStart = value.lastIndexOf('\n', selStart - 1) + 1;
  const endsAtLineStart = selEnd > selStart && value[selEnd - 1] === '\n';
  let blockEnd = value.indexOf('\n', endsAtLineStart ? selEnd - 1 : selEnd);
  if (blockEnd < 0) blockEnd = value.length;
  const lines = value.slice(blockStart, blockEnd).split('\n');
  let firstDelta = 0;
  let total = 0;
  const next = lines.map((line, i) => {
    if (outdent) {
      const n = line.startsWith('  ') ? 2 : line.startsWith('\t') || line.startsWith(' ') ? 1 : 0;
      if (i === 0) firstDelta = -n;
      total -= n;
      return line.slice(n);
    }
    if (i === 0) firstDelta = 2;
    total += 2;
    return `  ${line}`;
  });
  ta.setSelectionRange(blockStart, blockEnd);
  insertText(ta, next.join('\n'));
  ta.setSelectionRange(Math.max(blockStart, selStart + firstDelta), Math.max(blockStart, selEnd + total));
}

function CodeField({ id, label, language, hint, value, onInput, placeholder }) {
  const escaped = useRef(false);
  const lines = value ? value.split('\n').length : 0;
  const onKeyDown = (e) => {
    const ta = e.currentTarget;
    if (e.key === 'Escape') {
      escaped.current = true;
      return;
    }
    const wasEscaped = escaped.current;
    escaped.current = false;
    if (e.key === 'Tab' && !e.ctrlKey && !e.altKey && !e.metaKey) {
      if (wasEscaped) return; // Esc, Tab: move focus as usual
      e.preventDefault();
      const multiline = ta.value.slice(ta.selectionStart, ta.selectionEnd).includes('\n');
      if (e.shiftKey || multiline) indentLines(ta, e.shiftKey);
      else insertText(ta, '  ');
    } else if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.altKey && !e.isComposing) {
      const indent = lineIndent(ta.value, ta.selectionStart);
      const before = ta.value.slice(0, ta.selectionStart).trimEnd();
      const extra = /[{([]$/.test(before) ? '  ' : '';
      if (indent || extra) {
        e.preventDefault();
        insertText(ta, `\n${indent}${extra}`);
      }
    }
  };
  return html`<section class="bst-code" aria-labelledby=${`${id}-label`}>
    <header class="bst-code-head">
      <span class="bst-code-label" id=${`${id}-label`}>${label}</span>
      <span class="bst-code-lang">${language}</span>
      <span class="bst-code-hint">${hint}</span>
      <span class="bst-code-lines">${lines ? `${lines} line${lines === 1 ? '' : 's'}` : ''}</span>
    </header>
    <textarea
      id=${id}
      class="bst-textarea"
      value=${value}
      placeholder=${placeholder}
      spellcheck=${false}
      autocomplete="off"
      autocapitalize="off"
      wrap="off"
      aria-labelledby=${`${id}-label`}
      aria-describedby=${`${id}-hint`}
      onInput=${(e) => onInput(e.currentTarget.value)}
      onKeyDown=${onKeyDown}
    />
    <span class="sr-only" id=${`${id}-hint`}>Tab inserts spaces. Press Escape, then Tab, to move focus out of the editor.</span>
  </section>`;
}

// ------------------------------------------------------------------------------------ new boost

function NewBoostButton({ onCreate, variant = 'primary', label = 'New boost' }) {
  const [open, setOpen] = useState(false);
  const [host, setHost] = useState('');
  const [touched, setTouched] = useState(false);
  const anchor = useRef(null);
  const error = hostError(host);
  const create = () => {
    setTouched(true);
    if (error) return;
    onCreate(normalizeHost(host));
    setOpen(false);
    setHost('');
    setTouched(false);
  };
  return html`<${Button} buttonRef=${anchor} variant=${variant} icon="plus" aria-haspopup="dialog" aria-expanded=${String(open)} onClick=${() => setOpen((o) => !o)}>${label}<//>
    <${Popover} open=${open} anchor=${anchor.current} placement="bottom-end" label="New boost" class="bst-new" onClose=${() => setOpen(false)}>
      <div class="bst-new-title">New boost</div>
      <p class="bst-new-text">Pick the site to customize. The boost also applies to its subdomains.</p>
      <${TextField}
        label="Site"
        value=${host}
        placeholder="example.com"
        autoFocus
        error=${touched && error ? error : undefined}
        onInput=${(v) => {
          setHost(v);
          setTouched(false);
        }}
        onCommit=${create}
      />
      <div class="ip-confirm-actions">
        <${Button} size="sm" variant="ghost" onClick=${() => setOpen(false)}>Cancel<//>
        <${Button} size="sm" variant="primary" onClick=${create}>Create<//>
      </div>
    <//>`;
}

// ------------------------------------------------------------------------------------ editor

function Editor({ id, summary, controller, onDeleted }) {
  const ready = prefetched.get(id) ?? null;
  const [base, setBase] = useState(ready);
  const [draft, setDraft] = useState(ready ? editable(ready) : null);
  const [missing, setMissing] = useState(false);
  const [status, setStatus] = useState(null);
  const [hostTouched, setHostTouched] = useState(false);
  const latest = useLatest({ base, draft, summary });
  const summaryKey = JSON.stringify([summary.name, summary.host]);

  useEffect(() => {
    let cancelled = false;
    // The prefetch is the editor's *first* frame; from here on the live request is the truth.
    prefetched.delete(id);
    invoke('boosts.get', { id }).then(
      (boost) => {
        if (cancelled) return;
        if (!boost) {
          setMissing(true);
          return;
        }
        const fresh = editable(boost);
        const { base: prevBase, draft: prevDraft } = latest.current;
        setBase(boost);
        // Keep unsaved edits when the summary changed underneath (e.g. saved elsewhere).
        if (!prevDraft || !prevBase || sameEdit(prevDraft, editable(prevBase))) setDraft(fresh);
      },
      (e) => {
        if (!cancelled) setStatus({ kind: 'error', text: `Couldn't load this boost (${e.message ?? e})` });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [id, summaryKey]);

  const dirty = Boolean(base && draft && !sameEdit(draft, editable(base)));

  const save = () => {
    const { base: b, draft: d, summary: s } = latest.current;
    if (!b || !d) return false;
    if (hostError(d.host)) {
      setHostTouched(true);
      setStatus({ kind: 'error', text: 'Enter a valid site before saving' });
      return false;
    }
    const host = normalizeHost(d.host);
    const boost = { ...b, name: d.name.trim() || host, host, css: d.css, js: d.js, enabled: s.enabled };
    send({ type: 'upsertBoost', boost });
    setBase(boost);
    setDraft({ ...d, name: boost.name, host });
    setStatus({ kind: 'saved', at: Date.now() });
    return true;
  };

  // Parent flushes unsaved edits before switching boosts.
  controller.current = {
    flush: () => {
      const { base: b, draft: d } = latest.current;
      if (b && d && !sameEdit(d, editable(b)) && !hostError(d.host)) save();
    },
  };

  useEffect(() => {
    const onKey = (e) => {
      if (e.ctrlKey && !e.altKey && !e.shiftKey && (e.key === 's' || e.key === 'S')) {
        e.preventDefault();
        save();
      }
    };
    const onHide = () => {
      if (document.visibilityState === 'hidden') controller.current?.flush();
    };
    window.addEventListener('keydown', onKey);
    document.addEventListener('visibilitychange', onHide);
    return () => {
      window.removeEventListener('keydown', onKey);
      document.removeEventListener('visibilitychange', onHide);
    };
  }, []);

  useEffect(() => {
    if (status?.kind !== 'saved') return undefined;
    const t = setTimeout(() => setStatus((s) => (s === status ? null : s)), 2500);
    return () => clearTimeout(t);
  }, [status]);

  if (missing) {
    return html`<${EmptyState} icon="warning" title="This boost doesn't exist anymore">It may have been deleted in another tab.<//>`;
  }
  if (!draft) return html`<div class="bst-loading" aria-busy="true" />`;

  const edit = (patch) => {
    setDraft((d) => ({ ...d, ...patch }));
    if (status?.kind !== 'error') setStatus(null);
  };
  const hostProblem = hostError(draft.host);
  const displayHost = normalizeHost(draft.host) || 'this site';
  let statusText = null;
  if (status?.kind === 'error') statusText = html`<span class="bst-status is-error">${status.text}</span>`;
  else if (dirty) statusText = html`<span class="bst-status is-dirty">Unsaved changes</span>`;
  else if (status?.kind === 'saved') statusText = html`<span class="bst-status is-saved"><${Icon} name="check" size=${14} />Saved</span>`;

  return html`<div class="bst-editor">
    <div class="bst-head">
      <span class=${classNames('bst-head-icon', !summary.enabled && 'is-off')} aria-hidden="true"><${Icon} name="boost" size=${20} /></span>
      <input
        class="bst-name"
        value=${draft.name}
        placeholder=${displayHost}
        aria-label="Boost name"
        spellcheck=${false}
        onInput=${(e) => edit({ name: e.currentTarget.value })}
        onKeyDown=${(e) => e.key === 'Enter' && save()}
      />
      <label class="bst-enabled">
        <span>${summary.enabled ? 'On' : 'Off'}</span>
        <${Toggle} checked=${summary.enabled} ariaLabel="Boost enabled" onChange=${() => send({ type: 'toggleBoost', id })} />
      </label>
    </div>

    <div class="bst-bar">
      <div class="bst-site">
        <${TextField}
          label="Site"
          icon="globe"
          value=${draft.host}
          placeholder="example.com"
          hint="Applies to this site and its subdomains."
          error=${hostTouched && hostProblem ? hostProblem : undefined}
          onInput=${(v) => {
            setHostTouched(false);
            edit({ host: v });
          }}
          onBlur=${() => setHostTouched(true)}
          onCommit=${save}
        />
      </div>
    </div>

    <${CodeField}
      id="boost-css"
      label="CSS"
      language="css"
      hint="Added at document start"
      value=${draft.css}
      placeholder=${`/* CSS for ${displayHost} */`}
      onInput=${(v) => edit({ css: v })}
    />
    <${CodeField}
      id="boost-js"
      label="JavaScript"
      language="js"
      hint="Runs after DOMContentLoaded"
      value=${draft.js}
      placeholder=${`// JavaScript for ${displayHost}`}
      onInput=${(v) => edit({ js: v })}
    />

    <footer class="bst-foot">
      <div class="bst-foot-text">
        ${statusText ?? html`<span class="bst-status">Saving reloads open ${displayHost} tabs.</span>`}
      </div>
      <${ConfirmButton}
        label="Delete"
        icon="trash"
        variant="ghost"
        title="Delete this boost?"
        message=${`${draft.name || displayHost} will stop applying to ${displayHost}. You can't undo this.`}
        confirmLabel="Delete boost"
        onConfirm=${() => {
          send({ type: 'deleteBoost', id });
          onDeleted(id);
        }}
      />
      <${Button} variant="primary" icon="check" disabled=${!dirty} onClick=${save} title=${`Save (${shortcut('Ctrl+S')})`}>Save<//>
    </footer>
  </div>`;
}

// ------------------------------------------------------------------------------------ page

function Boosts({ state }) {
  const [selected, setSelected] = useState(readId);
  const controller = useRef(null);
  const pending = useRef(null);
  const boosts = state.boosts ?? [];
  const summary = boosts.find((b) => b.id === selected) ?? null;
  const listRef = useRef(null);
  const listIndicator = useRef(null);
  // `pages.navIndicator`: the tint behind the selected boost glides down the list.
  useNavIndicator(listRef, listIndicator, '.bst-item.is-selected');

  useEffect(() => {
    const onPop = () => setSelected(readId());
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  }, []);

  const swap = (id, { replace = false } = {}) => {
    controller.current?.flush();
    motion.viewTransition(EDITOR_KEY, () => {
      writeId(id, { replace });
      setSelected(id);
      // The state update renders on a microtask, so the transition has to wait exactly that long
      // for the new DOM — and no longer: the boost itself was fetched before this started.
      return Promise.resolve().then(() => Promise.resolve());
    });
  };

  /**
   * Switch to another boost. `boosts.get` runs **before** the transition (see `prefetched`); if it
   * fails or is slow the swap happens anyway, and the editor loads it the usual way.
   */
  const select = (id, { replace = false } = {}) => {
    if (id === selected) return;
    if (id == null || prefetched.has(id) || !motion.enabled(EDITOR_KEY)) {
      swap(id, { replace });
      return;
    }
    invoke('boosts.get', { id }).then(
      (boost) => {
        if (boost) prefetched.set(id, boost);
        swap(id, { replace });
      },
      () => swap(id, { replace }),
    );
  };

  // Select a boost created from this page once it shows up in the state.
  useEffect(() => {
    const p = pending.current;
    if (!p) return;
    const created = boosts.find((b) => !p.known.has(b.id) && b.host === p.host) ?? boosts.find((b) => !p.known.has(b.id));
    if (created) {
      pending.current = null;
      select(created.id);
    }
  }, [boosts]);

  const create = (host) => {
    pending.current = { host, known: new Set(boosts.map((b) => b.id)) };
    send({ type: 'upsertBoost', boost: { id: 0, name: host, host, enabled: true, css: '', js: '' } });
  };

  const onDeleted = (id) => {
    const i = boosts.findIndex((b) => b.id === id);
    const next = boosts[i + 1] ?? boosts[i - 1] ?? null;
    controller.current = null;
    writeId(next?.id ?? null, { replace: true });
    setSelected(next?.id ?? null);
  };

  if (!boosts.length && selected == null) {
    return html`<div class="ip">
      <${PageHeader} icon="boost" title="Boosts" subtitle="Restyle and extend sites with your own CSS and JavaScript." />
      <${EmptyState} icon="boost" title="No boosts yet">
        A boost changes how a site looks or works, for you only. Create one here, or open a site and choose
        “New Boost for this Site” in the command bar.
      <//>
      <div class="bst-empty-action"><${NewBoostButton} onCreate=${create} label="Create a boost" /></div>
    </div>`;
  }

  return html`<div class="ip bst-page">
    <${PageHeader} icon="boost" title="Boosts" subtitle="Restyle and extend sites with your own CSS and JavaScript.">
      <${NewBoostButton} onCreate=${create} />
    <//>
    <div class="bst">
      <nav class="bst-list" ref=${listRef} aria-label="Boosts">
        <span key="indicator" class="ip-nav-indicator" ref=${listIndicator} aria-hidden="true" />
        ${boosts.map(
          (b) => html`<button
            key=${b.id}
            type="button"
            class=${classNames('bst-item', b.id === selected && 'is-selected', !b.enabled && 'is-off')}
            aria-current=${b.id === selected ? 'page' : undefined}
            onClick=${() => select(b.id)}
          >
            <span class="bst-item-icon" aria-hidden="true"><${Icon} name="boost" size=${15} /></span>
            <span class="bst-item-text">
              <span class="bst-item-name">${b.name || b.host}</span>
              <span class="bst-item-host">${b.enabled ? b.host : `${b.host} · off`}</span>
            </span>
          </button>`,
        )}
      </nav>
      <main class="bst-main">
        ${selected == null
          ? html`<${EmptyState} icon="edit" title="Choose a boost">Select a boost on the left to edit its CSS and JavaScript.<//>`
          : summary
            ? html`<${Editor} key=${selected} id=${selected} summary=${summary} controller=${controller} onDeleted=${onDeleted} />`
            : html`<${EmptyState} icon="warning" title="This boost doesn't exist anymore">It may have been deleted in another tab.<//>`}
      </main>
    </div>
  </div>`;
}

mountPage(Boosts);
