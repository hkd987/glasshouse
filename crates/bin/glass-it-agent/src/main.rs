//! The IT self-service agent (AE-4): "My VPN won't connect", "my disk is
//! full", "everything is slow". It runs the checks that fit the request,
//! applies fixes from IT's approved list, and when it can't fix something it
//! opens a help desk ticket with what it found attached.
//!
//! When policy lets it ask AI models (`model.complete`, ADR 0018), a model
//! reads the request and picks the checks, and writes IT an assessment in
//! any ticket it opens. Without one, or when the model is refused or answers
//! something it can't use, keywords pick the checks, as they always have.
//! Everything it does goes through the `system.*` and `helpdesk.*` actions,
//! so tiers, the approved list, snapshots and the audit log apply whoever
//! picked the step. A declined or blocked step, or a refused model call, is
//! reported, never retried.

use std::process::ExitCode;

use anyhow::Context;
use glass_agent_sdk::{Agent, CallToolResult};
use serde_json::{Map, Value, json};

/// The VPN client's rc.d service.
const VPN_SERVICE: &str = "openvpn";

/// How the model sorts a request: the same choices the keywords make. It
/// shows the answer's shape in words, not as JSON, so a reply that only
/// repeats the instructions has no object in it to take for an answer.
const TRIAGE: &str = "You sort requests sent to a computer's IT agent. \
Answer with one JSON object and nothing else. Its field areas lists the checks \
the request points to, from network (internet, Wi-Fi, VPN, websites, connecting), \
storage (disk space, caches) and performance (slowness, freezing, fans), and is \
empty when nothing fits. Its field restart is a service the person asked by name \
to restart, or null. Its field no_fix is true for hardware and anything else none \
of the checks covers, such as printers, monitors, keyboards and cameras.";
const TRIAGE_TOKENS: u32 = 200;

/// How the model writes up a ticket for IT.
const ASSESS: &str = "You are a computer's IT agent, writing in a help desk \
ticket for IT staff. From what the person reported and what the checks found, \
say in under 120 words what is most likely wrong and what IT should try next. \
Plain words, no greeting, no sign-off.";
const ASSESS_TOKENS: u32 = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Area {
    Network,
    Storage,
    Performance,
}

impl Area {
    fn check(self) -> (&'static str, &'static str) {
        match self {
            Area::Network => ("system_network_check", "Checking the network"),
            Area::Storage => ("system_storage_check", "Checking storage"),
            Area::Performance => (
                "system_performance_check",
                "Checking what's slowing things down",
            ),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Area::Network => "network",
            Area::Storage => "storage",
            Area::Performance => "performance",
        }
    }
}

/// What the request is about. Several areas can match; none means "check
/// everything".
fn areas(prompt: &str) -> Vec<Area> {
    let p = prompt.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| p.contains(w));
    let mut out = Vec::new();
    if any(&[
        "network", "internet", "wifi", "wi-fi", "vpn", "dns", "connect", "online", "offline",
        "website",
    ]) {
        out.push(Area::Network);
    }
    if any(&["disk", "storage", "space", "full", "cache", "clean"]) {
        out.push(Area::Storage);
    }
    if any(&[
        "slow",
        "sluggish",
        "freez",
        "hang",
        "fan",
        "performance",
        "lag",
    ]) {
        out.push(Area::Performance);
    }
    if out.is_empty() && !problem_without_fix(&p) {
        out = vec![Area::Network, Area::Storage, Area::Performance];
    }
    out
}

impl Area {
    const ALL: [Area; 3] = [Area::Network, Area::Storage, Area::Performance];

    fn named(key: &str) -> Option<Area> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }
}

/// What the model made of the request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Triage {
    areas: Vec<Area>,
    restart: Option<String>,
    no_fix: bool,
}

/// The model's answer to [`TRIAGE`], if it is one: a JSON object, perhaps
/// with words around it, naming only checks that exist.
fn triage(reply: &str) -> Option<Triage> {
    let json = reply.get(reply.find('{')?..=reply.rfind('}')?)?;
    let value: Value = serde_json::from_str(json).ok()?;
    let mut areas = Vec::new();
    for key in value["areas"].as_array()? {
        let area = Area::named(key.as_str()?)?;
        if !areas.contains(&area) {
            areas.push(area);
        }
    }
    let restart = match &value["restart"] {
        Value::Null => None,
        Value::String(name)
            if !name.is_empty()
                && name.len() <= 64
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
        {
            Some(name.to_lowercase())
        }
        _ => return None,
    };
    Some(Triage {
        areas,
        restart,
        no_fix: value["no_fix"].as_bool().unwrap_or(false),
    })
}

/// Asks a model, if policy lets this agent. A refusal turns models off for
/// the rest of the task: it is information, not something to retry.
async fn ask(
    agent: &Agent,
    models: &mut bool,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> anyhow::Result<Option<String>> {
    if !*models {
        return Ok(None);
    }
    let reply = agent
        .call(
            "model_complete",
            json!({
                "system": system,
                "messages": [{ "role": "user", "content": user }],
                "max_tokens": max_tokens,
            }),
        )
        .await?;
    report(&reply, "model");
    if reply.is_error {
        *models = false;
        return Ok(None);
    }
    Ok(Some(reply.text()))
}

/// Things this agent has no fix for yet: straight to a ticket.
fn problem_without_fix(prompt: &str) -> bool {
    [
        "printer", "print", "monitor", "display", "keyboard", "mouse", "camera", "headset",
    ]
    .iter()
    .any(|w| prompt.contains(w))
}

/// "restart ntpd" names a service; IT's list (the tool's enum) decides.
fn service_named(prompt: &str, agent: &Agent) -> Option<String> {
    let lower = prompt.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    let at = words.iter().position(|w| *w == "restart")?;
    let named = words
        .get(at + 1)
        .filter(|w| **w != "the")
        .or(words.get(at + 2))?;
    agent
        .has_tool("system_service_restart")
        .then(|| (*named).to_owned())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-it-agent: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let agent = Agent::from_env()
        .await
        .context("connecting to the task socket")?;
    eprintln!("task {}: {}", agent.task_id, agent.prompt);
    let prompt = agent.prompt.clone();

    // A model reads the request, when policy lets this agent ask one.
    let mut models = agent.has_tool("model_complete");
    let read = if models {
        agent.progress("Reading your request", None, None).await?;
        ask(&agent, &mut models, TRIAGE, &prompt, TRIAGE_TOKENS)
            .await?
            .and_then(|reply| triage(&reply))
    } else {
        None
    };
    if models && read.is_none() {
        eprintln!("model: no usable answer; keywords pick the checks");
    }

    // Explicit request: restart a service from IT's list.
    let restart = service_named(&prompt, &agent).or_else(|| {
        read.as_ref()
            .and_then(|t| t.restart.clone())
            .filter(|_| agent.has_tool("system_service_restart"))
    });
    if let Some(service) = restart {
        agent
            .progress(&format!("Asking to restart {service}"), None, None)
            .await?;
        let result = agent
            .call("system_service_restart", json!({ "service": service }))
            .await?;
        let caption = if result.is_error {
            format!("Couldn't restart {service}: {}", result.text())
        } else {
            format!("Fixed: {}", result.text())
        };
        agent.progress(&caption, None, None).await?;
        return Ok(());
    }

    let no_fix = read
        .as_ref()
        .map_or_else(|| problem_without_fix(&prompt.to_lowercase()), |t| t.no_fix);
    let areas = match &read {
        Some(t) if t.areas.is_empty() && !t.no_fix => Area::ALL.to_vec(),
        Some(t) => t.areas.clone(),
        None => areas(&prompt),
    };
    let total = areas.len() as u32;
    let mut diagnostics = Map::new();
    let mut problems: Vec<String> = Vec::new();
    let mut fixed: Vec<String> = Vec::new();

    for (i, area) in areas.iter().enumerate() {
        let (tool, caption) = area.check();
        agent
            .progress(caption, Some(i as u32 + 1), Some(total))
            .await?;
        let result = agent.call(tool, json!({})).await?;
        if result.is_error {
            problems.push(format!(
                "{}: couldn't check ({})",
                area.key(),
                result.text()
            ));
            continue;
        }
        let data = result.structured_content.clone().unwrap_or(Value::Null);
        diagnostics.insert(area.key().into(), data.clone());
        let findings: Vec<String> = data["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f.as_str().map(str::to_owned))
            .collect();

        // The fix on IT's list for a full disk or a large cache: clean it.
        if *area == Area::Storage && data["cache_bytes"].as_u64().unwrap_or(0) >= 1024 * 1024 {
            agent
                .progress("Cleaning up cache files", None, None)
                .await?;
            let cleaned = agent.call("system_storage_clean_cache", json!({})).await?;
            report(&cleaned, "cache");
            if !cleaned.is_error {
                fixed.push(cleaned.text());
                continue;
            }
        }
        problems.extend(findings);
    }

    // VPN trouble with a working network: restart the VPN client, if IT
    // lists it and it's set up here.
    if prompt.to_lowercase().contains("vpn")
        && diagnostics
            .get("network")
            .is_some_and(|n| n["healthy"] == json!(true))
        && agent.has_tool("system_service_restart")
    {
        agent
            .progress("Asking to restart the VPN client", None, None)
            .await?;
        let restarted = agent
            .call("system_service_restart", json!({ "service": VPN_SERVICE }))
            .await?;
        report(&restarted, "vpn");
        if restarted.is_error {
            problems.push(format!(
                "Couldn't restart the VPN client: {}",
                restarted.text()
            ));
        } else {
            fixed.push(restarted.text());
        }
    }

    if no_fix {
        problems.push(format!("No fix on the approved list for: {prompt}"));
    }

    if problems.is_empty() {
        let caption = if fixed.is_empty() {
            "Everything checks out. No fix needed".to_owned()
        } else {
            format!("Fixed. {}", fixed.join(". "))
        };
        agent.progress(&caption, None, None).await?;
        return Ok(());
    }

    // Can't fix it here: hand IT everything we found.
    let mut details = format!(
        "Reported: {prompt}\n\nFound:\n{}\n\nFixed here:\n{}",
        problems
            .iter()
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n"),
        if fixed.is_empty() {
            "- nothing".to_owned()
        } else {
            fixed
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    );
    if models {
        // The model gets what the person reported and what the checks
        // found, in words: never the raw data (processes, addresses), which
        // goes to IT alone.
        agent
            .progress("Writing up what I found for IT", None, None)
            .await?;
        if let Some(assessment) = ask(&agent, &mut models, ASSESS, &details, ASSESS_TOKENS).await? {
            details.push_str(&format!(
                "\n\nThe IT agent's assessment:\n{}",
                assessment.trim()
            ));
        }
    }
    agent
        .progress("Asking to open a help desk ticket", None, None)
        .await?;
    let ticket = agent
        .call(
            "helpdesk_ticket_create",
            json!({
                "summary": summary_line(&prompt),
                "details": details,
                "diagnostics": Value::Object(diagnostics),
            }),
        )
        .await?;
    report(&ticket, "ticket");
    let caption = if ticket.is_error {
        format!(
            "Couldn't fix it, and no ticket was opened: {}",
            problems.join(". ")
        )
    } else {
        format!("{}. IT has the diagnostics", ticket.text())
    };
    agent.progress(&caption, None, None).await?;
    Ok(())
}

fn summary_line(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or(prompt).trim();
    let mut summary: String = line.chars().take(120).collect();
    if summary.is_empty() {
        summary = "Problem reported to the IT agent".into();
    }
    summary
}

fn report(result: &CallToolResult, step: &str) {
    if result.is_error {
        eprintln!("{step}: not done: {}", result.text());
    } else {
        eprintln!("{step}: {}", result.text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_pick_the_checks_that_fit() {
        assert_eq!(areas("My VPN won't connect"), [Area::Network]);
        assert_eq!(areas("my disk is full"), [Area::Storage]);
        assert_eq!(areas("everything is so slow"), [Area::Performance]);
        assert_eq!(areas("help").len(), 3, "unclear: check everything");
        assert!(
            areas("the printer won't print").is_empty(),
            "no fix: straight to a ticket"
        );
    }

    #[test]
    fn ae4_a_models_answer_picks_the_checks_when_it_names_real_ones() {
        assert_eq!(
            triage(
                r#"{"areas": ["network", "storage", "network"], "restart": null, "no_fix": false}"#
            ),
            Some(Triage {
                areas: vec![Area::Network, Area::Storage],
                restart: None,
                no_fix: false,
            })
        );
        // Words around the object, as models add.
        assert_eq!(
            triage(
                "Sure:\n```json\n{\"areas\": [], \"restart\": \"NTPD\", \"no_fix\": false}\n```"
            ),
            Some(Triage {
                areas: vec![],
                restart: Some("ntpd".into()),
                no_fix: false,
            })
        );
        assert_eq!(
            triage(r#"{"areas": [], "restart": null, "no_fix": true}"#).map(|t| t.no_fix),
            Some(true)
        );
        // Anything it can't act on exactly falls back to keywords.
        for unusable in [
            "Answered on model credits: My disk is filling up",
            r#"{"areas": ["printers"], "restart": null}"#,
            r#"{"areas": "network"}"#,
            r#"{"areas": [], "restart": "cron; reboot"}"#,
            r#"{"areas": [], "restart": 7}"#,
            "} {",
        ] {
            assert_eq!(triage(unusable), None, "{unusable}");
        }
    }

    #[test]
    fn ticket_summaries_are_one_short_line() {
        assert_eq!(
            summary_line("Printer jammed\nmore detail"),
            "Printer jammed"
        );
        assert_eq!(summary_line("x".repeat(300).as_str()).len(), 120);
    }
}
