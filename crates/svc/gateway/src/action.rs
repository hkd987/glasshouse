//! `model.complete` as agents see it: the action's declaration and its
//! input. The runtime routes the call itself (ADR 0018); this provider only
//! declares the action and checks its input.

use glass_actions::{ActionDef, ActionError, ActionOutput, ActionProvider, Preview, TaskContext};
use glass_types::{ActionName, SideEffect, Tier};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The action's name. Its MCP tool is `model_complete`.
pub const MODEL_COMPLETE: &str = "model.complete";

/// The longest prompt one call may send, in characters (system prompt and
/// messages together).
pub const MAX_PROMPT_CHARS: usize = 400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// The most tools one call may offer (ADR 0041).
pub const MAX_TOOLS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    /// The words; may be empty when the message carries tool calls or
    /// results.
    #[serde(default)]
    pub content: String,
    /// An assistant's: the tools the model asked for (ADR 0041).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// A user's: what the tools the model asked for returned, each by the
    /// call's id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
}

impl Message {
    /// A message that is only words.
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }
}

/// A tool the model may ask for (ADR 0041): the agent's own, as its task
/// socket lists it. The model only proposes; the agent makes the call,
/// where every rule applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema for its input.
    pub input_schema: Value,
}

/// A tool the model asked for, with its input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// The provider's id for the call, which its result names.
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// What a tool the model asked for returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResult {
    /// The call it answers.
    pub id: String,
    pub content: String,
    /// The call was refused or failed: the model should know.
    #[serde(default)]
    pub is_error: bool,
}

/// What an agent sends: Anthropic Messages shaped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteInput {
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    #[serde(default)]
    pub system: Option<String>,
    /// A model id from policy's list. Policy routes the call when it's left
    /// out, which is what most agents should do (MP-7).
    #[serde(default)]
    pub model: Option<String>,
    /// The tools the model may ask for (ADR 0041).
    #[serde(default)]
    pub tools: Vec<Tool>,
}

/// Whether `name` is a tool name every provider takes.
fn tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl CompleteInput {
    /// Parses and checks an agent's input.
    pub fn parse(input: &Value) -> Result<Self, ActionError> {
        let input: Self = glass_actions::parse_input(input)?;
        let first_is_user = input.messages.first().map(|m| m.role) == Some(Role::User);
        if !first_is_user {
            return Err(ActionError::InvalidInput(
                "messages must start with a user message".into(),
            ));
        }
        if input.messages.iter().any(|m| {
            m.content.trim().is_empty() && m.tool_calls.is_empty() && m.tool_results.is_empty()
        }) {
            return Err(ActionError::InvalidInput(
                "every message needs some text, tool calls or tool results".into(),
            ));
        }
        if input.tools.len() > MAX_TOOLS {
            return Err(ActionError::InvalidInput(format!(
                "a call offers at most {MAX_TOOLS} tools"
            )));
        }
        if let Some(bad) = input.tools.iter().find(|t| !tool_name(&t.name)) {
            return Err(ActionError::InvalidInput(format!(
                "{:?} isn't a tool name: letters, digits, _ and -, at most 64",
                bad.name
            )));
        }
        // Calls only from the model, results only from the agent, each
        // result answering a call in the message before it.
        let mut asked: Vec<&str> = Vec::new();
        for m in &input.messages {
            match m.role {
                Role::User if !m.tool_calls.is_empty() => {
                    return Err(ActionError::InvalidInput(
                        "only the model's (assistant) messages carry tool calls".into(),
                    ));
                }
                Role::Assistant if !m.tool_results.is_empty() => {
                    return Err(ActionError::InvalidInput(
                        "tool results go in a user message".into(),
                    ));
                }
                Role::User => {
                    if let Some(r) = m
                        .tool_results
                        .iter()
                        .find(|r| !asked.contains(&r.id.as_str()))
                    {
                        return Err(ActionError::InvalidInput(format!(
                            "tool result {:?} answers no call in the message before it",
                            r.id
                        )));
                    }
                    asked.clear();
                }
                Role::Assistant => {
                    asked = m.tool_calls.iter().map(|c| c.id.as_str()).collect();
                }
            }
        }
        if input
            .messages
            .iter()
            .flat_map(|m| &m.tool_calls)
            .any(|c| !tool_name(&c.name))
        {
            return Err(ActionError::InvalidInput(
                "a tool call names no tool".into(),
            ));
        }
        if input.max_tokens == 0 {
            return Err(ActionError::InvalidInput(
                "max_tokens must be at least 1".into(),
            ));
        }
        if input.chars() > MAX_PROMPT_CHARS {
            return Err(ActionError::InvalidInput(format!(
                "the prompt is {} characters; the most one call may send is {MAX_PROMPT_CHARS}",
                input.chars()
            )));
        }
        Ok(input)
    }

    /// Characters in the system prompt, the messages, their tool calls
    /// and results, and the tools offered.
    pub fn chars(&self) -> usize {
        let json = |v: &Value| v.to_string().chars().count();
        self.system.as_deref().map_or(0, |s| s.chars().count())
            + self
                .messages
                .iter()
                .map(|m| {
                    m.content.chars().count()
                        + m.tool_calls
                            .iter()
                            .map(|c| c.name.len() + json(&c.input))
                            .sum::<usize>()
                        + m.tool_results
                            .iter()
                            .map(|r| r.content.chars().count())
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self
                .tools
                .iter()
                .map(|t| t.name.len() + t.description.chars().count() + json(&t.input_schema))
                .sum::<usize>()
    }

    /// "2 messages, 1,204 characters", and the tools offered.
    pub fn describe(&self) -> String {
        let n = self.messages.len();
        let tools = match self.tools.len() {
            0 => String::new(),
            1 => ", 1 tool".into(),
            t => format!(", {t} tools"),
        };
        format!(
            "{n} message{}, {} characters{tools}",
            if n == 1 { "" } else { "s" },
            thousands(self.chars())
        )
    }
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The declaration every runtime registers (through `glass-first-party`).
pub fn definition() -> ActionDef {
    ActionDef {
        name: MODEL_COMPLETE.parse().expect("static name"),
        title: "Use an AI model".into(),
        description: "Get a reply from an AI model. The OS chooses a model IT approved for the \
                      data you have read, replaces secrets and personal details with placeholders \
                      before anything leaves this computer, and logs the call. Leave model out \
                      unless you were told to use a particular one."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The conversation so far, starting with a user message.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": { "enum": ["user", "assistant"] },
                            "content": { "type": "string" },
                            "tool_calls": {
                                "type": "array",
                                "description": "An assistant message's: the tool calls the model asked for, as its reply gave them.",
                                "items": {
                                    "type": "object",
                                    "properties": { "id": { "type": "string" }, "name": { "type": "string" }, "input": { "type": "object" } },
                                    "required": ["id", "name", "input"],
                                    "additionalProperties": false
                                }
                            },
                            "tool_results": {
                                "type": "array",
                                "description": "A user message's: what each call returned, by its id. Make the calls yourself first: the model only proposes them.",
                                "items": {
                                    "type": "object",
                                    "properties": { "id": { "type": "string" }, "content": { "type": "string" }, "is_error": { "type": "boolean" } },
                                    "required": ["id", "content"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["role"],
                        "additionalProperties": false
                    }
                },
                "tools": {
                    "type": "array",
                    "maxItems": MAX_TOOLS,
                    "description": "Tools the model may ask for, such as your own from tools/list. The reply's tool_calls say which; you make each call, then send the results back.",
                    "items": {
                        "type": "object",
                        "properties": { "name": { "type": "string" }, "description": { "type": "string" }, "input_schema": { "type": "object" } },
                        "required": ["name", "input_schema"],
                        "additionalProperties": false
                    }
                },
                "max_tokens": { "type": "integer", "minimum": 1, "description": "The most the model may write." },
                "system": { "type": "string", "description": "Instructions for the model." },
                "model": { "type": "string", "description": "An approved model id; usually left out." }
            },
            "required": ["messages", "max_tokens"],
            "additionalProperties": false
        }),
        side_effect: SideEffect::Disclose,
        // Labels, routing and redaction govern model calls; IT can still put
        // them at Ask or Never for an agent or group (ADR 0018).
        default_tier: Tier::Auto,
        ask: Default::default(),
    }
}

/// Declares `model.complete` and checks its input. It never sends anything:
/// the runtime routes model calls through the [`crate::Gateway`] after
/// policy has decided where they may go.
#[derive(Debug, Default)]
pub struct ModelAction;

impl ActionProvider for ModelAction {
    fn actions(&self) -> Vec<ActionDef> {
        vec![definition()]
    }

    fn preview(
        &self,
        _ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        if action.as_str() != MODEL_COMPLETE {
            return Err(ActionError::InvalidInput(format!(
                "the model gateway has no action {action}"
            )));
        }
        let input = CompleteInput::parse(input)?;
        Ok(Preview {
            summary: format!("Ask an AI model: {}", input.describe()),
            targets: vec!["AI model".into()],
            ..Preview::default()
        })
    }

    fn invoke(
        &self,
        _ctx: &TaskContext,
        _action: &ActionName,
        _input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        // Fail closed: a call that reaches here skipped routing.
        Err(ActionError::Failed(
            "model calls go through the model gateway, which didn't route this one".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn input(v: Value) -> Result<CompleteInput, ActionError> {
        CompleteInput::parse(&v)
    }

    #[test]
    fn input_is_checked_before_anything_else() {
        let ok = input(json!({
            "messages": [{"role": "user", "content": "Summarize the invoices"}],
            "max_tokens": 200,
            "system": "Be brief."
        }))
        .unwrap();
        assert_eq!(ok.describe(), "1 message, 31 characters");
        for bad in [
            json!({"messages": [], "max_tokens": 10}),
            json!({"messages": [{"role": "assistant", "content": "hi"}], "max_tokens": 10}),
            json!({"messages": [{"role": "user", "content": " "}], "max_tokens": 10}),
            json!({"messages": [{"role": "user", "content": "hi"}], "max_tokens": 0}),
            json!({"messages": [{"role": "user", "content": "hi"}], "max_tokens": 10, "stream": true}),
            json!({"messages": [{"role": "user", "content": "x".repeat(MAX_PROMPT_CHARS + 1)}], "max_tokens": 10}),
        ] {
            assert!(
                matches!(input(bad.clone()), Err(ActionError::InvalidInput(_))),
                "{bad}"
            );
        }
        assert_eq!(thousands(1_204), "1,204");
        assert_eq!(thousands(400_000), "400,000");
    }

    #[test]
    fn adr0041_tool_calls_come_from_the_model_and_results_answer_them() {
        let tool = json!({ "name": "files_list", "input_schema": { "type": "object" } });
        let call = json!({ "id": "c1", "name": "files_list", "input": {} });
        let ok = input(json!({
            "messages": [
                {"role": "user", "content": "List my files"},
                {"role": "assistant", "content": "", "tool_calls": [call]},
                {"role": "user", "tool_results": [{"id": "c1", "content": "a.txt"}]}
            ],
            "max_tokens": 100,
            "tools": [tool]
        }))
        .unwrap();
        assert!(ok.describe().ends_with(", 1 tool"), "{}", ok.describe());
        assert!(ok.chars() > "List my files".len() + "a.txt".len());
        for bad in [
            // A result for a call nobody made.
            json!({"messages": [{"role": "user", "content": "x"}, {"role": "assistant", "content": "y"},
                {"role": "user", "tool_results": [{"id": "c9", "content": "?"}]}], "max_tokens": 10}),
            // The agent can't write the model's calls, nor the model results.
            json!({"messages": [{"role": "user", "content": "x", "tool_calls": [call]}], "max_tokens": 10}),
            json!({"messages": [{"role": "user", "content": "x"},
                {"role": "assistant", "tool_results": [{"id": "c1", "content": "?"}]}], "max_tokens": 10}),
            // A tool name providers refuse, and too many tools.
            json!({"messages": [{"role": "user", "content": "x"}], "max_tokens": 10,
                "tools": [{"name": "files.list", "input_schema": {}}]}),
            json!({"messages": [{"role": "user", "content": "x"}], "max_tokens": 10,
                "tools": vec![tool.clone(); MAX_TOOLS + 1]}),
        ] {
            assert!(
                matches!(input(bad.clone()), Err(ActionError::InvalidInput(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_provider_previews_but_never_sends() {
        let ctx = glass_actions::testing::context(Path::new("/home/demo"));
        let name: ActionName = MODEL_COMPLETE.parse().unwrap();
        let args = json!({"messages": [{"role": "user", "content": "hello"}], "max_tokens": 5});
        let preview = ModelAction.preview(&ctx, &name, &args).unwrap();
        assert_eq!(preview.summary, "Ask an AI model: 1 message, 5 characters");
        assert!(ModelAction.invoke(&ctx, &name, &args).is_err());
        let def = definition();
        assert_eq!(def.mcp_name(), "model_complete");
        assert_eq!(def.side_effect, SideEffect::Disclose);
    }
}
