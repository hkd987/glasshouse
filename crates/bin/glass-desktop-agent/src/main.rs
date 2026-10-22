//! The general desktop agent (AE-3, ADR 0041): anything the person asks
//! that no specialist does. A model chooses each step from the tools this
//! task's socket offers; the agent makes each call itself, over the socket,
//! so tiers, asks, Never, labels, scope, undo and the audit log apply to
//! every step as they would to any agent's. The model only proposes.
//!
//! The loop: offer the model the tools and the conversation so far; run
//! the calls it asks for, in order, and send back what they returned; stop
//! when it answers in words, after [`MAX_STEPS`] rounds, or when no model
//! can be reached. A refused or failed call is a result the model reads,
//! never retried by the agent. The answer is left for the person with the
//! task.

use std::process::ExitCode;

use anyhow::{Context, bail};
use glass_agent_sdk::{Agent, CallToolResult};
use serde_json::{Value, json};

/// Rounds of model and tools before the agent stops with what it has.
const MAX_STEPS: usize = 20;
/// The most a model may write in one round, its thinking included (ADR
/// 0043).
const MAX_TOKENS: u32 = 16_000;
/// The most of one tool's result sent back to the model.
const MAX_RESULT_CHARS: usize = 20_000;
/// Its own tools, which the model never calls: the agent does.
const OWN: [&str; 2] = ["model_complete", "report_progress"];

const SYSTEM: &str = "You are the Glasshouse desktop agent, working for the person \
on this computer. Do what they ask using the tools you're given; they're the only \
way you can see or change anything. Some tools ask the person first, and policy may \
refuse a call: a refused or failed call comes back as an error result. Don't retry \
it; work around it or say what you couldn't do. Keep to what was asked. Personal \
details such as email addresses reach you as placeholders like [redacted:email#1]: \
each stands for the real value, which this computer puts back wherever you use the \
placeholder, in a tool call or your answer. Use it as you would the value; don't ask \
the person for it. Secrets such as keys and passwords stay hidden and are never put \
back. When you're done, answer the person in a few short paragraphs of plain words: \
what you did and what you found.";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-desktop-agent: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The tools a model may ask for: the socket's, but the agent's own.
fn offered(tools: &[glass_rpc::mcp::Tool]) -> Vec<Value> {
    tools
        .iter()
        .filter(|t| !OWN.contains(&t.name.as_str()))
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect()
}

/// What a tool returned, as the model reads it: its words, cut short.
fn result_text(result: &CallToolResult) -> String {
    let text = result.text();
    if text.chars().count() <= MAX_RESULT_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(MAX_RESULT_CHARS).collect();
    cut.push_str("\n[cut short]");
    cut
}

/// What a model's reply asks of the agent.
#[derive(Debug, PartialEq)]
enum Step {
    /// Make these calls and send back what they return.
    Calls(Vec<Value>),
    /// Done: this is the answer.
    Answer(String),
    /// Cut off at `max_tokens`: thinking can take all of a step's room,
    /// and a call it was writing may be cut short, so it is neither an
    /// answer nor calls to make (ADR 0043).
    CutOff,
}

fn step(reply: &CallToolResult) -> Step {
    let data = reply.structured_content.as_ref();
    if data.is_some_and(|d| d["stop_reason"] == "max_tokens") {
        return Step::CutOff;
    }
    let calls = data
        .and_then(|d| d["tool_calls"].as_array().cloned())
        .unwrap_or_default();
    if !calls.is_empty() {
        return Step::Calls(calls);
    }
    let text = reply.text();
    Step::Answer(if text.trim().is_empty() {
        "Done.".to_owned()
    } else {
        text
    })
}

/// A tile caption from the answer's first line.
fn caption(answer: &str) -> String {
    let line = answer
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("Done");
    let mut caption: String = line.trim().chars().take(100).collect();
    if line.trim().chars().count() > 100 {
        caption.push('…');
    }
    caption
}

async fn run() -> anyhow::Result<()> {
    let agent = Agent::from_env()
        .await
        .context("connecting to the task socket")?;
    eprintln!("task {}: {}", agent.task_id, agent.prompt);
    if !agent.has_tool("model_complete") {
        bail!("this agent needs an AI model, and policy gives it none here");
    }
    let tools = offered(&agent.tool_list().await?);
    let mut messages = vec![json!({ "role": "user", "content": agent.prompt })];

    for round in 1..=MAX_STEPS {
        agent.progress("Thinking", None, None).await?;
        let reply = agent
            .call(
                "model_complete",
                json!({
                    "system": SYSTEM,
                    "messages": messages,
                    "max_tokens": MAX_TOKENS,
                    "tools": tools,
                }),
            )
            .await?;
        if reply.is_error {
            // No model, or none for this data: there's nothing to do
            // without one.
            bail!("no AI model could help: {}", reply.text());
        }
        let calls = match step(&reply) {
            Step::Calls(calls) => calls,
            Step::Answer(answer) => {
                agent.answer(&caption(&answer), &answer).await?;
                eprintln!("answered after {round} round(s)");
                return Ok(());
            }
            Step::CutOff => bail!(
                "the AI model ran out of room before it finished a step \
                 ({MAX_TOKENS} tokens a step, its thinking included)"
            ),
        };
        let data = reply.structured_content.as_ref();
        let mut said = json!({ "role": "assistant", "content": reply.text(), "tool_calls": calls });
        // The model's reasoning, sent back as it came so the next step
        // carries it on (ADR 0043).
        if let Some(replay) = data.map(|d| &d["replay"]).filter(|r| r.is_object()) {
            said["replay"] = replay.clone();
        }
        messages.push(said);
        let mut results = Vec::with_capacity(calls.len());
        for call in &calls {
            let name = call["name"].as_str().unwrap_or_default();
            let id = call["id"].clone();
            // Only what the socket offers; anything else is the model's
            // mistake, told back to it.
            if OWN.contains(&name) || !agent.has_tool(name) {
                results.push(json!({
                    "id": id,
                    "content": format!("There's no tool {name:?} here."),
                    "is_error": true,
                }));
                continue;
            }
            agent
                .progress(&format!("Using {}", name.replace('_', " ")), None, None)
                .await?;
            let result = agent.call(name, call["input"].clone()).await?;
            eprintln!(
                "{name}: {}",
                if result.is_error { "not done" } else { "done" }
            );
            results.push(json!({
                "id": id,
                "content": result_text(&result),
                "is_error": result.is_error,
            }));
        }
        messages.push(json!({ "role": "user", "tool_results": results }));
    }
    let answer = format!(
        "I stopped after {MAX_STEPS} rounds without finishing. What I did is in this task's activity."
    );
    agent.answer("Stopped before finishing", &answer).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ae3_the_model_is_offered_the_sockets_tools_but_the_agents_own() {
        let tool = |name: &str| glass_rpc::mcp::Tool {
            name: name.into(),
            title: None,
            description: format!("{name} does a thing"),
            input_schema: json!({ "type": "object" }),
            annotations: None,
        };
        let offered = offered(&[
            tool("files_list"),
            tool("model_complete"),
            tool("report_progress"),
            tool("mail_draft"),
        ]);
        let names: Vec<&str> = offered.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["files_list", "mail_draft"]);
        assert_eq!(offered[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn adr0043_a_reply_cut_off_at_max_tokens_is_no_answer_and_no_calls() {
        let reply = |stop: &str, calls: Value, text: &str| {
            CallToolResult::ok(text, json!({ "stop_reason": stop, "tool_calls": calls }))
        };
        let call = json!([{ "id": "toolu_1", "name": "files_list", "input": { "path": "Do" } }]);
        assert_eq!(step(&reply("max_tokens", json!(null), "")), Step::CutOff);
        assert_eq!(step(&reply("max_tokens", call.clone(), "")), Step::CutOff);
        assert_eq!(
            step(&reply("max_tokens", json!(null), "You have three fi")),
            Step::CutOff
        );
        assert_eq!(
            step(&reply("tool_use", call.clone(), "")),
            Step::Calls(call.as_array().cloned().unwrap_or_default())
        );
        assert_eq!(
            step(&reply("end_turn", json!(null), "Three files.")),
            Step::Answer("Three files.".into())
        );
        assert_eq!(
            step(&reply("end_turn", json!(null), " ")),
            Step::Answer("Done.".into())
        );
    }

    #[test]
    fn ae3_the_caption_is_the_answers_first_line_short() {
        assert_eq!(
            caption("\n\nYou have 3 invoices.\nMore detail"),
            "You have 3 invoices."
        );
        assert_eq!(caption(&"x".repeat(150)).chars().count(), 101);
        assert_eq!(caption(""), "Done");
    }
}
