//! The task socket: an MCP server, one per task, bound inside the task's
//! sandbox. It is the agent's only way to affect anything outside it.
//!
//! The socket is owned by the task's user with mode 0600, and every call
//! re-checks the peer's uid, so another user's process can't drive the task
//! even if it found the socket.

use std::sync::Arc;

use glass_rpc::mcp::{
    self, CallToolParams, Implementation, InitializeResult, ListToolsResult, ServerCapabilities,
    Tool, ToolAnnotations, ToolsCapability,
};
use glass_rpc::{Connection, Handler, RpcError, parse_params, to_result};
use glass_types::{SideEffect, TaskId, Tier};
use serde_json::{Value, json};

use crate::task::Task;
use crate::{REPORT_PROGRESS_TOOL, Runtime};

pub(crate) struct AgentApi {
    pub rt: Arc<Runtime>,
    pub task: TaskId,
}

impl Handler for AgentApi {
    async fn call(
        &self,
        conn: Arc<Connection>,
        method: String,
        params: Value,
    ) -> Result<Value, RpcError> {
        let task = self
            .rt
            .task(self.task)
            .ok_or_else(|| RpcError::not_found("this task has ended"))?;
        let peer = conn.peer().uid;
        if peer != task.user.uid && peer != 0 {
            return Err(RpcError::permission_denied(
                "this socket belongs to another user's task",
            ));
        }
        match method.as_str() {
            mcp::methods::INITIALIZE => {
                let requested = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(mcp::PROTOCOL_VERSION);
                to_result(&InitializeResult {
                    protocol_version: mcp::negotiate_version(requested).into(),
                    capabilities: ServerCapabilities {
                        tools: Some(ToolsCapability {
                            list_changed: false,
                        }),
                    },
                    server_info: Implementation {
                        name: "glasshouse".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                    instructions: Some(instructions(&task)),
                })
            }
            mcp::methods::PING => Ok(json!({})),
            mcp::methods::TOOLS_LIST => to_result(&ListToolsResult {
                tools: self.tools(&task),
            }),
            mcp::methods::TOOLS_CALL => {
                let call: CallToolParams = parse_params(params)?;
                to_result(&self.rt.call_tool(&task, &call.name, call.arguments).await?)
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

impl AgentApi {
    /// The tools this task may use. Actions policy puts at Never are left
    /// out entirely; the handoff card's "Can't touch" list tells the user.
    fn tools(&self, task: &Task) -> Vec<Tool> {
        let mut tools = vec![Tool {
            name: REPORT_PROGRESS_TOOL.into(),
            title: Some("Report progress".into()),
            description:
                "Update the one-line status the user sees on your tile. Call it when you start \
                          each step. Include done/total when the work is countable."
                    .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "caption": { "type": "string", "description": "Plain-language current step, under 80 characters" },
                    "done": { "type": "integer", "minimum": 0 },
                    "total": { "type": "integer", "minimum": 1 }
                },
                "required": ["caption"],
                "additionalProperties": false
            }),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            }),
        }];
        for def in self.rt.registry.defs() {
            let tier = task.policy.tier_for(&def.name, def.default_tier).tier;
            let note = match tier {
                Tier::Never => continue,
                Tier::Ask => " The user is asked first; the call waits for their answer.",
                Tier::Notify => " Runs, and the user sees it in the activity feed.",
                Tier::Auto => "",
            };
            tools.push(Tool {
                name: def.mcp_name(),
                title: Some(def.title.clone()),
                description: format!("{}{note}", def.description),
                input_schema: def.input_schema.clone(),
                annotations: Some(ToolAnnotations {
                    read_only_hint: Some(def.side_effect == SideEffect::None),
                    destructive_hint: Some(def.side_effect == SideEffect::Irreversible),
                    open_world_hint: Some(def.side_effect == SideEffect::Irreversible),
                    ..Default::default()
                }),
            });
        }
        tools
    }
}

fn instructions(task: &Task) -> String {
    let folders = if task.ctx.grants.is_empty() {
        "none".to_owned()
    } else {
        task.ctx
            .grants
            .iter()
            .map(|g| {
                format!(
                    "{} ({})",
                    g.name,
                    if g.writable {
                        "read-write"
                    } else {
                        "read-only"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "You are the {agent} agent working for {user} on this task:\n{prompt}\n\n\
         Folders you were given: {folders}. File paths start with a folder name, such as \"{example}/notes.txt\".\n\
         Everything you do is shown to the user live, logged, and undoable where possible. Some actions ask the \
         user first and wait; if the user declines, continue without it. Call {progress} at each step.",
        agent = task.entry.name,
        user = task.user.name,
        prompt = task.prompt,
        example = task
            .ctx
            .grants
            .first()
            .map_or("Folder", |g| g.name.as_str()),
        progress = REPORT_PROGRESS_TOOL,
    )
}
