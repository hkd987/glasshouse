//! Model calls through the pipeline and the gateway (ADR 0018), with the
//! scripted provider: allowed calls, routing by the labels of what the task
//! has read (DP-2), redaction both ways (DP-3), the audit record (DP-5) and
//! failover (MP-8). The test plays the agent over the task socket.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use glass_api::{AnswerApproval, ApprovalAnswer, LayerChange, TaskRequest};
use glass_audit::{AuditRecord, Outcome};
use glass_rpc::Client;
use glass_rpc::mcp::{CallToolResult, ListToolsResult};
use glass_runtime::RuntimeError;
use glass_types::{TaskId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness, harness_with, tool};

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

/// [`MODELS`] with a cheaper model on the internal route, where agents may
/// choose by cost (ADR 0044).
fn by_cost() -> String {
    MODELS
        .replace(
            "internal = [\"flaky\", \"echo\"]",
            "internal = [\"echo\", \"cheap\"]",
        )
        .replace(
            "max_output_tokens = 1000",
            "max_output_tokens = 1000\nby_cost = true",
        )
        + "\n[[models.model]]\nid = \"cheap\"\nprovider = \"scripted\"\nmodel = \"cheap-1\"\nname = \"Cheap\"\nusd_per_mtok_in = 0.1\nusd_per_mtok_out = 0.5\n"
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

/// GA-1 (ADR 0049): the data beside a reply is what `model.complete`
/// declares (`glass_gateway::ModelAction`), whether it's words, words with
/// details redacted, or tools the model asks for with the sealed replay.
#[tokio::test]
async fn ga1_what_each_action_returns_is_what_it_declares() {
    use glass_actions::ActionProvider;

    let h = harness_with(MODELS);
    let (_, agent) = start(&h, &["Invoices"]).await;
    let shape = glass_gateway::ModelAction
        .returns(&glass_gateway::MODEL_COMPLETE.parse().unwrap())
        .unwrap();
    let tools: ListToolsResult = agent.call("tools/list", Value::Null).await.unwrap();
    let list = tools.tools.iter().find(|t| t.name == "files_list").unwrap();
    let offered = json!([{
        "name": list.name,
        "description": list.description,
        "input_schema": list.input_schema,
    }]);
    let mut seen = Vec::new();
    for input in [
        ask("Say hello in one line"),
        ask(&format!(
            "Tell ana@example.com that key {AWS_KEY} was rotated"
        )),
        json!({
            "messages": [{"role": "user", "content": "What's in my invoices? Use files_list {\"path\": \"Invoices\"}"}],
            "max_tokens": 200,
            "tools": offered,
        }),
    ] {
        let reply = tool(&agent, "model_complete", input).await;
        assert!(!reply.is_error, "{}", reply.text());
        let mut data = reply.structured_content.unwrap();
        data.as_object_mut()
            .unwrap()
            .remove(glass_actions::AUDIT_DETAIL);
        glass_actions::shape::conforms(&data, &shape).unwrap_or_else(|e| panic!("{e}: {data}"));
        seen.push(data);
    }
    // Each optional part was there to check.
    assert_eq!(seen[1]["redacted"]["email"], 1);
    assert!(seen[2]["tool_calls"].is_array() && seen[2]["replay"].is_object());
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
async fn dp2_the_receipt_says_where_model_calls_took_the_tasks_data() {
    // The harness's providers as outside ones: the call tries Backup, then
    // Anthropic answers. Both were sent what the task read.
    let outside = MODELS
        .replace(
            "id = \"scripted\"\nkind = \"scripted\"\nname = \"Scripted\"",
            "id = \"scripted\"\nkind = \"anthropic\"\nname = \"Anthropic\"",
        )
        .replace(
            "id = \"down\"\nkind = \"scripted\"\nname = \"Backup\"",
            "id = \"down\"\nkind = \"openrouter\"\nname = \"OpenRouter\"",
        );
    assert_ne!(outside, MODELS);
    for (policy, models, outcome) in [
        (
            outside.as_str(),
            vec!["OpenRouter", "Anthropic"],
            "Nothing left the machine but its model calls to OpenRouter and Anthropic. \
             Nothing was changed.",
        ),
        // Scripted replies are made on this computer.
        (
            MODELS,
            vec![],
            "Nothing left the machine. Nothing was changed.",
        ),
    ] {
        let h = harness_with(policy);
        let (task, agent) = start(&h, &["Invoices"]).await;
        for _ in 0..2 {
            let reply = tool(&agent, "model_complete", ask("hello")).await;
            assert!(!reply.is_error, "{}", reply.text());
        }
        h.rt.stop(&h.me, task).unwrap();
        eventually(|| h.rt.get(&h.me, task).unwrap().view.receipt.is_some()).await;
        let receipt = h.rt.get(&h.me, task).unwrap().view.receipt.unwrap();
        assert_eq!(receipt.models, models);
        assert_eq!(receipt.outcome(), outcome);
    }
}

#[tokio::test]
async fn mp8_an_agent_asks_for_the_cheapest_or_the_strongest_approved_model() {
    let h = harness_with(&by_cost());
    let (task, agent) = start(&h, &["Invoices"]).await;
    for (prefer, above, answered) in [
        (Some("cheapest"), None, "cheap"),
        (Some("strongest"), None, "echo"),
        (None, None, "echo"),
        // A step up from the cheap model, and none above the top.
        (Some("cheapest"), Some("cheap"), "echo"),
        (Some("cheapest"), Some("echo"), "echo"),
    ] {
        let mut args = ask("hello");
        if let Some(prefer) = prefer {
            args["prefer"] = json!(prefer);
        }
        if let Some(above) = above {
            args["above"] = json!(above);
        }
        let reply = tool(&agent, "model_complete", args).await;
        assert!(!reply.is_error, "{}", reply.text());
        let data = reply.structured_content.unwrap();
        assert_eq!(data["model"], answered);
        // A step up is there from the cheap model, not from the top.
        assert_eq!(data["stronger"], answered == "cheap");
    }
    let records = model_records(&h, task);
    let routes: Vec<&str> = records
        .iter()
        .map(|r| r.event.detail["model"]["route"].as_str().unwrap())
        .collect();
    assert_eq!(
        routes,
        [
            "model route for internal data, cheapest first",
            "model route for internal data, strongest first",
            "model route for internal data",
            "model route for internal data, cheapest above cheap first",
            "model route for internal data, cheapest above echo first",
        ]
    );

    // Where policy doesn't let agents choose, the route's order stands.
    let h = harness_with(&by_cost().replace("by_cost = true", ""));
    let (_, agent) = start(&h, &["Invoices"]).await;
    let mut args = ask("hello");
    args["prefer"] = json!("cheapest");
    let reply = tool(&agent, "model_complete", args).await;
    assert_eq!(reply.structured_content.unwrap()["model"], "echo");
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
async fn dp10_a_label_on_a_file_outlives_a_rewrite_and_still_routes_its_reader() {
    let h = harness_with(&labeled());
    let file = h.home().join("Invoices/inv-001.txt");
    if glass_platform::xattr::write_label(&file, glass_types::Label::Internal).is_err() {
        return; // no user attributes on this filesystem
    }
    // One task rewrites the file...
    let mut request = writer_request(&h, &["Invoices"]);
    request.scope.write = vec![h.home().join("Invoices")];
    let first = h.rt.start(&h.me, request).unwrap().id;
    let writer = h.agent(first).await;
    let wrote = tool(
        &writer,
        "files_write",
        json!({"path": "Invoices/inv-001.txt", "content": "Acme, $1,200, paid\n"}),
    )
    .await;
    assert!(!wrote.is_error, "{}", wrote.text());
    // ...and another reads it: it's still internal, so the reader's calls
    // can't go where only public data may (ModelGateway's
    // DP10_ALabelStaysWithItsFile).
    let (_, reader) = start(&h, &["Invoices"]).await;
    let read = tool(
        &reader,
        "files_read",
        json!({"path": "Invoices/inv-001.txt"}),
    )
    .await;
    assert!(!read.is_error, "{}", read.text());
    let refused = tool(&reader, "model_complete", ask("Summarize the invoice")).await;
    assert!(
        refused
            .text()
            .starts_with("Not sent: no model is approved for internal data"),
        "{}",
        refused.text()
    );
    assert!(h.model.seen().is_empty(), "nothing left the machine");
}

#[tokio::test]
async fn dp11_a_summary_of_an_internal_file_is_internal_wherever_it_is_written() {
    let h = harness_with(&labeled());
    let file = h.home().join("Invoices/inv-001.txt");
    if glass_platform::xattr::write_label(&file, glass_types::Label::Internal).is_err() {
        return; // no user attributes on this filesystem
    }
    // A task reads the internal invoice and writes its summary into a
    // folder with no label of its own...
    let mut request = writer_request(&h, &["Invoices"]);
    request.scope.write = vec![h.home().join("Private")];
    let first = h.rt.start(&h.me, request).unwrap().id;
    let writer = h.agent(first).await;
    let read = tool(
        &writer,
        "files_read",
        json!({"path": "Invoices/inv-001.txt"}),
    )
    .await;
    assert!(!read.is_error, "{}", read.text());
    let wrote = tool(
        &writer,
        "files_write",
        json!({"path": "Private/summary.txt", "content": "Acme owes $1,200\n"}),
    )
    .await;
    assert!(!wrote.is_error, "{}", wrote.text());
    assert_eq!(
        glass_platform::xattr::read_label(&h.home().join("Private/summary.txt")).unwrap(),
        Some(glass_types::Label::Internal),
        "the summary carries what it was made from"
    );
    // ...so a task that reads only the summary is routed as internal.
    let (_, reader) = start(&h, &["Private"]).await;
    let read = tool(
        &reader,
        "files_read",
        json!({"path": "Private/summary.txt"}),
    )
    .await;
    assert!(!read.is_error, "{}", read.text());
    let refused = tool(&reader, "model_complete", ask("Summarize it")).await;
    assert!(
        refused
            .text()
            .starts_with("Not sent: no model is approved for internal data"),
        "{}",
        refused.text()
    );
    assert!(h.model.seen().is_empty(), "nothing left the machine");
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

/// ADR 0041: the model proposes a tool call; the agent makes it over its
/// socket, where policy decides it; the result goes back to the model,
/// redacted; the record names the tools, never their inputs.
#[tokio::test]
async fn adr0041_the_model_proposes_a_tool_call_and_the_agent_makes_it_under_policy() {
    let h = harness_with(MODELS);
    let (task, agent) = start(&h, &["Invoices"]).await;
    let tools: ListToolsResult = agent.call("tools/list", Value::Null).await.unwrap();
    let list = tools.tools.iter().find(|t| t.name == "files_list").unwrap();
    let offered = json!([{
        "name": list.name,
        "description": list.description,
        "input_schema": list.input_schema,
    }]);

    let first = tool(
        &agent,
        "model_complete",
        json!({
            "messages": [{"role": "user", "content": "What's in my invoices? Use files_list {\"path\": \"Invoices\"}"}],
            "max_tokens": 200,
            "tools": offered,
        }),
    )
    .await;
    assert!(!first.is_error, "{}", first.text());
    let data = first.structured_content.clone().unwrap();
    assert_eq!(data["stop_reason"], "tool_use");
    let call = &data["tool_calls"][0];
    assert_eq!(call["name"], "files_list");

    // The agent makes the call itself: an action like any other.
    let listed = tool(&agent, "files_list", call["input"].clone()).await;
    assert!(!listed.is_error, "{}", listed.text());
    let second = tool(
        &agent,
        "model_complete",
        json!({
            "messages": [
                {"role": "user", "content": "What's in my invoices? Use files_list {\"path\": \"Invoices\"}"},
                {"role": "assistant", "content": first.text(), "tool_calls": [call], "replay": data["replay"]},
                {"role": "user", "tool_results": [{"id": call["id"], "content": listed.text()}]}
            ],
            "max_tokens": 200,
            "tools": offered,
        }),
    )
    .await;
    assert!(!second.is_error, "{}", second.text());
    assert!(second.text().contains("inv-001.txt"), "{}", second.text());
    // ADR 0043: the reply's blocks went back as the model wrote them, and
    // nothing before them had changed, redaction included.
    assert!(
        second.text().contains("Its reasoning came back unchanged."),
        "{}",
        second.text()
    );

    // On the record: two model calls offering one tool, the first naming
    // files_list; the list itself an action policy decided.
    let records = model_records(&h, task);
    assert_eq!(records.len(), 2);
    let m = &records[0].event.detail["model"];
    assert_eq!(m["tools_offered"], 1);
    assert_eq!(m["tools_called"], json!(["files_list"]));
    let log = std::fs::read_to_string(h.audit_path()).unwrap();
    assert!(
        !records
            .iter()
            .any(|r| r.event.detail.to_string().contains("inv-001")),
        "no tool result in a model call's record"
    );
    assert!(log.contains("files.list"));
    assert!(!log.contains("\"signature\""), "no replay in the record");
}

/// ADR 0041's safety property where it matters most: a call the model
/// proposes that policy puts at Ask waits for the person like any other,
/// and a decline reaches the model as an error result with nothing done.
#[tokio::test(flavor = "multi_thread")]
async fn adr0041_a_model_proposed_call_at_ask_waits_and_a_decline_does_nothing() {
    let h = harness_with(MODELS);
    let mut request = h.request(&[], &["Reports"]);
    request.agent = "writer".parse().unwrap();
    request.scope.restrict = vec![("files.write".into(), Tier::Ask)];
    let task = h.rt.start(&h.me, request).unwrap().id;
    let agent = std::sync::Arc::new(h.agent(task).await);
    let tools: ListToolsResult = agent.call("tools/list", Value::Null).await.unwrap();
    let write = tools
        .tools
        .iter()
        .find(|t| t.name == "files_write")
        .unwrap();
    let offered = json!([{
        "name": write.name,
        "description": write.description,
        "input_schema": write.input_schema,
    }]);
    let prompt = r#"Save a note. Use files_write {"path": "Reports/note.txt", "content": "hi"}"#;
    let first = tool(
        &agent,
        "model_complete",
        json!({ "messages": [{"role": "user", "content": prompt}], "max_tokens": 200, "tools": offered }),
    )
    .await;
    let call = first.structured_content.clone().unwrap()["tool_calls"][0].clone();
    assert_eq!(call["name"], "files_write");

    // The agent makes the call; it waits for the person.
    let pending = {
        let agent = std::sync::Arc::clone(&agent);
        let input = call["input"].clone();
        tokio::spawn(async move { tool(&agent, "files_write", input).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(ask.task, task);
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Deny,
        },
    )
    .unwrap();
    let declined = pending.await.unwrap();
    assert!(
        declined.is_error && declined.text().contains("declined"),
        "{}",
        declined.text()
    );
    assert!(
        !h.home().join("Reports/note.txt").exists(),
        "nothing written"
    );

    // The model reads the refusal as the call's result.
    let second = tool(
        &agent,
        "model_complete",
        json!({
            "messages": [
                {"role": "user", "content": prompt},
                {"role": "assistant", "content": first.text(), "tool_calls": [call]},
                {"role": "user", "tool_results": [{"id": call["id"], "content": declined.text(), "is_error": true}]}
            ],
            "max_tokens": 200,
            "tools": offered,
        }),
    )
    .await;
    assert!(!second.is_error, "{}", second.text());
    assert!(second.text().contains("declined"), "{}", second.text());
    let second_data = second.structured_content.clone().unwrap();
    assert!(
        second_data["tool_calls"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "a result is answered, not a new call: {second_data}"
    );
}
