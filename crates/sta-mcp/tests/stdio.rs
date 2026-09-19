//! The bridge over real stdio: both MCP protocol eras (`initialize` and the stateless
//! `server/discover` with per-request `_meta`), an offline `tools/list`, and a tool call without a
//! browser (an `isError` result, not a protocol error).

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

struct Bridge {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl Bridge {
    fn start(tag: &str) -> (Bridge, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sta-mcp-stdio-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        use std::os::windows::process::CommandExt;
        let mut child = Command::new(env!("CARGO_BIN_EXE_sta-mcp"))
            .args(["--data-dir", dir.to_str().unwrap(), "--no-launch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW: `cargo test` must not flash a console
            .spawn()
            .expect("spawn sta-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout: ChildStdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        (Bridge { child, stdin: Some(stdin), lines: rx }, dir)
    }

    fn send(&mut self, msg: Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn response(&mut self, id: u64) -> Value {
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(20)).expect("response line");
            let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout must be JSON-RPC only: {line} ({e})"));
            if v["id"] == json!(id) {
                return v;
            }
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "stdio-test", "version": "1.0" }
    })
}

#[test]
fn initialize_era_lists_tools_and_reports_errors_as_results() {
    let (mut b, dir) = Bridge::start("init");
    b.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"stdio-test","version":"1.0"}}}));
    let init = b.response(1);
    assert_eq!(init["result"]["serverInfo"]["name"], "sta", "{init}");
    assert!(init["result"]["capabilities"]["tools"].is_object(), "{init}");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("untrusted"));
    b.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    b.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let list = b.response(2);
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), sta_core::agent::tools::TOOLS.len(), "{names:?}");
    assert_eq!(names.len(), 23, "{names:?}");
    assert_eq!(names[0], "tabs_list");
    assert!(names.contains(&"fill_form") && names.contains(&"downloads_list"), "{names:?}");
    b.send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tabs_list","arguments":{}}}));
    let call = b.response(3);
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().starts_with("Error [browser_not_running]"), "{call}");
    b.send(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"format_disk","arguments":{}}}));
    assert!(b.response(4)["error"].is_object(), "unknown tools are protocol errors");
    drop(b);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn discover_era_works_without_initialize() {
    let (mut b, dir) = Bridge::start("discover");
    b.send(json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta": meta()}}));
    let d = b.response(1);
    assert!(d["result"]["supportedVersions"].as_array().unwrap().iter().any(|v| v == "2026-07-28"), "{d}");
    b.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta": meta()}}));
    let list = b.response(2);
    assert_eq!(list["result"]["tools"].as_array().map(Vec::len), Some(23), "{list}");
    assert_eq!(list["result"]["cacheScope"], "private", "{list}");
    drop(b);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn exits_on_stdin_eof() {
    let (mut b, dir) = Bridge::start("eof");
    drop(b.stdin.take());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if b.child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the bridge must exit when stdin closes");
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(b);
    let _ = std::fs::remove_dir_all(dir);
}
