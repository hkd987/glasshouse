//! The subset of the Model Context Protocol the OS speaks to agents (NV-2).
//!
//! The per-task agent socket is an MCP server: app actions are MCP tools. Any
//! MCP client, including Claude Code and Codex through `glass-mcp`, can drive
//! the machine without an OS-specific SDK (HX-1, HX-6).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol revision this server implements.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Revisions we accept from clients; we answer with the client's when we
/// support it, else with ours.
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub mod methods {
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "notifications/initialized";
    pub const PING: &str = "ping";
    pub const TOOLS_LIST: &str = "tools/list";
    pub const TOOLS_CALL: &str = "tools/call";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: Value,
    pub client_info: Implementation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: Implementation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    pub list_changed: bool,
}

/// Picks the protocol version to answer an `initialize` with.
pub fn negotiate_version(requested: &str) -> &'static str {
    SUPPORTED_VERSIONS
        .iter()
        .find(|v| **v == requested)
        .copied()
        .unwrap_or(PROTOCOL_VERSION)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// Hints clients use to decide how carefully to treat a tool.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    pub tools: Vec<Tool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Content {
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    pub content: Vec<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(default)]
    pub is_error: bool,
}

impl CallToolResult {
    /// A successful result with a text summary and structured data.
    pub fn ok(text: impl Into<String>, structured: Value) -> Self {
        Self {
            content: vec![Content::Text { text: text.into() }],
            structured_content: (!structured.is_null()).then_some(structured),
            is_error: false,
        }
    }

    /// A tool-level failure the model should see and react to, such as a
    /// policy block. Protocol errors use JSON-RPC errors instead.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text { text: text.into() }],
            structured_content: None,
            is_error: true,
        }
    }

    /// The concatenated text content.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .map(|c| match c {
                Content::Text { text } => text.as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn wire_names_are_camel_case() {
        let result = CallToolResult::ok("done", json!({"n": 1}));
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(
            v,
            json!({"content":[{"type":"text","text":"done"}],"structuredContent":{"n":1},"isError":false})
        );
    }

    #[test]
    fn negotiates_supported_versions() {
        assert_eq!(negotiate_version("2025-03-26"), "2025-03-26");
        assert_eq!(negotiate_version("1999-01-01"), PROTOCOL_VERSION);
    }
}
