//! The IT self-service agent (AE-4): "My VPN won't connect", "my disk is
//! full", "everything is slow". It runs the checks that fit the request,
//! applies fixes from IT's approved list, and when it can't fix something it
//! opens a help desk ticket with what it found attached.
//!
//! Scripted for now: keywords pick the checks, as a stand-in until the
//! harness bundles and model gateway land (ADR 0010). Everything it does
//! goes through the `system.*` and `helpdesk.*` actions, so tiers, the
//! approved list, snapshots and the audit log apply exactly as they will for
//! the model-backed agent. A declined or blocked step is reported, never
//! retried.

use std::process::ExitCode;

use anyhow::Context;
use glass_agent_sdk::{Agent, CallToolResult};
use serde_json::{Map, Value, json};

/// The VPN client's rc.d service.
const VPN_SERVICE: &str = "openvpn";

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

    // Explicit request: restart a service from IT's list.
    if let Some(service) = service_named(&prompt, &agent) {
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

    let areas = areas(&prompt);
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

    if problem_without_fix(&prompt.to_lowercase()) {
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
    agent
        .progress("Asking to open a help desk ticket", None, None)
        .await?;
    let ticket = agent
        .call(
            "helpdesk_ticket_create",
            json!({
                "summary": summary_line(&prompt),
                "details": format!(
                    "Reported: {prompt}\n\nFound:\n{}\n\nFixed here:\n{}",
                    problems.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n"),
                    if fixed.is_empty() { "- nothing".to_owned() } else {
                        fixed.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
                    }
                ),
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
    fn ticket_summaries_are_one_short_line() {
        assert_eq!(
            summary_line("Printer jammed\nmore detail"),
            "Printer jammed"
        );
        assert_eq!(summary_line("x".repeat(300).as_str()).len(), 120);
    }
}
