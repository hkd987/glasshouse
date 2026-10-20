//! The skill-evals agent (ADR 0034 §4): runs a skill's evals and reports
//! the scores, so a change to a skill shows whether it is better.
//!
//! Its prompt names what to run: a skill (`cv-…`), the skill and a draft
//! of a change to it (`cv-… draft <id>`), or a new skill's draft
//! (`draft <id>`). A skill and a draft run in one task, so their scores
//! compare: the same cases, the same model, the same run. The cases are
//! the skill's own, and only those the draft adds; a draft can't rewrite a
//! case to pass it.
//!
//! Each case's prompt goes to `model_complete` with the skill as its
//! instructions, `runs` times, and is graded; a rubric grader asks the
//! model again. Everything goes through the task socket: the gateway
//! routes by the skill's label, and every call is audited. A refused model
//! call ends the run (scores without answers mean nothing); the scores go
//! to `skill_results`, kept on this computer.

use std::process::ExitCode;

use anyhow::{Context, bail};
use glass_agent_sdk::Agent;
use glass_managed::skills::{Bundle, Evals, Grader, rubric_passed, rubric_request};
use serde_json::{Value, json};

/// Recorded with the scores.
const RUNNER: &str = concat!("skill-evals ", env!("CARGO_PKG_VERSION"));
/// The most an answer may run to.
const ANSWER_TOKENS: u32 = 1024;
const RUBRIC_TOKENS: u32 = 256;

/// One thing to run.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    /// A skill's latest version.
    Version { skill: String },
    /// A draft on this computer.
    Draft { id: String },
}

/// What the prompt asks for: a skill, a draft, or both.
fn targets(prompt: &str) -> Vec<Target> {
    let words: Vec<&str> = prompt.split_whitespace().collect();
    let skill = words
        .iter()
        .find(|w| w.starts_with("cv-") && w.len() == 35)
        .map(|w| Target::Version {
            skill: (*w).to_owned(),
        });
    let draft = words
        .windows(2)
        .find(|w| w[0].eq_ignore_ascii_case("draft") && w[1].len() == 64)
        .map(|w| Target::Draft {
            id: w[1].to_owned(),
        });
    skill.into_iter().chain(draft).collect()
}

/// A target's bundle and, for a version, its number.
struct Opened {
    target: Target,
    bundle: Bundle,
    version: Option<u64>,
    label: String,
}

async fn open(agent: &Agent, skill: Option<&str>, target: &Target) -> anyhow::Result<Opened> {
    let input = match target {
        Target::Version { skill } => json!({ "skill": skill }),
        Target::Draft { id } => match skill {
            Some(skill) => json!({ "skill": skill, "draft": id }),
            None => json!({ "draft": id }),
        },
    };
    let read = agent.call("skill_read", input).await?;
    if read.is_error {
        bail!("{}", read.text());
    }
    let data = read.structured_content.unwrap_or(Value::Null);
    let bundle: Bundle = serde_json::from_value(json!({ "files": data["files"] }))
        .context("reading the skill's files")?;
    let version = matches!(target, Target::Version { .. })
        .then(|| data["versions"].as_array()?.last()?["number"].as_u64())
        .flatten();
    let label = match target {
        Target::Version { .. } => format!("v{}", version.unwrap_or(0)),
        Target::Draft { .. } => "the draft".to_owned(),
    };
    Ok(Opened {
        target: target.clone(),
        bundle,
        version,
        label,
    })
}

/// One model call: its answer and the model that gave it.
async fn ask(
    agent: &Agent,
    system: String,
    user: String,
    max_tokens: u32,
) -> anyhow::Result<(String, String)> {
    let result = agent
        .call(
            "model_complete",
            json!({
                "system": system,
                "messages": [{ "role": "user", "content": user }],
                "max_tokens": max_tokens,
            }),
        )
        .await?;
    if result.is_error {
        bail!("no model could run the evals: {}", result.text());
    }
    let model = result
        .structured_content
        .as_ref()
        .and_then(|d| d["model"].as_str())
        .unwrap_or("unknown")
        .to_owned();
    Ok((result.text(), model))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-skill-evals: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let agent = Agent::from_env()
        .await
        .context("connecting to the task socket")?;
    eprintln!("task {}: {}", agent.task_id, agent.prompt);
    let targets = targets(&agent.prompt);
    if targets.is_empty() {
        bail!("say which skill to run: its id, and \"draft <id>\" for a change to it");
    }
    let skill = targets.iter().find_map(|t| match t {
        Target::Version { skill } => Some(skill.clone()),
        Target::Draft { .. } => None,
    });
    agent.progress("Reading the skill", None, None).await?;
    let mut opened = Vec::new();
    for target in &targets {
        opened.push(open(&agent, skill.as_deref(), target).await?);
    }
    // The first target's cases, and only those the others add.
    let mut evals: Option<Evals> = None;
    for o in &opened {
        let own = o
            .bundle
            .check()
            .map_err(|e| anyhow::anyhow!("{} doesn't check out: {e}", o.label))?
            .evals;
        evals = Some(match evals {
            None => own,
            Some(base) => base.with_new_cases(&own),
        });
    }
    let evals = evals.context("nothing to run")?;
    let total = evals.calls() * opened.len() as u32;
    let mut done = 0;
    let mut summaries = Vec::new();
    for o in &opened {
        let mut model = None;
        let mut cases = Vec::new();
        for case in &evals.cases {
            let (system, user) = o.bundle.case_request(case);
            let mut passed = 0;
            for _ in 0..evals.runs {
                agent
                    .progress(
                        &format!("Running {} on {}", case.id, o.label),
                        Some(done),
                        Some(total),
                    )
                    .await?;
                let (answer, answered_by) =
                    ask(&agent, system.clone(), user.clone(), ANSWER_TOKENS).await?;
                done += 1;
                model.get_or_insert(answered_by);
                let mut ok = true;
                for grader in &case.graders {
                    let pass = match grader.grade(&answer) {
                        Some(pass) => pass,
                        None => {
                            let Grader::Rubric { criteria } = grader else {
                                continue;
                            };
                            let (system, user) = rubric_request(criteria, &case.prompt, &answer);
                            let (verdict, _) = ask(&agent, system, user, RUBRIC_TOKENS).await?;
                            done += 1;
                            rubric_passed(&verdict)
                        }
                    };
                    ok &= pass;
                }
                if ok {
                    passed += 1;
                }
            }
            cases.push(json!({ "id": case.id, "passed": passed, "runs": evals.runs }));
        }
        let mut report = json!({
            "model": model.unwrap_or_else(|| "unknown".into()),
            "runner": RUNNER,
            "cases": cases,
        });
        match &o.target {
            Target::Version { skill } => {
                report["skill"] = json!(skill);
                if let Some(v) = o.version {
                    report["version"] = json!(v);
                }
            }
            Target::Draft { id } => report["draft"] = json!(id),
        }
        let kept = agent.call("skill_results", report).await?;
        if kept.is_error {
            bail!("the results weren't kept: {}", kept.text());
        }
        let summary = kept
            .structured_content
            .as_ref()
            .and_then(|d| d["summary"].as_str())
            .unwrap_or_default()
            .to_owned();
        summaries.push(format!("{}: {summary}", o.label));
    }
    // The last caption is the tile's outcome line.
    agent
        .progress(&summaries.join("; "), Some(total), Some(total))
        .await?;
    eprintln!("{}", summaries.join("; "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sk3_the_prompt_names_a_skill_a_draft_or_both() {
        let skill = format!("cv-{}", "a".repeat(32));
        let draft = "b".repeat(64);
        assert_eq!(
            targets(&format!("Run {skill} draft {draft}")),
            [
                Target::Version {
                    skill: skill.clone()
                },
                Target::Draft { id: draft.clone() }
            ]
        );
        assert_eq!(
            targets(&format!("draft {draft}")),
            [Target::Draft { id: draft }]
        );
        assert_eq!(targets(&skill), [Target::Version { skill }]);
        assert!(targets("evaluate my refunds skill").is_empty());
    }
}
