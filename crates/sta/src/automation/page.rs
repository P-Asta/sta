//! Page access for tools [owner: automation]: the main frame and its loader, an isolated world per
//! document, element refs, and running functions on the document or an element in that world
//! (`DOM.resolveNode` + `Runtime.callFunctionOn`, which fail closed after a navigation).
//!
//! Page scripts run only in the isolated world `sta-agent` (the page can't see or change them)
//! and only as the fixed functions in this file.

use super::cdp::{self, CdpError, p};
use sta_core::Id;
use sta_core::agent::channel::ToolError;
use sta_core::agent::refs::{RefProblem, RefTable};
use sta_core::agent::ErrorCode;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

/// Name of the isolated world.
const WORLD: &str = "sta-agent";
/// Default timeout of a single DevTools call.
pub const CALL_MS: i64 = 5_000;

#[derive(Debug, Clone)]
pub struct Frame {
    pub id: String,
    pub loader_id: String,
}

#[derive(Debug, Clone)]
struct RefState {
    table: RefTable<()>,
    loader_id: String,
    cdp_generation: u64,
}

thread_local! {
    static REFS: RefCell<HashMap<Id, RefState>> = RefCell::new(HashMap::new());
    /// (browser, loader id) → isolated world context id.
    static WORLDS: RefCell<HashMap<(i32, String), i64>> = RefCell::new(HashMap::new());
}

pub fn cdp_error(e: CdpError) -> ToolError {
    match e {
        CdpError::Timeout => ToolError::new(ErrorCode::Timeout, "The page didn't answer in time (busy, or blocked by a dialog)"),
        CdpError::NoBrowser => ToolError::new(ErrorCode::TabNotLoaded, "The tab's page is gone"),
        CdpError::Detached => ToolError::new(ErrorCode::StaleRef, "The page was replaced"),
        CdpError::SendFailed => ToolError::new(ErrorCode::Internal, "The page could not be reached"),
        CdpError::Protocol { message, .. } => ToolError::new(ErrorCode::Internal, format!("Page error: {}", message.chars().take(200).collect::<String>())),
    }
}

/// Milliseconds left until `deadline`, capped for one call.
pub fn left(deadline: Instant, cap: i64) -> i64 {
    let ms = deadline.saturating_duration_since(Instant::now()).as_millis() as i64;
    ms.clamp(1, cap)
}

pub async fn call<P: cdp::Params>(browser: i32, params: P, deadline: Instant) -> Result<Value, ToolError> {
    cdp::call(browser, params, left(deadline, CALL_MS)).await.map_err(cdp_error)
}

/// The main frame (id, loader, URL) from `Page.getFrameTree`.
pub async fn main_frame(browser: i32, deadline: Instant) -> Result<Frame, ToolError> {
    let v = call(browser, p::GetFrameTree {}, deadline).await?;
    let f = &v["frameTree"]["frame"];
    Ok(Frame { id: f["id"].as_str().unwrap_or_default().to_string(), loader_id: f["loaderId"].as_str().unwrap_or_default().to_string() })
}

/// The isolated world of the frame's current document.
pub async fn world(browser: i32, frame: &Frame, deadline: Instant) -> Result<i64, ToolError> {
    let key = (browser, frame.loader_id.clone());
    if let Some(ctx) = WORLDS.with(|w| w.borrow().get(&key).copied()) {
        return Ok(ctx);
    }
    let v = call(browser, p::CreateIsolatedWorld { frame_id: frame.id.clone(), world_name: WORLD.into() }, deadline).await?;
    let ctx = v["executionContextId"].as_i64().ok_or_else(|| ToolError::new(ErrorCode::Internal, "No isolated world"))?;
    WORLDS.with(|w| {
        let mut w = w.borrow_mut();
        w.retain(|(b, _), _| *b != browser);
        w.insert(key, ctx);
    });
    Ok(ctx)
}

fn stale(message: &str) -> ToolError {
    ToolError::new(ErrorCode::StaleRef, message)
}

/// Runs `function` (a JS function declaration; `this` = the object) on a resolved object and returns
/// its JSON value.
async fn call_on(browser: i32, object_id: String, function: &str, args: Vec<Value>, deadline: Instant, cap: i64) -> Result<Value, ToolError> {
    let arguments = args.into_iter().map(|v| p::CallArgument { object_id: None, value: Some(v) }).collect();
    let r = cdp::call(
        browser,
        p::CallFunctionOn { function_declaration: function.into(), object_id: object_id.clone(), arguments, return_by_value: true, await_promise: false, silent: true },
        left(deadline, cap),
    )
    .await;
    // Objects of the isolated world are released with it; release explicitly anyway.
    super::exec::spawn(async move {
        let _ = cdp::call(browser, p::ReleaseObject { object_id }, 2_000).await;
    });
    let v = r.map_err(cdp_error)?;
    if let Some(ex) = v.get("exceptionDetails") {
        let text = ex["exception"]["description"].as_str().or(ex["text"].as_str()).unwrap_or("script error");
        return Err(ToolError::new(ErrorCode::Internal, format!("Page script failed: {}", text.chars().take(200).collect::<String>())));
    }
    Ok(v["result"]["value"].clone())
}

/// Resolves a backend node in the isolated world of the current document. Fails closed
/// (`stale_ref`) when the node belongs to another document.
async fn resolve(browser: i32, backend_node_id: i64, ctx: i64, deadline: Instant) -> Result<String, ToolError> {
    match cdp::call(browser, p::ResolveNode { backend_node_id, execution_context_id: Some(ctx) }, left(deadline, CALL_MS)).await {
        Ok(v) => v["object"]["objectId"].as_str().map(str::to_string).ok_or_else(|| stale("The element is gone")),
        Err(CdpError::Protocol { .. }) => Err(stale("The element is no longer in the page")),
        Err(e) => Err(cdp_error(e)),
    }
}

/// Runs `function` with `this` = the document of the main frame.
pub async fn eval_document(browser: i32, frame: &Frame, function: &str, args: Vec<Value>, deadline: Instant, cap: i64) -> Result<Value, ToolError> {
    let ctx = world(browser, frame, deadline).await?;
    let doc = call(browser, p::GetDocument { depth: 0 }, deadline).await?;
    let backend = doc["root"]["backendNodeId"].as_i64().ok_or_else(|| ToolError::new(ErrorCode::Internal, "No document"))?;
    let object = match resolve(browser, backend, ctx, deadline).await {
        Ok(o) => o,
        Err(_) => {
            // The cached world may belong to a document that just went away: once more, fresh.
            WORLDS.with(|w| w.borrow_mut().retain(|(b, _), _| *b != browser));
            let ctx = world(browser, frame, deadline).await?;
            resolve(browser, backend, ctx, deadline).await?
        }
    };
    call_on(browser, object, function, args, deadline, cap).await
}

/// Runs `function` with `this` = an element (by backend node id) of the current document.
pub async fn eval_node(browser: i32, frame: &Frame, backend_node_id: i64, function: &str, args: Vec<Value>, deadline: Instant) -> Result<Value, ToolError> {
    let ctx = world(browser, frame, deadline).await?;
    let object = resolve(browser, backend_node_id, ctx, deadline).await.map_err(|e| {
        if e.code == ErrorCode::StaleRef {
            ToolError::new(ErrorCode::UnsupportedFrame, "The element can't be reached (it may be inside a frame)")
                .with_hint("Elements inside frames aren't supported yet; call page_snapshot again or open the frame's URL with tab_open.")
        } else {
            e
        }
    })?;
    call_on(browser, object, function, args, deadline, CALL_MS).await
}

/// Runs `function(other)` with `this` = element `a` and `other` = element `b`.
pub async fn eval_pair(browser: i32, frame: &Frame, a: i64, b: i64, function: &str, deadline: Instant) -> Result<Value, ToolError> {
    let ctx = world(browser, frame, deadline).await?;
    let oa = resolve(browser, a, ctx, deadline).await?;
    let ob = resolve(browser, b, ctx, deadline).await?;
    let r = cdp::call(
        browser,
        p::CallFunctionOn {
            function_declaration: function.into(),
            object_id: oa.clone(),
            arguments: vec![p::CallArgument { object_id: Some(ob.clone()), value: None }],
            return_by_value: true,
            await_promise: false,
            silent: true,
        },
        left(deadline, CALL_MS),
    )
    .await;
    super::exec::spawn(async move {
        let _ = cdp::call(browser, p::ReleaseObject { object_id: oa }, 2_000).await;
        let _ = cdp::call(browser, p::ReleaseObject { object_id: ob }, 2_000).await;
    });
    Ok(r.map_err(cdp_error)?["result"]["value"].clone())
}

/// `evaluate`: runs an agent-written function with `this` = the document (or the element
/// `element`, which is also its first argument), in the isolated world or — `main_world` — in the
/// page's own world (`DOM.resolveNode` without a context resolves there). Promises are awaited.
/// `Ok(Ok(remote object))`, or `Ok(Err(message))` when the function threw, didn't compile or its
/// result can't be returned as JSON.
pub async fn run_agent_function(browser: i32, frame: &Frame, main_world: bool, element: Option<i64>, function: &str, deadline: Instant, cap: i64) -> Result<Result<Value, String>, ToolError> {
    let doc = call(browser, p::GetDocument { depth: 0 }, deadline).await?;
    let doc_backend = doc["root"]["backendNodeId"].as_i64().ok_or_else(|| ToolError::new(ErrorCode::Internal, "No document"))?;
    let target_backend = element.unwrap_or(doc_backend);
    let resolve_in = |ctx: Option<i64>| async move {
        match cdp::call(browser, p::ResolveNode { backend_node_id: target_backend, execution_context_id: ctx }, left(deadline, CALL_MS)).await {
            Ok(v) => v["object"]["objectId"].as_str().map(str::to_string).ok_or_else(|| stale("The element is gone")),
            Err(CdpError::Protocol { .. }) => Err(stale("The element is no longer in the page")),
            Err(e) => Err(cdp_error(e)),
        }
    };
    let object = if main_world {
        resolve_in(None).await?
    } else {
        let ctx = world(browser, frame, deadline).await?;
        match resolve_in(Some(ctx)).await {
            Ok(o) => o,
            Err(e) if element.is_some() => return Err(e),
            Err(_) => {
                WORLDS.with(|w| w.borrow_mut().retain(|(b, _), _| *b != browser));
                let ctx = world(browser, frame, deadline).await?;
                resolve_in(Some(ctx)).await?
            }
        }
    };
    let arguments = if element.is_some() { vec![p::CallArgument { object_id: Some(object.clone()), value: None }] } else { Vec::new() };
    let r = cdp::call(
        browser,
        p::CallFunctionOn { function_declaration: function.into(), object_id: object.clone(), arguments, return_by_value: true, await_promise: true, silent: true },
        left(deadline, cap),
    )
    .await;
    super::exec::spawn(async move {
        let _ = cdp::call(browser, p::ReleaseObject { object_id: object }, 2_000).await;
    });
    match r {
        Ok(v) => match v.get("exceptionDetails") {
            Some(ex) => {
                let text = ex["exception"]["description"].as_str().or(ex["exception"]["value"].as_str()).or(ex["text"].as_str()).unwrap_or("The function threw");
                Ok(Err(text.chars().take(1000).collect()))
            }
            None => Ok(Ok(v["result"].clone())),
        },
        Err(CdpError::Protocol { message, .. }) => Ok(Err(message.chars().take(300).collect())),
        Err(e) => Err(cdp_error(e)),
    }
}

// ----------------------------------------------------------------------------------- refs

/// Hands out refs for a snapshot of `frame`'s document (a new document starts a new generation).
pub fn with_refs<R>(tab: Id, browser: i32, frame: &Frame, f: impl FnOnce(&mut RefTable<()>) -> R) -> R {
    let generation = cdp::generation(browser);
    REFS.with(|r| {
        let mut r = r.borrow_mut();
        let state = r.entry(tab).or_insert_with(|| RefState { table: RefTable::new(tab), loader_id: frame.loader_id.clone(), cdp_generation: generation });
        if state.loader_id != frame.loader_id || state.cdp_generation != generation {
            state.table.invalidate();
            state.loader_id = frame.loader_id.clone();
            state.cdp_generation = generation;
        }
        f(&mut state.table)
    })
}

/// The backend node id of a ref in the tab's *current* document (`stale_ref` otherwise).
pub fn resolve_ref(tab: Id, browser: i32, frame: &Frame, text: &str) -> Result<i64, ToolError> {
    let generation = cdp::generation(browser);
    REFS.with(|r| {
        let mut r = r.borrow_mut();
        let Some(state) = r.get_mut(&tab) else {
            return Err(match sta_core::agent::refs::RefId::parse(text) {
                None => ToolError::new(ErrorCode::InvalidArguments, format!("{text:?} is not a ref")).with_hint("Refs look like 12.1.5; take them from page_snapshot."),
                Some(id) if id.tab != tab => ToolError::new(ErrorCode::InvalidArguments, format!("Ref {text} belongs to tab {}", id.tab)),
                Some(_) => stale(&format!("Ref {text} is not from the current page")),
            });
        };
        if state.loader_id != frame.loader_id || state.cdp_generation != generation {
            state.table.invalidate();
            state.loader_id = frame.loader_id.clone();
            state.cdp_generation = generation;
        }
        match state.table.resolve(text) {
            Ok((backend, ())) => Ok(backend),
            Err(RefProblem::Malformed) => Err(ToolError::new(ErrorCode::InvalidArguments, format!("{text:?} is not a ref")).with_hint("Refs look like 12.1.5; take them from page_snapshot.")),
            Err(RefProblem::WrongTab) => Err(ToolError::new(ErrorCode::InvalidArguments, format!("Ref {text} belongs to another tab")).with_hint("Omit `tab` (the ref names its tab) or pass the tab the ref came from.")),
            Err(RefProblem::Stale) => Err(stale(&format!("Ref {text} is from an earlier version of the page"))),
        }
    })
}

/// The tab a ref names (`None` for text that isn't a ref).
pub fn ref_tab(text: &str) -> Option<Id> {
    sta_core::agent::refs::RefId::parse(text).map(|r| r.tab)
}

/// The agent detached from a browser: its worlds are gone.
pub fn on_agent_detached(browser: i32) {
    WORLDS.with(|w| w.borrow_mut().retain(|(b, _), _| *b != browser));
}

pub fn forget_tab(tab: Id) {
    REFS.with(|r| r.borrow_mut().remove(&tab));
}

pub fn on_browser_closed(browser: i32) {
    on_agent_detached(browser);
}

pub fn clear() {
    REFS.with(|r| r.borrow_mut().clear());
    WORLDS.with(|w| w.borrow_mut().clear());
    AX_USE.with(|a| a.borrow_mut().clear());
}

// ----------------------------------------------------------------------------------- scripts

/// `this` = document: viewport and state facts.
pub const PAGE_STATE: &str = "function() { return { visibility: this.visibilityState, readyState: this.readyState, width: innerWidth, height: innerHeight, dpr: devicePixelRatio, title: this.title }; }";

/// `this` = document or element, `format` = "text" | "markdown": readable text (at most 4 MB).
pub const EXTRACT_TEXT: &str = r#"function(format) {
  const root = this.nodeType === 9 ? (this.body || this.documentElement) : this;
  if (!root) return '';
  const limit = 4 * 1024 * 1024;
  if (format !== 'markdown') {
    const t = (root.innerText !== undefined ? root.innerText : root.textContent) || '';
    return t.length > limit ? t.slice(0, limit) : t;
  }
  const out = [];
  let size = 0;
  const push = (s) => { if (size < limit) { out.push(s); size += s.length; } };
  const skip = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'SVG', 'CANVAS', 'IFRAME']);
  const block = new Set(['P', 'DIV', 'SECTION', 'ARTICLE', 'HEADER', 'FOOTER', 'MAIN', 'NAV', 'ASIDE', 'FORM', 'TABLE', 'TR', 'UL', 'OL', 'BLOCKQUOTE', 'PRE', 'FIGURE', 'DETAILS', 'DL', 'DT', 'DD', 'BR', 'HR']);
  const walk = (node, depth) => {
    if (depth > 200 || size >= limit) return;
    if (node.nodeType === 3) { const t = node.nodeValue.replace(/\s+/g, ' '); if (t.trim()) push(t); return; }
    if (node.nodeType !== 1) return;
    const el = node;
    if (skip.has(el.tagName)) return;
    const style = el.ownerDocument.defaultView && el.ownerDocument.defaultView.getComputedStyle(el);
    if (style && (style.display === 'none' || style.visibility === 'hidden')) return;
    const tag = el.tagName;
    const h = /^H([1-6])$/.exec(tag);
    if (h) { push('\n\n' + '#'.repeat(+h[1]) + ' '); for (const c of el.childNodes) walk(c, depth + 1); push('\n\n'); return; }
    if (tag === 'A' && el.getAttribute('href')) { push('['); for (const c of el.childNodes) walk(c, depth + 1); push('](' + el.href + ')'); return; }
    if (tag === 'LI') { push('\n- '); for (const c of el.childNodes) walk(c, depth + 1); return; }
    if (tag === 'IMG') { const alt = el.getAttribute('alt'); if (alt) push('![' + alt + ']'); return; }
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
    if (tag === 'TD' || tag === 'TH') { push(' | '); for (const c of el.childNodes) walk(c, depth + 1); return; }
    const isBlock = block.has(tag);
    if (isBlock) push('\n');
    for (const c of el.childNodes) walk(c, depth + 1);
    if (isBlock) push('\n');
  };
  walk(root, 0);
  return out.join('').replace(/[ \t]+\n/g, '\n').replace(/\n{3,}/g, '\n\n').trim();
}"#;

/// `this` = element: can text be typed into it? `{ok, kind, value, disabled}`.
pub const EDITABLE: &str = r#"function() {
  const el = this;
  const blocked = ['checkbox', 'radio', 'button', 'submit', 'reset', 'image', 'file', 'range', 'color', 'hidden'];
  let kind = null;
  if (el instanceof HTMLTextAreaElement) kind = 'textarea';
  else if (el instanceof HTMLInputElement) kind = blocked.includes(el.type) ? ('input-' + el.type) : 'input';
  else if (el.isContentEditable) kind = 'contenteditable';
  const value = kind === 'contenteditable' ? el.textContent : (el.value !== undefined ? String(el.value) : null);
  return { ok: kind === 'input' || kind === 'textarea' || kind === 'contenteditable', kind: kind || el.tagName.toLowerCase(), value, disabled: !!el.disabled || !!el.readOnly };
}"#;

/// `this` = element: is it the focused element (through shadow roots)?
pub const IS_FOCUSED: &str = r#"function() {
  let a = this.ownerDocument.activeElement;
  while (a && a.shadowRoot && a.shadowRoot.activeElement) a = a.shadowRoot.activeElement;
  return a === this;
}"#;

/// `this` = element: select its whole content (for `clear`).
pub const SELECT_ALL: &str = r#"function() {
  if (typeof this.select === 'function') { this.select(); return true; }
  const range = this.ownerDocument.createRange();
  range.selectNodeContents(this);
  const sel = this.ownerDocument.defaultView.getSelection();
  sel.removeAllRanges();
  sel.addRange(range);
  return true;
}"#;

/// `this` = element: its current value / text.
pub const VALUE_OF: &str = "function() { return this.isContentEditable ? this.textContent : (this.value !== undefined ? String(this.value) : null); }";

/// `this` = element, `other` = hit-test element: is `other` the element or inside it (shadow DOM
/// included)?
pub const CONTAINS: &str = r#"function(other) {
  let n = other;
  while (n) {
    if (n === this) return true;
    n = n.parentNode || (n instanceof ShadowRoot ? n.host : null);
  }
  return false;
}"#;

/// `this` = document, `text`: does the page show it?
pub const HAS_TEXT: &str = "function(text) { const b = this.body || this.documentElement; return !!b && (b.innerText || b.textContent || '').includes(text); }";

/// `this` = document, `selector`: `true` / `false`, or `"invalid"` for a bad selector.
pub const HAS_SELECTOR: &str = "function(sel) { try { return !!this.querySelector(sel); } catch (e) { return 'invalid'; } }";

/// Screenshot helper: `this` = element, bounding box in CSS px of the viewport.
pub const BOUNDS: &str = "function() { const r = this.getBoundingClientRect(); return { x: r.left, y: r.top, width: r.width, height: r.height }; }";

/// `this` = document: size of the whole scrollable page in CSS px (full-page screenshots).
pub const PAGE_SIZE: &str = r#"function() {
  const s = this.scrollingElement || this.documentElement;
  const b = this.body;
  const width = Math.max(s ? s.scrollWidth : 0, b ? b.scrollWidth : 0, innerWidth);
  const height = Math.max(s ? s.scrollHeight : 0, b ? b.scrollHeight : 0, innerHeight);
  return { width, height, dpr: devicePixelRatio, visibility: this.visibilityState, viewportWidth: innerWidth, viewportHeight: innerHeight };
}"#;

/// `this` = element: what kind of form field it is (`fill_form`, `select_option`):
/// `{kind: text|select|check|value|file|other, type, role, checked, multiple, disabled}`.
pub const FIELD_KIND: &str = r#"function() {
  const el = this;
  if (el instanceof HTMLSelectElement) return { kind: 'select', type: 'select', multiple: el.multiple, disabled: el.disabled };
  if (el instanceof HTMLTextAreaElement) return { kind: 'text', type: 'textarea', disabled: el.disabled || el.readOnly };
  if (el instanceof HTMLInputElement) {
    const t = el.type;
    if (t === 'checkbox' || t === 'radio') return { kind: 'check', type: t, role: t, checked: el.checked, disabled: el.disabled };
    if (['date', 'time', 'datetime-local', 'month', 'week', 'range', 'color'].includes(t)) return { kind: 'value', type: t, disabled: el.disabled || el.readOnly };
    if (t === 'file') return { kind: 'file', type: t };
    if (['button', 'submit', 'reset', 'image', 'hidden'].includes(t)) return { kind: 'other', type: 'input type=' + t };
    return { kind: 'text', type: t, disabled: el.disabled || el.readOnly };
  }
  const role = (el.getAttribute && el.getAttribute('role')) || '';
  if (['checkbox', 'radio', 'switch', 'menuitemcheckbox', 'menuitemradio'].includes(role)) {
    return { kind: 'check', type: role, role, checked: el.getAttribute('aria-checked') === 'true', disabled: el.getAttribute('aria-disabled') === 'true' };
  }
  if (el.isContentEditable) return { kind: 'text', type: 'contenteditable', disabled: false };
  return { kind: 'other', type: (el.tagName || 'node').toLowerCase() + (role ? ' role=' + role : '') };
}"#;

/// `this` = checkbox / radio / switch element: its checked state.
pub const CHECKED_OF: &str = "function() { return (this instanceof HTMLInputElement) ? this.checked : this.getAttribute('aria-checked') === 'true'; }";

/// `this` = document: milliseconds until DevTools key events reach the page. Until a new document
/// has painted, Chromium drops input events silently, and a background tab never paints: its
/// first frame is released by a 500 ms timeout after the navigation commits (measured: keys in
/// the first ~550 ms of a never-shown tab are lost, `Input.insertText` is not). The response start
/// stands in for the commit, with a margin.
pub const KEY_READY_IN: &str = "function() { const n = performance.getEntriesByType('navigation')[0]; if (!n || performance.getEntriesByType('paint').some((e) => e.name === 'first-contentful-paint')) return 0; return Math.max(0, Math.ceil(n.responseStart + 750 - performance.now())); }";

/// `this` = `<select>`, `values`: selects the options matching each value (option value, then
/// label, then label ignoring case and spaces), fires `input` and `change`.
/// `{selected, values, positions, total}` (labels, values, 1-based positions among the `total`
/// options) or `{error: single|missing|option-disabled, value, options}`.
pub const SELECT_OPTIONS: &str = r#"function(values) {
  const el = this;
  if (!el.multiple && values.length > 1) return { error: 'single' };
  const options = [...el.options];
  const norm = (s) => String(s).replace(/\s+/g, ' ').trim().toLowerCase();
  const picked = [];
  for (const v of values) {
    const o = options.find((x) => x.value === v) || options.find((x) => x.label === v || x.text === v) || options.find((x) => norm(x.label) === norm(v) || norm(x.text) === norm(v));
    if (!o) return { error: 'missing', value: v, options: options.slice(0, 40).map((x) => x.label || x.text || x.value) };
    if (o.disabled) return { error: 'option-disabled', value: v };
    if (!picked.includes(o)) picked.push(o);
  }
  for (const o of options) o.selected = picked.includes(o);
  el.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return { selected: picked.map((o) => o.label || o.text || o.value), values: picked.map((o) => o.value), positions: picked.map((o) => o.index + 1), total: options.length };
}"#;

/// `this` = `<input>` (date, time, range, color, …), `value`: sets the value like a user edit
/// (`input` + `change`). Returns the value the input kept.
pub const SET_INPUT_VALUE: &str = r#"function(value) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
  setter.call(this, value);
  this.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return String(this.value);
}"#;

/// `this` = document or a scrollable element, `axis` ("x" | "y"), `sign` (1 | -1), `amount` in CSS
/// px (`null` = 80% of the visible width or height): scrolls at once and returns the position
/// `{x, y, maxX, maxY, moved, amount}`.
pub const SCROLL_BY: &str = r#"function(axis, sign, amount) {
  const isDoc = this.nodeType === 9;
  const doc = isDoc ? this : this.ownerDocument;
  const win = doc.defaultView;
  const target = isDoc ? (doc.scrollingElement || doc.documentElement) : this;
  const size = axis === 'x' ? (isDoc ? win.innerWidth : target.clientWidth) : (isDoc ? win.innerHeight : target.clientHeight);
  const px = amount === null ? Math.max(40, Math.round((size || 750) * 0.8)) : amount;
  const x0 = target.scrollLeft, y0 = target.scrollTop;
  const delta = { left: axis === 'x' ? sign * px : 0, top: axis === 'y' ? sign * px : 0, behavior: 'instant' };
  if (isDoc) win.scrollBy(delta); else target.scrollBy(delta);
  const x = target.scrollLeft, y = target.scrollTop;
  return { x, y, maxX: Math.max(0, target.scrollWidth - target.clientWidth), maxY: Math.max(0, target.scrollHeight - target.clientHeight), moved: x !== x0 || y !== y0, amount: px };
}"#;

/// `this` = element: its bounds and whether it is inside the viewport.
pub const IN_VIEW: &str = "function() { const r = this.getBoundingClientRect(); const w = innerWidth, h = innerHeight; return { x: r.left, y: r.top, width: r.width, height: r.height, inView: r.bottom > 0 && r.right > 0 && r.top < h && r.left < w }; }";

// ----------------------------------------------------------------------------------- accessibility

thread_local! {
    /// Browser → accessibility use counter (for the idle release).
    static AX_USE: RefCell<HashMap<i32, u64>> = RefCell::new(HashMap::new());
}

/// Accessibility is released this long after the last snapshot of a browser.
const AX_IDLE_MS: i64 = 30_000;

/// A snapshot used the accessibility tree of `browser`: release it again after 30 s without use,
/// so a page doesn't keep paying for accessibility updates.
pub fn note_ax_use(browser: i32) {
    let n = AX_USE.with(|a| {
        let mut a = a.borrow_mut();
        let e = a.entry(browser).or_insert(0);
        *e += 1;
        *e
    });
    crate::task::post_ui_delayed(AX_IDLE_MS, move || {
        let idle = AX_USE.with(|a| {
            let mut a = a.borrow_mut();
            let idle = a.get(&browser) == Some(&n);
            if idle {
                a.remove(&browser);
            }
            idle
        });
        if idle {
            release_ax(browser);
        }
    });
}

fn release_ax(browser: i32) {
    super::exec::spawn(async move {
        let _ = cdp::call(browser, p::AccessibilityDisable {}, 2_000).await;
    });
}

/// Stop / session end: release accessibility everywhere now.
pub fn release_all_ax() {
    let browsers: Vec<i32> = AX_USE.with(|a| a.borrow_mut().drain().map(|(b, _)| b).collect());
    for b in browsers {
        release_ax(b);
    }
}
