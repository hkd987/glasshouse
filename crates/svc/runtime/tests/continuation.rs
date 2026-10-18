//! Take over on an ended task: carrying on in the user's own harness, an
//! attended agent driven through `glass-mcp` (AW-22 for actions agents,
//! HX-1). The test plays the harness on the task's socket.

#![allow(clippy::unwrap_used)]

mod common;

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use glass_api::TileState;
use glass_audit::EventKind;
use glass_rpc::Client;
use glass_rpc::mcp::ListToolsResult;
use glass_runtime::{Caller, RuntimeError};
use serde_json::{Value, json};

use crate::common::{eventually, harness, tool};

#[tokio::test]
async fn an_ended_task_carries_on_in_the_users_harness_with_its_context() {
    let h = harness();
    let first =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = h.agent(first).await;
    let wrote = tool(
        &agent,
        "files_write",
        json!({"path": "Reports/summary.csv", "content": "half done\n"}),
    )
    .await;
    assert!(!wrote.is_error, "{}", wrote.text());
    h.rt.stop(&h.me, first).unwrap();
    eventually(|| !h.socket(first).exists()).await;

    let view = h.rt.continue_task(&h.me, first).unwrap();
    assert_eq!(view.agent.as_str(), "claude-code");
    assert_eq!(view.continues, Some(first));
    let socket = view
        .driver_socket
        .clone()
        .expect("an attended task has a socket");
    eventually(|| socket.exists()).await;
    let meta = std::fs::metadata(&socket).unwrap();
    assert_eq!(meta.uid(), h.me.uid, "the user's own socket");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert!(
        !h.socket(view.id).exists(),
        "no sandbox for an attended task"
    );

    // The harness connects and learns where the first task stopped.
    let harness = Client::connect(&socket).await.unwrap();
    let init: Value = harness
        .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "claude-code", "version": "0"}}))
        .await
        .unwrap();
    let instructions = init["instructions"].as_str().unwrap();
    assert!(
        instructions
            .contains("taking over this task from the Invoice demo agent, which was stopped"),
        "{instructions}"
    );
    assert!(
        instructions.contains("Reports/summary.csv"),
        "{instructions}"
    );
    let caption = h.rt.get(&h.me, view.id).unwrap().view.caption;
    assert_eq!(caption, "You're driving in Claude Code");
    let tools: ListToolsResult = harness.call("tools/list", json!({})).await.unwrap();
    assert!(tools.tools.iter().any(|t| t.name == "files_write"));

    // Its calls go through the same pipeline: tiers, snapshot, audit.
    let wrote = tool(
        &harness,
        "files_write",
        json!({"path": "Reports/summary.csv", "content": "all done\n"}),
    )
    .await;
    assert!(!wrote.is_error, "{}", wrote.text());
    assert_eq!(
        std::fs::read_to_string(h.home().join("Reports/summary.csv")).unwrap(),
        "all done\n"
    );
    let detail = h.rt.get(&h.me, view.id).unwrap();
    assert_eq!(detail.changes.len(), 1);
    assert!(
        detail.activity[0]
            .text
            .contains(&format!("continuing {}", first.short())),
        "{:?}",
        detail.activity[0]
    );

    // Hand back closes it as Done; the socket goes with it.
    let done = h.rt.hand_back(&h.me, view.id).await.unwrap();
    assert_eq!(done.state, TileState::Done);
    eventually(|| !socket.exists()).await;
    assert_eq!(h.tile(view.id), TileState::Done);

    let records = glass_audit::read(&h.audit_path(), |r| r.event.task == Some(view.id)).unwrap();
    let started = records
        .iter()
        .find(|r| r.event.kind == EventKind::TaskStarted)
        .unwrap();
    assert_eq!(started.event.detail["continues"], json!(first));
    assert_eq!(started.event.detail["attended"], json!(true));
    let action = records
        .iter()
        .find(|r| r.event.kind == EventKind::Action)
        .unwrap();
    assert_eq!(action.event.agent.as_ref().unwrap().as_str(), "claude-code");
    assert!(
        records
            .iter()
            .any(|r| r.event.kind == EventKind::TaskFinished)
    );
}

#[tokio::test]
async fn only_your_own_ended_tasks_can_be_continued() {
    let h = harness();
    let live = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    assert!(matches!(
        h.rt.continue_task(&h.me, live),
        Err(RuntimeError::Conflict(_))
    ));
    h.rt.stop(&h.me, live).unwrap();

    let mallory = Caller {
        name: "mallory".into(),
        uid: h.me.uid + 4242,
        gid: h.me.gid,
        home: "/home/mallory".into(),
    };
    assert!(matches!(
        h.rt.continue_task(&mallory, live),
        Err(RuntimeError::NotFound(_))
    ));

    // The harness is already the user's: take over makes no sense there.
    let attended = h.rt.continue_task(&h.me, live).unwrap().id;
    assert!(matches!(
        h.rt.take_over(&h.me, attended).await,
        Err(RuntimeError::Conflict(_))
    ));
    // Stop works as for any task.
    h.rt.stop(&h.me, attended).unwrap();
    assert_eq!(h.tile(attended), TileState::Stopped);
}
