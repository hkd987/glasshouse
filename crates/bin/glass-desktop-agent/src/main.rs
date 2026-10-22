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
/// The tool the agent offers the model for handing the rest of the task to
/// the strongest model policy approves (ADR 0044).
const STRONGER: &str = "use_a_stronger_model";

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
what you did and what you found. The answer is shown as plain text, not Markdown: no \
asterisks, headings or bullet marks, and a blank line between points.";

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
        .filter(|t| !OWN.contains(&t.name.as_str()) && t.name != STRONGER)
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect()
}

/// The model's way up (ADR 0044): every task starts on the cheapest model
/// policy approves for its data and moves up a step each time the model
/// asks ([`Ladder`]). Offered from the first step, so the tools never
/// change mid-task.
fn stronger_tool() -> Value {
    json!({
        "name": STRONGER,
        "description": "Hand the rest of this task to the next stronger, costlier model; call it \
            again to go up another step. Call it first when the task needs careful judgment, exact \
            figures from many numbers, or writing that must be just right; or when a step went \
            wrong in a way you can't put right. Not for lookups, lists or drafts you can do well \
            yourself. Say why in one line.",
        "input_schema": {
            "type": "object",
            "properties": { "reason": { "type": "string", "description": "Why, in one line" } },
            "required": ["reason"],
            "additionalProperties": false
        }
    })
}

/// Which model a task's steps go to (ADR 0044): the cheapest policy
/// approves for its data, then one step up at a time, to the cheapest
/// dearer than the model that last answered, when the model asks or a
/// step runs out of room. Policy decides which models these are.
#[derive(Debug, Default)]
struct Ladder {
    /// The model to go above, once the task has stepped up.
    above: Option<String>,
    /// The policy id of the model that answered the last step.
    answered: Option<String>,
    /// Whether policy approves a dearer model than that one here.
    stronger: bool,
}

impl Ladder {
    /// `model_complete`'s routing arguments for the next step.
    fn args(&self) -> Value {
        match &self.above {
            Some(above) => json!({ "prefer": "cheapest", "above": above }),
            None => json!({ "prefer": "cheapest" }),
        }
    }

    /// The reply's `model` and `stronger` (whether there's a step up).
    fn answered_by(&mut self, reply: &Value) {
        self.answered = reply["model"].as_str().map(str::to_owned);
        self.stronger = reply["stronger"].as_bool().unwrap_or(false);
    }

    /// One step up from the model that last answered. False at the top,
    /// or when policy doesn't let agents choose by cost.
    fn step_up(&mut self) -> bool {
        match &self.answered {
            Some(model) if self.stronger && self.above.as_ref() != Some(model) => {
                self.above = Some(model.clone());
                true
            }
            _ => false,
        }
    }
}

/// What a tool returned, as the model reads it: its words and its data,
/// cut short. The words are often only a summary ("7 messages in Inbox"),
/// and the data is the messages, events or people themselves (AE-3).
fn result_text(result: &CallToolResult) -> String {
    let mut text = result.text();
    if let Some(data) = &result.structured_content {
        text.push_str("\n\n");
        text.push_str(&data.to_string());
    }
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

/// When it is on this computer, for the start of the conversation: a
/// model knows no date of its own, and the calendar's times are local
/// (ADR 0031). It goes in the first message, not the instructions, so the
/// instructions and tools stay the same from task to task and the prompt
/// cache keeps them (ADR 0043).
fn now_line(now: time::OffsetDateTime, offset_secs: i32) -> String {
    let offset = time::UtcOffset::from_whole_seconds(offset_secs).unwrap_or(time::UtcOffset::UTC);
    let local = now.to_offset(offset);
    let format = time::macros::format_description!(
        "[weekday], [day padding:none] [month repr:long] [year], [hour]:[minute]"
    );
    let zone = time::macros::format_description!("[offset_hour sign:mandatory]:[offset_minute]");
    format!(
        "It is {} on this computer (UTC{}).",
        local.format(&format).unwrap_or_default(),
        local.format(&zone).unwrap_or_default()
    )
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
    let mut tools = offered(&agent.tool_list().await?);
    tools.push(stronger_tool());
    let mut ladder = Ladder::default();
    let opening = format!(
        "{}\n\n{}",
        now_line(time::OffsetDateTime::now_utc(), agent.utc_offset_secs),
        agent.prompt
    );
    let mut messages = vec![json!({ "role": "user", "content": opening })];

    for round in 1..=MAX_STEPS {
        agent.progress("Thinking", None, None).await?;
        let mut args = json!({
            "system": SYSTEM,
            "messages": messages,
            "max_tokens": MAX_TOKENS,
            "tools": tools,
        });
        if let (Some(args), Value::Object(routing)) = (args.as_object_mut(), ladder.args()) {
            args.extend(routing);
        }
        let reply = agent.call("model_complete", args).await?;
        if reply.is_error {
            // No model, or none for this data: there's nothing to do
            // without one.
            bail!("no AI model could help: {}", reply.text());
        }
        if let Some(data) = &reply.structured_content {
            ladder.answered_by(data);
        }
        let calls = match step(&reply) {
            Step::Calls(calls) => calls,
            Step::Answer(answer) => {
                agent.answer(&caption(&answer), &answer).await?;
                eprintln!("answered after {round} round(s)");
                return Ok(());
            }
            // A step up, and the step asked again as it was, since
            // nothing of the cut-off reply was kept.
            Step::CutOff if ladder.step_up() => {
                agent
                    .progress("Handing this to a stronger model", None, None)
                    .await?;
                continue;
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
            if name == STRONGER {
                let content = if ladder.step_up() {
                    let why = call["input"]["reason"].as_str().unwrap_or_default();
                    eprintln!("{STRONGER}: {why}");
                    agent
                        .progress("Handing this to a stronger model", None, None)
                        .await?;
                    "From the next step a stronger model carries on with this task."
                } else {
                    "This is already the strongest model this task may use; carry on."
                };
                results.push(json!({ "id": id, "content": content, "is_error": false }));
                continue;
            }
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
    fn ae3_the_conversation_opens_with_the_computers_local_time() {
        let at = time::macros::datetime!(2026-10-22 12:24 UTC);
        assert_eq!(
            now_line(at, 7200),
            "It is Thursday, 22 October 2026, 14:24 on this computer (UTC+02:00)."
        );
        assert_eq!(
            now_line(at, -5 * 3600),
            "It is Thursday, 22 October 2026, 07:24 on this computer (UTC-05:00)."
        );
        assert!(now_line(at, i32::MAX).ends_with("12:24 on this computer (UTC+00:00)."));
    }

    #[test]
    fn ae3_the_model_reads_a_tools_data_not_only_its_summary() {
        let listed = CallToolResult::ok(
            "2 messages in Inbox",
            json!({ "messages": [{ "from": "Priya Shah", "subject": "Can we move our 1:1?" }] }),
        );
        let text = result_text(&listed);
        assert!(text.starts_with("2 messages in Inbox\n\n{"), "{text}");
        assert!(text.contains("Can we move our 1:1?"), "{text}");
        // A file's text is its content, with no data to add.
        let read = CallToolResult::ok("Q4 plan: ship by November 14.", Value::Null);
        assert_eq!(result_text(&read), "Q4 plan: ship by November 14.");
        let long = CallToolResult::ok("x".repeat(MAX_RESULT_CHARS + 10), Value::Null);
        assert!(result_text(&long).ends_with("\n[cut short]"));
    }

    #[test]
    fn adr0044_the_task_climbs_one_step_at_a_time_and_stops_at_the_top() {
        let mut ladder = Ladder::default();
        assert_eq!(ladder.args(), json!({ "prefer": "cheapest" }));
        assert!(!ladder.step_up(), "no step answered yet");
        ladder.answered_by(&json!({ "model": "haiku", "stronger": true }));
        assert!(ladder.step_up());
        assert_eq!(
            ladder.args(),
            json!({ "prefer": "cheapest", "above": "haiku" })
        );
        ladder.answered_by(&json!({ "model": "sonnet", "stronger": true }));
        assert!(ladder.step_up());
        assert_eq!(ladder.args()["above"], "sonnet");
        // Opus is the top: no step up, and no call wasted finding out.
        ladder.answered_by(&json!({ "model": "opus", "stronger": false }));
        assert!(!ladder.step_up());
        assert_eq!(ladder.args()["above"], "sonnet");
        // Where policy doesn't let agents choose by cost, nothing moves.
        let mut fixed = Ladder::default();
        fixed.answered_by(&json!({ "model": "sonnet", "stronger": false }));
        assert!(!fixed.step_up());
    }

    #[test]
    fn adr0044_the_model_is_offered_a_way_up_that_never_clashes() {
        let tool = stronger_tool();
        assert_eq!(tool["name"], STRONGER);
        assert_eq!(tool["input_schema"]["required"], json!(["reason"]));
        // The name is the agent's: a socket tool by it is never offered,
        // so the model's list never holds two (the API refuses that).
        let tool = |name: &str| glass_rpc::mcp::Tool {
            name: name.into(),
            title: None,
            description: "A tool".into(),
            input_schema: json!({ "type": "object" }),
            annotations: None,
        };
        let offered = offered(&[tool("files_list"), tool(STRONGER)]);
        let names: Vec<&str> = offered.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["files_list"]);
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
