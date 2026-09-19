// Shared Preact components for sta surfaces. Styles live in base.css; colors come from the
// ThemeColors custom properties. Page-provided strings (titles, URLs, hosts) are always rendered
// as text nodes or attributes, never as HTML.
//
//   import { Button, IconButton, Menu, Favicon, … } from '/common/components.js';

import { h, html, render, useEffect, useLayoutEffect, useMemo, useRef, useState } from './vendor/htm-preact.js';
import { Icon } from './icons.js';
import { useLatest, useStableId } from './hooks.js';
import * as motion from './motion.js';
import { clamp, classNames, firstGraphemes, hostLetter, hostLetterColor } from './util.js';

/** Every animation in this module is one of these keys (`crates/sta-core/src/motion.rs`). */
const MENU_KEY = 'menus.popIn';
const BADGE_KEY = 'indicators.badges';

// ------------------------------------------------------------------------------------ Portal

function PortalContent({ children }) {
  return children;
}

/**
 * Render `children` into a container appended to `<body>`, outside any transformed or clipped
 * ancestor. Context does not cross the portal (the vendored Preact has no createPortal).
 */
export function Portal({ children }) {
  const host = useMemo(() => {
    const el = document.createElement('div');
    el.className = 'portal-host';
    return el;
  }, []);
  useLayoutEffect(() => {
    document.body.appendChild(host);
    return () => {
      render(null, host);
      host.remove();
    };
  }, [host]);
  useLayoutEffect(() => {
    render(h(PortalContent, { children }), host);
  });
  return null;
}

/**
 * Document event that closes every open Menu and Popover (except those rendered with
 * `closeOnDismiss={false}`) with reason `dismiss`. A page dispatches it when a press happened
 * somewhere it never sees, e.g. the floating sidebar on `sidebar.hover {dismiss}` (a press outside
 * it went to another browser, so their own outside-pointerdown rule can't fire).
 */
export const DISMISS_EVENT = 'sta:dismiss';

/** Closes every dismissible Menu and Popover of this page (see `DISMISS_EVENT`). */
export function dismissFloatingLayers() {
  document.dispatchEvent(new Event(DISMISS_EVENT));
}

/**
 * Whether `target` is inside `layer`'s portal host or any portal host opened after it (nested
 * menus/popovers opened from inside count as "inside").
 */
function insideLayer(layer, target) {
  if (!layer) return false;
  const host = layer.closest('.portal-host');
  if (!host) return layer.contains(target);
  const hosts = [...document.querySelectorAll('body > .portal-host')];
  return hosts.slice(Math.max(0, hosts.indexOf(host))).some((el) => el.contains(target));
}

// ------------------------------------------------------------------------------------ positioning

const VIEWPORT_MARGIN = 8;

/** `{left, top, right, bottom}` for an Element, a DOMRect-like object, or a `{x, y}` point. */
export function rectOf(anchor) {
  if (!anchor) return { left: 0, top: 0, right: 0, bottom: 0 };
  if (typeof anchor.getBoundingClientRect === 'function') return anchor.getBoundingClientRect();
  if ('left' in anchor) return anchor;
  return { left: anchor.x, right: anchor.x, top: anchor.y, bottom: anchor.y };
}

/**
 * Position a `position: fixed` element next to an anchor rect, flipping to the other side when
 * it doesn't fit and clamping into `bounds` (default: the viewport) minus an 8px margin.
 * @param {HTMLElement} el
 * @param {{left:number, top:number, right:number, bottom:number}} anchor
 * @param {{placement?: string, offset?: number, bounds?: {left:number, top:number, right:number, bottom:number}}} [opts]
 *   placement: `bottom-start|bottom-end|bottom-center|top-start|top-end|top-center|right-start|right-end|left-start|left-end`
 */
export function placeFloating(el, anchor, { placement = 'bottom-start', offset = 4, bounds } = {}) {
  const outer = bounds ?? { left: 0, top: 0, right: window.innerWidth, bottom: window.innerHeight };
  const b = {
    left: outer.left + VIEWPORT_MARGIN,
    top: outer.top + VIEWPORT_MARGIN,
    right: outer.right - VIEWPORT_MARGIN,
    bottom: outer.bottom - VIEWPORT_MARGIN,
  };
  el.style.maxHeight = `${Math.max(0, b.bottom - b.top)}px`;
  const w = el.offsetWidth;
  const hgt = el.offsetHeight;
  const [side, align = 'start'] = placement.split('-');
  let left;
  let top;
  if (side === 'top' || side === 'bottom') {
    left = align === 'end' ? anchor.right - w : align === 'center' ? (anchor.left + anchor.right - w) / 2 : anchor.left;
    if (align === 'start' && left + w > b.right && anchor.right - w >= b.left) left = anchor.right - w;
    const below = anchor.bottom + offset;
    const above = anchor.top - offset - hgt;
    top = side === 'bottom' ? below : above;
    if (side === 'bottom' && below + hgt > b.bottom && above >= b.top) top = above;
    if (side === 'top' && above < b.top && below + hgt <= b.bottom) top = below;
  } else {
    top = align === 'end' ? anchor.bottom - hgt : anchor.top;
    const right = anchor.right + offset;
    const leftSide = anchor.left - offset - w;
    left = side === 'right' ? right : leftSide;
    if (side === 'right' && right + w > b.right && leftSide >= b.left) left = leftSide;
    if (side === 'left' && leftSide < b.left && right + w <= b.right) left = right;
  }
  const x = Math.round(clamp(left, b.left, Math.max(b.left, b.right - w)));
  const y = Math.round(clamp(top, b.top, Math.max(b.top, b.bottom - hgt)));
  el.style.left = `${x}px`;
  el.style.top = `${y}px`;
  // `menus.popIn` grows out of the side the panel actually ended up on — after the flip and the
  // clamp above, which is why this is written here and not derived from `placement`. `--pop-origin`
  // is the transform origin; `--pop-dx` / `--pop-dy` are the direction the 3 px of travel comes
  // from, as multipliers so `--motion-distance: 0` can flatten them (base.css `sta-pop-in`).
  const anchorX = (anchor.left + anchor.right) / 2;
  const anchorY = (anchor.top + anchor.bottom) / 2;
  const vertical = side === 'top' || side === 'bottom';
  const fromBelow = vertical ? y >= anchorY : false;
  const originY = vertical ? (fromBelow ? 'top' : 'bottom') : y + hgt / 2 >= anchorY ? 'top' : 'bottom';
  const originX = vertical ? (x + w / 2 >= anchorX ? 'left' : 'right') : x >= anchorX ? 'left' : 'right';
  el.style.setProperty('--pop-origin', `${originY} ${originX}`);
  el.style.setProperty('--pop-dx', vertical ? '0' : x >= anchorX ? '-1' : '1');
  el.style.setProperty('--pop-dy', vertical ? (fromBelow ? '-1' : '1') : '0');
}

const FOCUSABLE = 'button:not(:disabled), [href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])';

// ------------------------------------------------------------------------------------ Icon buttons & buttons

/**
 * Text button.
 * @param {{variant?: 'default'|'primary'|'ghost'|'danger', size?: 'md'|'sm', icon?: string, iconEnd?: string, buttonRef?: object|Function}} props
 *   plus any `<button>` attributes (`onClick`, `disabled`, `type`…). `buttonRef` receives the
 *   `<button>` element (e.g. to anchor a Menu or Popover; `ref` on a component is not forwarded).
 */
export function Button({ variant = 'default', size = 'md', icon, iconEnd, children, class: className, type = 'button', buttonRef, ...rest }) {
  const iconSize = size === 'sm' ? 14 : 16;
  return html`<button
    ref=${buttonRef}
    type=${type}
    class=${classNames('btn', variant !== 'default' && `btn-${variant}`, size === 'sm' && 'btn-sm', className)}
    ...${rest}
  >
    ${icon && html`<${Icon} name=${icon} size=${iconSize} />`}${children}${iconEnd && html`<${Icon} name=${iconEnd} size=${iconSize} />`}
  </button>`;
}

/**
 * Square icon-only button with a required accessible `label` (also the tooltip; pass
 * `title=${null}` to suppress it).
 * @param {{icon: string, label: string, size?: 'sm'|'md'|'lg', iconSize?: number, pressed?: boolean, muted?: boolean, buttonRef?: object|Function}} props
 */
export function IconButton({ icon, label, size = 'md', iconSize, pressed, muted = false, class: className, type = 'button', title, buttonRef, children, ...rest }) {
  if (!label) console.warn(`[components] IconButton "${icon}" needs a label`);
  return html`<button
    ref=${buttonRef}
    type=${type}
    class=${classNames('icon-btn', size === 'sm' && 'is-sm', size === 'lg' && 'is-lg', muted && 'is-muted', className)}
    aria-label=${label}
    title=${title === undefined ? label : (title ?? undefined)}
    aria-pressed=${pressed == null ? undefined : String(Boolean(pressed))}
    ...${rest}
  >
    <${Icon} name=${icon} size=${iconSize ?? (size === 'sm' ? 14 : 16)} />${children}
  </button>`;
}

// ------------------------------------------------------------------------------------ Kbd

/**
 * Keyboard shortcut chips: `<Kbd keys="Ctrl+Shift+K" />` → three `<kbd>` chips. Also accepts an
 * array of key labels. `Ctrl++` renders "Ctrl" and "+".
 */
export function Kbd({ keys, class: className }) {
  const parts = Array.isArray(keys) ? keys : String(keys ?? '').split(/\+(?=.)/);
  return html`<span class=${classNames('kbd-group', className)}>
    ${parts.map((k, i) => html`<kbd class="kbd" key=${i}>${k}</kbd>`)}
  </span>`;
}

// ------------------------------------------------------------------------------------ Favicon

/**
 * Site icon. Shows `src` as an `<img>`; when it's missing or fails to load, a letter tile colored
 * from `host`. `src` is an opaque URL from core: it is only ever used as an attribute.
 * Boolean DOM properties are passed as booleans (`draggable=${false}`): Preact assigns props that
 * exist on the element as properties, and the string "false" would make `img.draggable` true.
 * @param {{src?: string|null, host?: string, size?: number, dim?: boolean, lazy?: boolean, title?: string}} props
 *   `dim`: desaturated (unloaded tabs). `lazy` (default true): note that hidden overlay views may
 *   not load lazy images until they are shown; pass `lazy=${false}` there.
 */
export function Favicon({ src, host = '', size = 16, dim = false, lazy = true, title, class: className }) {
  const [failedSrc, setFailedSrc] = useState(null);
  const style = { '--favicon-size': `${size}px` };
  if (src && failedSrc !== src) {
    return html`<img
      class=${classNames('favicon', dim && 'is-dim', className)}
      src=${src}
      alt=""
      width=${size}
      height=${size}
      loading=${lazy ? 'lazy' : 'eager'}
      decoding="async"
      draggable=${false}
      referrerpolicy="no-referrer"
      title=${title}
      style=${style}
      onError=${() => setFailedSrc(src)}
    />`;
  }
  return html`<span
    class=${classNames('favicon favicon-tile', dim && 'is-dim', className)}
    style=${{ ...style, background: hostLetterColor(host) }}
    title=${title}
    aria-hidden="true"
  >${hostLetter(host)}</span>`;
}

// ------------------------------------------------------------------------------------ Spinner & progress

/** 14px loading spinner (accent, 1.5px stroke). `label=${null}` makes it decorative. */
export function Spinner({ size = 14, label = 'Loading', class: className }) {
  const a11y = label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': 'true' };
  return html`<svg class=${classNames('spinner', className)} width=${size} height=${size} viewBox="0 0 16 16" fill="none" ...${a11y}>
    <circle cx="8" cy="8" r="6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-dasharray="26 12" />
  </svg>`;
}

/**
 * Linear progress. `value` in [0, 1]; `null`/`undefined` = indeterminate.
 * `bare`: no track (e.g. the URL pill's 2px loading bar).
 */
export function ProgressBar({ value, label, height = 4, bare = false, class: className }) {
  const indeterminate = value == null || Number.isNaN(value);
  const v = indeterminate ? 0 : clamp(value, 0, 1);
  return html`<div
    class=${classNames('progress', indeterminate && 'is-indeterminate', bare && 'is-bare', className)}
    role="progressbar"
    aria-label=${label}
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow=${indeterminate ? undefined : Math.round(v * 100)}
    style=${{ '--progress-h': `${height}px` }}
  >
    <div class="progress-fill" style=${indeterminate ? undefined : { transform: `scaleX(${v})` }} />
  </div>`;
}

/**
 * Circular progress (downloads button). `value` in [0, 1] or null (indeterminate). Children are
 * centered inside the ring (e.g. a download glyph).
 */
export function ProgressRing({ value, size = 20, stroke = 2, label, children, class: className }) {
  const indeterminate = value == null || Number.isNaN(value);
  const v = indeterminate ? 0.25 : clamp(value, 0, 1);
  const r = (size - stroke) / 2;
  const circumference = 2 * Math.PI * r;
  return html`<div
    class=${classNames('progress-ring', indeterminate && 'is-indeterminate', className)}
    role="progressbar"
    aria-label=${label}
    aria-valuemin="0"
    aria-valuemax="100"
    aria-valuenow=${indeterminate ? undefined : Math.round(v * 100)}
    style=${{ width: `${size}px`, height: `${size}px` }}
  >
    <svg class="progress-ring-svg" width=${size} height=${size} viewBox=${`0 0 ${size} ${size}`} fill="none" aria-hidden="true">
      <circle class="progress-ring-track" cx=${size / 2} cy=${size / 2} r=${r} stroke-width=${stroke} />
      <circle
        class="progress-ring-arc"
        cx=${size / 2}
        cy=${size / 2}
        r=${r}
        stroke-width=${stroke}
        stroke-linecap="round"
        stroke-dasharray=${`${circumference} ${circumference}`}
        stroke-dashoffset=${circumference * (1 - v)}
      />
    </svg>
    ${children}
  </div>`;
}

/**
 * `indicators.audio`: three bars rising and falling, for a tab that is playing sound. Sized like the
 * 16-grid glyphs so it can stand in for the `speaker` icon. The bars only move while the window has
 * focus (`<html data-focused>`, tokens.css); otherwise — and at `reduced`, at `off` and with the key
 * switched off — they are a still glyph, which still says "this tab has sound".
 * @param {{size?: number, label?: string|null, class?: string}} props
 */
export function AudioBars({ size = 12, label = null, class: className }) {
  const a11y = label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': 'true' };
  return html`<svg class=${classNames('audio-bars', className)} width=${size} height=${size} viewBox="0 0 16 16" ...${a11y}>
    <rect x="2" y="4" width="3" height="8" rx="1.5" />
    <rect x="6.5" y="2" width="3" height="10" rx="1.5" />
    <rect x="11" y="5" width="3" height="7" rx="1.5" />
  </svg>`;
}

/**
 * `indicators.badges`: pop `ref`'s element whenever `value` changes to a different value. The
 * trigger is the keyed diff, never a render — a re-render with the same count does nothing.
 * @param {{current: Element|null}} ref
 * @param {string|number|null|undefined} value
 */
export function useCountPop(ref, value) {
  const last = useRef(value);
  const changed = last.current !== value;
  last.current = value;
  useLayoutEffect(() => {
    if (changed) motion.pop(ref.current, BADGE_KEY);
  });
}

// ------------------------------------------------------------------------------------ Menu

/**
 * @typedef {object} MenuItem
 * @property {string} label
 * @property {string} [key]
 * @property {string} [icon] glyph name
 * @property {string} [hint] shortcut text, e.g. "Ctrl+D"
 * @property {boolean} [disabled]
 * @property {boolean} [danger]
 * @property {boolean} [checked] renders a check and role="menuitemcheckbox"
 * @property {boolean} [selected] listbox selection (role="listbox" menus)
 * @property {() => void} [onSelect]
 * @property {MenuEntry[]} [submenu]
 *
 * @typedef {MenuItem | {type: 'separator'} | {type: 'header', label: string}} MenuEntry
 */

const isActionable = (item) => item && item.type !== 'separator' && item.type !== 'header' && !item.disabled;

function firstActionable(items, from = 0, dir = 1) {
  const n = items.length;
  for (let k = 0; k < n; k++) {
    const i = (((from + dir * k) % n) + n) % n;
    if (isActionable(items[i])) return i;
  }
  return -1;
}

const SUBMENU_DELAY = 140;

function MenuList(props) {
  const {
    items,
    level,
    anchor,
    placement,
    offset,
    bounds,
    inline,
    role,
    label,
    minWidth,
    autoFocus,
    initialIndex,
    onDone,
    onCloseLevel,
    onSelect,
    className,
  } = props;
  const ref = useRef(null);
  const id = useStableId('menu');
  const [active, setActive] = useState(() => (initialIndex != null && isActionable(items[initialIndex]) ? initialIndex : autoFocus ? firstActionable(items) : -1));
  const [sub, setSub] = useState(null); // {index, focus}
  const timer = useRef(0);
  const typeahead = useRef({ text: '', at: 0 });
  const hasIcons = items.some((it) => it.icon || it.checked !== undefined);
  const itemRole = role === 'listbox' ? 'option' : 'menuitem';

  useLayoutEffect(() => {
    if (!inline && ref.current) placeFloating(ref.current, anchor, { placement, offset, bounds });
  }, [items.length]);

  useLayoutEffect(() => {
    if (autoFocus && ref.current) ref.current.focus({ preventScroll: true });
  }, [autoFocus]);

  useEffect(() => () => clearTimeout(timer.current), []);

  useEffect(() => {
    // Keep the keyboard-highlighted item visible in scrollable menus. Scroll only the menu itself:
    // scrollIntoView would also scroll the page (e.g. an inline menu below the fold on mount).
    const menu = ref.current;
    const item = active >= 0 ? menu?.children[active] : null;
    if (!item || menu.scrollHeight <= menu.clientHeight) return;
    const style = getComputedStyle(menu);
    const top = menu.getBoundingClientRect().top + menu.clientTop + parseFloat(style.paddingTop);
    const bottom = menu.getBoundingClientRect().top + menu.clientTop + menu.clientHeight - parseFloat(style.paddingBottom);
    const r = item.getBoundingClientRect();
    if (r.top < top) menu.scrollTop -= top - r.top;
    else if (r.bottom > bottom) menu.scrollTop += r.bottom - bottom;
  }, [active]);

  const openSub = (index, focus) => {
    clearTimeout(timer.current);
    setSub({ index, focus });
  };

  const activate = (index, viaKeyboard) => {
    const item = items[index];
    if (!isActionable(item)) return;
    if (item.submenu) {
      openSub(index, viaKeyboard);
      return;
    }
    onDone('select');
    item.onSelect?.();
    onSelect?.(item);
  };

  const onKeyDown = (e) => {
    if (e.target !== ref.current) return; // a focused submenu handles its own keys
    const n = items.length;
    switch (e.key) {
      case 'ArrowDown':
        setActive(firstActionable(items, active < 0 ? 0 : active + 1, 1));
        break;
      case 'ArrowUp':
        setActive(firstActionable(items, active < 0 ? n - 1 : active - 1, -1));
        break;
      case 'Home':
        setActive(firstActionable(items, 0, 1));
        break;
      case 'End':
        setActive(firstActionable(items, n - 1, -1));
        break;
      case 'ArrowRight':
        if (items[active]?.submenu && isActionable(items[active])) openSub(active, true);
        break;
      case 'ArrowLeft':
        if (level > 0) onCloseLevel();
        break;
      case 'Enter':
      case ' ':
        activate(active, true);
        break;
      case 'Escape':
        if (level > 0) onCloseLevel();
        else onDone('escape');
        break;
      case 'Tab':
        onDone('tab');
        break;
      default: {
        if (e.key.length !== 1 || e.ctrlKey || e.altKey || e.metaKey) return;
        const now = Date.now();
        const t = typeahead.current;
        t.text = now - t.at > 700 ? e.key.toLowerCase() : t.text + e.key.toLowerCase();
        t.at = now;
        for (let k = 1; k <= n; k++) {
          const i = (Math.max(active, 0) + (t.text.length > 1 ? k - 1 : k)) % n;
          if (isActionable(items[i]) && items[i].label.toLowerCase().startsWith(t.text)) {
            setActive(i);
            break;
          }
        }
      }
    }
    e.preventDefault();
    e.stopPropagation();
  };

  const onItemPointerMove = (index) => {
    if (active !== index) setActive(index);
    const item = items[index];
    if (sub?.index === index) {
      clearTimeout(timer.current);
      return;
    }
    clearTimeout(timer.current);
    if (item.submenu && isActionable(item)) {
      timer.current = setTimeout(() => setSub({ index, focus: false }), SUBMENU_DELAY);
    } else if (sub) {
      timer.current = setTimeout(() => setSub(null), SUBMENU_DELAY * 1.5);
    }
  };

  let subAnchor = null;
  if (sub && ref.current?.children[sub.index]) {
    const r = ref.current.children[sub.index].getBoundingClientRect();
    subAnchor = { left: r.left, right: r.right, top: r.top - 5, bottom: r.bottom + 5 };
  }

  return html`
    <div
      ref=${ref}
      class=${classNames('menu', inline && 'is-inline', level > 0 && 'is-submenu', className)}
      role=${role}
      aria-label=${label}
      aria-orientation="vertical"
      aria-activedescendant=${active >= 0 ? `${id}-${active}` : undefined}
      tabindex="-1"
      style=${minWidth ? { minWidth: `${minWidth}px` } : undefined}
      onKeyDown=${onKeyDown}
      onPointerLeave=${() => {
        if (!sub) setActive(-1);
      }}
      onContextMenu=${(e) => e.preventDefault()}
    >
      ${items.map((item, index) => {
        if (item.type === 'separator') return html`<div key=${`sep-${index}`} class="menu-sep" role="separator" />`;
        if (item.type === 'header') return html`<div key=${`hdr-${index}`} class="menu-header" role="presentation">${item.label}</div>`;
        const checkable = role !== 'listbox' && item.checked !== undefined;
        return html`<div
          key=${item.key ?? `${index}-${item.label}`}
          id=${`${id}-${index}`}
          class=${classNames('menu-item', index === active && 'is-active', item.danger && 'is-danger')}
          role=${checkable ? 'menuitemcheckbox' : itemRole}
          aria-checked=${checkable ? String(Boolean(item.checked)) : undefined}
          aria-selected=${role === 'listbox' ? String(Boolean(item.selected)) : undefined}
          aria-disabled=${item.disabled ? 'true' : undefined}
          aria-haspopup=${item.submenu ? 'menu' : undefined}
          aria-expanded=${item.submenu ? String(sub?.index === index) : undefined}
          onPointerMove=${() => onItemPointerMove(index)}
          onClick=${() => activate(index, false)}
        >
          ${hasIcons &&
          html`<span class="menu-icon">
            ${item.icon ? html`<${Icon} name=${item.icon} size=${16} />` : item.checked ? html`<${Icon} name="check" size=${16} />` : null}
          </span>`}
          <span class="menu-label">${item.label}</span>
          ${item.hint && html`<span class="menu-hint">${item.hint}</span>`}
          ${item.submenu && html`<span class="menu-trail"><${Icon} name="chevron-right" size=${14} /></span>`}
          ${role === 'listbox' && item.selected && html`<span class="menu-trail"><${Icon} name="check" size=${14} /></span>`}
        </div>`;
      })}
    </div>
    ${sub &&
    subAnchor &&
    items[sub.index]?.submenu &&
    h(MenuList, {
      key: `sub-${sub.index}`,
      items: items[sub.index].submenu,
      level: level + 1,
      anchor: subAnchor,
      placement: 'right-start',
      offset: 2,
      bounds,
      inline: false,
      role: 'menu',
      label: items[sub.index].label,
      autoFocus: sub.focus,
      onDone,
      onSelect,
      onCloseLevel: () => {
        setSub(null);
        ref.current?.focus({ preventScroll: true });
      },
    })}
  `;
}

/**
 * `menus.popIn`'s exit: the closing panel is left behind as an inert clone that sinks back the way
 * it came — towards its anchor, using the same `--pop-dx` / `--pop-dy` direction `placeFloating`
 * wrote for the pop-in — so a menu does not simply blink out of existence.
 *
 * Preact runs a component's own hook cleanups **before** it recurses into its children and before
 * any DOM is removed, so `find()` still returns the live panel here even when a child rendered it
 * (motion.js `useExitGhost`).
 * @param {() => Element|null|undefined} find
 */
function usePopExitGhost(find) {
  motion.useExitGhost(MENU_KEY, find, () => {
    const el = find();
    const axis = (name, fallback) => Number.parseFloat(el ? getComputedStyle(el).getPropertyValue(name) : '') || fallback;
    const d = motion.distance(3);
    return [
      { opacity: 1, translate: 'none', scale: 1 },
      { opacity: 0, translate: `${axis('--pop-dx', 0) * d}px ${axis('--pop-dy', -1) * d}px`, scale: 1 - motion.distance(0.015) },
    ];
  });
}

function MenuLayer(props) {
  const layer = useRef(null);
  const latest = useLatest(props);
  const done = (reason) => latest.current.onClose?.(reason);
  usePopExitGhost(() => layer.current?.querySelector('.menu'));
  // Captured during the first render: child layout effects (the menu focusing itself) run before
  // this component's own effects.
  const previousFocusRef = useRef(undefined);
  if (previousFocusRef.current === undefined) previousFocusRef.current = document.activeElement;

  useLayoutEffect(() => {
    const previousFocus = previousFocusRef.current;
    return () => {
      const focusLost = !document.activeElement || document.activeElement === document.body || layer.current?.contains(document.activeElement);
      if (latest.current.restoreFocus !== false && focusLost && previousFocus?.isConnected) previousFocus.focus?.({ preventScroll: true });
    };
  }, []);

  useEffect(() => {
    const anchorEl = latest.current.anchor instanceof Element ? latest.current.anchor : null;
    const onPointerDown = (e) => {
      if (insideLayer(layer.current, e.target) || anchorEl?.contains(e.target)) return;
      done('outside');
    };
    const onBlur = () => done('blur');
    const onResize = () => done('resize');
    const onDismiss = () => latest.current.closeOnDismiss !== false && done('dismiss');
    document.addEventListener('pointerdown', onPointerDown, true);
    document.addEventListener(DISMISS_EVENT, onDismiss);
    window.addEventListener('blur', onBlur);
    window.addEventListener('resize', onResize);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown, true);
      document.removeEventListener(DISMISS_EVENT, onDismiss);
      window.removeEventListener('blur', onBlur);
      window.removeEventListener('resize', onResize);
    };
  }, []);

  const { items, x, y, anchor, placement, offset, bounds, role, label, minWidth, initialIndex, onSelect, class: className, autoFocus } = props;
  const anchorRect = useMemo(() => (anchor ? rectOf(anchor) : { left: x ?? 0, right: x ?? 0, top: y ?? 0, bottom: y ?? 0 }), []);
  return html`<div ref=${layer} class="menu-layer">
    <${MenuList}
      items=${items}
      level=${0}
      anchor=${anchorRect}
      placement=${placement ?? 'bottom-start'}
      offset=${offset ?? (anchor ? 4 : 0)}
      bounds=${bounds}
      role=${role}
      label=${label}
      minWidth=${minWidth}
      autoFocus=${autoFocus}
      initialIndex=${initialIndex}
      onDone=${done}
      onSelect=${onSelect}
      className=${className}
    />
  </div>`;
}

/**
 * Popup menu (context menus, "…" menus, select lists) with keyboard navigation (↑ ↓ Home End,
 * → ← submenus, Enter/Space, Esc, type-ahead), hover-opened submenus, outside-click / blur /
 * resize dismissal, and viewport clamping. Render it while open; it closes by calling `onClose`.
 *
 * ```js
 * const [menu, setMenu] = useState(null);
 * <div onContextMenu=${(e) => { e.preventDefault(); setMenu({ x: e.clientX, y: e.clientY }); }}>
 * ${menu && html`<${Menu} x=${menu.x} y=${menu.y} items=${items} onClose=${() => setMenu(null)} />`}
 * ```
 *
 * @param {object} props
 * @param {MenuEntry[]} props.items
 * @param {number} [props.x] viewport point (context menus) …
 * @param {number} [props.y]
 * @param {Element|{left:number,top:number,right:number,bottom:number}} [props.anchor] … or an anchor element/rect
 * @param {string} [props.placement] see `placeFloating` (default `bottom-start`)
 * @param {(reason: 'select'|'escape'|'tab'|'outside'|'blur'|'resize'|'dismiss') => void} props.onClose
 * @param {(item: MenuItem) => void} [props.onSelect] called after the item's own `onSelect`
 * @param {'menu'|'listbox'} [props.role]
 * @param {string} [props.label] accessible name
 * @param {number} [props.initialIndex] highlighted item on open
 * @param {boolean} [props.autoFocus=true] focus the menu (keyboard navigation) on open
 * @param {number} [props.minWidth]
 * @param {object} [props.bounds] clamp rect (default viewport)
 * @param {boolean} [props.inline] render statically in place (galleries/tests): no portal, positioning or dismissal
 * @param {boolean} [props.restoreFocus=true]
 * @param {boolean} [props.closeOnDismiss=true] close on `DISMISS_EVENT` (false: the menu is core state, e.g. the app menu panel)
 */
export function Menu(props) {
  const { inline = false, role = 'menu', autoFocus = true } = props;
  if (inline) {
    return h(MenuList, {
      ...props,
      level: 0,
      role,
      autoFocus: false,
      anchor: null,
      onDone: (reason) => props.onClose?.(reason),
      onCloseLevel: () => {},
      className: props.class,
    });
  }
  return h(Portal, null, h(MenuLayer, { ...props, role, autoFocus }));
}

// ------------------------------------------------------------------------------------ Popover

function PopoverLayer(props) {
  const ref = useRef(null);
  const latest = useLatest(props);
  usePopExitGhost(() => ref.current);

  useLayoutEffect(() => {
    const el = ref.current;
    const { anchor, placement = 'bottom-start', offset = 6, bounds, autoFocus = true } = latest.current;
    const place = () => placeFloating(el, rectOf(anchor), { placement, offset, bounds });
    place();
    const ro = new ResizeObserver(place);
    ro.observe(el);
    // The viewport can change under an open popover (e.g. the floating sidebar docking for it).
    window.addEventListener('resize', place);
    const previousFocus = document.activeElement;
    if (autoFocus) (el.querySelector('[autofocus]') ?? el.querySelector(FOCUSABLE) ?? el).focus({ preventScroll: true });
    return () => {
      ro.disconnect();
      window.removeEventListener('resize', place);
      const active = document.activeElement;
      if (latest.current.restoreFocus !== false && (!active || active === document.body || el.contains(active)) && previousFocus?.isConnected) {
        previousFocus.focus?.({ preventScroll: true });
      }
    };
  }, []);

  useEffect(() => {
    const anchorEl = latest.current.anchor instanceof Element ? latest.current.anchor : null;
    const onPointerDown = (e) => {
      if (insideLayer(ref.current, e.target) || anchorEl?.contains(e.target)) return;
      latest.current.onClose?.('outside');
    };
    const onKeyDown = (e) => {
      if (e.key === 'Escape' && !e.defaultPrevented) {
        e.preventDefault();
        latest.current.onClose?.('escape');
      }
    };
    const onDismiss = () => latest.current.closeOnDismiss !== false && latest.current.onClose?.('dismiss');
    document.addEventListener('pointerdown', onPointerDown, true);
    document.addEventListener('keydown', onKeyDown);
    document.addEventListener(DISMISS_EVENT, onDismiss);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown, true);
      document.removeEventListener('keydown', onKeyDown);
      document.removeEventListener(DISMISS_EVENT, onDismiss);
    };
  }, []);

  const { role = 'dialog', label, class: className, style, children } = props;
  return html`<div ref=${ref} class=${classNames('popover', className)} role=${role} aria-label=${label} tabindex="-1" style=${style}>
    ${children}
  </div>`;
}

/**
 * Anchored floating panel (downloads popover, space sheet…). Closes on outside pointerdown
 * (ignoring the anchor element, so toggle buttons work) and Esc; focuses its first focusable
 * element (or `[autofocus]`) and restores focus when it closes.
 * @param {object} props
 * @param {boolean} props.open
 * @param {Element|{left:number,top:number,right:number,bottom:number}} props.anchor
 * @param {string} [props.placement] default `bottom-start`
 * @param {number} [props.offset] default 6
 * @param {(reason: 'outside'|'escape'|'dismiss') => void} props.onClose
 * @param {string} [props.label] accessible name
 * @param {string} [props.role] default `dialog`
 * @param {boolean} [props.autoFocus=true]
 * @param {boolean} [props.closeOnDismiss=true] close on `DISMISS_EVENT` (false: the popover is core state, e.g. a sidebar panel)
 * @param {boolean} [props.inline] render statically in place (galleries/tests)
 */
export function Popover(props) {
  if (!props.open) return null;
  if (props.inline) {
    const { role = 'dialog', label, class: className, style, children } = props;
    return html`<div class=${classNames('popover is-inline', className)} role=${role} aria-label=${label} style=${style}>${children}</div>`;
  }
  return h(Portal, null, h(PopoverLayer, props));
}

// ------------------------------------------------------------------------------------ form controls

/**
 * Switch. With `label`, renders a full-width row (label left, switch right).
 * @param {{checked: boolean, onChange: (checked: boolean) => void, label?: string, description?: string, ariaLabel?: string, disabled?: boolean}} props
 */
export function Toggle({ checked, onChange, label, description, ariaLabel, disabled = false, class: className }) {
  const button = html`<button
    type="button"
    role="switch"
    class=${classNames('toggle', !label && className)}
    aria-checked=${String(Boolean(checked))}
    aria-label=${label ? undefined : ariaLabel}
    disabled=${disabled}
    onClick=${() => onChange?.(!checked)}
  ><span class="toggle-thumb" /></button>`;
  if (!label) return button;
  return html`<label class=${classNames('toggle-row', className)}>
    <span class="stack" style=${{ gap: '2px' }}>
      <span>${label}</span>
      ${description && html`<span class="field-hint">${description}</span>`}
    </span>
    ${button}
  </label>`;
}

/** Native checkbox with a label. */
export function Checkbox({ checked, onChange, label, disabled = false, class: className }) {
  return html`<label class=${classNames('checkbox', className)}>
    <input type="checkbox" checked=${Boolean(checked)} disabled=${disabled} onChange=${(e) => onChange?.(e.currentTarget.checked)} />
    <span>${label}</span>
  </label>`;
}

/**
 * Select built from a button + listbox Menu (consistent look in every CEF view; ↑/↓ on the closed
 * button change the value directly, Alt+↓ / Enter / Space / F4 open the list).
 * @param {{value: any, options: Array<{value: any, label: string, icon?: string, disabled?: boolean}>, onChange: (value: any) => void, label?: string, ariaLabel?: string, placeholder?: string, disabled?: boolean}} props
 */
export function Select({ value, options, onChange, label, ariaLabel, placeholder = 'Select…', disabled = false, class: className }) {
  const [open, setOpen] = useState(false);
  const button = useRef(null);
  const labelId = useStableId('select-label');
  const buttonId = useStableId('select');
  const index = options.findIndex((o) => o.value === value);
  const current = options[index];

  const step = (dir) => {
    for (let i = index + dir; i >= 0 && i < options.length; i += dir) {
      if (!options[i].disabled) {
        onChange?.(options[i].value);
        return;
      }
    }
  };
  const onKeyDown = (e) => {
    if ((e.key === 'ArrowDown' && e.altKey) || e.key === 'F4') setOpen(true);
    else if (e.key === 'ArrowDown') step(1);
    else if (e.key === 'ArrowUp') step(-1);
    else return;
    e.preventDefault();
  };

  const control = html`<button
      ref=${button}
      id=${buttonId}
      type="button"
      class=${classNames('select', !label && className)}
      aria-haspopup="listbox"
      aria-expanded=${String(open)}
      aria-labelledby=${label ? `${labelId} ${buttonId}` : undefined}
      aria-label=${label ? undefined : ariaLabel}
      disabled=${disabled}
      onClick=${() => setOpen((o) => !o)}
      onKeyDown=${onKeyDown}
    >
      <span class="select-value">${current ? current.label : placeholder}</span>
      <${Icon} name="chevron-down" size=${14} />
    </button>
    ${open &&
    html`<${Menu}
      role="listbox"
      anchor=${button.current}
      label=${label ?? ariaLabel}
      minWidth=${button.current?.offsetWidth}
      initialIndex=${index >= 0 ? index : undefined}
      items=${options.map((o) => ({ key: String(o.value), label: o.label, icon: o.icon, disabled: o.disabled, selected: o.value === value, onSelect: () => onChange?.(o.value) }))}
      onClose=${() => setOpen(false)}
    />`}`;

  if (!label) return control;
  return html`<div class=${classNames('field', className)}>
    <span class="field-label" id=${labelId}>${label}</span>
    ${control}
  </div>`;
}

function assignRef(ref, value) {
  if (typeof ref === 'function') ref(value);
  else if (ref) ref.current = value;
}

/**
 * Text input (or textarea with `multiline`) with optional label, hint/error, leading icon and
 * clear button. Controlled: pass `value` and `onInput(value)`.
 * @param {object} props
 * @param {string} props.value
 * @param {(value: string) => void} [props.onInput]
 * @param {(value: string) => void} [props.onCommit] Enter (Ctrl+Enter when multiline)
 * @param {() => void} [props.onCancel] Esc
 * @param {string} [props.label]
 * @param {string} [props.hint]
 * @param {string} [props.error] shown instead of the hint; sets aria-invalid
 * @param {string} [props.icon] leading glyph
 * @param {boolean} [props.clearable]
 * @param {boolean} [props.autoFocus]
 * @param {boolean} [props.selectOnFocus] select all when focused
 * @param {boolean} [props.multiline]
 * @param {object|Function} [props.inputRef]
 *   plus `placeholder`, `type`, `disabled`, `maxLength`, `name`, `spellcheck`, `onBlur`…
 */
export function TextField(props) {
  const {
    value,
    onInput,
    onCommit,
    onCancel,
    label,
    hint,
    error,
    icon,
    clearable = false,
    autoFocus = false,
    selectOnFocus = false,
    multiline = false,
    inputRef,
    type = 'text',
    class: className,
    ...rest
  } = props;
  const el = useRef(null);
  const id = useStableId('field');
  const hintId = useStableId('field-hint');

  useLayoutEffect(() => {
    if (autoFocus && el.current) {
      el.current.focus({ preventScroll: true });
      if (selectOnFocus) el.current.select();
    }
  }, []);

  const setRef = (node) => {
    el.current = node;
    assignRef(inputRef, node);
  };

  const onKeyDown = (e) => {
    if (e.key === 'Enter' && onCommit && (!multiline || e.ctrlKey) && !e.isComposing) {
      e.preventDefault();
      onCommit(e.currentTarget.value);
    } else if (e.key === 'Escape' && onCancel) {
      e.preventDefault();
      e.stopPropagation();
      onCancel();
    }
    rest.onKeyDown?.(e);
  };

  const inputProps = {
    ...rest,
    ref: setRef,
    id,
    class: 'input',
    value: value ?? '',
    spellcheck: rest.spellcheck ?? false,
    'aria-invalid': error ? 'true' : undefined,
    'aria-describedby': error || hint ? hintId : undefined,
    onInput: (e) => onInput?.(e.currentTarget.value),
    onKeyDown,
    onFocus: (e) => {
      if (selectOnFocus) e.currentTarget.select();
      rest.onFocus?.(e);
    },
  };
  const input = multiline ? h('textarea', inputProps) : h('input', { ...inputProps, type });
  const wrapped =
    icon || clearable
      ? html`<div class="input-wrap">
          ${icon && html`<span class="input-icon"><${Icon} name=${icon} size=${16} /></span>`}
          ${input}
          ${clearable &&
          value &&
          html`<${IconButton}
            class="input-clear"
            icon="close"
            label="Clear"
            size="sm"
            muted
            onClick=${() => {
              onInput?.('');
              el.current?.focus();
            }}
          />`}
        </div>`
      : input;

  if (!label && !hint && !error) {
    return className ? html`<div class=${className}>${wrapped}</div>` : wrapped;
  }
  return html`<div class=${classNames('field', className)}>
    ${label && html`<label class="field-label" for=${id}>${label}</label>`}
    ${wrapped}
    ${error ? html`<span class="field-error" id=${hintId}>${error}</span>` : hint && html`<span class="field-hint" id=${hintId}>${hint}</span>`}
  </div>`;
}

// ------------------------------------------------------------------------------------ EmojiPicker

/** ~120 common emoji for space icons, roughly grouped (places, work, hobbies, nature, symbols). */
export const COMMON_EMOJI = Object.freeze([
  '🏠', '🚀', '💼', '🎮', '🎨', '🎵', '📚', '🧪', '💡', '🔥',
  '⭐', '❤️', '🌙', '☀️', '🌈', '🌊', '🌲', '🌸', '🍀', '🌻',
  '🍎', '🍕', '☕', '🍷', '🎉', '🎁', '🎯', '🏆', '⚽', '🏀',
  '🎾', '🚴', '🏃', '🧘', '✈️', '🚗', '🚲', '🗺️', '🏖️', '🏔️',
  '🏕️', '🌍', '💻', '🖥️', '⌨️', '📱', '📷', '🎬', '🎧', '🎤',
  '🎸', '🎹', '📝', '📌', '📎', '📁', '🗂️', '📊', '📈', '🗓️',
  '⏰', '🔔', '🔒', '🔑', '🛠️', '⚙️', '🧰', '🔬', '🔭', '🧬',
  '🩺', '🏥', '🏦', '💰', '💳', '🛒', '🛍️', '📦', '🏷️', '👶',
  '🐶', '🐱', '🦊', '🐻', '🐼', '🐸', '🐙', '🦄', '🐝', '🦋',
  '🌵', '🍄', '🍁', '❄️', '⚡', '💧', '✨', '💎', '👑', '🎓',
  '🏛️', '⚖️', '📰', '💬', '📣', '🧠', '👀', '🤖', '👾', '👻',
  '🧩', '♟️', '🎲', '😀', '😎', '🤓', '🥳', '👍', '💪', '✅',
]);

/**
 * Compact emoji grid plus a free-text field (any emoji or up to two characters).
 * Arrow keys move within the grid; Enter/Space selects.
 * @param {{value?: string, onSelect: (icon: string) => void, label?: string, columns?: number, emoji?: string[], showInput?: boolean}} props
 */
export function EmojiPicker({ value, onSelect, label = 'Icon', columns = 10, emoji = COMMON_EMOJI, showInput = true, class: className }) {
  const selectedIndex = emoji.indexOf(value);
  const [focusIndex, setFocusIndex] = useState(Math.max(0, selectedIndex));
  const [custom, setCustom] = useState(value && selectedIndex < 0 ? value : '');
  const grid = useRef(null);

  const focusCell = (i) => {
    const next = clamp(i, 0, emoji.length - 1);
    setFocusIndex(next);
    grid.current?.children[next]?.focus();
  };

  const onKeyDown = (e) => {
    const moves = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: columns, ArrowUp: -columns };
    if (e.key in moves) focusCell(focusIndex + moves[e.key]);
    else if (e.key === 'Home') focusCell(0);
    else if (e.key === 'End') focusCell(emoji.length - 1);
    else return;
    e.preventDefault();
  };

  return html`<div class=${classNames('emoji-picker', className)}>
    ${showInput &&
    html`<${TextField}
      value=${custom}
      placeholder="Type any emoji or text"
      aria-label=${`${label}: custom`}
      maxLength=${16}
      onInput=${(raw) => {
        setCustom(raw);
        const icon = firstGraphemes(raw.trim(), 2);
        if (icon) onSelect?.(icon);
      }}
    />`}
    <div
      ref=${grid}
      class="emoji-grid"
      role="listbox"
      aria-label=${label}
      style=${{ '--emoji-cols': columns }}
      onKeyDown=${onKeyDown}
    >
      ${emoji.map(
        (e, i) => html`<button
          key=${e}
          type="button"
          class="emoji-cell"
          role="option"
          aria-selected=${String(e === value)}
          aria-label=${e}
          tabindex=${i === focusIndex ? 0 : -1}
          onFocus=${() => setFocusIndex(i)}
          onClick=${() => {
            setCustom('');
            onSelect?.(e);
          }}
        >${e}</button>`,
      )}
    </div>
  </div>`;
}
