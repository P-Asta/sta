// Settings › Extensions (ext design FINAL PLAN §4 "Settings › Extensions"; SEC-7, R-SEC-2).
//
// This is the only place an extension can be turned **on**, and the only place it can be removed.
// The Ctrl+E picker never does either: it sends the user here (`runExtension` on an extension that is
// off opens this section at its row).
//
// Turning on an extension another program added is a disclosure, not a toggle:
//   1. "Turn on" asks the shell for the details (`requestExtensionDetails` → Chrome's own permission
//      warnings, host access and where the code came from);
//   2. the row opens a panel showing exactly those, with the source spelled out;
//   3. the confirm button is **not** the default — "Not now" is first and really focused, the panel
//      announces the warnings when they arrive (`aria-live`), and Esc closes it.
// The panel stays *inline*, next to the row it is about (the warnings are about that extension, and a
// floating dialog would take them away from it), so it does the four things `Popover` would have done
// for it by hand: `aria-expanded`/`aria-controls` on the button, focus into the panel, a live region,
// and Esc.
//
// A local CRX another program registered can only be removed (D6a): sta cannot show the user where
// that code came from, so it never offers to run it — and it says that **in the row**, not in a
// tooltip on a dimmed button, because it is the one sentence that explains why there is no choice.
//
// Remove has **no confirmation of sta's own**: Chromium always shows its own "Remove …?" dialog for
// an extension it is asked to uninstall (gate S7 — `showConfirmDialog: false` is only honoured for an
// extension removing itself), and two confirmations in a row for one click is worse than one.

import { html, useEffect, useLayoutEffect, useRef, useState } from '/common/vendor/htm-preact.js';
import { dispatch, isMock } from '/common/ipc.js';
import { Icon } from '/common/icons.js';
import { Button, Favicon, IconButton, Toggle } from '/common/components.js';
import { classNames } from '/common/util.js';

const report = (e) => console.error('[settings/extensions]', e);
const send = (command) => dispatch(command).catch(report);

/** Icon URLs the page's CSP can load (sta's own `__ext-icon` route). */
const loadableIcon = (url) => (/^(sta:|data:image\/|https:)/i.test(url ?? '') ? url : null);

const STATE_LABELS = {
  enabled: 'On',
  off: 'Off',
  needsApproval: 'Needs your OK',
};

/** Why a blocked extension is off — mirrors `extensions::ExtensionBlock::status` (sta-core). */
const BLOCK_LABELS = {
  policy: 'Turned off by your organization',
  unsupported: 'Not supported by this version of Chrome',
  damaged: 'This extension looks damaged',
  safety: 'Chrome turned this off for safety',
  requirement: "This extension needs something sta doesn't have",
  custodian: "Needs a parent's approval",
  unknown: 'Chrome turned this off',
};

/** Why sta will not offer to turn this on at all (D6a / policy). */
const REFUSALS = {
  externalLocal: "sta can't run this: another program installed it from a file, so sta cannot show you where that code came from.",
  managed: 'Your organization manages this extension.',
};

const statusOf = (item) => (item.state === 'blocked' ? (BLOCK_LABELS[item.blocked] ?? BLOCK_LABELS.unknown) : (STATE_LABELS[item.state] ?? item.state));

// Served by the shell out of the extension's own directory (ARCHITECTURE §4.6). In mock mode the
// page is on http, where a `sta://` image is a CSP violation and there is no extension to read
// anyway, so the rows fall back to their letter tiles.
const iconUrl = (id) => (isMock ? null : `sta://settings/__ext-icon/${id}/32`);

/** The row the URL asked for (`sta://settings/?section=extensions&ext=<id>`). */
function requestedId() {
  try {
    return new URLSearchParams(location.search).get('ext') || null;
  } catch {
    return null;
  }
}

function Warnings({ details }) {
  if (!details) return html`<p class="xs-detail-loading">Reading what this extension asks for…</p>`;
  const warnings = details.warnings ?? [];
  return html`<div class="xs-detail-body">
    ${warnings.length > 0
      ? html`<ul class="xs-warnings">
          ${warnings.map((w, i) => html`<li key=${i}>${w}</li>`)}
        </ul>`
      : html`<p class="xs-detail-line">It asks for no special permissions.</p>`}
    ${details.hostAccess && html`<p class="xs-detail-line"><b>Site access:</b> ${details.hostAccess}</p>`}
    ${details.source && html`<p class="xs-detail-line"><b>Source:</b> ${details.source}</p>`}
  </div>`;
}

function TurnOnPanel({ item, details, onCancel }) {
  const panel = useRef(null);
  // The click that opened this panel made the Turn on button `busy` and therefore disabled, and the
  // browser blurs a control that becomes disabled — so without this the keyboard user is left with
  // focus on `<body>`, next to a panel they were never sent to.
  useLayoutEffect(() => {
    const el = panel.current;
    if (el) (el.querySelector('[autofocus]') ?? el).focus({ preventScroll: true });
  }, []);
  return html`<div
    ref=${panel}
    class="xs-detail"
    id=${`ext-turnon-${item.id}`}
    role="group"
    aria-label=${`Turn on ${item.name}`}
    tabindex="-1"
    onKeyDown=${(e) => {
      if (e.key === 'Escape' && !e.defaultPrevented) {
        e.preventDefault();
        e.stopPropagation();
        onCancel();
      }
    }}
  >
    <p class="xs-detail-lead"><b>${item.name}</b>${' was added by another program. Before it runs, sta shows what Chrome says it can do.'}</p>
    <div aria-live="polite">
      <${Warnings} details=${details} />
    </div>
    <div class="xs-detail-actions">
      <${Button} size="sm" variant="ghost" autofocus onClick=${onCancel}>Not now<//>
      <${Button}
        size="sm"
        disabled=${!details}
        onClick=${() => {
          send({ type: 'setExtensionEnabled', id: item.id, enabled: true });
          onCancel();
        }}
      >Turn on anyway<//>
    </div>
  </div>`;
}

function Row({ item, details, busy, open, setOpen, highlight }) {
  const on = item.state === 'enabled';
  const canEnable = item.install !== 'externalLocal' && item.install !== 'managed';
  const needsOk = item.state === 'needsApproval';
  const status = statusOf(item);
  const turnOn = useRef(null);
  // Up to two lines: an unpacked extension's path (or the file another program installed) is the one
  // thing R-SEC-2 asks the user to judge, and ellipsizing it away at `C:\ast\tmp\s7\p3-v-ux\data\ux…`
  // hid exactly the part that matters (UXV-3). The full text is still the row's tooltip.
  const desc = [status, item.version, item.sourceLabel].filter(Boolean).join(' · ');
  const refusal = canEnable ? null : REFUSALS[item.install];
  const disabled = busy || item.state === 'blocked';
  return html`<div class=${classNames('ip-setting', 'xs-row', highlight && 'is-target')} id=${`ext-${item.id}`} data-ext=${item.id}>
    <div class="xs-main">
      <span class=${classNames('xs-icon', !on && 'is-off')}>
        <${Favicon} src=${loadableIcon(iconUrl(item.id))} host=${item.name} size=${20} lazy=${false} />
      </span>
      <div class="ip-setting-text">
        <span class="ip-setting-label">${item.name}</span>
        <span class="ip-setting-desc xs-desc" title=${desc}>${desc}</span>
        ${refusal && html`<span class="ip-setting-desc xs-refusal" id=${`ext-refusal-${item.id}`}>${refusal}</span>`}
      </div>
      <div class="ip-setting-control xs-controls">
        ${on && item.options && html`<${Button} size="sm" variant="ghost" onClick=${() => send({ type: 'runExtension', id: item.id, action: 'options' })}>Options<//>`}
        <${IconButton}
          icon="external"
          label=${`View ${item.name} in the Chrome Web Store`}
          muted
          onClick=${() => send({ type: 'runExtension', id: item.id, action: 'webStore' })}
        />
        <${IconButton}
          icon="trash"
          label=${`Remove ${item.name}`}
          title=${`Remove ${item.name} (Chrome asks first)`}
          muted
          disabled=${item.install === 'managed' || busy}
          onClick=${() => send({ type: 'removeExtension', id: item.id })}
        />
        ${refusal
          ? null // the row says why in words; a dimmed button with a tooltip said it to nobody (UXV-3)
          : needsOk
            ? html`<${Button}
              size="sm"
              buttonRef=${turnOn}
              disabled=${busy}
              aria-haspopup="dialog"
              aria-expanded=${String(open === 'turnOn')}
              aria-controls=${open === 'turnOn' ? `ext-turnon-${item.id}` : null}
              onClick=${() => {
                const next = open === 'turnOn' ? null : 'turnOn';
                setOpen(next);
                if (next) send({ type: 'requestExtensionDetails', id: item.id });
              }}
            >Turn on<//>`
            : html`<${Toggle}
              checked=${on}
              disabled=${disabled}
              ariaLabel=${`${item.name} enabled`}
              onChange=${(v) => send({ type: 'setExtensionEnabled', id: item.id, enabled: v })}
            />`}
      </div>
    </div>
    ${open === 'turnOn' &&
    html`<${TurnOnPanel}
      item=${item}
      details=${details}
      onCancel=${() => {
        setOpen(null);
        // Back where the keyboard was, the way a Popover restores focus to its anchor.
        requestAnimationFrame(() => turnOn.current?.focus?.({ preventScroll: true }));
      }}
    />`}
  </div>`;
}

export function ExtensionsSection({ state }) {
  const ext = state.extensions ?? {};
  const items = ext.items ?? [];
  const details = ext.details ?? [];
  const busy = ext.busy ?? null;
  const needsOk = ext.needsOk ?? 0;
  const [open, setOpen] = useState({});
  const [target, setTarget] = useState(requestedId);

  // The picker sends the user here at one row: scroll to it once, then stop highlighting it.
  useEffect(() => {
    if (!target) return;
    const el = document.getElementById(`ext-${target}`);
    if (el) requestAnimationFrame(() => el.scrollIntoView({ block: 'center' }));
    const timer = setTimeout(() => setTarget(null), 2500);
    return () => clearTimeout(timer);
  }, [target, items.length]);

  const openFor = (id) => open[id] ?? null;
  const setOpenFor = (id) => (value) => setOpen((o) => ({ ...o, [id]: value }));

  return html`<section id="extensions" class="set-section" aria-labelledby="extensions-title">
    <div class="set-section-head">
      <h2 class="set-section-title" id="extensions-title">Extensions</h2>
      <${Button} variant="ghost" size="sm" iconEnd="external" onClick=${() => send({ type: 'openUrl', url: 'https://chromewebstore.google.com/', target: 'newTab' })}>
        Get extensions
      <//>
    </div>
    ${ext.safeMode &&
    html`<div class="xs-banner is-warn" role="status">
      <${Icon} name="warning" size=${16} />
      <span>
        ${'sta closed unexpectedly twice, so this session started in '}<b>safe mode</b>${': your tabs were restored without loading them. If it keeps happening, turn off the extension you added last.'}
      </span>
    </div>`}
    ${needsOk > 0 &&
    html`<div class="xs-banner" role="status">
      <${Icon} name="info" size=${16} />
      <span>
        ${needsOk === 1
          ? '1 extension was added by another program. It stays off until you allow it.'
          : `${needsOk} extensions were added by other programs. They stay off until you allow them.`}
      </span>
    </div>`}
    <div class="ip-card">
      ${items.length === 0
        ? html`<div class="xs-empty">
            <${Icon} name="puzzle" size=${18} />
            <span>${'No extensions installed. sta runs Chrome extensions — use '}<b>Get extensions</b>${', then press '}<b>Ctrl+E</b>${' to use them.'}</span>
          </div>`
        : items.map(
            (item) => html`<${Row}
              key=${item.id}
              item=${item}
              details=${details.find((d) => d.id === item.id) ?? null}
              busy=${busy === item.id}
              open=${openFor(item.id)}
              setOpen=${setOpenFor(item.id)}
              highlight=${target === item.id}
            />`,
          )}
    </div>
    <p class="xs-note">
      ${'Extensions run the way Chrome runs them, but sta has no extension toolbar: press '}<b>Ctrl+E</b>${' '}
      ${"to open one. Toolbar-click actions, extension shortcuts and side panels don't work yet. Removing an extension opens Chrome's own confirmation."}
    </p>
  </section>`;
}
