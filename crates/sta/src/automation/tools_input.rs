//! Input tools beyond click / type / press_key [owner: automation] (docs/MCP.md "Tools"):
//! `hover`, `select_option`, `scroll`, `fill_form`.
//!
//! Same rules as `tools.rs`: full access, tab targeting and site policy on every call, one call per
//! tab at a time, the refs' document re-checked right before input. Mouse input (`hover`) needs the
//! tab on screen; the others use DOM, focus and keyboard input and work on background tabs.
//! Page-derived strings (option labels) come back inside the untrusted-content boundary; values
//! the agent fills are never echoed or logged.

use super::cdp::p;
use super::page::{self, Frame};
use super::tools::{self, Ctx, Output, Target, Want, err, invalid};
use super::{guards, session};
use sta_core::agent::channel::{Content, ToolError};
use sta_core::agent::tools::*;
use sta_core::agent::{ErrorCode, keys, text};
use serde_json::{Value, json};
use std::time::Instant;

// ----------------------------------------------------------------------------------- hover

pub async fn hover(ctx: &Ctx, args: HoverArgs) -> Result<Output, ToolError> {
    tools::full_access_required("hover")?;
    let tab = tools::pick_tab(ctx, args.tab, args.element.as_deref())?;
    let _lock = tools::lock_tab(tab).await?;
    let t = tools::target(ctx, tab, Want::default()).await?;
    tools::check_visible(tab)?;
    tools::check_input_allowed(tab)?;
    session::take_action_token(ctx.conn)?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let (x, y, what) = match (&args.element, args.x, args.y) {
        (Some(r), None, None) => {
            let (x, y) = tools::element_point(&t, &frame, r, ctx.deadline).await?;
            (x, y, format!("ref {r}"))
        }
        (None, Some(x), Some(y)) if x.is_finite() && y.is_finite() => (x, y, format!("point ({x:.0}, {y:.0})")),
        (None, Some(_), Some(_)) => return Err(invalid("x and y must be numbers")),
        _ => return Err(invalid("Give either `ref` or both `x` and `y`")),
    };
    tools::ensure_same_document(&t, &frame, ctx.deadline).await?;
    guards::mark_controlled(tab, ctx.session);
    let _ = guards::take_events(tab);
    tools::mouse(&t, "mouseMoved", x, y, "none", 0, ctx.deadline).await?;
    // Let hover handlers (menus, tooltips) run before the agent looks again.
    tools::wait_ms(150).await;
    let events = guards::take_events(tab);
    let mut lines = vec![format!("The mouse is over {what} in tab {tab}. Take a page_snapshot or page_screenshot to see what appeared.")];
    let (extra, mut structured) = tools::describe_events(tab, &events, false, Some(&t.url));
    lines.extend(extra);
    structured["x"] = json!(x);
    structured["y"] = json!(y);
    Ok(Output { content: vec![Content::Text { text: lines.join("\n") }], structured: Some(structured), tab: Some(tab), site: t.site })
}

// ----------------------------------------------------------------------------------- select_option

fn field_kind(info: &Value) -> (&str, &str) {
    (info["kind"].as_str().unwrap_or("other"), info["type"].as_str().unwrap_or("element"))
}

/// The options `select_in` selected.
struct Selection {
    /// Option labels: page strings.
    labels: Vec<String>,
    /// 1-based positions among the select's `total` options, in the order of `labels`.
    positions: Vec<u64>,
    total: u64,
}

/// Selects `values` in the `<select>` `backend` (ref `r`).
async fn select_in(ctx: &Ctx, t: &Target, frame: &Frame, r: &str, backend: i64, values: &[String]) -> Result<Selection, ToolError> {
    let info = page::eval_node(t.browser, frame, backend, page::FIELD_KIND, vec![], ctx.deadline).await?;
    let (kind, ty) = field_kind(&info);
    if kind != "select" {
        let ty: String = ty.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '=' | '-')).take(40).collect();
        return Err(invalid(format!("Ref {r} is not a <select> element (it is {ty})")).with_hint("For custom dropdowns, click the control and then the option in a new page_snapshot."));
    }
    if info["disabled"].as_bool() == Some(true) {
        return Err(invalid(format!("Ref {r} is disabled")));
    }
    guards::mark_controlled(t.tab, ctx.session);
    tools::ensure_same_document(t, frame, ctx.deadline).await?;
    let v = page::eval_node(t.browser, frame, backend, page::SELECT_OPTIONS, vec![json!(values)], ctx.deadline).await?;
    let n = tools::nonce();
    match v["error"].as_str() {
        None => Ok(Selection {
            labels: v["selected"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            positions: v["positions"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default(),
            total: v["total"].as_u64().unwrap_or_default(),
        }),
        Some("single") => Err(invalid(format!("Ref {r} allows only one option; give one value"))),
        Some("option-disabled") => Err(invalid(format!("The option {} of ref {r} is disabled", text::quote(v["value"].as_str().unwrap_or_default(), 100)))),
        Some(_) => {
            let options: Vec<String> = v["options"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(|s| text::quote(s, 80)).collect()).unwrap_or_default();
            Err(err(ErrorCode::ElementNotFound, format!("No option of ref {r} matches {}. Its options: {}", text::quote(v["value"].as_str().unwrap_or_default(), 100), text::untrusted(&n, &options.join(", "))))
                .with_hint("Use one of the option values or labels listed."))
        }
    }
}

pub async fn select_option(ctx: &Ctx, args: SelectOptionArgs) -> Result<Output, ToolError> {
    tools::full_access_required("select_option")?;
    if args.values.is_empty() || args.values.len() > 100 {
        return Err(invalid("values needs 1 to 100 entries"));
    }
    let tab = tools::pick_tab(ctx, args.tab, Some(&args.element))?;
    let _lock = tools::lock_tab(tab).await?;
    let t = tools::target(ctx, tab, Want::default()).await?;
    tools::check_input_allowed(tab)?;
    session::take_action_token(ctx.conn)?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let backend = page::resolve_ref(tab, t.browser, &frame, &args.element)?;
    let _ = guards::take_events(tab);
    let (starts_before, _) = guards::load_info(tab);
    let selected = select_in(ctx, &t, &frame, &args.element, backend, &args.values).await?.labels;
    let navigated = tools::settle(&t, starts_before, ctx.deadline).await;
    let events = guards::take_events(tab);
    let n = tools::nonce();
    let labels: Vec<String> = selected.iter().map(|s| text::quote(s, 120)).collect();
    let mut lines = vec![format!("Selected {} option{} in ref {} in tab {tab}:\n{}", selected.len(), if selected.len() == 1 { "" } else { "s" }, args.element, text::untrusted(&n, &labels.join(", ")))];
    let (extra, mut structured) = tools::describe_events(tab, &events, navigated, Some(&t.url));
    lines.extend(extra);
    structured["selected"] = json!(selected.len());
    Ok(Output { content: vec![Content::Text { text: lines.join("\n") }], structured: Some(structured), tab: Some(tab), site: t.site })
}

// ----------------------------------------------------------------------------------- scroll

pub async fn scroll(ctx: &Ctx, args: ScrollArgs) -> Result<Output, ToolError> {
    tools::full_access_required("scroll")?;
    if args.element.is_none() && args.direction.is_none() {
        return Err(invalid("Give `ref` (scroll it into view) or `direction` (scroll the page)"));
    }
    if args.direction.is_none() && args.amount.is_some() {
        return Err(invalid("`amount` needs a `direction`"));
    }
    let tab = tools::pick_tab(ctx, args.tab, args.element.as_deref())?;
    let _lock = tools::lock_tab(tab).await?;
    let t = tools::target(ctx, tab, Want::default()).await?;
    tools::check_input_allowed(tab)?;
    session::take_action_token(ctx.conn)?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let backend = match &args.element {
        Some(r) => Some(page::resolve_ref(tab, t.browser, &frame, r)?),
        None => None,
    };
    // A tab opened in the background has no viewport until it is shown once (it keeps its size
    // when it goes to the background again).
    let state = tools::page_state(&t, &frame, ctx.deadline).await?;
    if state["width"].as_f64().unwrap_or(0.0) < 1.0 || state["height"].as_f64().unwrap_or(0.0) < 1.0 {
        return Err(err(ErrorCode::TabNotVisible, format!("Tab {tab} has never been on screen, so it has no viewport to scroll"))
            .with_hint("Call tab_show once; afterwards scrolling works while the tab is in the background too."));
    }
    guards::mark_controlled(tab, ctx.session);
    let Some(direction) = args.direction else {
        // `ref` alone: bring it into view.
        let (r, backend) = (args.element.as_deref().unwrap_or_default(), backend.unwrap_or_default());
        page::call(t.browser, p::ScrollIntoViewIfNeeded { backend_node_id: backend }, ctx.deadline)
            .await
            .map_err(|_| err(ErrorCode::ElementNotFound, format!("Ref {r} can't be scrolled into view (hidden or not rendered?)")))?;
        let b = page::eval_node(t.browser, &frame, backend, page::IN_VIEW, vec![], ctx.deadline).await?;
        let in_view = b["inView"].as_bool().unwrap_or(false);
        let text = format!(
            "Scrolled ref {r} into view in tab {tab}{}.",
            if in_view { "" } else { " (it still isn't inside the visible area: it may be hidden, or the tab has no size in the background)" }
        );
        return Ok(Output { content: vec![Content::Text { text }], structured: Some(json!({ "tab": tab, "inView": in_view, "x": b["x"], "y": b["y"] })), tab: Some(tab), site: t.site });
    };
    let (axis, sign) = match direction {
        ScrollDirection::Up => ("y", -1),
        ScrollDirection::Down => ("y", 1),
        ScrollDirection::Left => ("x", -1),
        ScrollDirection::Right => ("x", 1),
    };
    let amount = args.amount.map(|a| json!(a.min(100_000))).unwrap_or(Value::Null);
    let call_args = vec![json!(axis), json!(sign), amount];
    let v = match backend {
        Some(b) => page::eval_node(t.browser, &frame, b, page::SCROLL_BY, call_args, ctx.deadline).await?,
        None => page::eval_document(t.browser, &frame, page::SCROLL_BY, call_args, ctx.deadline, page::CALL_MS).await?,
    };
    let (x, y) = (v["x"].as_f64().unwrap_or(0.0), v["y"].as_f64().unwrap_or(0.0));
    let (max_x, max_y) = (v["maxX"].as_f64().unwrap_or(0.0), v["maxY"].as_f64().unwrap_or(0.0));
    let moved = v["moved"].as_bool().unwrap_or(false);
    let what = match &args.element {
        Some(r) => format!("ref {r}"),
        None => "the page".to_string(),
    };
    let dir = serde_json::to_value(direction).ok().and_then(|d| d.as_str().map(str::to_string)).unwrap_or_default();
    let at_end = match direction {
        ScrollDirection::Up => y <= 0.5,
        ScrollDirection::Down => y >= max_y - 1.0,
        ScrollDirection::Left => x <= 0.5,
        ScrollDirection::Right => x >= max_x - 1.0,
    };
    let mut text = if moved {
        format!("Scrolled {what} {dir} in tab {tab}: now at x {x:.0}, y {y:.0} (of at most {max_x:.0}, {max_y:.0}).")
    } else {
        format!("{what} didn't scroll {dir} in tab {tab} (x {x:.0}, y {y:.0}, at most {max_x:.0}, {max_y:.0}).")
    };
    if at_end {
        text.push_str(&format!(" It is at the {} end.", match direction {
            ScrollDirection::Up => "top",
            ScrollDirection::Down => "bottom",
            ScrollDirection::Left => "left",
            ScrollDirection::Right => "right",
        }));
    }
    if !moved && args.element.is_some() && !at_end {
        text.push_str(" The element may not be scrollable itself: omit ref to scroll the page.");
    }
    Ok(Output {
        content: vec![Content::Text { text }],
        structured: Some(json!({ "tab": tab, "x": x, "y": y, "maxX": max_x, "maxY": max_y, "moved": moved, "atEnd": at_end })),
        tab: Some(tab),
        site: t.site,
    })
}

// ----------------------------------------------------------------------------------- fill_form

/// Sets the checked state of a checkbox, radio button or switch with the keyboard (focus, Space),
/// falling back to a mouse click when the tab is on screen.
#[allow(clippy::too_many_arguments)]
async fn set_checked(ctx: &Ctx, t: &Target, frame: &Frame, r: &str, backend: i64, role: &str, want: bool, deadline: Instant) -> Result<(), ToolError> {
    if role == "radio" && !want {
        return Err(invalid(format!("Ref {r} is a radio button: it can't be unchecked, check another one instead")));
    }
    let current = page::eval_node(t.browser, frame, backend, page::CHECKED_OF, vec![], deadline).await?.as_bool().unwrap_or(false);
    if current == want {
        return Ok(());
    }
    guards::mark_controlled(t.tab, ctx.session);
    tools::focus_ref(t, frame, backend, deadline).await?;
    tools::ensure_same_document(t, frame, deadline).await?;
    let space = keys::parse("Space").map_err(invalid)?;
    tools::wait_keyboard_ready(t, frame, deadline).await?;
    tools::key_press(t, &space, deadline).await?;
    // A page may update the state from its own key handler a moment later.
    for _ in 0..10 {
        tools::wait_ms(30).await;
        let now = page::eval_node(t.browser, frame, backend, page::CHECKED_OF, vec![], deadline).await?.as_bool().unwrap_or(false);
        if now == want {
            return Ok(());
        }
    }
    // Some custom widgets only react to the mouse.
    if tools::check_visible(t.tab).is_ok() {
        let (x, y) = tools::element_point(t, frame, r, deadline).await?;
        tools::ensure_same_document(t, frame, deadline).await?;
        let delivered = tools::mouse(t, "mouseMoved", x, y, "none", 0, deadline).await? && tools::mouse(t, "mousePressed", x, y, "left", 1, deadline).await? && tools::mouse(t, "mouseReleased", x, y, "left", 1, deadline).await?;
        tools::wait_ms(30).await;
        let now = page::eval_node(t.browser, frame, backend, page::CHECKED_OF, vec![], deadline).await?.as_bool().unwrap_or(false);
        if delivered && now == want {
            return Ok(());
        }
        return Err(err(ErrorCode::FocusLost, format!("Ref {r} didn't change to {} (the page rejected it)", if want { "checked" } else { "unchecked" })));
    }
    Err(err(ErrorCode::TabNotVisible, format!("Ref {r} didn't react to the keyboard and tab {} is in the background", t.tab)).with_hint("Call tab_show so the field can be clicked, then retry."))
}

pub async fn fill_form(ctx: &Ctx, args: FillFormArgs) -> Result<Output, ToolError> {
    tools::full_access_required("fill_form")?;
    args.validate().map_err(invalid)?;
    let first_ref = args.fields.first().map(|f| f.element.clone());
    let tab = tools::pick_tab(ctx, args.tab, first_ref.as_deref())?;
    let _lock = tools::lock_tab(tab).await?;
    let t = tools::target(ctx, tab, Want::default()).await?;
    tools::check_input_allowed(tab)?;
    session::take_action_token(ctx.conn)?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    // Every ref must belong to this tab's current page before anything is filled.
    let mut backends = Vec::with_capacity(args.fields.len());
    for (i, f) in args.fields.iter().enumerate() {
        let backend = page::resolve_ref(tab, t.browser, &frame, &f.element).map_err(|e| prefix(e, i, args.fields.len(), &f.element, 0))?;
        backends.push(backend);
    }
    let _ = guards::take_events(tab);
    let (starts_before, _) = guards::load_info(tab);
    let mut done: Vec<String> = Vec::new();
    // "ref …: <label>" of each select: page strings, sent inside an untrusted-content boundary.
    let mut chosen: Vec<String> = Vec::new();
    for (i, (f, backend)) in args.fields.iter().zip(backends).enumerate() {
        let r = f.element.as_str();
        let step = async {
            tools::check_input_allowed(tab)?;
            let info = page::eval_node(t.browser, &frame, backend, page::FIELD_KIND, vec![], ctx.deadline).await?;
            let (kind, ty) = field_kind(&info);
            let ty: String = ty.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '=' | '-')).take(40).collect();
            if info["disabled"].as_bool() == Some(true) {
                return Err(invalid(format!("Ref {r} is disabled or read-only")));
            }
            match (kind, &f.value, f.checked) {
                ("file", _, _) => Err(err(ErrorCode::FileChooserBlocked, "That is a file input; agents can't choose files")),
                ("check", None, Some(want)) => {
                    let role = info["role"].as_str().unwrap_or("checkbox").to_string();
                    set_checked(ctx, &t, &frame, r, backend, &role, want, ctx.deadline).await?;
                    Ok(format!("ref {r} ({ty}) {}", if want { "checked" } else { "unchecked" }))
                }
                ("check", Some(_), None) => Err(invalid(format!("Ref {r} is a {ty}: give `checked`, not `value`"))),
                (_, None, Some(_)) => Err(invalid(format!("Ref {r} is a {ty}, not a checkbox: give `value`"))),
                ("select", Some(v), None) => {
                    let selection = select_in(ctx, &t, &frame, r, backend, std::slice::from_ref(v)).await?;
                    if let Some(label) = selection.labels.first() {
                        chosen.push(format!("ref {r}: {}", text::quote(label, 120)));
                    }
                    let position = selection.positions.first().copied().unwrap_or_default();
                    Ok(format!("ref {r} (select) set to option {position} of {}", selection.total))
                }
                ("value", Some(v), None) => {
                    guards::mark_controlled(tab, ctx.session);
                    tools::ensure_same_document(&t, &frame, ctx.deadline).await?;
                    let kept = page::eval_node(t.browser, &frame, backend, page::SET_INPUT_VALUE, vec![json!(v)], ctx.deadline).await?;
                    if kept.as_str() != Some(v.as_str()) {
                        return Err(invalid(format!("Ref {r} ({ty}) didn't accept the value")).with_hint("Use the input's own format, e.g. 2026-09-17 for dates, 13:45 for times, #ff8800 for colors, a number in range."));
                    }
                    Ok(format!("ref {r} ({ty}) set"))
                }
                ("text", Some(v), None) => {
                    tools::fill_text(ctx, &t, &frame, r, backend, v, true, false).await?;
                    Ok(format!("ref {r} ({ty}) filled with {} character{}", v.chars().count(), if v.chars().count() == 1 { "" } else { "s" }))
                }
                _ => Err(invalid(format!("Ref {r} is not a form field (it is {ty})")).with_hint("fill_form fills text fields, selects, checkboxes, radio buttons and date/range/color inputs; use click for anything else.")),
            }
        };
        match step.await {
            Ok(line) => done.push(line),
            Err(e) => return Err(prefix(e, i, args.fields.len(), r, done.len())),
        }
    }
    let navigated = tools::settle(&t, starts_before, ctx.deadline).await;
    let events = guards::take_events(tab);
    let mut lines = vec![format!("Filled {} field{} in tab {tab}:", done.len(), if done.len() == 1 { "" } else { "s" })];
    lines.extend(done.iter().map(|d| format!("- {d}")));
    if !chosen.is_empty() {
        let n = tools::nonce();
        lines.push(format!("Chosen option{}:\n{}", if chosen.len() == 1 { "" } else { "s" }, text::untrusted(&n, &chosen.join("\n"))));
    }
    let (extra, mut structured) = tools::describe_events(tab, &events, navigated, Some(&t.url));
    lines.extend(extra);
    structured["filled"] = json!(done.len());
    Ok(Output { content: vec![Content::Text { text: lines.join("\n") }], structured: Some(structured), tab: Some(tab), site: t.site })
}

/// Names the failing field in an error: `Field 2 of 5 (ref 3.1.7): … (1 field before it was filled)`.
fn prefix(e: ToolError, index: usize, total: usize, r: &str, filled: usize) -> ToolError {
    let before = match filled {
        0 => " Nothing was filled.".to_string(),
        1 => " The 1 field before it was filled.".to_string(),
        n => format!(" The {n} fields before it were filled."),
    };
    let mut out = ToolError::new(e.code, format!("Field {} of {total} (ref {r}): {}.{before}", index + 1, e.message.trim_end_matches('.')));
    out.hint = e.hint;
    out
}
