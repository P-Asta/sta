//! Page tools beyond snapshot / text / screenshot [owner: automation] (docs/MCP.md "Tools"):
//! `page_find`, `evaluate`, `console_messages`.
//!
//! - `page_find` searches the same readable text `page_text` returns, with Rust's `regex` (never a
//!   page-side regex), so match offsets are `page_text` offsets.
//! - `evaluate` needs full access and the `agentScripts` setting (`isolated`, or `main` for the page's
//!   own world). The function text is never logged. Results are page-derived: they come back inside
//!   the untrusted-content boundary.
//! - `console_messages` reads the in-memory buffer `console.rs` fills while access is on.

use super::page;
use super::tools::{self, Ctx, Output, Want, err, invalid};
use super::{console, guards, session};
use sta_core::agent::channel::{Content, ToolError};
use sta_core::agent::tools::*;
use sta_core::agent::{ErrorCode, find, text};
use sta_core::AgentScripts;
use serde_json::{Value, json};

/// Longest `evaluate` run (ms), within the call's deadline.
const SCRIPT_MS: i64 = 30_000;

// ----------------------------------------------------------------------------------- page_find

pub async fn page_find(ctx: &Ctx, args: PageFindArgs) -> Result<Output, ToolError> {
    let re = find::pattern(args.text.as_deref(), args.regex.as_deref(), args.case_sensitive.unwrap_or(false)).map_err(|e| {
        let what = if args.regex.is_some() && args.text.is_none() { "regex" } else { "text/regex" };
        invalid(format!("{what}: {e}"))
    })?;
    let max_results = args.max_results.map_or(find::DEFAULT_RESULTS, |m| (m as usize).clamp(1, find::MAX_RESULTS));
    let tab = tools::pick_tab(ctx, args.tab, args.element.as_deref())?;
    let _lock = tools::lock_tab(tab).await?;
    let t = tools::target(ctx, tab, Want::default()).await?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let value = match &args.element {
        Some(r) => {
            let backend = page::resolve_ref(tab, t.browser, &frame, r)?;
            page::eval_node(t.browser, &frame, backend, page::EXTRACT_TEXT, vec![json!("text")], ctx.deadline).await?
        }
        None => page::eval_document(t.browser, &frame, page::EXTRACT_TEXT, vec![json!("text")], ctx.deadline, 10_000).await?,
    };
    let haystack = value.as_str().unwrap_or_default();
    let result = find::find(haystack, &re, max_results);
    let what = match (&args.text, &args.regex) {
        (Some(s), _) => format!("text {}", text::quote(s, 100)),
        (_, Some(s)) => format!("regex {}", text::quote(s, 100)),
        _ => String::new(),
    };
    let scope = match &args.element {
        Some(r) => format!("ref {r} in tab {tab}"),
        None => format!("tab {tab}"),
    };
    let total = if result.capped { format!("at least {}", result.total) } else { result.total.to_string() };
    let out = if result.total == 0 {
        format!("No match for {what} in {scope} ({} characters of text searched).", haystack.chars().count())
    } else {
        let n = tools::nonce();
        let lines: Vec<String> = result.matches.iter().map(|m| format!("- offset {}: {}", m.offset, text::quote(&m.context, 400))).collect();
        let shown = if result.matches.len() < result.total { format!(", showing the first {}", result.matches.len()) } else { String::new() };
        format!(
            "{total} match{} for {what} in {scope}{shown}. Offsets are page_text offsets (in the text format).\n{}",
            if result.total == 1 { "" } else { "es" },
            text::untrusted(&n, &lines.join("\n"))
        )
    };
    let offsets: Vec<Value> = result.matches.iter().map(|m| json!({ "offset": m.offset, "length": m.len })).collect();
    Ok(Output {
        content: vec![Content::Text { text: out }],
        structured: Some(json!({ "tab": tab, "total": result.total, "capped": result.capped, "matches": offsets, "textLength": haystack.chars().count() })),
        tab: Some(tab),
        site: t.site,
    })
}

// ----------------------------------------------------------------------------------- evaluate

/// A readable rendering of a `Runtime.RemoteObject` returned by value.
fn render_result(remote: &Value) -> (String, String) {
    let kind = remote["type"].as_str().unwrap_or("undefined").to_string();
    let rendered = if let Some(u) = remote["unserializableValue"].as_str() {
        u.to_string()
    } else if kind == "undefined" {
        "undefined".to_string()
    } else if let Some(v) = remote.get("value") {
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    } else {
        remote["description"].as_str().map(|d| format!("<{kind}: {d}>")).unwrap_or_else(|| format!("<{kind}>"))
    };
    (kind, rendered)
}

pub async fn evaluate(ctx: &Ctx, args: EvaluateArgs) -> Result<Output, ToolError> {
    tools::full_access_required("evaluate")?;
    let world = args.world.unwrap_or_default();
    let scripts = tools::settings().map(|s| s.agent_scripts).unwrap_or_default();
    match (scripts, world) {
        (AgentScripts::Off, _) => return Err(err(ErrorCode::ScriptsDisabled, "Page scripts are off for agents in sta Settings")),
        (AgentScripts::Isolated, ScriptWorld::Main) => {
            return Err(err(ErrorCode::ScriptsDisabled, "Scripts in the page's own world are off for agents (only the isolated world is allowed)")
                .with_hint("Retry with world \"isolated\" (the DOM is the same; the page's JavaScript variables aren't visible), or ask the user to allow \"Page\" scripts."));
        }
        _ => {}
    }
    let function = args.function.trim();
    if function.is_empty() || function.chars().count() > 20_000 {
        return Err(invalid("function must be 1 to 20000 characters"));
    }
    let tab = tools::pick_tab(ctx, args.tab, args.element.as_deref())?;
    let _lock = tools::lock_tab(tab).await?;
    let t = tools::target(ctx, tab, Want::default()).await?;
    session::take_action_token(ctx.conn)?;
    let frame = page::main_frame(t.browser, ctx.deadline).await?;
    let element = match &args.element {
        Some(r) => Some(page::resolve_ref(tab, t.browser, &frame, r)?),
        None => None,
    };
    // A script can click, submit and navigate: the same guards as input.
    guards::mark_controlled(tab, ctx.session);
    let _ = guards::take_events(tab);
    let (starts_before, _) = guards::load_info(tab);
    let main = world == ScriptWorld::Main;
    let outcome = page::run_agent_function(t.browser, &frame, main, element, function, ctx.deadline, SCRIPT_MS).await.map_err(|e| {
        if e.code == ErrorCode::Timeout {
            err(ErrorCode::Timeout, format!("The function didn't finish in tab {tab} (a promise that never settles, a long loop, or a dialog)"))
        } else {
            e
        }
    })?;
    let world_name = if main { "main" } else { "isolated" };
    let n = tools::nonce();
    let remote = match outcome {
        Ok(v) => v,
        Err(message) => {
            return Err(err(ErrorCode::ScriptError, format!("The function failed in tab {tab} ({world_name} world): {}", text::untrusted(&n, &message))));
        }
    };
    let navigated = tools::settle(&t, starts_before, ctx.deadline).await;
    let events = guards::take_events(tab);
    let (kind, rendered) = render_result(&remote);
    let budget = text::budget(None);
    let (keep, truncated) = text::fit_chars(&rendered, budget);
    let body: String = rendered.chars().take(keep).collect();
    let mut lines = vec![format!(
        "Result of the function in tab {tab} ({world_name} world, {kind}{}):\n{}",
        if truncated { format!(", cut to {keep} of {} characters", rendered.chars().count()) } else { String::new() },
        text::untrusted(&n, &body)
    )];
    let (extra, mut structured) = tools::describe_events(tab, &events, navigated, Some(&t.url));
    lines.extend(extra);
    structured["world"] = json!(world_name);
    structured["type"] = json!(kind);
    structured["truncated"] = json!(truncated);
    Ok(Output { content: vec![Content::Text { text: lines.join("\n") }], structured: Some(structured), tab: Some(tab), site: t.site })
}

// ----------------------------------------------------------------------------------- console

pub async fn console_messages(ctx: &Ctx, args: ConsoleMessagesArgs) -> Result<Output, ToolError> {
    let tab = tools::pick_tab(ctx, args.tab, None)?;
    let t = tools::target(ctx, tab, Want { dialog_ok: true }).await?;
    let level = args.level.unwrap_or_default();
    let limit = args.limit.map_or(sta_core::agent::console::DEFAULT_LIMIT, |l| (l as usize).clamp(1, sta_core::agent::console::MAX_MESSAGES));
    let (listing, kept, dropped) = console::listing(tab, level, limit, text::budget(None));
    let filter = if level == ConsoleLevel::Debug { String::new() } else { format!(" at level {} or above", level.as_str()) };
    let out = if listing.lines.is_empty() {
        format!(
            "No console messages{filter} in tab {tab} ({kept} recorded). Messages are recorded while agent access is on; reload the page (tab_navigate action reload) to see messages logged while it loads."
        )
    } else {
        let n = tools::nonce();
        let mut head = format!("{} console message{}{filter} in tab {tab}, oldest first", listing.lines.len(), if listing.lines.len() == 1 { "" } else { "s" });
        if listing.omitted > 0 {
            head.push_str(&format!(" ({} older one{} left out: raise limit or filter by level)", listing.omitted, if listing.omitted == 1 { "" } else { "s" }));
        }
        if dropped > 0 {
            head.push_str(&format!("; {dropped} earlier message{} no longer kept", if dropped == 1 { " is" } else { "s are" }));
        }
        format!("{head}:\n{}", text::untrusted(&n, &listing.lines.join("\n")))
    };
    Ok(Output {
        content: vec![Content::Text { text: out }],
        structured: Some(json!({ "tab": tab, "shown": listing.lines.len(), "matching": listing.matching, "omitted": listing.omitted, "dropped": dropped })),
        tab: Some(tab),
        site: t.site,
    })
}
