// Shell fallback for overlay surfaces (find, permission, switcher, toast, peek) whose real
// ui/<host>/ page does not exist yet. Served by scheme.rs only when the UI asset is missing.
// Minimal but functional: renders UiState intents, sizes itself with surface.setSize, sends ui.ready.

const host = location.host;
const query = typeof window.__staQuery === 'function' ? window.__staQuery.bind(window) : null;
const root = document.getElementById('root');
let state = null;
let handledSeq = -1;
let toastTimer = null;
let toastId = null;

function invoke(cmd, payload = null) {
  return new Promise((resolve, reject) => {
    if (!query) return reject(Object.assign(new Error('IPC unavailable'), { code: 'unavailable' }));
    query({
      request: JSON.stringify({ cmd, payload }),
      persistent: false,
      onSuccess: (r) => resolve(r === '' ? null : JSON.parse(r)),
      onFailure: (code, message) => reject(Object.assign(new Error(message), { code })),
    });
  });
}
const dispatch = (command) => invoke('dispatch', command);

function el(tag, props = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === 'class') e.className = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v);
  }
  for (const c of children) e.append(typeof c === 'string' ? document.createTextNode(c) : c);
  return e;
}

let lastSize = '';
function size(width, height) {
  const key = `${width}x${height}`;
  if (key === lastSize) return;
  lastSize = key;
  root.style.width = width ? `${width}px` : '';
  root.style.height = `${height}px`;
  invoke('surface.setSize', width ? { width, height } : { height }).catch(() => {});
}

function applyTheme(s) {
  const space = s.spaces && s.spaces.find((sp) => sp.id === s.activeSpace);
  const c = space && space.colors;
  if (!c) return;
  const map = { surface: '--surface', border: '--border', text: '--text', textMuted: '--text-muted', accent: '--accent' };
  for (const [k, v] of Object.entries(map)) if (c[k]) document.documentElement.style.setProperty(v, c[k]);
  document.documentElement.dataset.theme = s.dark ? 'dark' : 'light';
}

const renderers = {
  find(s) {
    const f = s.find;
    root.className = '';
    if (!f) return root.replaceChildren();
    if (f.seq !== handledSeq || !root.querySelector('input')) {
      const input = el('input', { id: 'input', placeholder: 'Find in page', value: f.text || '' });
      input.value = f.text || '';
      input.addEventListener('keydown', (e) => {
        if (e.key === 'Escape') dispatch({ type: 'closeFind' });
        if (e.key === 'Enter') dispatch({ type: 'findInPage', text: input.value, forward: !e.shiftKey, matchCase: false, findNext: true });
      });
      input.addEventListener('input', () => dispatch({ type: 'findInPage', text: input.value, forward: true, matchCase: false, findNext: false }));
      root.replaceChildren(input, el('span', { id: 'count', class: 'muted' }, ''), el('button', { onclick: () => dispatch({ type: 'closeFind' }) }, '×'));
      if (f.seq !== handledSeq) {
        handledSeq = f.seq;
        input.focus();
        input.select();
      }
    }
    size(360, 44);
  },
  permission(s) {
    const p = s.permissionPrompts && s.permissionPrompts[0];
    root.className = 'column';
    if (!p) return root.replaceChildren();
    const answer = (allow) => () => dispatch({ type: 'resolvePermission', id: p.id, allow, remember: false });
    root.replaceChildren(
      el('div', { class: 'ellipsis' }, `${p.host} wants to use your ${p.kinds.join(', ')}`),
      el('div', { class: 'row' }, el('button', { id: 'block', onclick: answer(false) }, 'Block'), el('button', { id: 'allow', onclick: answer(true) }, 'Allow')),
    );
    size(340, 84);
  },
  switcher(s) {
    const sw = s.switcher;
    if (!sw) return root.replaceChildren();
    root.className = '';
    const cards = sw.tabs.map((t, i) =>
      el('div', { class: `card${i === sw.selected ? ' sel' : ''}`, onclick: () => dispatch({ type: 'mruSelect', index: i }) }, el('div', { class: 'ellipsis' }, t.title || t.url)),
    );
    root.replaceChildren(el('div', { class: 'cards' }, ...cards));
    size(sw.tabs.length * 140 + 20, 170);
  },
  toast(s) {
    const t = s.toast;
    root.className = '';
    if (!t) {
      toastId = null;
      clearTimeout(toastTimer);
      return root.replaceChildren();
    }
    const parts = [el('span', { class: 'ellipsis', id: 'message' }, t.message)];
    if (t.action) {
      parts.push(el('button', { onclick: () => { dispatch(t.action.command); dispatch({ type: 'dismissToast', id: t.id }); } }, t.action.label));
    }
    root.replaceChildren(...parts);
    if (toastId !== t.id) {
      toastId = t.id;
      clearTimeout(toastTimer);
      const id = t.id;
      toastTimer = setTimeout(() => dispatch({ type: 'dismissToast', id }), t.durationMs);
    }
    size(Math.min(480, Math.max(160, 40 + t.message.length * 7 + (t.action ? 90 : 0))), 36);
  },
  peek(s) {
    const p = s.peek;
    root.className = '';
    if (!p) return root.replaceChildren();
    const buttons = [el('button', { id: 'close', onclick: () => dispatch({ type: 'closePeek' }) }, '×')];
    if (!p.popup) {
      buttons.push(el('button', { id: 'split', onclick: () => dispatch({ type: 'expandPeek', split: true }) }, 'Split'));
      buttons.push(el('button', { id: 'expand', onclick: () => dispatch({ type: 'expandPeek' }) }, 'Expand'));
    }
    root.replaceChildren(...buttons, el('span', { class: 'ellipsis muted' }, p.tab.host || p.tab.url));
    root.style.width = '100%';
    size(null, 40);
  },
};

function accept(s) {
  if (!s || (state && s.revision < state.revision)) return;
  state = s;
  applyTheme(s);
  (renderers[host] || (() => {}))(s);
}

if (query) {
  query({
    request: JSON.stringify({ cmd: '__subscribe', payload: null }),
    persistent: true,
    onSuccess: (raw) => {
      const msg = JSON.parse(raw);
      if (msg.event === 'state') accept(msg.payload);
      if (msg.event === 'find.result' && host === 'find') {
        const c = document.getElementById('count');
        if (c) c.textContent = msg.payload.count ? `${msg.payload.active}/${msg.payload.count}` : '0/0';
      }
    },
    onFailure: () => {},
  });
}

window.sta = { invoke, dispatch, getState: () => state, placeholder: true };
invoke('state.get').then(accept).then(() => invoke('ui.ready')).catch((e) => console.error(e));
