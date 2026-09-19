//! The **debug-only** catalog of the MCP test surface (docs/TESTING.md).
//!
//! rename:keep-file — nothing here ships: the whole module is behind the `test-hooks` feature,
//! which a release build cannot enable (`crates/sta/src/test_hooks/mod.rs` turns that into a
//! `compile_error!`). It is the one source of truth for the browser (argument parsing) and the
//! bridge (`tools/list` schemas) exactly like [`super::tools`] is for the 23 shipped tools.
//!
//! Conventions, checked by the tests at the bottom:
//! - every name matches `^test_[a-z_]+$`;
//! - every schema is a closed object (`additionalProperties: false`);
//! - every tool carries `"x-sta-test": true` in its annotations, so a client can filter the
//!   surface out in one predicate, and none of them is `readOnlyHint` (they are not for models).

use super::tools::ToolDef;
use serde_json::{Value, json};

/// Prefix every tool of the test surface has. The browser's dispatcher and the bridge both key
/// off it.
pub const PREFIX: &str = "test_";

/// Largest number of calls one `test_batch` may carry.
pub const MAX_BATCH: usize = 16;
/// Largest inline capture (base64 characters) before `too_large`.
pub const MAX_INLINE_BASE64: usize = 6 * 1024 * 1024;

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

fn empty() -> Value {
    object(json!({}), &[])
}

fn timeout_prop() -> Value {
    json!({ "type": "integer", "minimum": 0, "maximum": 120000, "description": "Milliseconds to wait (default 10000)." })
}

fn tab_prop() -> Value {
    json!({ "type": "integer", "minimum": 1, "description": "Tab id." })
}

/// `{surface} | {tab} | {browser} | {targetId} | {match}`: how every JavaScript and CDP tool names
/// a target.
fn target_prop() -> Value {
    json!({
        "type": "object",
        "description": "One of `surface` (sta:// host name), `tab`, `browser` (a CEF browser id from test_targets, which also addresses DevTools windows and Chrome-created browsers), `targetId` or `match` (a substring of the target URL).",
        "properties": {
            "surface": { "type": "string", "description": "sta:// surface host: `sidebar`, `topbar`, `empty`, `command`, `find`, `permission`, `switcher`, `toast`, `peek`, `agent`." },
            "tab": tab_prop(),
            "browser": { "type": "integer", "minimum": 1, "description": "CEF browser identifier from test_targets (`browser`)." },
            "targetId": { "type": "string", "description": "DevTools target id from test_targets." },
            "match": { "type": "string", "description": "Substring of the target URL." },
        },
        "additionalProperties": false,
    })
}

fn point_list(what: &str) -> Value {
    json!({
        "type": "array",
        "description": what,
        "maxItems": 64,
        "items": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 },
    })
}

fn key_props() -> Value {
    json!({
        "key": { "type": "integer", "minimum": 0, "maximum": 255, "description": "Virtual-key code." },
        "ctrl": { "type": "boolean" },
        "shift": { "type": "boolean" },
        "alt": { "type": "boolean" },
    })
}

/// The test surface, in a stable order (the docs check compares it with docs/TESTING.md).
pub const TOOLS: &[ToolDef] = &[
    // ------------------------------------------------------------------ A. core and shell
    ToolDef {
        name: "test_info",
        title: "Shell snapshot",
        description: "The shell's debug.info snapshot (window, tabs, overlays, rounded, browsers, controller, ipc, keyboard, permissions, suggest, sidebarHover, automation, foreign, devtools, devtoolsCdp, downloads, focus), plus a monotonic `at` in milliseconds for browser-side timing. Ask for `sections` to keep the answer small.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "sections": { "type": "array", "maxItems": 20, "items": { "type": "string" }, "description": "Section names to include; omit for all." },
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "test_state",
        title: "Core UiState",
        description: "The core's UiState, exactly as the `state.get` IPC request returns it to sta's own surfaces.",
        read_only: false,
        schema: empty,
    },
    ToolDef {
        name: "test_dispatch",
        title: "Dispatch a command",
        description: "Dispatches one core Command (shell events included) through the controller, like debug.dispatch.",
        read_only: false,
        schema: || object(json!({ "command": { "type": "object", "description": "A core Command, as the mock protocol spells it." } }), &["command"]),
    },
    ToolDef {
        name: "test_execute",
        title: "Run effects",
        description: "Runs one Effect or an array of Effects through controller::run_effects, like debug.execute.",
        read_only: false,
        schema: || object(json!({ "effects": { "description": "An Effect object or an array of them." } }), &["effects"]),
    },
    ToolDef {
        name: "test_push_state",
        title: "Push a state event",
        description: "Pushes a `state` event to every IPC subscriber now (debug.pushState).",
        read_only: false,
        schema: empty,
    },
    ToolDef {
        name: "test_open_tab",
        title: "Open a tab",
        description: "Opens any URL (internal pages included) in a tab with a fresh id, without agent policy, without marking the tab agent-controlled (debug.openTab).",
        read_only: false,
        schema: || object(json!({ "url": { "type": "string" }, "show": { "type": "boolean", "description": "Show the tab (default true)." } }), &["url"]),
    },
    ToolDef {
        name: "test_focus",
        title: "Focus a surface or tab",
        description: "Requests focus for a surface's or a tab's BrowserView (debug.focus).",
        read_only: false,
        schema: || object(json!({ "surface": { "type": "string", "description": "sta:// surface host name." }, "tab": tab_prop() }), &[]),
    },
    ToolDef {
        name: "test_accelerator",
        title: "Run an accelerator",
        description: "Runs the accelerator handler bound to a key combo and returns its command id (debug.accelerator).",
        read_only: false,
        schema: || object(key_props(), &["key"]),
    },
    ToolDef {
        name: "test_send_key",
        title: "Send a key to the window",
        description: "Window::send_key_press on the main window (debug.sendKey). Fails with window_busy when the window is not active.",
        read_only: false,
        schema: || object(key_props(), &["key"]),
    },
    ToolDef {
        name: "test_reset_permissions",
        title: "Reset content settings",
        description: "Resets every CEF content setting the permission `bits` map to for an origin, like an expired one-time grant (debug.resetPermissions).",
        read_only: false,
        schema: || object(json!({ "origin": { "type": "string" }, "bits": { "type": "integer", "minimum": 0 } }), &["origin", "bits"]),
    },
    ToolDef {
        name: "test_counts",
        title: "Command counters",
        description: "Per-command dispatch counters and the number of accelerators (sugar over test_info's controller and keyboard sections).",
        read_only: false,
        schema: empty,
    },
    // ------------------------------------------------------------------ B. JavaScript
    ToolDef {
        name: "test_eval",
        title: "Evaluate JavaScript",
        description: "Runs an expression in any target — an sta:// surface, a tab, or a DevTools target id — through Runtime.evaluate, optionally with a user gesture. No settings gate, no scope, no site approval, and the tab is not marked agent-controlled.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "target": target_prop(),
                    "expression": { "type": "string" },
                    "awaitPromise": { "type": "boolean", "description": "Default true." },
                    "returnByValue": { "type": "boolean", "description": "Default true." },
                    "userGesture": { "type": "boolean", "description": "Default false." },
                    "sessionId": { "type": "string", "description": "DevTools session from test_attach." },
                    "timeoutMs": timeout_prop(),
                }),
                &["target", "expression"],
            )
        },
    },
    ToolDef {
        name: "test_invoke",
        title: "window.sta.invoke",
        description: "Calls window.sta.invoke(cmd, payload) inside a surface's own frame — the real trusted-page IPC path (message router, ipc.rs trusted_frame) — and returns `{ok}` or `{err, msg}`.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "target": target_prop(),
                    "cmd": { "type": "string" },
                    "payload": { "description": "Request payload (any JSON)." },
                    "timeoutMs": timeout_prop(),
                }),
                &["target", "cmd"],
            )
        },
    },
    // ------------------------------------------------------------------ C. input
    ToolDef {
        name: "test_real_keys",
        title: "Real OS keys",
        description: "Injects real OS keyboard input (SendInput) while sta is the foreground window, so Views accelerators and the keyboard handler run (debug.realKeys). `combo` like `ctrl+shift+k`, or explicit `steps`.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "combo": { "type": "string", "description": "`ctrl+shift+k`, `f5`, `alt+1`, `escape`, …" },
                    "steps": {
                        "type": "array",
                        "maxItems": 64,
                        "items": object(json!({ "key": { "type": "string" }, "down": { "type": "boolean" }, "up": { "type": "boolean" }, "waitMs": { "type": "integer", "minimum": 0, "maximum": 5000 } }), &[]),
                    },
                    "delayMs": { "type": "integer", "minimum": 0, "maximum": 2000, "description": "Between key transitions (default 0)." },
                    "activate": { "type": "boolean", "description": "Bring the window to the foreground first (default true)." },
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "test_post_mouse",
        title: "Post mouse messages",
        description: "Posts mouse messages to sta's own window in client DIP (no OS cursor, no other window sees them): `move`, `down`, `up`, `dblclick` or `waitMs` steps (debug.postMouse).",
        read_only: false,
        schema: || {
            object(
                json!({
                    "steps": {
                        "type": "array",
                        "maxItems": 64,
                        "items": object(
                            json!({
                                "type": { "type": "string", "enum": ["move", "down", "up", "dblclick"] },
                                "x": { "type": "number" },
                                "y": { "type": "number" },
                                "button": { "type": "string", "enum": ["left", "right"] },
                                "waitMs": { "type": "integer", "minimum": 0, "maximum": 5000 },
                            }),
                            &[],
                        ),
                    },
                }),
                &["steps"],
            )
        },
    },
    ToolDef {
        name: "test_hover_input",
        title: "Sidebar hover input",
        description: "Drives the sidebar hover reveal (debug.hoverInput): turn the pointer reveal on or off, feed a virtual pointer in client DIP, or run the guarded real-cursor path and get the hover snapshot back.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "enabled": { "type": "boolean" },
                    "pointer": { "description": "`{x, y, buttons?, overWindow?, ownedPopup?}`, or null for the real cursor." },
                    "realCursor": object(json!({ "x": { "type": "number" }, "y": { "type": "number" }, "holdMs": { "type": "integer", "minimum": 0, "maximum": 3000 } }), &["x", "y"]),
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "test_tab_key",
        title: "Key event into a tab",
        description: "Sends a key event to a tab's browser host so it reaches on_pre_key_event like the user's typing, without the OS foreground (debug.tabKey).",
        read_only: false,
        schema: || object(json!({ "tab": tab_prop(), "key": { "type": "string", "description": "One character (default `a`)." } }), &["tab"]),
    },
    // ------------------------------------------------------------------ D. native window, pixels, OS
    ToolDef {
        name: "test_window",
        title: "Native window info",
        description: "What win-probe.ps1 reported: hwnd, position, size, dpi, zoomed, iconic, thickFrame, enabled, foreground, the held modifier keys, and with `all` every top-level window of this process (class, title, owner, visible, hidden) including dialogs and Chrome-created ones.",
        read_only: false,
        schema: || object(json!({ "all": { "type": "boolean", "description": "Every top-level window of this process (default false)." } }), &[]),
    },
    ToolDef {
        name: "test_hit_test",
        title: "WM_NCHITTEST",
        description: "Sends WM_NCHITTEST to sta's own window at window-relative points and returns the codes (1 CLIENT, 2 CAPTION, 10-17 borders).",
        read_only: false,
        schema: || {
            object(
                json!({
                    "points": point_list("Window-relative points, device pixels unless `space` says otherwise."),
                    "space": { "type": "string", "enum": ["dip", "device"], "description": "Default `device` (what win-probe.ps1 took); `dip` scales by the window's DPI first." },
                }),
                &["points"],
            )
        },
    },
    ToolDef {
        name: "test_window_message",
        title: "Post a window message",
        description: "Posts WM_CLOSE, SC_RESTORE or SC_MINIMIZE to sta's own top-level window, or to one `hwnd` of this process (a native dialog).",
        read_only: false,
        schema: || {
            object(
                json!({
                    "message": { "type": "string", "enum": ["close", "restore", "minimize"] },
                    "hwnd": { "type": "integer", "description": "A window of this process; omit for the main window." },
                }),
                &["message"],
            )
        },
    },
    ToolDef {
        name: "test_dialog",
        title: "Answer a native dialog",
        description: "Presses one key in a modal dialog sta's main window owns (Chromium's \"Add extension?\" dialog and its siblings are views widgets: no child window to post to, and they hold the foreground themselves, so test_real_keys cannot reach them). `hwnd` defaults to the foreground window; it must belong to this process, be owned by sta's main window, be visible and be foreground, else `window_busy` or `no_such_target`.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "press": { "type": "string", "enum": ["enter", "escape", "space", "tab"], "description": "Default `enter` (the dialog's default button)." },
                    "hwnd": { "type": "integer", "description": "A window of this process; omit for the foreground window." },
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "test_capture",
        title: "Capture the window",
        description: "PNG of sta's own top-level window (PW_RENDERFULLCONTENT, never the screen), written to `out` and reported with its size and scale. `inline` returns base64 instead, up to 6 MB.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "out": { "type": "string", "description": "File path to write (default: a file in the profile's logs folder)." },
                    "region": { "type": "string", "enum": ["window", "client"], "description": "Default `window`." },
                    "hwnd": { "type": "integer", "description": "A window of this process; omit for the main window." },
                    "inline": { "type": "boolean", "description": "Return base64 `data` instead of writing a file (default false)." },
                }),
                &[],
            )
        },
    },
    ToolDef {
        name: "test_pixels",
        title: "Sample capture pixels",
        description: "Colors (`#rrggbb`) of a PNG written by test_capture at the given points, in DIP by default or in device pixels.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "path": { "type": "string" },
                    "points": point_list("Points in the capture."),
                    "space": { "type": "string", "enum": ["dip", "device"], "description": "Default `dip`." },
                }),
                &["path", "points"],
            )
        },
    },
    ToolDef {
        name: "test_clipboard_get",
        title: "Read the clipboard",
        description: "The clipboard's Unicode text (CF_UNICODETEXT), or null when it holds none.",
        read_only: false,
        schema: empty,
    },
    ToolDef {
        name: "test_clipboard_set",
        title: "Write the clipboard",
        description: "Puts Unicode text on the clipboard (platform::set_clipboard_text).",
        read_only: false,
        schema: || object(json!({ "text": { "type": "string", "maxLength": 100000 } }), &["text"]),
    },
    ToolDef {
        name: "test_zone_identifier",
        title: "Mark of the Web",
        description: "The `Zone.Identifier` alternate data stream of a downloaded file, or null when it has none.",
        read_only: false,
        schema: || object(json!({ "path": { "type": "string" } }), &["path"]),
    },
    ToolDef {
        name: "test_console_windows",
        title: "Console windows",
        description: "Console windows on the desktop: `current` (visible now), `seen` (every console window shown since arming or the last reset, so a flash is caught too — each with `className`, `title`, `pid`, `chain` (its ancestry, resolved while the process was alive), `userVisible` and `ours`) and `ours` (the entries attributable to `roots`, by ancestry or, for a console the default terminal hosts, by its title). Assert on `seen`: a window Windows Terminal hosts belongs to no tree of ours. `reset` clears the seen list.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "reset": { "type": "boolean", "description": "Forget everything seen so far (default false)." },
                    "roots": { "type": "array", "maxItems": 16, "items": { "type": "integer", "minimum": 0 }, "description": "Process ids whose descendants count as `ours`." },
                }),
                &[],
            )
        },
    },
    // ------------------------------------------------------------------ E. DevTools and targets
    ToolDef {
        name: "test_targets",
        title: "List targets",
        description: "Every DevTools target the browser knows: its own browsers (surfaces, tabs, Chrome-created ones) with sta's role, and with `cdp` also what Target.getTargets reports (extension service workers).",
        read_only: false,
        schema: || object(json!({ "cdp": { "type": "boolean", "description": "Also ask Target.getTargets (default true)." } }), &[]),
    },
    ToolDef {
        name: "test_cdp",
        title: "Raw DevTools call",
        description: "Any DevTools method on any target, bypassing the agent allowlist, exactly like the debug.cdp request the suites use today. The single most dangerous tool of the surface (docs/TESTING.md).",
        read_only: false,
        schema: || {
            object(
                json!({
                    "target": target_prop(),
                    "method": { "type": "string" },
                    "params": { "type": "object" },
                    "sessionId": { "type": "string" },
                    "timeoutMs": timeout_prop(),
                }),
                &["target", "method"],
            )
        },
    },
    ToolDef {
        name: "test_cdp_events",
        title: "Recent DevTools events",
        description: "The last 200 DevTools events of a target's session (params truncated), newest last; `clear` empties the buffer.",
        read_only: false,
        schema: || object(json!({ "target": target_prop(), "clear": { "type": "boolean" } }), &["target"]),
    },
    ToolDef {
        name: "test_attach",
        title: "Attach to a target",
        description: "Target.attachToTarget on a target id (an extension service worker, for instance) and returns the sessionId for test_cdp and test_eval.",
        read_only: false,
        schema: || object(json!({ "targetId": { "type": "string" }, "target": target_prop() }), &["targetId"]),
    },
    // ------------------------------------------------------------------ F. Chrome-created browsers
    ToolDef {
        name: "test_foreign",
        title: "Chrome-created browsers",
        description: "foreign.rs's snapshot: entries, counters, events, hidden windows and hook counters (debug.foreign).",
        read_only: false,
        schema: empty,
    },
    ToolDef {
        name: "test_foreign_close",
        title: "Close a Chrome-created browser",
        description: "Closes one Chrome-created browser now, ignoring the close rules (debug.foreign.close).",
        read_only: false,
        schema: || object(json!({ "id": { "type": "integer" } }), &["id"]),
    },
    ToolDef {
        name: "test_foreign_trigger",
        title: "Make Chromium create a browser",
        description: "Target.createTarget on the shell's own DevTools client, so Chromium creates a browser the way an extension's tabs.create does (debug.foreign.trigger).",
        read_only: false,
        schema: || object(json!({ "url": { "type": "string" } }), &["url"]),
    },
    // ------------------------------------------------------------------ G. batching
    ToolDef {
        name: "test_batch",
        title: "Several calls at once",
        description: "Runs up to 16 test calls in order in one round trip (a poll predicate that needs two or three observations). No nested test_batch and no test_real_keys inside.",
        read_only: false,
        schema: || {
            object(
                json!({
                    "calls": {
                        "type": "array",
                        "maxItems": 16,
                        "items": object(json!({ "name": { "type": "string" }, "args": { "type": "object" } }), &["name"]),
                    },
                    "stopOnError": { "type": "boolean", "description": "Default true." },
                }),
                &["calls"],
            )
        },
    },
];

/// The test tool `name`, or `None`.
pub fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

/// `true` for a name that belongs to the test surface (armed or not).
pub fn is_test_tool(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// `_meta` key every test tool carries in `tools/list`, so a client can filter the whole surface
/// out in one predicate (`tool._meta?.["sta/test"] === true`).
pub const META_KEY: &str = "sta/test";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_shape() {
        assert_eq!(TOOLS.len(), 35);
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        let unique = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), unique, "duplicate tool name");
        for t in TOOLS {
            assert!(is_test_tool(t.name), "{}", t.name);
            assert!(t.name.len() <= 64 && t.name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{}", t.name);
            assert!(!t.read_only, "{}: the test surface is never read-only", t.name);
            assert!(t.description.len() <= 1000, "{}", t.name);
            let schema = (t.schema)();
            assert_eq!(schema["type"], "object", "{}", t.name);
            assert_eq!(schema["additionalProperties"], false, "{}", t.name);
            let props = schema["properties"].as_object().unwrap();
            for r in schema["required"].as_array().unwrap() {
                assert!(props.contains_key(r.as_str().unwrap()), "{}: required {r} is not a property", t.name);
            }
            for (name, p) in props {
                if p["type"] == "object" && p.get("properties").is_some() {
                    assert_eq!(p["additionalProperties"], false, "{}.{name}", t.name);
                }
                if p["type"] == "array" && p["items"]["type"] == "object" {
                    assert_eq!(p["items"]["additionalProperties"], false, "{}.{name}", t.name);
                }
            }
        }
        assert_eq!(META_KEY, "sta/test");
    }

    /// No test tool may shadow a shipped tool, and no shipped tool may look like a test tool.
    #[test]
    fn the_two_catalogs_are_disjoint() {
        for t in TOOLS {
            assert!(super::super::tools::find(t.name).is_none(), "{}", t.name);
        }
        for t in super::super::tools::TOOLS {
            assert!(!is_test_tool(t.name), "{}", t.name);
        }
    }
}
