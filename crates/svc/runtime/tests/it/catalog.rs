//! Catalog first (GA-2): `catalog.search` through the real pipeline. It
//! runs at Auto, sees only what the user's groups may install (DV-11), is
//! audited like every action, and isn't there at all for an agent policy
//! doesn't allow it to.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use glass_api::{ScopeRequest, TaskRequest};
use glass_app_catalog::Applied;
use glass_audit::{EventKind, Outcome};
use glass_managed::{CatalogItem, CatalogRelease, Declared, InstallMode, ItemKind};
use glass_rpc::Client;
use glass_rpc::mcp::ListToolsResult;
use glass_types::{Label, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness, harness_with, tool};

fn item(id: &str, name: &str, groups: &[(&str, InstallMode)]) -> CatalogItem {
    CatalogItem {
        id: id.parse().unwrap(),
        kind: ItemKind::App,
        name: name.into(),
        version: "3.0.0".into(),
        publisher: "Acme IT".into(),
        publisher_verified: true,
        internal: true,
        summary: "Track vendor disputes with overdue reminders".into(),
        keywords: Vec::new(),
        declares: Declared {
            actions: vec!["records.read".into()],
            default_tiers: BTreeMap::new(),
            max_label: Label::Internal,
            egress: Vec::new(),
        },
        groups: groups.iter().map(|(g, m)| ((*g).to_owned(), *m)).collect(),
        pinned: false,
        provenance: Default::default(),
        app: None,
    }
}

fn publish(h: &Harness) {
    h.catalog.set(Applied {
        version: 4,
        release: CatalogRelease {
            items: vec![
                item(
                    "ap-disputes",
                    "AP Disputes",
                    &[("ap-ops", InstallMode::Available)],
                ),
                item(
                    "finance-disputes",
                    "Finance Disputes",
                    &[("finance", InstallMode::Available)],
                ),
                item(
                    "public-disputes",
                    "Shared Disputes",
                    &[
                        ("everyone", InstallMode::Available),
                        ("ap-ops", InstallMode::Blocked),
                    ],
                ),
            ],
        },
    });
}

/// Starts the user's own harness (attended) and connects to its socket.
async fn harness_task(h: &Harness) -> (glass_types::TaskId, Client) {
    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: "claude-code".parse().unwrap(),
                prompt: "Build me a way to track vendor disputes".into(),
                scope: ScopeRequest::default(),
                title: None,
            },
        )
        .unwrap();
    let socket = view.driver_socket.clone().unwrap();
    eventually(|| socket.exists()).await;
    let client = Client::connect(&socket).await.unwrap();
    let _: Value = client
        .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}))
        .await
        .unwrap();
    (view.id, client)
}

#[tokio::test]
async fn ga2_catalog_search_runs_at_auto_scoped_to_the_users_groups_and_audited() {
    let h = harness_with(
        r#"
[[group]]
name = "ap-ops"
members = ["{me}"]
agents = ["claude-code"]
"#,
    );
    publish(&h);
    let (task, client) = harness_task(&h).await;
    let tools: ListToolsResult = client.call("tools/list", json!({})).await.unwrap();
    assert!(tools.tools.iter().any(|t| t.name == "catalog_search"));

    let found = tool(
        &client,
        "catalog_search",
        json!({"need": "track vendor disputes"}),
    )
    .await;
    assert!(!found.is_error, "{}", found.text());
    let text = found.text();
    assert!(text.contains("AP Disputes"), "{text}");
    assert!(
        !text.contains("Finance Disputes"),
        "another group's item: {text}"
    );
    assert!(
        !text.contains("Shared Disputes"),
        "blocked for ap-ops: {text}"
    );
    assert!(h.rt.approvals(&h.me).is_empty(), "Auto: nobody is asked");

    let records = glass_audit::read(&h.audit_path(), |r| {
        r.event.task == Some(task) && r.event.kind == EventKind::Action
    })
    .unwrap();
    let record = records
        .iter()
        .find(|r| {
            r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == "catalog.search")
        })
        .expect("audited");
    assert_eq!(record.event.decision.as_ref().unwrap().tier, Tier::Auto);
    assert_eq!(record.event.outcome, Some(Outcome::Completed));
    assert!(
        record
            .event
            .summary
            .contains("Search the org's catalog for \"track vendor disputes\""),
        "{}",
        record.event.summary
    );
}

#[tokio::test]
async fn ga2_an_agent_policy_doesnt_allow_never_sees_catalog_search() {
    let h = harness();
    publish(&h);
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = h.agent(task).await;
    let tools: ListToolsResult = agent.call("tools/list", json!({})).await.unwrap();
    assert!(
        !tools.tools.iter().any(|t| t.name == "catalog_search"),
        "invoice-demo isn't allowed catalog.search"
    );
    let refused = tool(&agent, "catalog_search", json!({"need": "disputes"})).await;
    assert!(refused.is_error, "a readable refusal, not a protocol error");
    assert!(
        !refused.text().contains("AP Disputes"),
        "{}",
        refused.text()
    );
}

#[tokio::test]
async fn adr0023_a_policy_applied_live_reaches_new_tasks_and_spares_running_ones() {
    // AD-3, ADR 0021: managed settings swap policy and registry whole;
    // tasks started after use the new policy, running ones keep theirs.
    let h = harness();
    publish(&h);
    let (_running, client) = harness_task(&h).await;
    h.apply_policy(
        r#"
[[group]]
name = "catalog-off"
members = ["{me}"]
tiers = { "catalog.search" = "never" }
"#,
        "managed v2",
    );
    assert_eq!(h.rt.status().policy, "managed v2");

    let still = tool(
        &client,
        "catalog_search",
        json!({"need": "vendor disputes"}),
    )
    .await;
    assert!(
        !still.is_error,
        "the running task keeps its policy: {}",
        still.text()
    );

    let (_new, fresh) = harness_task(&h).await;
    let tools: ListToolsResult = fresh.call("tools/list", json!({})).await.unwrap();
    assert!(
        !tools.tools.iter().any(|t| t.name == "catalog_search"),
        "a task started after the change gets the new policy"
    );
    let refused = tool(&fresh, "catalog_search", json!({"need": "vendor disputes"})).await;
    assert!(refused.is_error, "{}", refused.text());
}
