//! The org's people (AD-1) through the real pipeline: `people.search` runs
//! at Auto from the directory the device last applied, is audited like
//! every action, and isn't there for an agent or app that doesn't declare
//! it; Mail sends as the person the directory says they are.

#![allow(clippy::unwrap_used)]

mod common;

use glass_api::{AppCallResult, AppSession, ScopeRequest, TaskRequest, app_methods};
use glass_audit::{EventKind, Outcome};
use glass_managed::directory::Applied;
use glass_managed::{DirectoryRelease, Person};
use glass_rpc::Client;
use glass_rpc::mcp::ListToolsResult;
use glass_types::{TaskId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness, harness_with, tool};

fn publish(h: &Harness) {
    let person = |login: &str, name: &str, email: &str, title: &str| Person {
        login: login.into(),
        name: name.into(),
        email: email.into(),
        title: title.into(),
        department: String::new(),
    };
    h.directory.set(Applied {
        version: 3,
        release: DirectoryRelease {
            people: vec![
                person(
                    &h.me.name,
                    "Demo Person",
                    "demo.person@acme.example",
                    "Analyst",
                ),
                person("priya", "Priya Shah", "priya@acme.example", "Counsel"),
                person("dana", "Dana Ruiz", "dana@acme.example", "AP lead"),
            ],
        },
    });
}

async fn open(client: &Client, app: &str) -> AppSession {
    client
        .call(app_methods::APP_OPEN, json!({ "app": app }))
        .await
        .unwrap()
}

async fn call(client: &Client, session: TaskId, action: &str, input: Value) -> AppCallResult {
    client
        .call(
            app_methods::APP_CALL,
            json!({ "session": session, "action": action, "input": input }),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn ad1_people_search_runs_at_auto_from_the_synced_directory_and_is_audited() {
    let h = harness_with(
        r#"
[[group]]
name = "ap-ops"
members = ["{me}"]
agents = ["claude-code"]
"#,
    );
    publish(&h);
    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: "claude-code".parse().unwrap(),
                prompt: "Email Priya the renewal terms".into(),
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
    let tools: ListToolsResult = client.call("tools/list", json!({})).await.unwrap();
    assert!(tools.tools.iter().any(|t| t.name == "people_search"));

    let found = tool(&client, "people_search", json!({"query": "pri"})).await;
    assert!(!found.is_error, "{}", found.text());
    assert!(
        found.text().contains("Priya Shah <priya@acme.example>"),
        "{}",
        found.text()
    );
    assert!(h.rt.approvals(&h.me).is_empty(), "Auto: nobody is asked");

    let records = glass_audit::read(&h.audit_path(), |r| {
        r.event.task == Some(view.id) && r.event.kind == EventKind::Action
    })
    .unwrap();
    let record = records
        .iter()
        .find(|r| {
            r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == "people.search")
        })
        .expect("audited");
    assert_eq!(record.event.decision.as_ref().unwrap().tier, Tier::Auto);
    assert_eq!(record.event.outcome, Some(Outcome::Completed));
    assert!(
        record
            .event
            .summary
            .contains("Look up \"pri\" in the org's directory"),
        "{}",
        record.event.summary
    );
}

#[tokio::test]
async fn ad1_an_agent_policy_doesnt_allow_never_sees_people_search() {
    let h = harness();
    publish(&h);
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = h.agent(task).await;
    let tools: ListToolsResult = agent.call("tools/list", json!({})).await.unwrap();
    assert!(
        !tools.tools.iter().any(|t| t.name == "people_search"),
        "invoice-demo isn't allowed people.search"
    );
    let refused = tool(&agent, "people_search", json!({"query": "pri"})).await;
    assert!(refused.is_error, "a readable refusal, not a protocol error");
    assert!(!refused.text().contains("Priya"), "{}", refused.text());
}

#[tokio::test]
async fn ui5_dv3_mail_and_calendar_look_up_people_and_the_store_may_not() {
    let h = harness();
    publish(&h);
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    for app in ["mail", "calendar"] {
        let session = open(&apps, app).await;
        let found = call(
            &apps,
            session.session,
            "people.search",
            json!({"query": "dana"}),
        )
        .await;
        assert!(found.ok, "{app}: {}", found.text);
        assert_eq!(found.data["people"][0]["email"], "dana@acme.example");
    }
    // The Store's manifest doesn't declare it (DV-3).
    let store = open(&apps, "store").await;
    let refused = call(
        &apps,
        store.session,
        "people.search",
        json!({"query": "dana"}),
    )
    .await;
    assert!(!refused.ok);
    assert!(
        !refused.text.contains("dana@acme.example"),
        "{}",
        refused.text
    );
}

#[tokio::test]
async fn au10_nv3_mail_goes_from_the_persons_directory_name_and_address() {
    let h = harness();
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let mail = open(&apps, "mail").await;
    let draft =
        json!({ "to": ["priya@acme.example"], "subject": "Renewal", "body": "Terms attached." });

    // Without a directory, as before: the login.
    let drafted = call(&apps, mail.session, "mail.draft", draft.clone()).await;
    assert!(drafted.ok, "{}", drafted.text);
    publish(&h);
    let drafted = call(&apps, mail.session, "mail.draft", draft).await;
    assert!(drafted.ok, "{}", drafted.text);

    let drafts = call(
        &apps,
        mail.session,
        "mail.list",
        json!({ "folder": "drafts" }),
    )
    .await;
    let from: Vec<&str> = drafts.data["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["from"].as_str().unwrap())
        .collect();
    assert!(
        from.contains(&"Demo Person <demo.person@acme.example>"),
        "{from:?}"
    );
    assert!(from.contains(&h.me.name.as_str()), "{from:?}");
}
