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
    /// The computer's offset from UTC when the task started, in seconds
    /// east: its local time, which the calendar's times are in (ADR 0031).
    pub const UTC_OFFSET: &str = "GLASS_UTC_OFFSET";
    /// The id of the routine this task runs, empty for any other task
    /// (DA-4): `routines.read` gives its script.
    pub const ROUTINE: &str = "GLASS_ROUTINE";
    /// The agent's own computer (ADR 0066), kept between its tasks, where it
    /// is root and may run what it likes; empty in a sandbox that goes with
    /// the task.
    pub const COMPUTER: &str = "GLASS_COMPUTER";
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
    /// The computer's offset from UTC, in seconds east, when the task
    /// started; 0 when glassd didn't say.
    pub utc_offset_secs: i32,
    /// The routine this task runs, by id (DA-4).
    pub routine: Option<String>,
    /// Its own computer, by id, when it has one (ADR 0066).
    pub computer: Option<String>,
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
            utc_offset_secs: var(env::UTC_OFFSET)
                .ok()
                .and_then(|text| text.parse().ok())
                .unwrap_or(0),
            routine: var(env::ROUTINE).ok().filter(|id| !id.is_empty()),
            computer: var(env::COMPUTER).ok().filter(|id| !id.is_empty()),
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

    /// The tools this task may use, as its socket lists them: names,
    /// descriptions and input schemas, for a model to choose from (ADR
    /// 0041).
    pub async fn tool_list(&self) -> Result<Vec<glass_rpc::mcp::Tool>, AgentError> {
        let listed: ListToolsResult = self.client.call("tools/list", json!({})).await?;
        Ok(listed.tools)
    }

    /// The last word: the tile's caption ([`caption`] makes one from the
    /// answer), and what the agent tells the person in full, which they see
    /// with the task (ADR 0041).
    pub async fn answer(&self, caption: &str, answer: &str) -> Result<(), AgentError> {
        self.call(
            "report_progress",
            json!({ "caption": caption, "answer": answer }),
        )
        .await
        .map(|_| ())
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

/// A tile caption from an answer: its first line, 100 characters at most,
/// without the Markdown marks models put on it (`## `, `- `, `**`,
/// backticks), which a caption would show as they are.
pub fn caption(answer: &str) -> String {
    let line = answer
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.chars().all(|c| c == '-' || c == '*' || c == '#'))
        .unwrap_or("Done");
    // A heading's marks, never a "#3" that starts a sentence.
    let line = line
        .trim_start_matches('#')
        .strip_prefix(' ')
        .filter(|_| line.starts_with('#'))
        .unwrap_or(line);
    let line = ["- ", "* ", "+ "]
        .iter()
        .find_map(|m| line.strip_prefix(m))
        .unwrap_or(line);
    let line = line.replace("**", "").replace("__", "").replace('`', "");
    let line = line.trim();
    let mut caption: String = line.chars().take(100).collect();
    if line.chars().count() > 100 {
        caption.push('…');
    }
    caption
}

#[cfg(test)]
mod caption_tests {
    use super::caption;

    #[test]
    fn ae3_the_caption_is_the_answers_first_line_short_and_plain() {
        assert_eq!(
            caption("\n\nYou have 3 invoices.\nMore detail"),
            "You have 3 invoices."
        );
        assert_eq!(caption(&"x".repeat(150)).chars().count(), 101);
        assert_eq!(caption(""), "Done");
        // Markdown's marks, which a caption would show as they are.
        assert_eq!(
            caption("**Checklist of LaunchNotes and Invoices**\n\n1. plan.txt"),
            "Checklist of LaunchNotes and Invoices"
        );
        assert_eq!(caption("---\n## What I found\n"), "What I found");
        assert_eq!(caption("- `plan.txt` is new"), "plan.txt is new");
        assert_eq!(caption("#3 of 5 is overdue"), "#3 of 5 is overdue");
    }
}
