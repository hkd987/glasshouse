//! A provider that answers on this machine with a deterministic reply and
//! sends nothing anywhere: for tests, development and the e2e suite.

use std::sync::Mutex;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{Message, ModelProvider, ProviderError, ProviderReply, ProviderRequest};

/// What a scripted provider was sent, for tests to inspect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
}

#[derive(Debug)]
pub struct ScriptedProvider {
    fails: Option<ProviderError>,
    seen: Mutex<Vec<Seen>>,
}

impl Default for ScriptedProvider {
    fn default() -> Self {
        Self::echo()
    }
}

impl ScriptedProvider {
    /// Replies by describing what it received, including the last user
    /// message and the instructions as they arrived, so a test can see
    /// what left the machine.
    pub fn echo() -> Self {
        Self {
            fails: None,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Fails every call with `error`, to exercise failover.
    pub fn failing(error: ProviderError) -> Self {
        Self {
            fails: Some(error),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Every request it has been sent.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// A rough token count: four characters a token, as providers estimate.
fn tokens(chars: usize) -> u64 {
    chars.div_ceil(4) as u64
}

/// What its thinking block is "signed" with, as Anthropic binds one (ADR
/// 0043): everything the model was sent before writing it.
fn signature(request: &ProviderRequest<'_>, before: &[Message]) -> String {
    let prefix = json!({ "system": request.system, "tools": request.tools, "messages": before });
    format!(
        "scripted:{}",
        hex::encode(Sha256::digest(prefix.to_string().as_bytes()))
    )
}

/// The reply's blocks, Anthropic shaped: an empty thinking block bound to
/// the request, then the text and the tool calls.
fn blocks(request: &ProviderRequest<'_>, text: &str, calls: &[crate::ToolCall]) -> Vec<Value> {
    let mut blocks = vec![json!({
        "type": "thinking",
        "thinking": "",
        "signature": signature(request, request.messages),
    })];
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    blocks.extend(
        calls
            .iter()
            .map(|c| json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.input })),
    );
    blocks
}

/// Checks each replayed thinking block against what came before it, as
/// Anthropic does: a changed conversation is refused. Says whether the last
/// assistant message's came back.
fn replayed(request: &ProviderRequest<'_>) -> Result<bool, ProviderError> {
    let mut last = false;
    for (i, m) in request.messages.iter().enumerate() {
        if m.role != crate::Role::Assistant {
            continue;
        }
        last = false;
        let Some(replay) = &m.replay else { continue };
        for block in replay.blocks.iter().filter(|b| b["type"] == "thinking") {
            if block["signature"].as_str() != Some(&signature(request, &request.messages[..i])) {
                return Err(ProviderError::Rejected(format!(
                    "messages.{i}: Invalid `signature` in `thinking` block. The block is bound \
                     to a different conversation."
                )));
            }
            last = true;
        }
    }
    Ok(last)
}

impl ModelProvider for ScriptedProvider {
    fn complete(&self, request: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError> {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Seen {
                model: request.model.to_owned(),
                system: request.system.map(str::to_owned),
                messages: request.messages.to_vec(),
                max_tokens: request.max_tokens,
            });
        if let Some(error) = &self.fails {
            return Err(error.clone());
        }
        let reasoned = replayed(request)?;
        let chars: usize = request.system.map_or(0, |s| s.chars().count())
            + request
                .messages
                .iter()
                .map(|m| {
                    m.content.chars().count()
                        + m.tool_results
                            .iter()
                            .map(|r| r.content.chars().count())
                            .sum::<usize>()
                })
                .sum::<usize>();
        // Tool use, deterministically (ADR 0041): a fresh request that
        // names a tool it was offered calls that tool, with the JSON object
        // written after its name; once results come back it answers.
        if let Some(call) = scripted_call(request) {
            let text = format!("Calling {}.", call.name);
            return Ok(ProviderReply {
                blocks: blocks(request, &text, std::slice::from_ref(&call)),
                text,
                tool_calls: vec![call],
                input_tokens: tokens(chars),
                output_tokens: 10,
                stop_reason: Some("tool_use".into()),
                ..ProviderReply::default()
            });
        }
        let last: String = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == crate::Role::User)
            .map(|m| m.content.chars().take(2000).collect())
            .unwrap_or_default();
        let mut text = format!(
            "Scripted reply from {model} (nothing left this computer). You sent {n} message(s), \
             {chars} characters. Your last message read: {last}",
            model = request.model,
            n = request.messages.len(),
        );
        if let Some(system) = request.system {
            let system: String = system.chars().take(2000).collect();
            text.push_str(&format!(" Your instructions read: {system}"));
        }
        let results: Vec<String> = request
            .messages
            .last()
            .map(|m| {
                m.tool_results
                    .iter()
                    .map(|r| r.content.chars().take(500).collect())
                    .collect()
            })
            .unwrap_or_default();
        if !results.is_empty() {
            text = format!(
                "Scripted reply from {model} (nothing left this computer). The tools returned: {}",
                results.join(" | "),
                model = request.model,
            );
            if reasoned {
                text.push_str(" Its reasoning came back unchanged.");
            }
        }
        let output = tokens(text.chars().count()).min(u64::from(request.max_tokens));
        Ok(ProviderReply {
            input_tokens: tokens(chars),
            output_tokens: output,
            stop_reason: Some("end_turn".into()),
            cost_usd: None,
            served_by: None,
            blocks: blocks(request, &text, &[]),
            text,
            tool_calls: Vec::new(),
            ..ProviderReply::default()
        })
    }
}

/// The tool a fresh request asks for in words: the first offered tool its
/// last message names, with the JSON object after the name as its input
/// (`{}` when there's none). Development stand-ins for real providers (the
/// console's for OpenRouter) call tools the same way.
pub fn scripted_call(request: &ProviderRequest<'_>) -> Option<crate::ToolCall> {
    let last = request.messages.last()?;
    if last.role != crate::Role::User || !last.tool_results.is_empty() {
        return None;
    }
    let text = &last.content;
    let (at, tool) = request
        .tools
        .iter()
        .filter_map(|t| text.find(t.name.as_str()).map(|at| (at, t)))
        .min_by_key(|(at, _)| *at)?;
    let after = &text[at + tool.name.len()..];
    let input = after
        .find('{')
        .and_then(|open| {
            let mut stream =
                serde_json::Deserializer::from_str(&after[open..]).into_iter::<serde_json::Value>();
            stream.next()?.ok()
        })
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    Some(crate::ToolCall {
        id: format!("call_{}", request.messages.len()),
        name: tool.name.clone(),
        input,
    })
}
