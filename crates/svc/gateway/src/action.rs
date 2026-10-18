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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// What an agent sends: Anthropic Messages shaped, text only.
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
        if input.messages.iter().any(|m| m.content.trim().is_empty()) {
            return Err(ActionError::InvalidInput(
                "every message needs some text".into(),
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

    /// Characters in the system prompt and messages.
    pub fn chars(&self) -> usize {
        self.system.as_deref().map_or(0, |s| s.chars().count())
            + self
                .messages
                .iter()
                .map(|m| m.content.chars().count())
                .sum::<usize>()
    }

    /// "2 messages, 1,204 characters".
    pub fn describe(&self) -> String {
        let n = self.messages.len();
        format!(
            "{n} message{}, {} characters",
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
                            "content": { "type": "string" }
                        },
                        "required": ["role", "content"],
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
            json!({"messages": [{"role": "user", "content": "hi"}], "max_tokens": 10, "tools": []}),
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
