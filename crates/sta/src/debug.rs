//! Debug-only IPC requests [owner: chrome]. Compiled only with `debug_assertions`.
//!
//! Lets automated tests (via `tools/cdp.mjs eval <surface> ...` on a trusted page) drive shell
//! modules directly, independent of what the core reducer currently emits.
//!
//! | request | payload | result |
//! |---|---|---|
//! | `debug.info` | – | `{window, tabs, overlays, rounded, browsers, controller, ipc, keyboard, permissions, suggest, devtools, devtoolsCdp, focus}` snapshot |
//! | `debug.pushState` | – | `null`; pushes a `state` event to every subscriber now |
//! | `debug.execute` | `Effect` or `Effect[]` | `null`; runs the effects via `controller::run_effects` |
//! | `debug.dispatch` | `Command` (shell events allowed) | `null` |
//! | `debug.openTab` | `{url, show?=true}` | `{tab}`; `CreateBrowser` (+ `ShowContent Single`) with a fresh id |
//! | `debug.accelerator` | `{key, ctrl?, shift?, alt?}` | `{commandId}`; runs the accelerator handler for that combo |
//! | `debug.sendKey` | `{key, ctrl?, shift?, alt?}` | `null`; `Window::send_key_press` (did not reach accelerators in practice) |
//! | `debug.focus` | `{surface}` or `{tab}` | `null`; `request_focus` on that surface's / tab's BrowserView |
//! | `debug.realKeys` | `{combo}` or `{steps}`, `delayMs?=0`, `activate?=true` | `{sent, aborted, reason?}` after the last key |
//! | `debug.resetPermissions` | `{origin, bits}` | number of content setting types reset: every content setting CEF permission `bits` map to, reset for `origin` like an expired one-time grant |
//! | `debug.hoverInput` | `{enabled?, pointer?}` or `{realCursor: {x, y, holdMs?}}` | sidebar hover snapshot; see below |
//! | `debug.postMouse` | `{steps: [{type: "move"\|"down"\|"up"\|"dblclick", x, y, button?="left"\|"right"} \| {waitMs}]}` | `{posted}` after the last step |
//! | `debug.motion` | `{floorMs?: number\|null}` | the motion snapshot (`debug.info.motion`); `floorMs` raises the acknowledged exits' floor (ARCHITECTURE §4.7) so a check can act inside a linger, `null` restores the real 50/60 ms |
//! | `debug.foreign` | – | Chrome-created browsers (foreign.rs): entries, counters, events, hidden windows and hook counters |
//! | `debug.foreign.close` | `{id}` | `true` if that Chrome-created browser was closed now (ignores the close rules) |
//! | `debug.foreign.trigger` | `{url}` | `{targetId}`; `Target.createTarget` on the caller's DevTools session (devtools_cdp.rs, `User::Debug`): Chromium creates its own browser like an extension's `tabs.create` |
//!
//! `debug.hoverInput` drives the sidebar hover reveal (sidebar_hover.rs) without the user's mouse:
//! - `enabled`: pointer reveal on/off (the e2e suites start with `STA_DEBUG_HOVER_REVEAL=0`);
//! - `pointer: {x, y, buttons?, overWindow?=true, ownedPopup?}`: a virtual pointer in window client
//!   DIP that the poll samples instead of the cursor (real timers); `pointer: null` = real cursor;
//! - `realCursor: {x, y, holdMs}`: one guarded check of the real path. Only while our window is
//!   the foreground window: moves the cursor there (`SetCursorPos`), reads back where it landed
//!   (Windows clamps moves to the desktop), waits `holdMs`, snapshots, and puts the cursor back
//!   unless the user moved it away from that landing point meanwhile (`{snapshot, target, landed,
//!   userMoved, restored}`). 409 otherwise.
//!
//! `debug.postMouse` posts mouse messages to our own top-level window (client DIP → pixels), so
//! clicks go through Chromium's native input path (aura hit test, focus) without moving the OS
//! cursor or reaching other windows. One task per message (16 ms apart), except `dblclick`: press,
//! release, press, release posted at once (with the real cursor resting over our window, Windows
//! synthesizes a move there after each release, which would break up paced clicks).
//!
//! `debug.realKeys` injects **real OS keyboard input** (`SendInput`) so the whole path (Views
//! focus manager, accelerators, `KeyboardHandler`, the page) is exercised:
//! - `combo`: `"ctrl+shift+k"`, `"f5"`, `"alt+1"`, `"escape"`, `"enter"` (modifiers down in order,
//!   key down/up, modifiers up in reverse);
//! - `steps`: `[{key: "ctrl", down: true}, {key: "tab"}, {waitMs: 300}, {key: "ctrl", up: true}]`
//!   (a step without `down`/`up` is a press);
//! - first brings our window to the foreground (`AttachThreadInput` + `SetForegroundWindow`), and
//!   checks `GetForegroundWindow() == our HWND` immediately before **every** key transition. If that
//!   fails the sequence aborts (409 before the first key; `aborted: true` later); key-ups for keys it
//!   still holds are only sent if our window is (again) the foreground window. Input is never sent
//!   while another app is in the foreground.
//!
//! Note: CDP `Input.dispatchKeyEvent` never reaches Views accelerators (DevTools marks the events
//! `skip_if_unhandled`), which is why `debug.accelerator` and `debug.realKeys` exist.
//!
//! Public API: `pub fn handle(browser_id: i32, cmd: &str, payload: Value, callback: &Cb) -> Reply`

use crate::browsers::{self, Surface};
use crate::ipc::{self, Reply, parse};
use crate::{controller, keyboard, overlays, permissions, rounded, sidebar_hover, suggest, tabs, task, window};
use sta_core::{Command, ContentLayout, Effect, Id};
use cef::wrapper::message_router::BrowserSideCallback;
use cef::*;
use serde::Deserialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};

type Cb = Arc<Mutex<dyn BrowserSideCallback>>;

#[derive(Deserialize)]
struct Key {
    key: i32,
    #[serde(default)]
    ctrl: bool,
    #[serde(default)]
    shift: bool,
    #[serde(default)]
    alt: bool,
}

fn reply_later(callback: &Cb, f: impl FnOnce() -> Result<String, (i32, String)> + 'static) -> Reply {
    let cb = callback.clone();
    task::post_ui(move || {
        let result = f();
        let Ok(cb) = cb.lock() else { return };
        match result {
            Ok(json) => cb.success_str(&json),
            Err((code, msg)) => cb.failure(code, &msg),
        }
    });
    Reply::Deferred
}

pub fn handle(browser_id: i32, cmd: &str, payload: Value, callback: &Cb) -> Reply {
    if cmd != "debug.info" {
        log_debug!("{cmd} from browser {browser_id}: {payload}");
    }
    match cmd {
        // Views getters run outside the router lock.
        "debug.info" => reply_later(callback, || Ok(info().to_string())),
        "debug.pushState" => {
            task::post_ui(ipc::push_state);
            Reply::null()
        }
        // `{floorMs}` raises the acknowledged exits' floor (`{floorMs: null}` restores it), so a check
        // can act *inside* a linger — the window every generation check exists for — instead of racing
        // a 108 ms timer over IPC. Everything else about the protocol is unchanged: the page still
        // blanks and still acks, the shell still refuses to hide before the floor.
        "debug.motion" => {
            if let Some(v) = payload.get("floorMs") {
                let ms = match (v.is_null(), v.as_i64()) {
                    (true, _) => None,
                    (false, Some(ms)) => Some(ms),
                    (false, None) => return Reply::bad_request("floorMs must be a number or null"),
                };
                crate::motion::debug_set_floor(ms);
            }
            // `{slideMs}` stretches (or shortens) the floating sidebar's slide, so a check can look
            // at the card part-way out instead of racing a 160 ms animation.
            if let Some(v) = payload.get("slideMs") {
                let ms = match (v.is_null(), v.as_i64()) {
                    (true, _) => None,
                    (false, Some(ms)) => Some(ms),
                    (false, None) => return Reply::bad_request("slideMs must be a number or null"),
                };
                crate::motion::debug_set_slide_ms(ms);
            }
            reply_later(callback, || Ok(window::motion_snapshot().to_string()))
        }
        "debug.execute" => {
            let effects: Vec<Effect> = if payload.is_array() {
                match serde_json::from_value(payload) {
                    Ok(e) => e,
                    Err(e) => return Reply::bad_request(e),
                }
            } else {
                match serde_json::from_value(payload) {
                    Ok(e) => vec![e],
                    Err(e) => return Reply::bad_request(e),
                }
            };
            task::post_ui(move || controller::run_effects(effects));
            Reply::null()
        }
        "debug.dispatch" => match serde_json::from_value::<Command>(payload) {
            Ok(c) => {
                controller::dispatch(c);
                Reply::null()
            }
            Err(e) => Reply::bad_request(e),
        },
        "debug.openTab" => {
            #[derive(Deserialize)]
            struct Q {
                url: String,
                #[serde(default)]
                show: Option<bool>,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            let Some(tab) = controller::alloc_id() else { return Reply::Err(503, "store not ready".into()) };
            let internal = sta_core::urls::is_internal(&q.url);
            let mut effects = vec![Effect::CreateBrowser { tab, url: q.url, internal, muted: false }];
            if q.show.unwrap_or(true) {
                effects.push(Effect::ShowContent { layout: ContentLayout::Single { tab } });
            }
            task::post_ui(move || controller::run_effects(effects));
            Reply::value(&serde_json::json!({ "tab": tab }))
        }
        "debug.accelerator" => {
            let k: Key = match parse(payload) {
                Ok(k) => k,
                Err(r) => return r,
            };
            let Some(id) = keyboard::find_binding(k.key, k.shift, k.ctrl, k.alt) else {
                return Reply::Err(404, "no binding for that combo".into());
            };
            task::post_ui(move || {
                keyboard::on_accelerator(id);
            });
            Reply::value(&serde_json::json!({ "commandId": id }))
        }
        "debug.sendKey" => {
            let k: Key = match parse(payload) {
                Ok(k) => k,
                Err(r) => return r,
            };
            let flags = (u32::from(k.shift) << 1) | (u32::from(k.ctrl) << 2) | (u32::from(k.alt) << 3);
            reply_later(callback, move || {
                let active = window::main_window().filter(|w| w.is_active() != 0);
                match active {
                    Some(w) => {
                        w.send_key_press(k.key, flags);
                        Ok("null".into())
                    }
                    None => Err((409, "the sta window is not active".into())),
                }
            })
        }
        "debug.focus" => {
            #[derive(Deserialize)]
            struct Q {
                #[serde(default)]
                surface: Option<String>,
                #[serde(default)]
                tab: Option<Id>,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            reply_later(callback, move || {
                if let Some(tab) = q.tab {
                    tabs::focus_browser(tab);
                    return Ok("null".into());
                }
                let surface = q.surface.as_deref().and_then(Surface::from_host).ok_or((400, "unknown surface".to_string()))?;
                let mut browser = browsers::surface_browser(surface).ok_or((404, "surface has no browser".to_string()))?;
                let view = browser_view_get_for_browser(Some(&mut browser)).ok_or((404, "no view".to_string()))?;
                view.request_focus();
                Ok("null".into())
            })
        }
        "debug.resetPermissions" => {
            #[derive(Deserialize)]
            struct Q {
                origin: String,
                bits: u32,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            reply_later(callback, move || {
                permissions::debug_reset(&q.origin, q.bits).map(|n| n.to_string()).map_err(|e| (503, e))
            })
        }
        #[cfg(windows)]
        "debug.realKeys" => real_keys::start(payload, callback),
        #[cfg(windows)]
        "debug.hoverInput" if payload.get("realCursor").is_some() => mouse::real_cursor(payload, callback),
        "debug.hoverInput" => reply_later(callback, move || sidebar_hover::debug_input(&payload).map(|v| v.to_string())),
        #[cfg(windows)]
        "debug.postMouse" => mouse::post(payload, callback),
        "debug.cdp" | "debug.cdpEvents" | "debug.tabKey" => crate::automation::spike::handle(cmd, payload, callback),
        "debug.foreign" => reply_later(callback, || Ok(crate::foreign::debug_snapshot().to_string())),
        "debug.foreign.close" => {
            let id = payload.get("id").and_then(Value::as_i64).unwrap_or(-1) as i32;
            reply_later(callback, move || Ok(crate::foreign::debug_close(id).to_string()))
        }
        "debug.foreign.trigger" => {
            // A Chrome-created browser like an extension's (`Target.createTarget` on the caller's
            // DevTools session through the shell's own client): `{url}` → `{targetId}`.
            #[derive(Deserialize)]
            struct Q {
                url: String,
            }
            let q: Q = match parse(payload) {
                Ok(q) => q,
                Err(r) => return r,
            };
            let cb = callback.clone();
            task::post_ui(move || {
                let params = serde_json::json!({ "url": q.url });
                crate::devtools_cdp::call(browser_id, crate::devtools_cdp::User::Debug, "Target.createTarget", params, 10_000, move |result| {
                    let Ok(cb) = cb.lock() else { return };
                    match result {
                        Ok(v) => cb.success_str(&v.to_string()),
                        Err(e) => cb.failure(502, &e),
                    }
                });
            });
            Reply::Deferred
        }
        _ => Reply::Err(404, format!("unknown debug request: {cmd}")),
    }
}

fn info() -> Value {
    let focused = overlays::focused_browser();
    serde_json::json!({
        "window": window::debug_snapshot(),
        "tabs": tabs::debug_snapshot(),
        "overlays": overlays::debug_snapshot(),
        "rounded": rounded::debug_snapshot(),
        "browsers": browsers::debug_snapshot(),
        "controller": controller::debug_snapshot(),
        "ipc": { "subscribers": ipc::subscriber_count() },
        "keyboard": keyboard::debug_snapshot(),
        "permissions": permissions::debug_snapshot(),
        "suggest": suggest::debug_snapshot(),
        "sidebarHover": sidebar_hover::debug_snapshot(),
        "motion": window::motion_snapshot(),
        "update": crate::update::debug_snapshot(),
        "automation": crate::automation::debug_snapshot(),
        "foreign": crate::foreign::debug_snapshot(),
        "devtools": crate::devtools::debug_snapshot(),
        "devtoolsCdp": crate::devtools_cdp::debug_snapshot(),
        "extensions": crate::extensions::debug_snapshot(),
        "extBackend": crate::ext_backend::debug_snapshot(),
        "extPopup": crate::ext_popup::debug_snapshot(),
        "safeMode": crate::safe_mode::debug_snapshot(),
        "downloadsInProgress": crate::downloads::in_progress_count(),
        "focus": {
            "browser": focused,
            "role": focused.and_then(browsers::role_of).map(|r| format!("{r:?}")),
            "foreground": foreground_is_ours(),
        },
    })
}

#[cfg(windows)]
fn foreground_is_ours() -> bool {
    let hwnd = window::hwnd_value();
    hwnd != 0 && crate::platform::input::foreground_window() == hwnd
}

#[cfg(not(windows))]
fn foreground_is_ours() -> bool {
    false
}

#[cfg(windows)]
mod mouse {
    use super::{Cb, Reply};
    use crate::platform::input;
    use crate::{sidebar_hover, task, window};
    use cef::{ImplDisplay, ImplWindow};
    use serde::Deserialize;
    use serde_json::Value;

    fn scale() -> f64 {
        window::main_window()
            .and_then(|w| w.display())
            .map(|d| d.device_scale_factor() as f64)
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0)
    }

    fn reply(cb: &Cb, result: Result<String, (i32, String)>) {
        let Ok(cb) = cb.lock() else { return };
        match result {
            Ok(json) => cb.success_str(&json),
            Err((code, msg)) => cb.failure(code, &msg),
        }
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RealCursor {
        x: f64,
        y: f64,
        #[serde(default)]
        hold_ms: Option<i64>,
    }

    /// `debug.hoverInput {realCursor}`: see the module docs.
    pub fn real_cursor(payload: Value, callback: &Cb) -> Reply {
        let spec: RealCursor = match payload.get("realCursor").cloned().map(serde_json::from_value) {
            Some(Ok(s)) => s,
            Some(Err(e)) => return Reply::bad_request(e),
            None => return Reply::bad_request("realCursor missing"),
        };
        let cb = callback.clone();
        task::post_ui(move || {
            let hwnd = window::hwnd_value();
            if hwnd == 0 || input::foreground_window() != hwnd {
                reply(&cb, Err((409, format!("the sta window is not the foreground window ({})", input::describe_foreground()))));
                return;
            }
            let (Some((cx, cy)), Some(before)) = (input::client_origin(hwnd), input::cursor_pos()) else {
                reply(&cb, Err((503, "no cursor or window position".into())));
                return;
            };
            let s = scale();
            let target = (cx + (spec.x * s).round() as i32, cy + (spec.y * s).round() as i32);
            if !input::set_cursor_pos(hwnd, target.0, target.1) {
                reply(&cb, Err((409, "the cursor could not be moved (foreground changed)".into())));
                return;
            }
            // Windows clamps the move to the desktop (a maximized window's client origin can be
            // off-screen): compare against where the cursor actually landed, not the target.
            let Some(landed) = input::cursor_pos() else {
                input::restore_cursor_pos(before.0, before.1);
                reply(&cb, Err((503, "the cursor position could not be read back".into())));
                return;
            };
            log_info!("debug.hoverInput: real cursor moved to {landed:?} (target {target:?}) for {} ms", spec.hold_ms.unwrap_or(400));
            task::post_ui_delayed(spec.hold_ms.unwrap_or(400).clamp(0, 3000), move || {
                let snapshot = sidebar_hover::debug_snapshot();
                let untouched = input::cursor_pos() == Some(landed);
                // Undoing our own move is always right while the cursor is still where we put it,
                // whichever window is in the foreground now.
                let restored = untouched && input::restore_cursor_pos(before.0, before.1);
                let json = serde_json::json!({
                    "snapshot": snapshot,
                    "target": [target.0, target.1],
                    "landed": [landed.0, landed.1],
                    "userMoved": !untouched,
                    "restored": restored,
                });
                reply(&cb, Ok(json.to_string()));
            });
        });
        Reply::Deferred
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Step {
        #[serde(default, rename = "type")]
        kind: Option<String>,
        #[serde(default)]
        x: f64,
        #[serde(default)]
        y: f64,
        #[serde(default)]
        button: Option<String>,
        #[serde(default)]
        wait_ms: Option<i64>,
    }

    #[derive(Deserialize)]
    struct Request {
        steps: Vec<Step>,
    }

    const WM_MOUSEMOVE: u32 = 0x0200;
    const WM_LBUTTONDOWN: u32 = 0x0201;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_RBUTTONDOWN: u32 = 0x0204;
    const WM_RBUTTONUP: u32 = 0x0205;
    const MK_LBUTTON: usize = 0x0001;
    const MK_RBUTTON: usize = 0x0002;

    /// `debug.postMouse`: see the module docs.
    pub fn post(payload: Value, callback: &Cb) -> Reply {
        let req: Request = match crate::ipc::parse(payload) {
            Ok(r) => r,
            Err(r) => return r,
        };
        for s in &req.steps {
            let known = s.wait_ms.is_some() || matches!(s.kind.as_deref(), Some("move" | "down" | "dblclick" | "up"));
            if !known || !matches!(s.button.as_deref(), None | Some("left" | "right")) {
                return Reply::bad_request("steps: {type: move|down|dblclick|up, x, y, button?: left|right} or {waitMs}");
            }
        }
        let cb = callback.clone();
        task::post_ui(move || run(req.steps, 0, 0, 0, cb));
        Reply::Deferred
    }

    fn run(steps: Vec<Step>, index: usize, held: usize, posted: usize, cb: Cb) {
        let Some(step) = steps.get(index) else {
            reply(&cb, Ok(serde_json::json!({ "posted": posted }).to_string()));
            return;
        };
        if let Some(ms) = step.wait_ms {
            task::post_ui_delayed(ms.clamp(0, 5000), move || run(steps, index + 1, held, posted, cb));
            return;
        }
        let s = scale();
        let (x, y) = ((step.x * s).round() as i32, (step.y * s).round() as i32);
        let right = step.button.as_deref() == Some("right");
        let (down, up, bit) = if right { (WM_RBUTTONDOWN, WM_RBUTTONUP, MK_RBUTTON) } else { (WM_LBUTTONDOWN, WM_LBUTTONUP, MK_LBUTTON) };
        if step.kind.as_deref() == Some("dblclick") {
            // Both clicks in one burst. A released capture makes Windows synthesize a WM_MOUSEMOVE
            // at the *real* cursor; with an idle cursor over our window that move would land
            // between two paced clicks and reset Chromium's click count. Posted messages are
            // retrieved before synthesized input, so a burst keeps the double-click intact.
            let hwnd = window::hwnd_value();
            let ok = [(down, held | bit), (up, held & !bit), (down, held | bit), (up, held & !bit)]
                .into_iter()
                .filter(|(m, w)| input::post_mouse(hwnd, *m, *w, x, y))
                .count();
            sidebar_hover::debug_note_mouse(step.x, step.y, Some(true));
            sidebar_hover::debug_note_mouse(step.x, step.y, Some(false));
            let posted = posted + ok;
            task::post_ui_delayed(16, move || run(steps, index + 1, held & !bit, posted, cb));
            return;
        }
        let (msg, held) = match step.kind.as_deref() {
            Some("down") => (down, held | bit),
            Some("up") => (up, held & !bit),
            _ => (WM_MOUSEMOVE, held),
        };
        // A button-down message carries its own button in wParam; an up message doesn't.
        let ok = input::post_mouse(window::hwnd_value(), msg, held, x, y);
        // A virtual hover pointer follows the posted mouse (same position and buttons).
        sidebar_hover::debug_note_mouse(step.x, step.y, (msg != WM_MOUSEMOVE).then_some(held != 0));
        let posted = posted + ok as usize;
        // One task per message: Chromium handles each before the next arrives.
        task::post_ui_delayed(16, move || run(steps, index + 1, held, posted, cb));
    }
}

#[cfg(windows)]
mod real_keys {
    use super::{Cb, Reply};
    use crate::platform::input;
    use crate::{task, window};
    use serde::Deserialize;
    use serde_json::Value;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Request {
        #[serde(default)]
        combo: Option<String>,
        #[serde(default)]
        steps: Vec<StepSpec>,
        #[serde(default)]
        delay_ms: Option<i64>,
        #[serde(default)]
        activate: Option<bool>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct StepSpec {
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        down: bool,
        #[serde(default)]
        up: bool,
        #[serde(default)]
        wait_ms: Option<i64>,
    }

    #[derive(Clone, Copy, Debug)]
    enum Step {
        Key { vk: u16, up: bool },
        Wait(i64),
    }

    /// Virtual-key code for a key name (`a`–`z`, `0`–`9`, `f1`–`f24`, named keys).
    fn vk_of(name: &str) -> Option<u16> {
        let n = name.trim().to_ascii_lowercase();
        let named = match n.as_str() {
            "ctrl" | "control" => 0x11,
            "shift" => 0x10,
            "alt" | "menu" => 0x12,
            "tab" => 0x09,
            "enter" | "return" => 0x0D,
            "escape" | "esc" => 0x1B,
            "space" => 0x20,
            "backspace" => 0x08,
            "delete" | "del" => 0x2E,
            "pageup" => 0x21,
            "pagedown" => 0x22,
            "end" => 0x23,
            "home" => 0x24,
            "left" => 0x25,
            "up" => 0x26,
            "right" => 0x27,
            "down" => 0x28,
            "plus" | "=" => 0xBB,
            "comma" | "," => 0xBC,
            "minus" | "-" => 0xBD,
            "[" => 0xDB,
            "]" => 0xDD,
            _ => 0,
        };
        if named != 0 {
            return Some(named);
        }
        let bytes = n.as_bytes();
        if bytes.len() == 1 && bytes[0].is_ascii_alphanumeric() {
            return Some(bytes[0].to_ascii_uppercase() as u16);
        }
        if let Some(num) = n.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()).filter(|d| (1..=24).contains(d)) {
            return Some(0x70 + num - 1);
        }
        None
    }

    fn parse(req: &Request) -> Result<Vec<Step>, String> {
        let mut steps = Vec::new();
        if let Some(combo) = &req.combo {
            let keys: Vec<u16> = combo.split('+').map(|k| vk_of(k).ok_or(format!("unknown key {k:?}"))).collect::<Result<_, _>>()?;
            for vk in &keys {
                steps.push(Step::Key { vk: *vk, up: false });
            }
            for vk in keys.iter().rev() {
                steps.push(Step::Key { vk: *vk, up: true });
            }
        }
        for s in &req.steps {
            if let Some(ms) = s.wait_ms {
                steps.push(Step::Wait(ms.clamp(0, 5000)));
                continue;
            }
            let name = s.key.as_deref().ok_or("step without key or waitMs")?;
            let vk = vk_of(name).ok_or(format!("unknown key {name:?}"))?;
            match (s.down, s.up) {
                (true, false) => steps.push(Step::Key { vk, up: false }),
                (false, true) => steps.push(Step::Key { vk, up: true }),
                _ => {
                    steps.push(Step::Key { vk, up: false });
                    steps.push(Step::Key { vk, up: true });
                }
            }
        }
        if steps.is_empty() {
            return Err("no keys".into());
        }
        Ok(steps)
    }

    thread_local! {
        /// Keys pressed by an aborted sequence that could not be released yet.
        static PENDING_RELEASE: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
    }

    /// Releases keys left down by an aborted sequence. Key-ups are only sent while our window is
    /// the foreground window: it is brought back (never typing into another app) and retried every
    /// 100 ms for 5 s; anything still pending is released at the start of the next request.
    fn release_pending(hwnd: isize, attempt: u32) {
        let pending = PENDING_RELEASE.with(|p| p.borrow().clone());
        if pending.is_empty() {
            return;
        }
        input::bring_to_foreground(hwnd);
        let mut left = Vec::new();
        for vk in pending.iter().rev() {
            if !input::send_key(hwnd, *vk, true) {
                left.insert(0, *vk);
            }
        }
        if left.len() < pending.len() {
            log_info!("debug.realKeys: released {} held key(s)", pending.len() - left.len());
        }
        PENDING_RELEASE.with(|p| *p.borrow_mut() = left.clone());
        if left.is_empty() {
            return;
        }
        if attempt >= 50 {
            log_error!("debug.realKeys: keys {left:x?} are still down (our window never became foreground again)");
            return;
        }
        task::post_ui_delayed(100, move || release_pending(hwnd, attempt + 1));
    }

    struct Run {
        hwnd: isize,
        steps: Vec<Step>,
        next: usize,
        delay: i64,
        held: Vec<u16>,
        sent: usize,
        cb: Cb,
    }

    pub fn start(payload: Value, callback: &Cb) -> Reply {
        let req: Request = match crate::ipc::parse(payload) {
            Ok(r) => r,
            Err(r) => return r,
        };
        let steps = match parse(&req) {
            Ok(s) => s,
            Err(e) => return Reply::bad_request(e),
        };
        // No delay between transitions by default: a combo goes out within a millisecond, so another
        // window can hardly take the foreground in the middle of it.
        let delay = req.delay_ms.unwrap_or(0).clamp(0, 2000);
        let activate = req.activate.unwrap_or(true);
        let cb = callback.clone();
        // Outside the router lock: window handles and foreground changes.
        task::post_ui(move || {
            let hwnd = window::hwnd_value();
            if activate {
                input::bring_to_foreground(hwnd);
            }
            if hwnd == 0 || input::foreground_window() != hwnd {
                let fg = input::describe_foreground();
                log_warn!("debug.realKeys: our window is not the foreground window ({fg}); no input sent");
                if let Ok(cb) = cb.lock() {
                    cb.failure(409, &format!("the sta window is not the foreground window ({fg})"));
                }
                return;
            }
            release_pending(hwnd, 50);
            let run = Rc::new(RefCell::new(Run { hwnd, steps, next: 0, delay, held: Vec::new(), sent: 0, cb }));
            // Give activation a moment to settle before the first key.
            let wait = if activate { 120 } else { 0 };
            task::post_ui_delayed(wait, move || step(run));
        });
        Reply::Deferred
    }

    fn step(run: Rc<RefCell<Run>>) {
        let (hwnd, next) = {
            let r = run.borrow();
            (r.hwnd, r.steps.get(r.next).copied())
        };
        let Some(next) = next else {
            finish(&run, None);
            return;
        };
        let delay = match next {
            Step::Wait(ms) => ms,
            Step::Key { vk, up } => {
                log_debug!("realKeys: vk {vk:#x} {} (focused browser {:?})", if up { "up" } else { "down" }, crate::overlays::focused_browser());
                // send_key re-checks GetForegroundWindow() == hwnd right before SendInput.
                if !input::send_key(hwnd, vk, up) {
                    let reason = format!("foreground window changed to {}", input::describe_foreground());
                    finish(&run, Some(&reason));
                    return;
                }
                let mut r = run.borrow_mut();
                r.sent += 1;
                if up {
                    r.held.retain(|k| *k != vk);
                } else if !r.held.contains(&vk) {
                    r.held.push(vk);
                }
                r.delay
            }
        };
        run.borrow_mut().next += 1;
        if delay == 0 && matches!(next, Step::Key { .. }) {
            // Next transition right away (same task): a tight burst.
            step(run);
        } else {
            task::post_ui_delayed(delay, move || step(run));
        }
    }

    fn finish(run: &Rc<RefCell<Run>>, abort: Option<&str>) {
        let (hwnd, held, sent, cb) = {
            let mut r = run.borrow_mut();
            (r.hwnd, std::mem::take(&mut r.held), r.sent, r.cb.clone())
        };
        if let Some(reason) = abort {
            log_warn!("debug.realKeys aborted after {sent} key events: {reason}");
            if !held.is_empty() {
                PENDING_RELEASE.with(|p| p.borrow_mut().extend(held.iter().copied()));
                release_pending(hwnd, 0);
            }
        }
        let json = serde_json::json!({ "sent": sent, "aborted": abort.is_some(), "reason": abort }).to_string();
        if let Ok(cb) = cb.lock() {
            cb.success_str(&json);
        }
    }
}
