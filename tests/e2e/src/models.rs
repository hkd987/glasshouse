//! The model gateway end to end (DP-1, DP-2, DP-3, DP-5, ADR 0018), with the
//! scripted provider, so nothing leaves the VM.
//!
//! It needs the test policy `glass-e2e models-policy` prints: the default
//! policy plus a scripted model, which the attended `claude-code` entry and
//! the IT agent may call. The suite plays `claude-code` over its task
//! socket, as `harness` does; the IT agent calls the same tool from its
//! jail (AE-4). `run-vm.sh` installs the policy, restarts glassd, runs this
//! scenario and puts things back.

use std::time::Duration;

use anyhow::{Context, bail, ensure};
use glass_api::{HandoffCard, ScopeRequest, TaskRequest, TaskView, TileState, methods};
use glass_audit::{EventKind, Outcome};
use glass_policy::Policy;
use glass_policy::document::{Models, PolicyDocument};
use glass_rpc::mcp::{CallToolResult, ListToolsResult};
use serde_json::{Value, json};

use crate::harness::Harness;

/// AWS's documentation example key, not a real one.
const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

const MODELS: &str = r#"
default = "echo"

[redact]
pii = ["email"]

[[provider]]
id = "scripted"
kind = "scripted"
name = "Scripted (no network)"

[[model]]
id = "echo"
provider = "scripted"
model = "echo-1"
name = "Echo"
usd_per_mtok_in = 3.0
usd_per_mtok_out = 15.0
"#;

/// The default policy, a scripted model, and model calls for the attended
/// `claude-code` entry and the IT agent.
pub fn policy() -> anyhow::Result<String> {
    let mut doc: PolicyDocument = toml::from_str(glass_policy::DEFAULT_POLICY)?;
    for id in ["claude-code", "it-agent"] {
        let agent = doc
            .agents
            .iter_mut()
            .find(|a| a.id.as_str() == id)
            .with_context(|| format!("the default policy has a {id} agent"))?;
        agent.allowed_actions.push(
            "model.complete"
                .parse()
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        );
    }
    doc.models = toml::from_str::<Models>(MODELS)?;
    let text = format!(
        "# Written by glass-e2e models-policy for the model gateway scenario.\n{}",
        toml::to_string(&doc)?
    );
    Policy::from_toml(&text)?;
    Ok(text)
}

async fn tool(
    client: &glass_rpc::Client,
    name: &str,
    arguments: Value,
) -> anyhow::Result<CallToolResult> {
    client
        .call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{name}: {e}"))
}

pub async fn models(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("models")?;
    let request = TaskRequest {
        agent: "claude-code".parse().map_err(|e| anyhow::anyhow!("{e}"))?,
        prompt: "Draft a reply to a vendor with a model's help".into(),
        scope: ScopeRequest {
            read: vec![ws.invoices.clone()],
            write: Vec::new(),
            restrict: Vec::new(),
        },
        title: None,
    };
    // DP-2: the card says where this task's data would go.
    let card: HandoffCard = h.call(methods::TASK_PLAN, &request).await?;
    ensure!(
        card.model == "Echo via Scripted (no network)",
        "card model line: {:?} (is the models-policy installed?)",
        card.model
    );

    let view: TaskView = h.call(methods::TASK_START, &request).await?;
    let socket = view
        .driver_socket
        .clone()
        .context("a socket to connect to")?;
    h.wait_for(view.id, TileState::Working, Duration::from_secs(60))
        .await?;
    let client = glass_rpc::Client::connect(&socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let init: Value = client
        .call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "glass-e2e", "version": "0"}}),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    ensure!(
        init["instructions"]
            .as_str()
            .unwrap_or_default()
            .contains("model_complete"),
        "the instructions name the model tool"
    );
    let tools: ListToolsResult = client
        .call("tools/list", Value::Null)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    ensure!(
        tools.tools.iter().any(|t| t.name == "model_complete"),
        "model_complete is listed"
    );

    // DP-3: the secret and the address are placeholders on the way out; the
    // address comes back, the key doesn't.
    let reply = tool(
        &client,
        "model_complete",
        json!({
            "messages": [{"role": "user", "content": format!("Tell ana@example.com that key {AWS_KEY} was rotated")}],
            "max_tokens": 200
        }),
    )
    .await?;
    ensure!(!reply.is_error, "{}", reply.text());
    let text = reply.text();
    ensure!(
        text.contains("Tell ana@example.com that key [redacted:aws-access-key#1] was rotated"),
        "{text}"
    );
    ensure!(!text.contains(AWS_KEY), "the key never comes back");

    // DP-2: a model outside the allowlist is a refusal the agent can read.
    let refused = tool(
        &client,
        "model_complete",
        json!({
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 50,
            "model": "not-approved"
        }),
    )
    .await?;
    ensure!(
        refused.is_error && refused.text().contains("isn't approved"),
        "{}",
        refused.text()
    );

    let done = h.act(methods::TASK_HAND_BACK, view.id).await?;
    ensure!(done.state == TileState::Done, "{:?}", done.state);
    finished(h, view).await?;

    // DP-5: both calls are on the record, with no prompt text.
    let records = h.audit(done.id).await?;
    let calls: Vec<_> = records
        .iter()
        .filter(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("model.complete"))
        .collect();
    ensure!(
        calls.len() == 2,
        "{} model calls on the record",
        calls.len()
    );
    let sent = &calls[0].event;
    ensure!(
        sent.outcome == Some(Outcome::Completed),
        "{:?}",
        sent.outcome
    );
    let m = &sent.detail["model"];
    ensure!(m["provider"] == "scripted" && m["model"] == "echo", "{m}");
    ensure!(m["label"] == "internal", "{m}");
    ensure!(m["input_tokens"].as_u64().unwrap_or(0) > 0, "{m}");
    ensure!(m["cost_usd"].as_f64().is_some(), "{m}");
    ensure!(
        m["redacted"]["aws-access-key"] == 1 && m["redacted"]["email"] == 1,
        "{m}"
    );
    ensure!(
        m["request"]["sha256"].as_str().map(str::len) == Some(64),
        "{m}"
    );
    ensure!(
        calls[1].event.outcome == Some(Outcome::Blocked),
        "{:?}",
        calls[1].event.outcome
    );
    let log = serde_json::to_string(&records)?;
    ensure!(
        !log.contains(AWS_KEY) && !log.contains("ana@example.com"),
        "neither the key nor the address is in the audit log"
    );
    it_agent(h).await?;
    desktop_agent(h, &ws).await
}

/// AE-3, ADR 0041: the desktop agent, in its jail, offers the model its
/// tools; the scripted model asks for files_list; the agent makes the call,
/// policy decides it, and the model answers from what it returned.
async fn desktop_agent(h: &Harness, ws: &crate::harness::Workspace) -> anyhow::Result<()> {
    let task = h
        .start(
            "desktop-agent",
            r#"What's in my folder? Use files_list {"path": ""}"#,
            &[ws.invoices.as_path()],
            &[],
        )
        .await?;
    h.wait_for(task, TileState::Done, Duration::from_secs(60))
        .await?;
    let records = h.audit(task).await?;
    let models: Vec<_> = records
        .iter()
        .filter(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("model.complete"))
        .collect();
    ensure!(models.len() == 2, "{} model calls", models.len());
    let first = &models[0].event.detail["model"];
    ensure!(
        first["tools_called"] == json!(["files_list"]) && first["tools_offered"].as_u64() > Some(0),
        "{first}"
    );
    let listed = records
        .iter()
        .find(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("files.list"))
        .context("the agent made the call the model proposed")?;
    ensure!(
        listed.event.outcome == Some(Outcome::Completed),
        "{:?}",
        listed.event.outcome
    );
    let detail = h.detail(task).await?;
    let answer = detail.answer.unwrap_or_default();
    ensure!(answer.contains("The tools returned"), "{answer}");
    Ok(())
}

/// AE-4: the IT agent, in its jail, asks the model what the request is
/// and, with nothing it can fix, for an assessment for IT's ticket. The
/// scripted reply isn't a sorting, so keywords pick the checks.
async fn it_agent(h: &Harness) -> anyhow::Result<()> {
    let task = h
        .start("it-agent", "The printer won't print", &[], &[])
        .await?;
    h.wait_for(task, TileState::NeedsYou, Duration::from_secs(60))
        .await?;
    let ask = h.the_ask(task).await?;
    ensure!(
        ask.summary.starts_with("Open a help desk ticket"),
        "{}",
        ask.summary
    );
    h.approve_once(&ask).await?;
    h.wait_for(task, TileState::Done, Duration::from_secs(30))
        .await?;
    let calls: Vec<_> = h
        .audit(task)
        .await?
        .into_iter()
        .filter(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("model.complete"))
        .collect();
    ensure!(
        calls.len() == 2,
        "{} IT agent model calls on the record",
        calls.len()
    );
    for call in &calls {
        let m = &call.event.detail["model"];
        ensure!(
            call.event.outcome == Some(Outcome::Completed) && m["provider"] == "scripted",
            "{:?} {m}",
            call.event.outcome
        );
    }
    Ok(())
}

async fn finished(h: &Harness, view: TaskView) -> anyhow::Result<()> {
    for _ in 0..150 {
        if h.audit(view.id)
            .await?
            .iter()
            .any(|r| r.event.kind == EventKind::TaskFinished)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    bail!("the task never finished")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_test_policy_is_valid() {
        let text = super::policy().unwrap();
        assert!(text.contains("model.complete"));
        assert!(text.contains("Scripted (no network)"));
    }
}
