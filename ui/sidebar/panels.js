// Sidebar panels (PROTOCOL §5 "Panels"): downloads popover, compact in-progress download card,
// new/edit space sheet (emoji, name, theme presets with live preview via `theme.colors`, delete
// with confirmation) and the "Edit pinned page" popover (`SidebarPanel::EditPinned`).

import { html, useEffect, useLayoutEffect, useMemo, useRef, useState } from '/common/vendor/htm-preact.js';
import { invoke } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { Button, EmojiPicker, IconButton, Menu, Popover, ProgressBar, TextField } from '/common/components.js';
import { classNames, describeDownload, downloadFraction, formatBytes, formatDuration, hashString, shortcut } from '/common/util.js';
import * as motion from '/common/motion.js';
import { fire, useExitGhost } from './lib.js';

// ------------------------------------------------------------------------------------ downloads

const extOf = (name) => {
  const m = /\.([a-z0-9]{1,5})$/i.exec(name ?? '');
  return m ? m[1].toUpperCase() : '';
};

function FileTile({ name }) {
  const ext = extOf(name);
  const hue = hashString(ext || name || '?') % 360;
  return html`<span class="dl-file" style=${{ '--file-hue': hue }} aria-hidden="true">
    <${Icon} name="download" size=${12} strokeWidth=${2} />
    ${ext && html`<span class="dl-ext">${ext.slice(0, 4)}</span>`}
  </span>`;
}

function DownloadRow({ d }) {
  const [menu, setMenu] = useState(null);
  const more = useRef(null);
  const control = (action) => fire({ type: 'downloadControl', id: d.id, action });
  const active = d.state === 'inProgress' || d.state === 'paused';
  const done = d.state === 'complete';
  const failed = d.state === 'interrupted' || d.state === 'cancelled';

  const menuItems = [
    done && { label: 'Open', icon: 'external', onSelect: () => control('open') },
    done && d.path && { label: 'Show in Folder', icon: 'folder', onSelect: () => control('showInFolder') },
    failed && { label: 'Retry', icon: 'reload', onSelect: () => control('retry') },
    active && { label: d.state === 'paused' ? 'Resume' : 'Pause', icon: d.state === 'paused' ? 'play' : 'pause', onSelect: () => control(d.state === 'paused' ? 'resume' : 'pause') },
    active && { label: 'Cancel', icon: 'close', onSelect: () => control('cancel') },
    { type: 'separator' },
    { label: 'Copy Download Link', icon: 'link', disabled: !d.url, onSelect: () => fire({ type: 'copyText', text: d.url }) },
    { label: 'Remove from List', icon: 'trash', disabled: active, onSelect: () => fire({ type: 'downloadDismiss', id: d.id }) },
  ].filter(Boolean);

  return html`<li class=${classNames('dl-row', `is-${d.state}`, menu && 'is-engaged')}>
    <button
      type="button"
      class="dl-hit"
      title=${done ? `Open ${d.fileName}` : d.fileName}
      aria-label=${`${d.fileName}, ${describeDownload(d)}`}
      onClick=${() => done && control('open')}
      onContextMenu=${(e) => {
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY });
      }}
    />
    <${FileTile} name=${d.fileName} />
    <span class="dl-name">${d.fileName}</span>
    <span class="dl-actions">
      ${d.state === 'inProgress' && html`<${IconButton} icon="pause" size="sm" iconSize=${13} label="Pause" class="dl-secondary" onClick=${() => control('pause')} />`}
      ${d.state === 'paused' && html`<${IconButton} icon="play" size="sm" iconSize=${13} label="Resume" onClick=${() => control('resume')} />`}
      ${active && html`<${IconButton} icon="close" size="sm" iconSize=${13} label="Cancel download" title="Cancel" onClick=${() => control('cancel')} />`}
      ${done && d.path && html`<${IconButton} icon="folder" size="sm" iconSize=${14} label="Show in folder" class="dl-secondary" onClick=${() => control('showInFolder')} />`}
      ${failed && html`<${IconButton} icon="reload" size="sm" iconSize=${13} label="Retry download" title="Retry" onClick=${() => control('retry')} />`}
      <${IconButton}
        icon="more"
        size="sm"
        iconSize=${14}
        label="More actions"
        class="dl-more"
        buttonRef=${more}
        aria-expanded=${menu ? 'true' : 'false'}
        onClick=${() => setMenu(menu ? null : { anchor: more.current })}
      />
    </span>
    <span class=${classNames('dl-status', failed && 'is-failed')}>${describeDownload(d)}</span>
    ${active &&
    html`<${ProgressBar}
      class="dl-progress"
      value=${downloadFraction(d)}
      height=${3}
      label=${`${d.fileName} progress`}
    />`}
    ${menu &&
    html`<${Menu}
      x=${menu.x}
      y=${menu.y}
      anchor=${menu.anchor}
      placement="bottom-end"
      label=${`${d.fileName} actions`}
      items=${menuItems}
      onClose=${() => setMenu(null)}
    />`}
  </li>`;
}

export function DownloadsPanel({ downloads, anchor, onClose }) {
  const shown = downloads.slice(0, 20);
  const width = Math.min(360, window.innerWidth - 16);
  return html`<${Popover}
    open
    anchor=${anchor}
    placement="top-start"
    offset=${6}
    label="Downloads"
    class="sidebar-popover downloads-popover"
    style=${{ width: `${width}px` }}
    closeOnDismiss=${false}
    onClose=${onClose}
  >
    <header class="popover-head">
      <span class="popover-title">Downloads</span>
      <${IconButton} icon="close" size="sm" iconSize=${13} label="Close downloads" title="Close (Esc)" muted onClick=${() => onClose('button')} />
    </header>
    ${shown.length === 0
      ? html`<div class="dl-empty">
          <span class="dl-empty-glyph"><${Icon} name="download" size=${22} /></span>
          <span>No downloads yet</span>
          <span class="dl-empty-hint">Files you download appear here.</span>
        </div>`
      : html`<ul class="dl-list">${shown.map((d) => html`<${DownloadRow} key=${d.id} d=${d} />`)}</ul>`}
  <//>`;
}

/**
 * Short status for the narrow card (arc_spec §2.20: `12.3 MB of 40 MB · 8s left`): the speed is
 * left to the downloads popover so the time left isn't cut off.
 */
function cardStatus(d) {
  if (d.state !== 'inProgress' || !d.totalBytes) return describeDownload(d);
  const amount = `${formatBytes(d.receivedBytes)} of ${formatBytes(d.totalBytes)}`;
  if (!(d.bytesPerSec > 0)) return amount;
  return `${amount} · ${formatDuration((d.totalBytes - d.receivedBytes) / d.bytesPerSec)} left`;
}

/** Compact card above the bottom bar for the newest in-progress download (arc_spec §2.20). */
export function DownloadCard({ download: d, onOpen }) {
  return html`<div class="dl-card" role="group" aria-label=${`Downloading ${d.fileName}`}>
    <button type="button" class="dl-card-main" title=${`Show downloads (${shortcut('Ctrl+J')})`} onClick=${onOpen}>
      <${FileTile} name=${d.fileName} />
      <span class="dl-text">
        <span class="dl-name">${d.fileName}</span>
        <span class="dl-status" title=${describeDownload(d)}>${cardStatus(d)}</span>
      </span>
    </button>
    <${IconButton}
      icon="close"
      size="sm"
      iconSize=${12}
      label="Cancel download"
      title="Cancel"
      muted
      onClick=${() => fire({ type: 'downloadControl', id: d.id, action: 'cancel' })}
    />
    <${ProgressBar} class="dl-card-progress" value=${downloadFraction(d)} height=${2} bare label="Download progress" />
  </div>`;
}

// ------------------------------------------------------------------------------------ space sheet

const HUE_STEP = 1;

/** Keeps a theme's secondary hue at the same offset as the preset it came from (default +40°). */
function withHue(theme, hue) {
  const offset = ((theme.hue2 - theme.hue + 540) % 360) - 180 || 40;
  return { ...theme, hue, hue2: (((hue + offset) % 360) + 360) % 360 };
}

const sameTheme = (a, b) => a && b && Math.abs(a.hue - b.hue) < 0.5 && Math.abs(a.hue2 - b.hue2) < 0.5 && Math.abs(a.chroma - b.chroma) < 0.0005;

/**
 * New / edit space sheet. Calls `onPreview(colors|null)` so the sidebar can preview the theme
 * live; `onClose(reason)` dismisses it (`created` / `deleted`: core already dropped the panel).
 */
export function SpaceSheet({ state, space, onClose, onPreview }) {
  const editing = Boolean(space);
  const presets = state.themePresets ?? [];
  const initialPreset = presets[state.spaces.length % Math.max(1, presets.length)];
  const [name, setName] = useState(space?.name ?? '');
  const [icon, setIcon] = useState(space?.icon ?? '✨');
  const [theme, setTheme] = useState(() => ({ ...(space?.theme ?? initialPreset?.theme ?? { hue: 300, hue2: 340, chroma: 0.06 }) }));
  const [colors, setColors] = useState(space?.colors ?? initialPreset?.colors ?? null);
  const [picker, setPicker] = useState(!editing);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const ref = useRef(null);
  const nameRef = useRef(null);
  const canDelete = editing && state.spaces.length > 1;

  // `sidebar.panels`: the sheet sinks back out as an inert ghost. The real sheet holds a focused text
  // field, so it is never kept on screen for an exit - it goes, and its clone finishes the movement.
  useExitGhost(
    'sidebar.panels',
    () => ref.current,
    () => [
      { opacity: 1, translate: 'none', scale: 1 },
      { opacity: 0, translate: `0 ${motion.distance(10)}px`, scale: 1 - motion.distance(0.01) },
    ],
  );

  // Live preview: preset colors are exact; custom hue/chroma asks core (theme.colors).
  useEffect(() => {
    const preset = presets.find((p) => sameTheme(p.theme, theme));
    if (preset) {
      setColors(preset.colors);
      return undefined;
    }
    let alive = true;
    const t = setTimeout(() => {
      invoke('theme.colors', { theme }).then(
        (c) => alive && c && setColors(c),
        (e) => console.error('[sidebar] theme.colors failed', e),
      );
    }, 30);
    return () => {
      alive = false;
      clearTimeout(t);
    };
  }, [theme.hue, theme.hue2, theme.chroma]);

  useEffect(() => {
    onPreview(colors);
  }, [colors]);
  useEffect(() => () => onPreview(null), []);

  // Esc and outside clicks close the sheet (the Popover rules, for a docked panel).
  useEffect(() => {
    const onKey = (e) => {
      if (e.key === 'Escape' && !e.defaultPrevented) {
        e.preventDefault();
        onClose('escape');
      }
    };
    const onDown = (e) => {
      if (ref.current?.contains(e.target) || e.target.closest?.('.portal-host')) return;
      onClose('outside');
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('pointerdown', onDown, true);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', onDown, true);
    };
  }, []);

  useLayoutEffect(() => {
    if (editing) nameRef.current?.focus({ preventScroll: true });
    else ref.current?.querySelector('.emoji-cell[tabindex="0"]')?.focus({ preventScroll: true });
  }, []);

  const submit = (e) => {
    e?.preventDefault?.();
    const trimmed = name.trim();
    if (editing) {
      const update = {
        type: 'updateSpace',
        id: space.id,
        name: trimmed && trimmed !== space.name ? trimmed : null,
        icon: icon && icon !== space.icon ? icon : null,
        theme: sameTheme(theme, space.theme) ? null : theme,
      };
      if (update.name != null || update.icon != null || update.theme != null) fire(update);
      onClose('saved');
    } else {
      // Core closes the NewSpace panel itself when it creates the space.
      fire({ type: 'newSpace', name: trimmed, icon: icon || '✨', theme });
      onClose('created');
    }
  };

  const columns = Math.max(5, Math.floor((window.innerWidth - 16 - 2 - 28 + 2) / 32));
  const previewStyle = colors
    ? { background: `linear-gradient(135deg, ${colors.gradientStart}, ${colors.gradientEnd})`, color: colors.text }
    : undefined;

  return html`<div
    ref=${ref}
    class="space-sheet"
    role="dialog"
    aria-modal="false"
    aria-label=${editing ? `Edit space ${space.name}` : 'New space'}
  >
    <header class="sheet-head">
      <span class="sheet-title">${editing ? 'Edit Space' : 'New Space'}</span>
      <${IconButton} icon="close" size="sm" iconSize=${13} label="Close" title="Close (Esc)" muted onClick=${() => onClose('button')} />
    </header>
    <div class="sheet-body">
      <div class="sheet-identity">
        <button
          type="button"
          class=${classNames('sheet-icon emoji', picker && 'is-open')}
          style=${previewStyle}
          title="Choose an icon"
          aria-label=${`Icon ${icon}. Choose an icon`}
          aria-expanded=${String(picker)}
          onClick=${() => setPicker((p) => !p)}
        >${icon}</button>
        <${TextField}
          class="sheet-name"
          value=${name}
          inputRef=${nameRef}
          placeholder=${editing ? space.name : 'Space name'}
          aria-label="Space name"
          maxLength=${40}
          onInput=${setName}
          onCommit=${submit}
        />
      </div>
      ${picker &&
      html`<${EmojiPicker}
        class="sheet-emoji"
        value=${icon}
        columns=${columns}
        label="Space icon"
        onSelect=${(v) => setIcon(v)}
      />`}
      <div class="sheet-section">
        <span class="sheet-label">Theme</span>
        <div class="theme-swatches" role="radiogroup" aria-label="Theme presets">
          ${presets.map((p) => {
            const selected = sameTheme(p.theme, theme);
            return html`<button
              key=${p.name}
              type="button"
              role="radio"
              aria-checked=${String(selected)}
              class=${classNames('theme-swatch', selected && 'is-selected')}
              title=${p.name}
              aria-label=${p.name}
              style=${{ '--sw-a': p.colors.gradientStart, '--sw-b': p.colors.gradientEnd, '--sw-accent': p.colors.accent }}
              onClick=${() => setTheme({ ...p.theme })}
            ><span class="theme-swatch-dot" /></button>`;
          })}
        </div>
        <label class="theme-slider">
          <span class="sheet-sublabel">Hue</span>
          <input
            type="range"
            class="hue-range"
            min="0"
            max="359"
            step=${HUE_STEP}
            value=${Math.round(theme.hue)}
            aria-label="Hue"
            onInput=${(e) => setTheme((t) => withHue(t, Number(e.currentTarget.value)))}
          />
        </label>
        <label class="theme-slider">
          <span class="sheet-sublabel">Color</span>
          <input
            type="range"
            class="chroma-range"
            min="0"
            max="0.08"
            step="0.005"
            value=${theme.chroma}
            aria-label="Color intensity"
            style=${colors ? { '--range-accent': colors.accent } : undefined}
            onInput=${(e) => setTheme((t) => ({ ...t, chroma: Number(e.currentTarget.value) }))}
          />
        </label>
      </div>
    </div>
    <footer class="sheet-foot">
      ${confirmDelete
        ? html`<div class="sheet-confirm" role="alert">
            <span class="sheet-confirm-text">Delete “${space.name}”? Its tabs move to the Archive.</span>
            <div class="sheet-confirm-actions">
              <${Button} size="sm" variant="ghost" onClick=${() => setConfirmDelete(false)}>Cancel<//>
              <${Button}
                size="sm"
                variant="danger"
                autofocus
                onClick=${() => {
                  fire({ type: 'deleteSpace', id: space.id });
                  onClose('deleted');
                }}
              >Delete Space<//>
            </div>
          </div>`
        : html`${editing &&
            html`<${Button}
              size="sm"
              variant="ghost"
              class="sheet-delete"
              icon="trash"
              disabled=${!canDelete}
              title=${canDelete ? 'Delete this space' : 'The last space cannot be deleted'}
              onClick=${() => setConfirmDelete(true)}
            >Delete<//>`}
            <span class="grow" />
            <${Button} size="sm" variant="ghost" onClick=${() => onClose('cancel')}>Cancel<//>
            <${Button} size="sm" variant="primary" onClick=${submit}>${editing ? 'Save' : 'Create Space'}<//>`}
    </footer>
  </div>`;
}

// ------------------------------------------------------------------------------------ edit pinned

/**
 * "Edit Pinned Page" (core panel `editPinned`, which docks a hidden sidebar: it holds input).
 * `onClose('saved')` after dispatching `editPinned` (core closes the panel itself); any other
 * reason (`cancel`, `escape`, `outside`, an unchanged save) leaves closing the panel to the caller.
 */
export function EditPinnedPopover({ tab, anchor, onClose }) {
  const [title, setTitle] = useState(tab.title);
  const [url, setUrl] = useState(tab.pinnedUrl ?? tab.url);
  const urlError = useMemo(() => {
    const v = url.trim();
    if (!v) return 'Enter a URL';
    try {
      new URL(v);
      return null;
    } catch {
      return /\s/.test(v) || !/\./.test(v) ? 'Enter a valid URL' : null;
    }
  }, [url]);
  const save = () => {
    if (urlError) return;
    const cmd = { type: 'editPinned', id: tab.id };
    if (title.trim() !== tab.title.trim()) cmd.title = title.trim();
    if (url.trim() !== (tab.pinnedUrl ?? '')) cmd.url = url.trim();
    if (!('title' in cmd || 'url' in cmd)) {
      onClose('cancel');
      return;
    }
    fire(cmd);
    onClose('saved');
  };
  return html`<${Popover}
    open
    anchor=${anchor}
    placement="bottom-start"
    label="Edit pinned page"
    class="sidebar-popover edit-pinned"
    style=${{ width: `${Math.min(320, window.innerWidth - 16)}px` }}
    closeOnDismiss=${false}
    onClose=${onClose}
  >
    <header class="popover-head">
      <span class="popover-title">Edit Pinned Page</span>
    </header>
    <div class="stack" style=${{ gap: '10px' }}>
      <${TextField} label="Title" value=${title} placeholder="Page title" autoFocus selectOnFocus onInput=${setTitle} onCommit=${save} />
      <${TextField} label="Pinned URL" value=${url} error=${url.trim() ? urlError : null} onInput=${setUrl} onCommit=${save} />
      <div class="row" style=${{ justifyContent: 'flex-end', gap: '6px' }}>
        <${Button} size="sm" variant="ghost" onClick=${() => onClose('cancel')}>Cancel<//>
        <${Button} size="sm" variant="primary" disabled=${Boolean(urlError)} onClick=${save}>Save<//>
      </div>
    </div>
  <//>`;
}
