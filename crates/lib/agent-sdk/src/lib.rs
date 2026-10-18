//! For agents written in Rust: connect to the task socket, list tools, call
//! them, report progress.
//!
//! This is a convenience, not a requirement. The task socket speaks plain
//! MCP, so any MCP client works the same way (PRD persona "agent builder":
//! no proprietary SDK lock-in).

use std::collections::BTreeSet;
use std::path::PathBuf;

pub use glass_rpc::mcp::CallToolResult;
use glass_rpc::mcp::ListToolsResult;
use glass_rpc::{Client, ClientError};
use serde_json::{Value, json};

/// Environment the runtime gives every agent.
pub mod env {
    pub const SOCKET: &str = "GLASS_AGENT_SOCKET";
    pub const TASK_ID: &str = "GLASS_TASK_ID";
    pub const PROMPT: &str = "GLASS_TASK_PROMPT";
    pub const WORK: &str = "GLASS_WORK";
    /// JSON object: the agent's customization points, with the user's layer.
    pub const SETTINGS: &str = "GLASS_AGENT_SETTINGS";
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("{0} is not set; agents are started by glassd")]
    MissingEnv(&'static str),
    #[error(transparent)]
    Rpc(#[from] ClientError),
}

/// A connected agent session.
#[derive(Debug)]
pub struct Agent {
    client: Client,
    tools: BTreeSet<String>,
    pub task_id: String,
    pub prompt: String,
    pub work_dir: PathBuf,
    /// The server's instructions for this task.
    pub instructions: String,
    /// The settings the catalog entry declares, with the user's layer
    /// applied (TL-2). Read them with [`Agent::setting`].
    pub settings: serde_json::Map<String, Value>,
}

impl Agent {
    /// Connects using the environment glassd provides and completes the MCP
    /// handshake.
    pub async fn from_env() -> Result<Self, AgentError> {
        let var =
            |name: &'static str| std::env::var(name).map_err(|_| AgentError::MissingEnv(name));
        let socket = var(env::SOCKET)?;
        let client = Client::connect(&socket).await?;
        let init: Value = client
            .call(
                "initialize",
                json!({
                    "protocolVersion": glass_rpc::mcp::PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "glass-agent-sdk", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        let listed: ListToolsResult = client.call("tools/list", json!({})).await?;
        Ok(Self {
            client,
            tools: listed.tools.into_iter().map(|t| t.name).collect(),
            task_id: var(env::TASK_ID).unwrap_or_default(),
            prompt: var(env::PROMPT).unwrap_or_default(),
            work_dir: var(env::WORK)
                .map(PathBuf::from)
                .unwrap_or_else(|_| "/work".into()),
            instructions: init["instructions"].as_str().unwrap_or_default().to_owned(),
            settings: std::env::var(env::SETTINGS)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default(),
        })
    }

    /// One setting, if the catalog declares it and it has a value.
    pub fn setting(&self, key: &str) -> Option<&Value> {
        self.settings.get(key).filter(|v| !v.is_null())
    }

    /// Whether policy gave this task the tool. Tools at tier Never are not
    /// listed at all.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains(name)
    }

    /// Calls a tool. Policy refusals arrive as results with `is_error` set,
    /// not as errors: the agent is expected to read them and carry on.
    pub async fn call(&self, tool: &str, arguments: Value) -> Result<CallToolResult, AgentError> {
        Ok(self
            .client
            .call(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await?)
    }

    /// Updates the tile caption and, when countable, progress (AW-4).
    pub async fn progress(
        &self,
        caption: &str,
        done: Option<u32>,
        total: Option<u32>,
    ) -> Result<(), AgentError> {
        let mut args = json!({ "caption": caption });
        if let (Some(done), Some(total)) = (done, total) {
            args["done"] = json!(done);
            args["total"] = json!(total);
        }
        self.call("report_progress", args).await.map(|_| ())
    }
}
