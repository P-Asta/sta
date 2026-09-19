//! The MCP side: a hand-written `rmcp::ServerHandler` (no macros) that serves the static tool
//! catalog from `sta-core` and forwards every call to the browser. Tool failures are
//! `isError` results (`Error [code]: message. Hint: …`) so the model can recover; only unknown
//! tools are protocol errors.
//!
//! Results never carry `structuredContent`: some clients (Claude Code) give the model only the
//! structured part when it is present, and sta's structured data (ids, counts, flags) leaves
//! out the page content, refs and titles the model needs. The same data goes to programs in the
//! result's `_meta` under [`STRUCTURED_META_KEY`].

use crate::channel::Channel;
use sta_core::agent::channel::Content;
use sta_core::agent::tools::{INSTRUCTIONS, TOOLS};
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, InitializeRequestParams, InitializeResult, ListToolsResult,
    MetaObject, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::Value;
use std::sync::Arc;

/// `tools/list` may be cached for an hour (the list only changes with a new bridge build).
const TOOLS_TTL_MS: u64 = 3_600_000;

/// The `_meta` key of a result's structured data (docs/MCP.md "Results").
pub const STRUCTURED_META_KEY: &str = "sta/structured";

#[derive(Clone)]
pub struct Bridge {
    channel: Arc<Channel>,
}

impl Bridge {
    pub fn new(channel: Arc<Channel>) -> Self {
        Bridge { channel }
    }
}

/// The tool definitions (static, deterministic order).
pub fn tools() -> Vec<Tool> {
    TOOLS
        .iter()
        .map(|t| {
            let schema = match (t.schema)() {
                Value::Object(map) => map,
                _ => Default::default(),
            };
            let annotations = if t.read_only {
                ToolAnnotations::with_title(t.title).read_only(true).open_world(true)
            } else {
                ToolAnnotations::with_title(t.title).read_only(false).destructive(true).open_world(true)
            };
            Tool::new(t.name, t.description, Arc::new(schema)).with_title(t.title).with_annotations(annotations)
        })
        .collect()
}

/// The debug-only test surface (docs/TESTING.md). Compiled only into a `--features test-hooks`
/// debug build, and served **only** to a session whose browser said `welcome{testHooks:true}`:
/// a normal agent session never learns that these tools exist.
#[cfg(all(debug_assertions, feature = "test-hooks"))]
pub fn test_tools() -> Vec<Tool> {
    use sta_core::agent::test_tools;
    test_tools::TOOLS
        .iter()
        .map(|t| {
            let schema = match (t.schema)() {
                Value::Object(map) => map,
                _ => Default::default(),
            };
            let annotations = ToolAnnotations::with_title(t.title).read_only(false).destructive(true).open_world(true);
            let mut tool = Tool::new(t.name, t.description, Arc::new(schema)).with_title(t.title).with_annotations(annotations);
            let mut meta = MetaObject::new();
            meta.insert(test_tools::META_KEY.to_string(), Value::Bool(true));
            tool.meta = Some(meta);
            tool
        })
        .collect()
}

#[cfg(not(all(debug_assertions, feature = "test-hooks")))]
pub fn test_tools() -> Vec<Tool> {
    Vec::new()
}

fn to_block(c: Content) -> ContentBlock {
    match c {
        Content::Text { text } => ContentBlock::text(text),
        Content::Image { mime_type, data } => ContentBlock::image(data, mime_type),
    }
}

/// A successful tool result: the content for the model, the structured data in `_meta` (never
/// `structuredContent`, see the module docs).
pub fn success_result(content: Vec<Content>, structured: Option<Value>) -> CallToolResult {
    let mut r = CallToolResult::success(content.into_iter().map(to_block).collect());
    if let Some(value) = structured {
        let mut meta = MetaObject::new();
        meta.insert(STRUCTURED_META_KEY.to_string(), value);
        r.meta = Some(meta);
    }
    r
}

impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        let mut info = Implementation::new("sta", env!("CARGO_PKG_VERSION"));
        info.title = Some("sta browser".into());
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_tool_list_changed().build())
            .with_server_info(info)
            .with_instructions(INSTRUCTIONS)
    }

    async fn initialize(&self, request: InitializeRequestParams, context: RequestContext<RoleServer>) -> Result<InitializeResult, ErrorData> {
        let c = &request.client_info;
        self.channel.set_client(&c.name, c.title.as_deref(), Some(&c.version));
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData> {
        // An armed browser's extra tools are never cacheable: the same bridge answers with the
        // shipped 23 until it has seen `welcome{testHooks:true}`.
        if self.channel.test_hooks() {
            let mut all = tools();
            all.extend(test_tools());
            return Ok(ListToolsResult::with_all_items(all).with_ttl_ms(0).with_cache_scope(CacheScope::Private));
        }
        Ok(ListToolsResult::with_all_items(tools()).with_ttl_ms(TOOLS_TTL_MS).with_cache_scope(CacheScope::Private))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().chain(test_tools()).find(|t| t.name == name)
    }

    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        if let Some(c) = context.client_info() {
            self.channel.set_client(&c.name, c.title.as_deref(), Some(&c.version));
        }
        let name = request.name.to_string();
        // A `test_*` name is forwarded on a test-hooks build (the browser answers `unknown_tool`
        // when it is not armed, so the bridge leaks nothing either way).
        let known = TOOLS.iter().any(|t| t.name == name) || test_tools().iter().any(|t| t.name == name);
        if !known {
            return Err(ErrorData::invalid_params(format!("unknown tool: {name}"), None));
        }
        let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let ct = context.ct.clone();
        let result = match self.channel.call(&name, args, async move { ct.cancelled().await }).await {
            Ok((content, structured)) => success_result(content, structured),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_text())]),
        };
        // The first call is what connects: if that welcome armed the surface, the list grew.
        if self.channel.take_test_hooks_change() {
            let _ = context.peer.notify_tool_list_changed().await;
        }
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_tool_list() {
        let list = tools();
        assert_eq!(list.len(), TOOLS.len());
        let json = serde_json::to_value(&list).unwrap();
        assert_eq!(json[0]["name"], "tabs_list");
        assert_eq!(json[0]["annotations"]["readOnlyHint"], true);
        let click = json.as_array().unwrap().iter().find(|t| t["name"] == "click").unwrap();
        assert_eq!(click["annotations"]["destructiveHint"], true);
        assert_eq!(click["inputSchema"]["additionalProperties"], false);
        let result = ListToolsResult::with_all_items(list).with_ttl_ms(TOOLS_TTL_MS).with_cache_scope(CacheScope::Private);
        let v = serde_json::to_value(result).unwrap();
        assert_eq!(v["cacheScope"], "private");
        assert_eq!(v["ttlMs"], TOOLS_TTL_MS);
        assert!(json.as_array().unwrap().iter().all(|t| t.get("outputSchema").is_none()), "no tool declares an output schema");
    }

    #[test]
    fn results_keep_structured_data_out_of_structured_content() {
        let r = success_result(vec![Content::Text { text: "- button \"Go\" [ref=2.1.1]".into() }], Some(serde_json::json!({ "tab": 2, "refs": 1 })));
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("structuredContent").is_none(), "{v}");
        assert_eq!(v["content"][0]["text"], "- button \"Go\" [ref=2.1.1]");
        assert_eq!(v["_meta"][STRUCTURED_META_KEY]["refs"], 1);
        let plain = serde_json::to_value(success_result(vec![Content::Text { text: "x".into() }], None)).unwrap();
        assert!(plain.get("_meta").is_none() && plain.get("structuredContent").is_none(), "{plain}");
    }
}
