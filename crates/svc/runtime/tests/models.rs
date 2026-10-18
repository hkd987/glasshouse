//! Model calls through the pipeline and the gateway (ADR 0018), with the
//! scripted provider: allowed calls, routing by the labels of what the task
//! has read (DP-2), redaction both ways (DP-3), the audit record (DP-5) and
//! failover (MP-8). The test plays the agent over the task socket.

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use glass_api::{LayerChange, TaskRequest};
use glass_audit::{AuditRecord, Outcome};
use glass_rpc::Client;
use glass_rpc::mcp::{CallToolResult, ListToolsResult};
use glass_runtime::RuntimeError;
use glass_types::{TaskId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, harness, harness_with, tool};

/// A model-backed agent, and models for public and internal data only.
/// Internal calls try a provider that is always rate limited first.
const MODELS: &str = r#"
[[group]]
name = "writers"
members = ["{me}"]
agents = ["writer"]

[[agent]]
id = "writer"
allowed_actions = ["files.*", "model.*"]

[models]
default = "echo"
max_output_tokens = 1000

[models.routes]
internal = ["flaky", "echo"]

[models.redact]
pii = ["email"]

[[models.provider]]
id = "scripted"
kind = "scripted"
name = "Scripted"

[[models.provider]]
id = "down"
kind = "scripted"
name = "Backup"

[[models.model]]
id = "echo"
provider = "scripted"
model = "echo-1"
name = "Echo"
usd_per_mtok_in = 3.0
usd_per_mtok_out = 15.0

[[models.model]]
id = "flaky"
provider = "down"
model = "flaky-1"
name = "Flaky"
"#;

/// [`MODELS`] where unlabeled data is public, models take public data
/// only, and the Reports folder is labeled internal.
fn labeled() -> String {
    MODELS
        .replace("internal = [\"flaky\", \"echo\"]", "internal = []")
        .replace(
            "default = \"echo\"",
            "default = \"echo\"\nunlabeled = \"public\"",
        )
        + "\n[[label]]\npath = \"{home}/Reports\"\nlabel = \"internal\"\n"
}

// AWS's documentation example key, not a real one.
const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

fn writer_request(h: &Harness, read: &[&str]) -> TaskRequest {
    let mut request = h.request(read, &[]);
    request.agent = "writer".parse().unwrap();
    request.prompt = "Draft a reply to the vendor".into();
    request
}

async fn start(h: &Harness, read: &[&str]) -> (TaskId, Client) {
    let task = h.rt.start(&h.me, writer_request(h, read)).unwrap().id;
    let agent = h.agent(task).await;
    (task, agent)
}

fn ask(text: &str) -> Value {
    json!({
        "messages": [{"role": "user", "content": text}],
        "max_tokens": 200,
        "system": "Reply in one line."
    })
}

fn model_records(h: &Harness, task: TaskId) -> Vec<AuditRecord> {
    glass_audit::read(&h.audit_path(), |r| {
        r.event.task == Some(task)
            && r.event.action.as_ref().map(|a| a.as_str()) == Some("model.complete")
    })
    .unwrap()
}

#[tokio::test]
async fn dp1_model_calls_are_an_action_on_the_task_socket() {
    let h = harness_with(MODELS);
    let (_, agent) = start(&h, &["Invoices"]).await;
    let tools: ListToolsResult = agent.call("tools/list", Value::Null).await.unwrap();
    let model = tools
        .tools
        .iter()
        .find(|t| t.name == "model_complete")
        .expect("the writer may call models");
    let hints = model.annotations.clone().unwrap();
    assert_eq!(hints.read_only_hint, Some(false));
    assert_eq!(hints.open_world_hint, Some(true), "data leaves the machine");

    // Scripted agents don't get the tool, or a word about it.
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = h.agent(task).await;
    let tools: ListToolsResult = agent.call("tools/list", Value::Null).await.unwrap();
    assert!(!tools.tools.iter().any(|t| t.name == "model_complete"));
}

#[tokio::test]
async fn dp3_secrets_and_pii_are_redacted_before_leaving_and_pii_comes_back() {
    let h = harness_with(MODELS);
    let (_, agent) = start(&h, &["Invoices"]).await;
    let reply = tool(
        &agent,
        "model_complete",
        ask(&format!(
            "Tell ana@example.com that key {AWS_KEY} was rotated"
        )),
    )
    .await;
    assert!(!reply.is_error, "{}", reply.text());

    // What left the machine.
    let seen = h.model.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].messages[0].content,
        "Tell [redacted:email#1] that key [redacted:aws-access-key#1] was rotated"
    );
    assert_eq!(seen[0].system.as_deref(), Some("Reply in one line."));
    assert_eq!(seen[0].model, "echo-1");

    // What the agent got back: PII restored, the secret not.
    let text = reply.text();
    assert!(
        text.contains("Tell ana@example.com that key [redacted:aws-access-key#1] was rotated"),
        "{text}"
    );
    assert!(!text.contains(AWS_KEY));
    let data = reply.structured_content.unwrap();
    assert_eq!(data["model"], "echo");
    assert_eq!(data["label"], "internal");
    assert_eq!(data["redacted"]["email"], 1);
}

#[tokio::test]
async fn dp5_every_call_is_audited_with_sizes_and_hashes_not_text() {
    let h = harness_with(MODELS);
    let (task, agent) = start(&h, &["Invoices"]).await;
    let secret_prompt = format!("Summarize for ana@example.com, key {AWS_KEY}");
    let reply = tool(&agent, "model_complete", ask(&secret_prompt)).await;
    assert!(!reply.is_error, "{}", reply.text());

    let records = model_records(&h, task);
    assert_eq!(records.len(), 1);
    let event = &records[0].event;
    assert_eq!(event.outcome, Some(Outcome::Completed));
    assert_eq!(event.agent.as_ref().unwrap().as_str(), "writer");
    assert_eq!(event.user.as_deref(), Some(h.me.name.as_str()));
    let decision = event.decision.as_ref().unwrap();
    assert_eq!(decision.tier, Tier::Auto);
    assert!(
        event.summary.starts_with("Ask Echo via Scripted"),
        "{}",
        event.summary
    );

    let m = &event.detail["model"];
    assert_eq!(m["model"], "echo");
    assert_eq!(m["provider"], "scripted");
    assert_eq!(m["provider_model"], "echo-1");
    assert_eq!(m["label"], "internal");
    assert_eq!(m["route"], "model route for internal data");
    assert!(m["input_tokens"].as_u64().unwrap() > 0);
    assert!(m["output_tokens"].as_u64().unwrap() > 0);
    assert!(m["cost_usd"].as_f64().unwrap() > 0.0);
    assert_eq!(m["redacted"]["aws-access-key"], 1);
    assert_eq!(m["redacted"]["email"], 1);
    assert_eq!(m["request"]["sha256"].as_str().unwrap().len(), 64);
    assert!(m["response"]["chars"].as_u64().unwrap() > 0);
    assert!(m.get("prompt").is_none(), "no prompt text by default");

    // Neither the secret nor the address is anywhere in the log.
    let log = std::fs::read_to_string(h.audit_path()).unwrap();
    assert!(!log.contains(AWS_KEY) && !log.contains("ana@example.com"));

    // The tile feed shows the call like any action.
    let detail = h.rt.get(&h.me, task).unwrap();
    let line = detail.view.feed.last().unwrap();
    assert_eq!(line.verb, "ask");
    assert_eq!(line.target, "Echo via Scripted");
}

#[tokio::test]
async fn mp8_an_outage_fails_over_to_the_next_model_and_is_logged() {
    let h = harness_with(MODELS);
    let (task, agent) = start(&h, &["Invoices"]).await;
    let reply = tool(&agent, "model_complete", ask("hello")).await;
    assert!(!reply.is_error, "{}", reply.text());
    let records = model_records(&h, task);
    let failed = &records[0].event.detail["model"]["failed_over"];
    assert_eq!(failed[0]["model"], "flaky");
    assert!(
        failed[0]["error"]
            .as_str()
            .unwrap()
            .contains("rate limited")
    );
    assert_eq!(records[0].event.detail["model"]["model"], "echo");
}

#[tokio::test]
async fn dp2_reading_labeled_data_routes_later_calls_by_its_label() {
    let h = harness_with(&labeled());
    let (task, agent) = start(&h, &["Invoices", "Reports"]).await;
    // Before it reads anything labeled, its calls carry public data.
    let first = tool(&agent, "model_complete", ask("Plan the reply")).await;
    assert!(!first.is_error, "{}", first.text());
    assert_eq!(first.structured_content.unwrap()["label"], "public");

    // The writer may open internal files...
    let read = tool(
        &agent,
        "files_read",
        json!({"path": "Reports/existing.txt"}),
    )
    .await;
    assert!(!read.is_error, "{}", read.text());
    // ...but no model is approved for internal data, so from now on its
    // calls can't leave (DP-11: what it read is in what it would send).
    let refused = tool(&agent, "model_complete", ask("Summarize the report")).await;
    assert!(refused.is_error);
    let text = refused.text();
    assert!(
        text.starts_with("Not sent: no model is approved for internal data"),
        "{text}"
    );
    assert_eq!(h.model.seen().len(), 1, "nothing more left the machine");

    let records = model_records(&h, task);
    let blocked = records.last().unwrap();
    assert_eq!(blocked.event.outcome, Some(Outcome::Blocked));
    let decision = blocked.event.decision.as_ref().unwrap();
    assert_eq!(decision.tier, Tier::Never);
    assert_eq!(decision.rule, "model route for internal data");
    assert_eq!(blocked.event.detail["model"]["label"], "internal");
    let detail = h.rt.get(&h.me, task).unwrap();
    assert!(
        detail
            .activity
            .iter()
            .any(|a| a.text.contains("no model is approved for internal data"))
    );
}

#[tokio::test]
async fn dp2_restricted_and_over_the_limit_data_never_taints_a_task() {
    // A blocked read reads nothing, so it can't raise the task's label.
    let h = harness_with(MODELS);
    let (_, agent) = start(&h, &["Invoices", "Finance"]).await;
    let read = tool(&agent, "files_read", json!({"path": "Finance/q3.csv"})).await;
    assert!(read.is_error, "confidential is above the writer's limit");
    let reply = tool(&agent, "model_complete", ask("hello")).await;
    assert!(!reply.is_error, "{}", reply.text());
    assert_eq!(reply.structured_content.unwrap()["label"], "internal");
}

#[tokio::test]
async fn dp2_a_model_outside_the_allowlist_is_refused_readably() {
    let h = harness_with(MODELS);
    let (task, agent) = start(&h, &["Invoices"]).await;
    let mut args = ask("hello");
    args["model"] = json!("gpt-x");
    let refused = tool(&agent, "model_complete", args).await;
    assert!(refused.is_error);
    assert!(
        refused
            .text()
            .contains("gpt-x isn't approved for internal data for this agent; leave model out, or use flaky or echo"),
        "{}",
        refused.text()
    );
    let records = model_records(&h, task);
    assert_eq!(records[0].event.outcome, Some(Outcome::Blocked));
    assert_eq!(records[0].event.detail["model"]["requested"], "gpt-x");
    assert!(h.model.seen().is_empty());

    // Asking for an approved model puts it first.
    let mut args = ask("hello");
    args["model"] = json!("echo");
    let reply = tool(&agent, "model_complete", args).await;
    assert!(!reply.is_error);
    let records = model_records(&h, task);
    assert!(
        records.last().unwrap().event.detail["model"]
            .get("failed_over")
            .is_none(),
        "echo went first, so flaky was never tried"
    );
}

#[tokio::test]
async fn dp2_policy_limits_how_much_a_call_may_ask_for() {
    let h = harness_with(MODELS);
    let (_, agent) = start(&h, &["Invoices"]).await;
    let mut args = ask("hello");
    args["max_tokens"] = json!(5000);
    let refused: CallToolResult = tool(&agent, "model_complete", args).await;
    assert!(refused.is_error);
    assert!(refused.text().contains("max_tokens can be at most 1000"));
}

#[tokio::test]
async fn dp2_the_handoff_card_names_where_the_tasks_data_would_go() {
    let h = harness_with(MODELS);
    let card =
        h.rt.plan(&h.me, &writer_request(&h, &["Invoices"]))
            .unwrap();
    assert_eq!(
        card.model, "Flaky via Backup",
        "the first model on the internal route"
    );
    // Finance is confidential, above what the writer may open: its files
    // never reach a model, so the card still routes internal data.
    let card =
        h.rt.plan(&h.me, &writer_request(&h, &["Invoices", "Finance"]))
            .unwrap();
    assert_eq!(card.model, "Flaky via Backup");
    let scripted = h.rt.plan(&h.me, &h.request(&["Invoices"], &[])).unwrap();
    assert_eq!(scripted.model, "No model (scripted agent)");

    let h = harness_with(&labeled());
    let card =
        h.rt.plan(&h.me, &writer_request(&h, &["Invoices"]))
            .unwrap();
    assert_eq!(card.model, "Echo via Scripted");
    let card =
        h.rt.plan(&h.me, &writer_request(&h, &["Invoices", "Reports"]))
            .unwrap();
    assert_eq!(card.model, "No approved model for internal data");
}

#[tokio::test]
async fn dp2_a_layer_cant_widen_the_model_allowlist_or_loosen_model_calls() {
    let extra = format!("{MODELS}\n[org.tiers]\n");
    // Put model calls at Ask for the writers group, so a layer could try to
    // loosen them.
    let extra = extra.replace(
        "agents = [\"writer\"]\n",
        "agents = [\"writer\"]\n[group.tiers]\n\"model.complete\" = \"ask\"\n",
    );
    let extra = extra.replace("\n[org.tiers]\n", "\n");
    let h = harness_with(&extra);
    let writer = "writer".parse().unwrap();
    let change = |settings: BTreeMap<String, Value>, restrict: Vec<(String, Tier)>| LayerChange {
        agent: "writer".parse().unwrap(),
        settings,
        restrict,
        note: String::new(),
    };
    let loosen = h.rt.layer_set(
        &h.me,
        change(BTreeMap::new(), vec![("model.complete".into(), Tier::Auto)]),
    );
    assert!(
        matches!(&loosen, Err(RuntimeError::Invalid(m)) if m.contains("Permissions only narrow")),
        "{loosen:?}"
    );
    // A layer may name any model as the agent's setting; policy still
    // decides at every call.
    let settings = BTreeMap::from([("model".to_owned(), json!("gpt-x"))]);
    h.rt.layer_set(&h.me, change(settings, Vec::new())).unwrap();
    assert_eq!(h.rt.layer_get(&h.me, &writer).unwrap().points.len(), 1);
    let (_, agent) = start(&h, &["Invoices"]).await;
    let mut args = ask("hello");
    args["model"] = json!("gpt-x");
    let refused = tool(&agent, "model_complete", args).await;
    assert!(refused.is_error);
    assert!(
        refused.text().contains("gpt-x isn't approved"),
        "{}",
        refused.text()
    );
    assert!(h.model.seen().is_empty());
}
