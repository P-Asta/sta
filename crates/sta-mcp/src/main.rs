//! `sta-mcp`: an MCP server over stdio that lets AI agents (Claude Code, Claude Desktop, VS Code,
//! Cursor, …) use the sta browser (docs/MCP.md).
//!
//! ```text
//! sta-mcp [--data-dir <dir>] [--no-launch]
//! sta-mcp --check [--data-dir <dir>]
//! ```
//!
//! - stdout carries MCP JSON-RPC only; diagnostics go to stderr; the process exits on stdin EOF.
//! - `tools/list` is static (works while sta is closed); `tools/call` connects lazily to the
//!   browser's agent pipe (named in `<data>/sta/agent-endpoint.json`; also found in the old layout
//!   of a data folder from before the rename that sta runs in place, `channel::endpoint_candidates`),
//!   launching the sibling `sta.exe` when it isn't running (unless `--no-launch`).
//! - The bridge holds no state beyond the connection: the browser enforces access, approval, scope
//!   and site policy for every call.
//! - `--check` (sta Settings → Test connection) reaches the running browser's pipe like a tool
//!   call would (endpoint file; owner, session, integrity and server-process checks), disconnects
//!   without a hello, prints one
//!   JSON line `{"ok": true, "message"}` or `{"ok": false, "code", "message"}` and exits 0 or 1.

// The debug-only MCP test surface (docs/TESTING.md) must never reach a release binary. The module
// itself is behind `debug_assertions` as well, which would silently *drop* it here instead — so
// this guard sits where a release build sees it and fails loudly.
#[cfg(all(feature = "test-hooks", not(debug_assertions)))]
compile_error!("the `test-hooks` feature is debug-only: it must never be built into a release binary");

mod channel;
mod server;
#[cfg(windows)]
mod win;

use rmcp::ServiceExt;
use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "sta-mcp: MCP server for the sta browser (stdio)

Usage: sta-mcp [--data-dir <dir>] [--no-launch]
       sta-mcp --check [--data-dir <dir>]

  --data-dir <dir>  sta data directory (default: %LOCALAPPDATA%\\{data_dir})
  --no-launch       never start sta; fail with browser_not_running instead
  --check           check that sta is reachable (no MCP; prints one JSON line)

Register it with your MCP client, e.g.:
  claude mcp add sta -- \"C:\\path\\to\\sta-mcp.exe\"
";

struct Args {
    data_dir: Option<PathBuf>,
    launch: bool,
    check: bool,
    /// Debug + `test-hooks` builds: print the test surface's catalog and exit (docs check).
    test_tools: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { data_dir: None, launch: true, check: false, test_tools: false };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-launch" => args.launch = false,
            "--check" => args.check = true,
            "--test-tools" if cfg!(all(debug_assertions, feature = "test-hooks")) => args.test_tools = true,
            "--data-dir" => args.data_dir = Some(PathBuf::from(it.next().ok_or("--data-dir needs a directory")?)),
            "--help" | "-h" => return Err(String::new()),
            "--version" | "-V" => {
                eprintln!("sta-mcp {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => match other.strip_prefix("--data-dir=") {
                Some(dir) if !dir.is_empty() => args.data_dir = Some(PathBuf::from(dir)),
                _ => return Err(format!("unknown argument {other:?}")),
            },
        }
    }
    Ok(args)
}

/// `--test-tools`: the debug-only test catalog as JSON, without connecting to anything (the docs
/// check compares it with docs/TESTING.md). The tools themselves still need an armed browser.
fn print_test_tools() {
    let list: Vec<serde_json::Value> = server::test_tools().iter().map(|t| serde_json::to_value(t).unwrap_or_default()).collect();
    println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}\n");
            }
            eprint!("{}", USAGE.replace("{data_dir}", channel::DATA_DIR_NAME));
            std::process::exit(if e.is_empty() { 0 } else { 2 });
        }
    };
    if args.test_tools {
        print_test_tools();
        std::process::exit(0);
    }
    let Some(data_dir) = args.data_dir.or_else(channel::default_data_dir) else {
        eprintln!("[sta-mcp] no data directory (LOCALAPPDATA is not set; pass --data-dir)");
        std::process::exit(2);
    };
    let data_dir = std::path::absolute(&data_dir).unwrap_or(data_dir);
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[sta-mcp] cannot start the runtime: {e}");
            std::process::exit(1);
        }
    };
    if args.check {
        let channel = channel::Channel::new(data_dir, false);
        let (ok, line) = match runtime.block_on(channel.check()) {
            Ok(message) => (true, serde_json::json!({ "ok": true, "message": message })),
            Err(e) => (false, serde_json::json!({ "ok": false, "code": e.code.as_str(), "message": e.message })),
        };
        println!("{line}");
        std::process::exit(if ok { 0 } else { 1 });
    }
    let result = runtime.block_on(async move {
        let bridge = server::Bridge::new(Arc::new(channel::Channel::new(data_dir, args.launch)));
        let service = bridge.serve(rmcp::transport::stdio()).await.map_err(|e| format!("MCP startup failed: {e}"))?;
        service.waiting().await.map_err(|e| format!("MCP server stopped: {e}"))?;
        Ok::<(), String>(())
    });
    if let Err(e) = result {
        eprintln!("[sta-mcp] {e}");
        std::process::exit(1);
    }
}
