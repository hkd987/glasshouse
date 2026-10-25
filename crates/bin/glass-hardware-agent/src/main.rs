//! The hardware agent (ADR 0057, slice 2): "Wi-Fi stops after sleep", "the
//! touchpad doesn't work". It reads this computer's hardware report, plans
//! what to read in the kernel's source of the release it runs, writes the
//! smallest patch, has the build service build it, fixes it from the
//! compiler's errors (three tries), and proposes it: the person reads the
//! change and says yes before the next restart tries it, every time.
//!
//! A model does the thinking (`model.complete`), taught by the
//! `kernel-patch` skill (`skill/SKILL.md`, its evals beside it). This
//! program does the rest: the tools, and turning the model's exact edits
//! into a diff against the source it read, so a diff is never malformed.
//! Everything goes through the `hardware.*` actions: tiers, the audit log
//! and the person's yes apply whoever picked the step. Without a model it
//! says so and stops.

use std::process::ExitCode;

use anyhow::Context;
use glass_agent_sdk::{Agent, CallToolResult};
use serde_json::{Value, json};

/// What the model knows about writing a kernel patch.
const SKILL: &str = include_str!("../skill/SKILL.md");
/// Room to think first, and a patch's edits.
const PLAN_TOKENS: u32 = 4_000;
const WRITE_TOKENS: u32 = 12_000;
/// Builds before giving up.
const TRIES: usize = 3;
/// Times the write step may ask to read more first, and how much each
/// adds: the plan names its reads before the searches say where to look.
const MORE_READS: usize = 2;
const MORE_CONTEXT: usize = 30_000;
/// The most of a file read whole, in lines.
const MAX_FILE_LINES: u32 = 8_000;
/// The most of the source given to the model at once.
const MAX_CONTEXT: usize = 60_000;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-hardware-agent: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// A tool's data, or why not.
async fn call(
    agent: &Agent,
    tool: &str,
    arguments: Value,
) -> anyhow::Result<Result<Value, String>> {
    let reply = agent.call(tool, arguments).await?;
    Ok(if reply.is_error {
        Err(reply.text())
    } else {
        Ok(reply.structured_content.unwrap_or(Value::Null))
    })
}

/// The model's answer as a JSON object, if it is one.
fn object(reply: &str) -> Option<Value> {
    let json = reply.get(reply.find('{')?..=reply.rfind('}')?)?;
    serde_json::from_str::<Value>(json)
        .ok()
        .filter(Value::is_object)
}

/// Asks the model one step; `None` when it can't be asked or answered
/// nothing usable.
async fn ask(
    agent: &Agent,
    step: &str,
    user: String,
    max_tokens: u32,
) -> anyhow::Result<Option<Value>> {
    let reply: CallToolResult = agent
        .call(
            "model_complete",
            json!({
                "system": format!("{SKILL}\n\nThis is the {step} step."),
                "messages": [{ "role": "user", "content": format!("Step: {step}.\n\n{user}") }],
                "max_tokens": max_tokens,
            }),
        )
        .await?;
    if reply.is_error {
        eprintln!("model: {}", reply.text());
        return Ok(None);
    }
    let cut = reply
        .structured_content
        .as_ref()
        .is_some_and(|d| d["stop_reason"] == "max_tokens");
    Ok((!cut).then(|| object(&reply.text())).flatten())
}

/// The report, short: each part, its status, devices and note.
fn summarize(report: &Value) -> String {
    let mut out = format!(
        "{} {}, {}\n",
        report["maker"].as_str().unwrap_or(""),
        report["model"].as_str().unwrap_or(""),
        report["processor"].as_str().unwrap_or("")
    );
    for part in report["parts"].as_array().into_iter().flatten() {
        out += &format!(
            "- {}: {}; {}; {}\n",
            part["kind"].as_str().unwrap_or("?"),
            part["status"].as_str().unwrap_or("?"),
            part["devices"]
                .as_array()
                .map(|d| d
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default(),
            part["note"].as_str().unwrap_or("")
        );
    }
    out
}

/// A file of the kernel's source, whole (up to [`MAX_FILE_LINES`]).
async fn read_file(agent: &Agent, path: &str) -> anyhow::Result<Result<String, String>> {
    let path = path.strip_prefix("sys/").unwrap_or(path);
    let mut text = String::new();
    let mut from = 1u32;
    loop {
        let read = match call(
            agent,
            "hardware_source_read",
            json!({ "path": path, "from": from, "lines": 400 }),
        )
        .await?
        {
            Ok(read) => read,
            Err(why) => return Ok(Err(why)),
        };
        text += read["text"].as_str().unwrap_or("");
        let total = read["total_lines"].as_u64().unwrap_or(0) as u32;
        from += 400;
        if from > total || from > MAX_FILE_LINES {
            return Ok(Ok(text));
        }
    }
}

/// Reads each of `reads` (at most four: 400 lines of a file under `sys/`,
/// from a line) into `context`, numbered, until it holds `limit`.
async fn read_source(
    agent: &Agent,
    reads: &Value,
    context: &mut String,
    limit: usize,
) -> anyhow::Result<()> {
    for read in reads.as_array().into_iter().flatten().take(4) {
        let path = read["path"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("sys/");
        agent
            .progress(&format!("Reading sys/{path}"), None, None)
            .await?;
        let args = json!({ "path": path, "from": read["from"].as_u64().unwrap_or(1).max(1), "lines": 400 });
        if let Ok(text) = call(agent, "hardware_source_read", args).await? {
            let from = text["from"].as_u64().unwrap_or(1);
            *context += &format!("sys/{path}, from line {from}:\n");
            for (i, line) in text["text"].as_str().unwrap_or("").lines().enumerate() {
                *context += &format!("{:>5}\t{line}\n", from + i as u64);
            }
        }
        if context.len() > limit {
            break;
        }
    }
    Ok(())
}

/// One file's edits applied to its text: each `old` found exactly once.
fn apply_edits(path: &str, text: &str, edits: &[(String, String)]) -> Result<String, String> {
    let mut out = text.to_owned();
    for (old, new) in edits {
        match out.matches(old.as_str()).count() {
            1 => out = out.replacen(old.as_str(), new, 1),
            0 => {
                return Err(format!(
                    "{path}: an edit's old text isn't in the file as written"
                ));
            }
            n => {
                return Err(format!(
                    "{path}: an edit's old text is in the file {n} times; give more lines"
                ));
            }
        }
    }
    Ok(out)
}

/// A unified diff of `before` to `after`, one hunk from the first changed
/// line to the last, with three lines of context.
fn diff(path: &str, before: &str, after: &str) -> String {
    let a: Vec<&str> = before.split_inclusive('\n').collect();
    let b: Vec<&str> = after.split_inclusive('\n').collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let start = prefix.saturating_sub(3);
    let a_end = (a.len() - suffix + 3).min(a.len());
    let b_end = (b.len() - suffix + 3).min(b.len());
    let line = |l: &str| {
        if l.ends_with('\n') {
            l.to_owned()
        } else {
            format!("{l}\n\\ No newline at end of file\n")
        }
    };
    let mut out = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -{},{} +{},{} @@\n",
        start + 1,
        a_end - start,
        start + 1,
        b_end - start
    );
    for l in &a[start..prefix] {
        out += &format!(" {}", line(l));
    }
    for l in &a[prefix..a.len() - suffix] {
        out += &format!("-{}", line(l));
    }
    for l in &b[prefix..b.len() - suffix] {
        out += &format!("+{}", line(l));
    }
    for l in &a[a.len() - suffix..a_end] {
        out += &format!(" {}", line(l));
    }
    out
}

/// The model's edits as one diff, against the source as the service has
/// it.
async fn edits_to_diff(agent: &Agent, edits: &Value) -> anyhow::Result<Result<String, String>> {
    let mut by_file: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for edit in edits.as_array().into_iter().flatten() {
        let (Some(path), Some(old), Some(new)) = (
            edit["path"].as_str(),
            edit["old"].as_str(),
            edit["new"].as_str(),
        ) else {
            return Ok(Err("each edit has a path, old text and new text".into()));
        };
        let path = if path.starts_with("sys/") {
            path.to_owned()
        } else {
            format!("sys/{path}")
        };
        match by_file.iter_mut().find(|(p, _)| *p == path) {
            Some((_, list)) => list.push((old.into(), new.into())),
            None => by_file.push((path, vec![(old.into(), new.into())])),
        }
    }
    if by_file.is_empty() {
        return Ok(Err("no edits".into()));
    }
    let mut out = String::new();
    for (path, list) in &by_file {
        let before = match read_file(agent, path).await? {
            Ok(text) => text,
            Err(why) => return Ok(Err(format!("{path}: {why}"))),
        };
        match apply_edits(path, &before, list) {
            Ok(after) if after != before => out += &diff(path, &before, &after),
            Ok(_) => return Ok(Err(format!("{path}: the edits change nothing"))),
            Err(why) => return Ok(Err(why)),
        }
    }
    Ok(Ok(out))
}

async fn run() -> anyhow::Result<()> {
    let agent = Agent::from_env()
        .await
        .context("connecting to the task socket")?;
    eprintln!("task {}: {}", agent.task_id, agent.prompt);
    let request = agent.prompt.clone();
    if !agent.has_tool("model_complete") {
        agent
            .answer(
                "Needs AI models",
                "Writing a kernel patch needs AI models, and policy doesn't let this agent ask one.",
            )
            .await?;
        return Ok(());
    }
    for tool in [
        "hardware_report",
        "hardware_source_read",
        "hardware_patch_build",
        "hardware_patch_propose",
    ] {
        if !agent.has_tool(tool) {
            agent
                .answer(
                    "Can't write patches here",
                    &format!("This computer doesn't give this agent {tool}."),
                )
                .await?;
            return Ok(());
        }
    }

    agent
        .progress("Reading this computer's hardware report", None, None)
        .await?;
    let report = match call(&agent, "hardware_report", json!({})).await? {
        Ok(report) => summarize(&report),
        Err(why) => format!("(the report couldn't be read: {why})"),
    };

    // Plan what to read.
    agent.progress("Planning what to read", None, None).await?;
    let Some(plan) = ask(
        &agent,
        "plan",
        format!("The person says: {request}\n\nThis computer's hardware report:\n{report}"),
        PLAN_TOKENS,
    )
    .await?
    else {
        agent
            .answer(
                "No plan",
                "The model didn't answer with a plan; nothing was changed.",
            )
            .await?;
        return Ok(());
    };
    if let Some(why) = plan["no_patch"].as_str() {
        agent.answer("No kernel patch for this", why).await?;
        return Ok(());
    }

    // Read what it planned.
    let mut context = String::new();
    for search in plan["search"].as_array().into_iter().flatten().take(4) {
        let args = json!({
            "pattern": search["pattern"].as_str().unwrap_or(""),
            "path": search["path"].as_str().unwrap_or("").trim_start_matches("sys/"),
        });
        agent
            .progress(
                &format!("Searching the kernel's source for {}", args["pattern"]),
                None,
                None,
            )
            .await?;
        if let Ok(found) = call(&agent, "hardware_source_search", args.clone()).await? {
            context += &format!(
                "Search {} in sys/{}:\n",
                args["pattern"],
                args["path"].as_str().unwrap_or("")
            );
            for m in found["matches"].as_array().into_iter().flatten().take(40) {
                context += &format!(
                    "{}:{}: {}\n",
                    m["file"].as_str().unwrap_or(""),
                    m["line"],
                    m["text"].as_str().unwrap_or("")
                );
            }
        }
    }
    read_source(&agent, &plan["read"], &mut context, MAX_CONTEXT).await?;
    let mut context: String = context.chars().take(MAX_CONTEXT).collect();

    // Write it, reading more first if it asks, then build it until it
    // builds.
    agent.progress("Writing the patch", None, None).await?;
    let write = |context: &str| {
        format!(
            "The person says: {request}\n\nThe hardware report:\n{report}\nYour plan: {plan}\n\nThe source you read (line numbers before a tab aren't part of it):\n{context}"
        )
    };
    let mut written = ask(&agent, "write", write(&context), WRITE_TOKENS).await?;
    for _ in 0..MORE_READS {
        let Some(more) = written
            .as_ref()
            .filter(|w| w["edits"].is_null() && w["no_patch"].is_null())
            .map(|w| w["read"].clone())
            .filter(|r| r.as_array().is_some_and(|r| !r.is_empty()))
        else {
            break;
        };
        let mut added = String::new();
        read_source(&agent, &more, &mut added, MORE_CONTEXT).await?;
        context += &added.chars().take(MORE_CONTEXT).collect::<String>();
        agent.progress("Writing the patch", None, None).await?;
        written = ask(&agent, "write", write(&context), WRITE_TOKENS).await?;
    }
    let mut tries = 0;
    let build = loop {
        let Some(patch) = written.take() else {
            agent
                .answer(
                    "No patch",
                    "The model didn't write a patch; nothing was changed.",
                )
                .await?;
            return Ok(());
        };
        if let Some(why) = patch["no_patch"].as_str() {
            agent.answer("No kernel patch for this", why).await?;
            return Ok(());
        }
        tries += 1;
        let errors = match edits_to_diff(&agent, &patch["edits"]).await? {
            Err(why) => why,
            Ok(diff) => {
                agent
                    .progress(
                        &format!("Building the patch (try {tries} of {TRIES})"),
                        Some(tries as u32),
                        Some(TRIES as u32),
                    )
                    .await?;
                let mut args = json!({
                    "id": patch["id"],
                    "summary": patch["summary"],
                    "diff": diff,
                    "modules": patch["modules"].as_array().cloned().unwrap_or_default(),
                    "kernel": patch["kernel"].as_bool().unwrap_or(false),
                });
                if let Some(targets) = patch["targets"].as_array() {
                    args["targets"] = json!(targets);
                }
                match call(&agent, "hardware_patch_build", args).await? {
                    Ok(built) if built["ok"] == true => {
                        break (built["build"].as_str().unwrap_or("").to_owned(), patch);
                    }
                    Ok(built) => built["log"].as_str().unwrap_or("").to_owned(),
                    Err(why) => why,
                }
            }
        };
        eprintln!("try {tries}: {errors}");
        if tries >= TRIES {
            agent
                .answer(
                    "The patch didn't build",
                    &format!("After {TRIES} tries the patch still didn't build, so nothing was set up. The last errors:\n{errors}"),
                )
                .await?;
            return Ok(());
        }
        agent
            .progress("Fixing what the compiler said", None, None)
            .await?;
        written = ask(
            &agent,
            "fix",
            format!(
                "Your patch: {patch}\n\nIt didn't build. What it said:\n{errors}\n\nThe source you read:\n{context}"
            ),
            WRITE_TOKENS,
        )
        .await?;
    };

    // Propose it: the person reads it and says yes, or not. A driver this
    // computer's kernel has built in never loads from a module: built
    // again as a kernel patch, the same edits.
    let (mut build, mut patch) = build;
    agent
        .progress("Asking you to try it at the next restart", None, None)
        .await?;
    let mut proposed = call(&agent, "hardware_patch_propose", json!({ "build": build })).await?;
    if let Err(why) = &proposed
        && why.contains("rebuilds the kernel")
        && patch["kernel"] != true
    {
        agent
            .progress(
                "Building it into the kernel, which has the driver built in",
                None,
                None,
            )
            .await?;
        patch["kernel"] = json!(true);
        let diff = edits_to_diff(&agent, &patch["edits"]).await?;
        let rebuilt = match diff {
            Ok(diff) => {
                let mut args = json!({
                    "id": patch["id"],
                    "summary": patch["summary"],
                    "diff": diff,
                    "modules": patch["modules"].as_array().cloned().unwrap_or_default(),
                    "kernel": true,
                });
                if let Some(targets) = patch["targets"].as_array() {
                    args["targets"] = json!(targets);
                }
                call(&agent, "hardware_patch_build", args).await?
            }
            Err(why) => Err(why),
        };
        if let Ok(built) = rebuilt.as_ref()
            && built["ok"] == true
        {
            build = built["build"].as_str().unwrap_or("").to_owned();
            proposed = call(&agent, "hardware_patch_propose", json!({ "build": build })).await?;
        }
    }
    let patch = patch;
    match proposed {
        Ok(set_up) => {
            agent
                .answer(
                    "Restart to try the patch",
                    &format!(
                        "Kernel patch {} is set up: {}. Restart to try it. If the computer doesn't start, or crashes or hangs before a day of use, it goes back by itself.",
                        set_up["id"].as_str().unwrap_or(""),
                        patch["summary"].as_str().unwrap_or("")
                    ),
                )
                .await?;
        }
        Err(why) => {
            agent
                .answer(
                    "Not set up",
                    &format!("The patch built, but it wasn't set up: {why}"),
                )
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0057_edits_become_a_diff_that_patch_takes() {
        let before =
            "a\nb\nc\nstatic int\nglass_resume(device_t dev)\n{\n\treturn (0);\n}\nx\ny\nz\nw\n";
        let after = apply_edits(
            "sys/dev/glass/glass.c",
            before,
            &[(
                "{\n\treturn (0);".into(),
                "{\n\tglass_reinit(device_get_softc(dev));\n\treturn (0);".into(),
            )],
        )
        .unwrap();
        let d = diff("sys/dev/glass/glass.c", before, &after);
        assert_eq!(
            d,
            "--- a/sys/dev/glass/glass.c\n+++ b/sys/dev/glass/glass.c\n@@ -4,6 +4,7 @@\n \
             static int\n glass_resume(device_t dev)\n {\n+\tglass_reinit(device_get_softc(dev));\n \
             \treturn (0);\n }\n x\n"
        );
        // An edit must name its place exactly once.
        assert!(
            apply_edits("f", before, &[("nowhere".into(), "x".into())])
                .unwrap_err()
                .contains("isn't in the file")
        );
        assert!(
            apply_edits("f", "a\na\n", &[("a".into(), "b".into())])
                .unwrap_err()
                .contains("2 times")
        );
    }

    #[test]
    fn adr0057_a_diff_at_the_end_of_a_file_without_a_newline() {
        let d = diff("sys/x.c", "a\nb", "a\nc");
        assert!(
            d.contains("-b\n\\ No newline at end of file\n+c\n\\ No newline at end of file\n"),
            "{d}"
        );
    }

    #[test]
    fn adr0057_the_skill_and_its_evals_check_out() {
        let bundle = glass_managed::skills::Bundle {
            files: [
                ("SKILL.md".to_owned(), SKILL.to_owned()),
                (
                    "evals.json".to_owned(),
                    include_str!("../skill/evals.json").to_owned(),
                ),
            ]
            .into(),
        };
        let checked = bundle.check().unwrap();
        assert_eq!(checked.name, "kernel-patch");
        assert_eq!(checked.evals.cases.len(), 5);
    }

    #[test]
    fn adr0057_a_model_s_answer_is_its_json_object() {
        assert_eq!(
            object("Here: {\"no_patch\": \"firmware\"} done").unwrap()["no_patch"],
            "firmware"
        );
        assert!(object("no json").is_none());
    }
}
