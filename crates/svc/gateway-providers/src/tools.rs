//! How each provider spells tool use (ADR 0041): Anthropic's content
//! blocks, OpenAI's function calls (OpenRouter), and Bedrock Converse's
//! `toolUse` and `toolResult`. A message with no tool calls or results is
//! sent as it always was.

use glass_gateway::{Message, Role, Tool, ToolCall};
use serde_json::{Value, json};

fn role(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

/// Providers say "tool_calls" or "tool_use": ours is "tool_use".
pub(crate) fn stop_reason(reason: Option<String>) -> Option<String> {
    reason.map(|r| {
        if r == "tool_calls" {
            "tool_use".into()
        } else {
            r
        }
    })
}

/// Anthropic Messages' `messages`.
pub(crate) fn anthropic_messages(messages: &[Message]) -> Value {
    Value::Array(
        messages
            .iter()
            .map(|m| {
                if m.tool_calls.is_empty() && m.tool_results.is_empty() {
                    return json!({ "role": role(m.role), "content": m.content });
                }
                let mut blocks = Vec::new();
                for r in &m.tool_results {
                    blocks.push(json!({
                        "type": "tool_result",
                        "tool_use_id": r.id,
                        "content": r.content,
                        "is_error": r.is_error,
                    }));
                }
                if !m.content.trim().is_empty() {
                    blocks.push(json!({ "type": "text", "text": m.content }));
                }
                for c in &m.tool_calls {
                    blocks.push(
                        json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.input }),
                    );
                }
                json!({ "role": role(m.role), "content": blocks })
            })
            .collect(),
    )
}

pub(crate) fn anthropic_tools(tools: &[Tool]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema }))
            .collect(),
    )
}

/// The `tool_use` blocks of Anthropic's reply.
pub(crate) fn anthropic_calls(content: &[Value]) -> Vec<ToolCall> {
    content
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .filter_map(|b| {
            Some(ToolCall {
                id: b["id"].as_str()?.to_owned(),
                name: b["name"].as_str()?.to_owned(),
                input: b.get("input").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect()
}

/// OpenAI-style chat `messages` (OpenRouter): results are `tool` messages
/// of their own, calls are the assistant's `tool_calls`.
pub(crate) fn openai_messages(system: Option<&str>, messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    if let Some(system) = system {
        out.push(json!({ "role": "system", "content": system }));
    }
    for m in messages {
        for r in &m.tool_results {
            let content = if r.is_error {
                format!("Error: {}", r.content)
            } else {
                r.content.clone()
            };
            out.push(json!({ "role": "tool", "tool_call_id": r.id, "content": content }));
        }
        if m.tool_calls.is_empty() {
            if !m.content.trim().is_empty() || m.tool_results.is_empty() {
                out.push(json!({ "role": role(m.role), "content": m.content }));
            }
            continue;
        }
        let calls: Vec<Value> = m
            .tool_calls
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "type": "function",
                    "function": { "name": c.name, "arguments": c.input.to_string() },
                })
            })
            .collect();
        let content = (!m.content.trim().is_empty()).then(|| m.content.clone());
        out.push(json!({ "role": "assistant", "content": content, "tool_calls": calls }));
    }
    out
}

pub(crate) fn openai_tools(tools: &[Tool]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.input_schema },
                })
            })
            .collect(),
    )
}

/// The `tool_calls` of an OpenAI-style reply message. Arguments come as a
/// JSON string; one that doesn't parse is sent as an empty object, and the
/// call's own checks say what's missing.
pub(crate) fn openai_calls(message: &Value) -> Vec<ToolCall> {
    message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let function = &c["function"];
            Some(ToolCall {
                id: c["id"].as_str()?.to_owned(),
                name: function["name"].as_str()?.to_owned(),
                input: function["arguments"]
                    .as_str()
                    .and_then(|a| serde_json::from_str::<Value>(a).ok())
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({})),
            })
        })
        .collect()
}

/// Bedrock Converse's `messages`.
pub(crate) fn converse_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let mut blocks = Vec::new();
            for r in &m.tool_results {
                blocks.push(json!({ "toolResult": {
                    "toolUseId": r.id,
                    "content": [{ "text": r.content }],
                    "status": if r.is_error { "error" } else { "success" },
                }}));
            }
            if !m.content.trim().is_empty()
                || (m.tool_calls.is_empty() && m.tool_results.is_empty())
            {
                blocks.push(json!({ "text": m.content }));
            }
            for c in &m.tool_calls {
                blocks.push(
                    json!({ "toolUse": { "toolUseId": c.id, "name": c.name, "input": c.input } }),
                );
            }
            json!({ "role": role(m.role), "content": blocks })
        })
        .collect()
}

pub(crate) fn converse_tools(tools: &[Tool]) -> Value {
    json!({
        "tools": tools
            .iter()
            .map(|t| json!({ "toolSpec": {
                "name": t.name,
                "description": t.description,
                "inputSchema": { "json": t.input_schema },
            }}))
            .collect::<Vec<_>>()
    })
}

/// The `toolUse` blocks of a Converse reply.
pub(crate) fn converse_calls(content: &[Value]) -> Vec<ToolCall> {
    content
        .iter()
        .filter_map(|b| b.get("toolUse"))
        .filter_map(|u| {
            Some(ToolCall {
                id: u["toolUseId"].as_str()?.to_owned(),
                name: u["name"].as_str()?.to_owned(),
                input: u.get("input").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use glass_gateway::ToolResult;

    use super::*;

    fn turn() -> Vec<Message> {
        vec![
            Message::text(Role::User, "List my files"),
            Message {
                role: Role::Assistant,
                content: "Looking.".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "files_list".into(),
                    input: json!({ "path": "Docs" }),
                }],
                tool_results: Vec::new(),
            },
            Message {
                role: Role::User,
                content: String::new(),
                tool_calls: Vec::new(),
                tool_results: vec![ToolResult {
                    id: "c1".into(),
                    content: "a.txt".into(),
                    is_error: false,
                }],
            },
        ]
    }

    #[test]
    fn adr0041_anthropic_gets_blocks_and_gives_back_tool_use() {
        let sent = anthropic_messages(&turn());
        assert_eq!(sent[0]["content"], "List my files");
        assert_eq!(sent[1]["content"][0]["type"], "text");
        assert_eq!(sent[1]["content"][1]["type"], "tool_use");
        assert_eq!(sent[1]["content"][1]["input"]["path"], "Docs");
        assert_eq!(sent[2]["content"][0]["type"], "tool_result");
        assert_eq!(sent[2]["content"][0]["tool_use_id"], "c1");
        let calls = anthropic_calls(&[
            json!({ "type": "text", "text": "Reading." }),
            json!({ "type": "tool_use", "id": "t2", "name": "files_read", "input": { "path": "a.txt" } }),
        ]);
        assert_eq!(calls[0].name, "files_read");
        assert_eq!(calls[0].input["path"], "a.txt");
    }

    #[test]
    fn adr0041_openai_style_gets_tool_messages_and_parses_arguments() {
        let sent = openai_messages(Some("Be brief."), &turn());
        assert_eq!(sent[0]["role"], "system");
        assert_eq!(
            sent[2]["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"Docs"}"#
        );
        assert_eq!(sent[3]["role"], "tool");
        assert_eq!(sent[3]["tool_call_id"], "c1");
        assert_eq!(
            sent.len(),
            4,
            "a results-only message adds no empty user message"
        );
        let calls = openai_calls(&json!({ "tool_calls": [
            { "id": "x", "type": "function", "function": { "name": "files_read", "arguments": "{\"path\":\"a.txt\"}" } },
            { "id": "y", "type": "function", "function": { "name": "files_list", "arguments": "not json" } }
        ]}));
        assert_eq!(calls[0].input["path"], "a.txt");
        assert_eq!(calls[1].input, json!({}));
        assert_eq!(
            stop_reason(Some("tool_calls".into())).as_deref(),
            Some("tool_use")
        );
        assert_eq!(
            openai_tools(&[Tool {
                name: "files_list".into(),
                description: "List".into(),
                input_schema: json!({ "type": "object" }),
            }])[0]["function"]["parameters"]["type"],
            "object"
        );
    }

    #[test]
    fn adr0041_converse_gets_tool_use_and_tool_result_blocks() {
        let sent = converse_messages(&turn());
        assert_eq!(sent[1]["content"][1]["toolUse"]["toolUseId"], "c1");
        assert_eq!(sent[2]["content"][0]["toolResult"]["status"], "success");
        assert_eq!(
            sent[2]["content"].as_array().unwrap().len(),
            1,
            "no empty text block"
        );
        let calls = converse_calls(&[
            json!({ "toolUse": { "toolUseId": "u1", "name": "files_list", "input": {} } }),
        ]);
        assert_eq!(calls[0].id, "u1");
    }
}
