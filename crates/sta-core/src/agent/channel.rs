//! The control channel between the MCP bridge (`sta-mcp.exe`) and the browser (docs/MCP.md
//! "Channel").
//!
//! - Transport: a named pipe `\\.\pipe\sta-agent-<random128>` on Windows, a Unix domain socket
//!   (`<data>/sta/agent.sock`, mode 0600) on macOS and Linux. Either way the browser writes what to
//!   open to `<data>/sta/agent-endpoint.json` ([`Endpoint`]) while agent access is not Off.
//! - Framing: UTF-8 NDJSON, one JSON object per line, at most [`MAX_LINE_BYTES`] per line.
//!   The limit belongs to **one message, not to the session**: a sender checks its own line with
//!   [`to_line_checked`] and turns an oversized one into a `too_large` error for that call
//!   ([`too_large`]), and a receiver's [`LineReader`] *skips* a line that breaks the rule anyway
//!   (counting it in [`Batch::skipped`]) instead of tearing the pipe down. Before that, a single
//!   ~20 MB `test_cdp` result answered "line too long" in the reader and killed the whole MCP
//!   session — every other in-flight call with it.
//! - Both sides reject unknown fields and unknown message types (`deny_unknown_fields`); `v` must
//!   equal [`PROTOCOL_VERSION`] exactly.
//!
//! ```text
//! bridge → browser  {"t":"hello","v":1,"build":"0.1.0","bridge":{"version":"0.1.0","pid":4410},"client":{"name":"claude-code","title":"Claude Code","version":"2.1.268"}}
//! browser → bridge  {"t":"pending","reason":"approval"}
//! browser → bridge  {"t":"welcome","v":1,"session":7,"access":"full"}
//! browser → bridge  {"t":"refused","code":"access_off","message":"…"}
//! bridge → browser  {"t":"call","id":17,"tool":"click","args":{"ref":"42.3.31"},"deadlineMs":30000}
//! bridge → browser  {"t":"cancel","id":17}
//! browser → bridge  {"t":"result","id":17,"content":[{"type":"text","text":"…"}]}
//! browser → bridge  {"t":"result","id":17,"content":[],"error":{"code":"stale_ref","message":"…","hint":"…"}}
//! browser → bridge  {"t":"bye","reason":"user_stopped"}
//! ```
//!
//! The golden tests at the bottom lock this format.

use super::errors::ErrorCode;
use crate::model::AgentAccess;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Channel protocol version; `hello.v` and `welcome.v` must match it exactly.
pub const PROTOCOL_VERSION: u32 = 1;
/// Longest NDJSON line either side accepts (screenshots are base64 in `result`). A message that
/// would be longer fails **its own call** and nothing else; see the module docs.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// File name of the endpoint file inside the profile directory (`<data>/sta/`).
pub const ENDPOINT_FILE: &str = "agent-endpoint.json";
/// Prefix of the pipe name (Windows).
pub const PIPE_PREFIX: &str = r"\\.\pipe\sta-agent-";
/// File name of the agent socket inside the profile directory (Unix).
pub const SOCKET_FILE: &str = "agent.sock";
/// The browser closes a connection that sends no `hello` within this time.
pub const HELLO_TIMEOUT_MS: u64 = 5_000;
/// Default and maximum call deadlines.
pub const DEFAULT_DEADLINE_MS: u64 = 30_000;
pub const MAX_DEADLINE_MS: u64 = 120_000;
/// A call waits at most this long for the user's approval, then fails with `not_approved`.
pub const APPROVAL_HOLD_MS: u64 = 20_000;

/// `<data>/sta/agent-endpoint.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Endpoint {
    /// What the bridge opens: the full pipe path (`\\.\pipe\sta-agent-…`) on Windows, the
    /// absolute socket path on Unix.
    pub pipe: String,
    /// Browser process id.
    pub pid: u32,
    pub protocol: u32,
    /// Browser build (`CARGO_PKG_VERSION`).
    pub build: String,
}

impl Endpoint {
    /// A channel name the browser could have created (the bridge never opens anything else): the
    /// random pipe name on Windows, an absolute `…/*.sock` path with no traversal on Unix (where
    /// the socket lives in the user's own data directory and its mode, not its name, is what keeps
    /// other users out).
    pub fn pipe_is_valid(&self) -> bool {
        #[cfg(windows)]
        {
            self.pipe
                .strip_prefix(PIPE_PREFIX)
                .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
        }
        #[cfg(not(windows))]
        {
            let path = std::path::Path::new(&self.pipe);
            path.is_absolute()
                && self.pipe.ends_with(".sock")
                && path.components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::RootDir))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BridgeInfo {
    pub version: String,
    pub pid: u32,
}

/// What the MCP client says about itself (`clientInfo`; self-reported, display only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Bridge → browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum BridgeMessage {
    Hello { v: u32, build: String, bridge: BridgeInfo, client: ClientInfo },
    Call {
        id: u64,
        tool: String,
        #[serde(default)]
        args: Value,
        #[serde(default = "default_deadline")]
        deadline_ms: u64,
    },
    Cancel { id: u64 },
}

fn default_deadline() -> u64 {
    DEFAULT_DEADLINE_MS
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PendingReason {
    /// The user has to approve the client in sta.
    Approval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusedCode {
    AccessOff,
    ApprovalDenied,
    VersionMismatch,
    TooManySessions,
    /// The user pressed Stop; agents stay paused until Resume.
    Paused,
    /// Another approval prompt is already waiting, or the user denied recently (60 s back-off).
    ApprovalBusy,
    ProtocolError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    /// The user pressed Stop. The bridge never reconnects after this.
    UserStopped,
    /// Agent access was turned off. The bridge never reconnects after this.
    AccessOff,
    Shutdown,
    ProtocolError,
}

impl ByeReason {
    /// The bridge must not reconnect after this reason.
    pub fn is_final(self) -> bool {
        matches!(self, ByeReason::UserStopped | ByeReason::AccessOff)
    }
}

/// One MCP content item of a tool result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum Content {
    Text { text: String },
    Image { mime_type: String, data: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ToolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), hint: code.default_hint().map(str::to_string) }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The text the model sees: `Error [code]: message. Hint: …`.
    pub fn to_text(&self) -> String {
        let mut text = format!("Error [{}]: {}", self.code.as_str(), self.message.trim_end_matches('.'));
        text.push('.');
        if let Some(h) = &self.hint {
            text.push_str(" Hint: ");
            text.push_str(h);
        }
        text
    }
}

/// Browser → bridge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum BrowserMessage {
    Pending { reason: PendingReason },
    Welcome {
        v: u32,
        session: u64,
        access: AgentAccess,
        /// The browser runs with the debug-only test surface armed (docs/TESTING.md): the bridge
        /// may then serve the `test_*` tools. Left out (and `false`) in every other build, so the
        /// wire format of a normal session is unchanged.
        #[serde(default, skip_serializing_if = "is_false")]
        test_hooks: bool,
    },
    Refused { code: RefusedCode, message: String },
    Progress { id: u64, message: String },
    Result {
        id: u64,
        content: Vec<Content>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structured: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ToolError>,
    },
    Bye { reason: ByeReason },
}

/// Serializes one message as an NDJSON line (with the trailing `\n`).
pub fn to_line<T: Serialize>(msg: &T) -> Vec<u8> {
    let mut v = serde_json::to_vec(msg).unwrap_or_else(|_| b"{}".to_vec());
    v.push(b'\n');
    v
}

/// [`to_line`], but `Err(len)` — the whole line's length — when the message does not fit in
/// [`MAX_LINE_BYTES`]. A sender must never put such a line on the pipe: it answers that one call
/// with [`too_large`] instead and leaves the session alone.
pub fn to_line_checked<T: Serialize>(msg: &T) -> Result<Vec<u8>, usize> {
    let line = to_line(msg);
    // The `\n` is framing, not payload: a line of exactly MAX_LINE_BYTES is allowed.
    if line.len() - 1 > MAX_LINE_BYTES { Err(line.len()) } else { Ok(line) }
}

/// The error a call gets when its own answer does not fit in one line. `bytes` is what the line
/// would have been, so the message can say how far over the limit it was.
pub fn too_large(bytes: usize) -> ToolError {
    let mib = |n: usize| (n as f64) / (1024.0 * 1024.0);
    ToolError::new(
        ErrorCode::TooLarge,
        format!("The answer is {:.1} MiB; one channel message may be at most {:.0} MiB", mib(bytes), mib(MAX_LINE_BYTES)),
    )
    .with_hint("Ask for less, or write the data to a file instead of returning it.")
}

/// Splits complete lines out of a receive buffer.
///
/// A line longer than [`MAX_LINE_BYTES`] is **dropped**, not an error: the session survives a peer
/// that breaks the framing rule, and the call that line belonged to fails on its own (by deadline,
/// or with the sender's own [`too_large`] answer). Dropping also keeps the buffer bounded — it
/// never holds more than one over-long line's worth before discarding it.
#[derive(Debug, Default)]
pub struct LineReader {
    buf: Vec<u8>,
    /// Mid-way through discarding an over-long line: everything until the next `\n` goes away.
    skipping: bool,
}

/// What one [`LineReader::push`] produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Batch {
    /// Complete lines, `\n` removed and a trailing `\r` trimmed; blank lines dropped.
    pub lines: Vec<Vec<u8>>,
    /// Lines discarded for exceeding [`MAX_LINE_BYTES`] (the peer is misbehaving; log it).
    pub skipped: usize,
}

impl LineReader {
    pub fn push(&mut self, data: &[u8]) -> Batch {
        let mut batch = Batch::default();
        self.buf.extend_from_slice(data);
        loop {
            if self.skipping {
                let Some(pos) = self.buf.iter().position(|b| *b == b'\n') else {
                    // Still inside the over-long line: keep nothing.
                    self.buf.clear();
                    return batch;
                };
                self.buf.drain(..=pos);
                self.skipping = false;
                batch.skipped += 1;
                continue;
            }
            let Some(pos) = self.buf.iter().position(|b| *b == b'\n') else { break };
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.len() > MAX_LINE_BYTES {
                batch.skipped += 1;
            } else if !line.iter().all(u8::is_ascii_whitespace) {
                batch.lines.push(line);
            }
        }
        if self.buf.len() > MAX_LINE_BYTES {
            // The line in flight is already too long: discard it as it arrives.
            self.buf.clear();
            self.skipping = true;
        }
        batch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn golden<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(value: T, text: &str) {
        assert_eq!(serde_json::to_string(&value).unwrap(), text);
        assert_eq!(serde_json::from_str::<T>(text).unwrap(), value);
    }

    #[test]
    fn bridge_messages_golden() {
        golden(
            BridgeMessage::Hello {
                v: 1,
                build: "0.1.0".into(),
                bridge: BridgeInfo { version: "0.1.0".into(), pid: 4410 },
                client: ClientInfo { name: "claude-code".into(), title: Some("Claude Code".into()), version: None },
            },
            r#"{"t":"hello","v":1,"build":"0.1.0","bridge":{"version":"0.1.0","pid":4410},"client":{"name":"claude-code","title":"Claude Code"}}"#,
        );
        golden(
            BridgeMessage::Call { id: 17, tool: "click".into(), args: json!({"ref":"42.3.31"}), deadline_ms: 30000 },
            r#"{"t":"call","id":17,"tool":"click","args":{"ref":"42.3.31"},"deadlineMs":30000}"#,
        );
        golden(BridgeMessage::Cancel { id: 17 }, r#"{"t":"cancel","id":17}"#);
        // Defaults and strictness.
        assert_eq!(
            serde_json::from_str::<BridgeMessage>(r#"{"t":"call","id":1,"tool":"tabs_list"}"#).unwrap(),
            BridgeMessage::Call { id: 1, tool: "tabs_list".into(), args: Value::Null, deadline_ms: DEFAULT_DEADLINE_MS }
        );
        assert!(serde_json::from_str::<BridgeMessage>(r#"{"t":"cancel","id":1,"extra":true}"#).is_err());
        assert!(serde_json::from_str::<BridgeMessage>(r#"{"t":"exec","id":1}"#).is_err());
        assert!(serde_json::from_str::<BridgeMessage>(r#"{"t":"hello","v":1,"build":"x","bridge":{"version":"x","pid":1,"exe":"c"},"client":{"name":"a"}}"#).is_err());
    }

    #[test]
    fn browser_messages_golden() {
        golden(BrowserMessage::Pending { reason: PendingReason::Approval }, r#"{"t":"pending","reason":"approval"}"#);
        golden(BrowserMessage::Welcome { v: 1, session: 7, access: AgentAccess::Full, test_hooks: false }, r#"{"t":"welcome","v":1,"session":7,"access":"full"}"#);
        golden(
            BrowserMessage::Welcome { v: 1, session: 7, access: AgentAccess::Full, test_hooks: true },
            r#"{"t":"welcome","v":1,"session":7,"access":"full","testHooks":true}"#,
        );
        golden(
            BrowserMessage::Refused { code: RefusedCode::AccessOff, message: "Agent access is off".into() },
            r#"{"t":"refused","code":"access_off","message":"Agent access is off"}"#,
        );
        golden(BrowserMessage::Progress { id: 3, message: "loading".into() }, r#"{"t":"progress","id":3,"message":"loading"}"#);
        golden(
            BrowserMessage::Result {
                id: 17,
                content: vec![Content::Text { text: "ok".into() }, Content::Image { mime_type: "image/jpeg".into(), data: "AAAA".into() }],
                structured: Some(json!({"tab": 4})),
                error: None,
            },
            r#"{"t":"result","id":17,"content":[{"type":"text","text":"ok"},{"type":"image","mimeType":"image/jpeg","data":"AAAA"}],"structured":{"tab":4}}"#,
        );
        golden(
            BrowserMessage::Result {
                id: 18,
                content: vec![],
                structured: None,
                error: Some(ToolError { code: ErrorCode::StaleRef, message: "ref 4.1.2 is stale".into(), hint: Some("Call page_snapshot again.".into()) }),
            },
            r#"{"t":"result","id":18,"content":[],"error":{"code":"stale_ref","message":"ref 4.1.2 is stale","hint":"Call page_snapshot again."}}"#,
        );
        golden(BrowserMessage::Bye { reason: ByeReason::UserStopped }, r#"{"t":"bye","reason":"user_stopped"}"#);
        assert!(ByeReason::UserStopped.is_final() && ByeReason::AccessOff.is_final() && !ByeReason::Shutdown.is_final());
        assert!(serde_json::from_str::<BrowserMessage>(r#"{"t":"welcome","v":1,"session":7,"access":"full","scripts":true}"#).is_err());
    }

    #[test]
    fn endpoint_golden_and_pipe_names() {
        let e = Endpoint { pipe: format!("{PIPE_PREFIX}{}", "0123456789abcdef0123456789abcdef"), pid: 99, protocol: 1, build: "0.1.0".into() };
        golden(e.clone(), r#"{"pipe":"\\\\.\\pipe\\sta-agent-0123456789abcdef0123456789abcdef","pid":99,"protocol":1,"build":"0.1.0"}"#);
        // The name rule is the one of the platform the bridge runs on: it decides what it opens.
        #[cfg(windows)]
        {
            assert!(e.pipe_is_valid());
            for bad in [r"\\.\pipe\sta-agent-0123", r"\\.\pipe\other-0123456789abcdef0123456789abcdef", r"\\evil\pipe\sta-agent-0123456789abcdef0123456789abcdef"] {
                assert!(!Endpoint { pipe: bad.into(), ..e.clone() }.pipe_is_valid(), "{bad}");
            }
            assert!(!Endpoint { pipe: format!("{PIPE_PREFIX}{}", "0123456789ABCDEF0123456789abcdef"), ..e }.pipe_is_valid());
        }
        #[cfg(not(windows))]
        {
            assert!(!e.pipe_is_valid(), "a Windows pipe name is not a socket path");
            let ok = Endpoint { pipe: format!("/Users/me/Library/Application Support/sta/sta/{SOCKET_FILE}"), ..e.clone() };
            assert!(ok.pipe_is_valid());
            for bad in ["relative/agent.sock", "/tmp/../etc/agent.sock", "/tmp/agent.socket", "/tmp/agent"] {
                assert!(!Endpoint { pipe: bad.into(), ..e.clone() }.pipe_is_valid(), "{bad}");
            }
        }
    }

    #[test]
    fn tool_error_text() {
        let e = ToolError::new(ErrorCode::TabNotVisible, "Tab 4 is in the background.");
        assert_eq!(e.to_text(), format!("Error [tab_not_visible]: Tab 4 is in the background. Hint: {}", ErrorCode::TabNotVisible.default_hint().unwrap()));
    }

    #[test]
    fn line_reader_splits_and_limits() {
        let mut r = LineReader::default();
        assert_eq!(r.push(b"{\"a\":1}\n{\"b\"").lines, vec![b"{\"a\":1}".to_vec()]);
        assert_eq!(r.push(b":2}\r\n\n").lines, vec![b"{\"b\":2}".to_vec()]);
        assert_eq!(to_line(&BridgeMessage::Cancel { id: 1 }), b"{\"t\":\"cancel\",\"id\":1}\n".to_vec());
    }

    /// An over-long line costs itself and nothing else: the reader drops it, keeps its buffer
    /// bounded and goes on parsing the lines around it (before this, it answered "line too long"
    /// and the caller closed the pipe, killing the session and every other call in flight).
    #[test]
    fn line_reader_skips_an_oversized_line_and_keeps_going() {
        let mut r = LineReader::default();
        let huge = vec![b'x'; MAX_LINE_BYTES + 1];
        // Arrives in pieces, as a pipe delivers it: the reader never holds more than the limit.
        let first = r.push(b"{\"a\":1}\n");
        assert_eq!((first.lines.len(), first.skipped), (1, 0));
        for chunk in huge.chunks(1 << 20) {
            let b = r.push(chunk);
            assert_eq!((b.lines.len(), b.skipped), (0, 0));
        }
        assert!(r.buf.len() <= MAX_LINE_BYTES, "the discarded line is not buffered");
        // The rest of that line, its newline, and a good line behind it.
        let b = r.push(b"yyy\n{\"b\":2}\n");
        assert_eq!((b.skipped, b.lines), (1, vec![b"{\"b\":2}".to_vec()]));
        // A line of exactly the limit is still accepted, in one piece.
        let exact = vec![b'z'; MAX_LINE_BYTES];
        let mut line = exact.clone();
        line.push(b'\n');
        let b = r.push(&line);
        assert_eq!((b.skipped, b.lines), (0, vec![exact]));
    }

    /// The sender's half of the same rule.
    #[test]
    fn oversized_messages_fail_their_own_call() {
        let small = BrowserMessage::Result { id: 1, content: vec![Content::Text { text: "ok".into() }], structured: None, error: None };
        assert_eq!(to_line_checked(&small).unwrap(), to_line(&small));
        let big = BrowserMessage::Result {
            id: 2,
            content: vec![Content::Image { mime_type: "image/png".into(), data: "A".repeat(MAX_LINE_BYTES + 1) }],
            structured: None,
            error: None,
        };
        let len = to_line_checked(&big).expect_err("too long");
        assert!(len > MAX_LINE_BYTES);
        let e = too_large(len);
        assert_eq!(e.code, ErrorCode::TooLarge);
        assert!(e.message.contains("8 MiB") && e.message.contains("MiB;"), "{}", e.message);
        // …and the replacement answer itself always fits.
        let replacement = BrowserMessage::Result { id: 2, content: vec![Content::Text { text: e.to_text() }], structured: None, error: Some(e) };
        assert!(to_line_checked(&replacement).is_ok());
    }
}
