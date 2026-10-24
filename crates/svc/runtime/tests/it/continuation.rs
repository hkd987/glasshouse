//! Take over on an ended task: carrying on in the user's own harness, an
//! attended agent driven through `glass-mcp` (AW-22 for actions agents,
//! HX-1). The test plays the harness on the task's socket.

#![allow(clippy::unwrap_used)]

use crate::common;

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

// ---- take over and hand back racing each other (the TakeOver model,
// specs/tla, ADR 0052) -------------------------------------------------------

fn root() -> Caller {
    Caller {
        name: "root".into(),
        uid: 0,
        gid: 0,
        home: "/root".into(),
    }
}

/// A task of the test user's, with its agent connected, taken over, and a
/// file the person typed while driving.
async fn driving(h: &common::Harness) -> glass_types::TaskId {
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let _agent = h.agent(task).await;
    h.rt.take_over(&h.me, task).await.unwrap();
    assert_eq!(h.tile(task), TileState::YouAreDriving);
    std::fs::write(h.home().join("Reports/by-hand.txt"), "typed by the person").unwrap();
    task
}

/// Many files typed while driving, so working them out takes the hand back
/// a while and what races it lands meanwhile.
fn typed_many(h: &common::Harness) {
    for i in 0..300 {
        std::fs::write(h.home().join(format!("Reports/note-{i}.txt")), "typed").unwrap();
    }
}

fn yours(h: &common::Harness, task: glass_types::TaskId) -> usize {
    h.rt.get(&h.me, task)
        .unwrap()
        .changes
        .iter()
        .filter(|c| c.action.as_str() == "you.edit")
        .count()
}

#[tokio::test]
async fn aw21_a_pause_while_you_drive_leaves_you_driving_and_your_edits_join_at_hand_back() {
    let h = harness();
    let task = driving(&h).await;
    // Pausing ended the drive without a hand back: a resume then ran the
    // agent and the person's edit was never recorded.
    assert!(matches!(
        h.rt.pause(&h.me, task),
        Err(RuntimeError::Conflict(_))
    ));
    assert_eq!(h.tile(task), TileState::YouAreDriving);
    assert!(matches!(
        h.rt.resume(&h.me, task),
        Err(RuntimeError::Conflict(_))
    ));
    let view = h.rt.hand_back(&h.me, task).await.unwrap();
    assert!(view.caption.contains("changed 1 file"), "{}", view.caption);
    assert_eq!(yours(&h, task), 1);
}

#[tokio::test]
async fn aw21_a_second_take_over_keeps_the_first_ones_snapshot() {
    let h = harness();
    let task = driving(&h).await;
    // A second snapshot would hide what was typed since the first.
    let again = h.rt.take_over(&h.me, task).await.unwrap();
    assert_eq!(again.state, TileState::YouAreDriving);
    let view = h.rt.hand_back(&h.me, task).await.unwrap();
    assert!(view.caption.contains("changed 1 file"), "{}", view.caption);
    assert_eq!(yours(&h, task), 1);
}

#[tokio::test]
async fn aw21_two_hand_backs_at_once_hand_back_once() {
    let h = harness();
    let task = driving(&h).await;
    typed_many(&h);
    // A double click, or the tile and glassctl together: the second let
    // the agent go on while the first was still working out what the
    // person changed, so a step of the agent's could be recorded as theirs.
    let (a, b) = tokio::join!(h.rt.hand_back(&h.me, task), async {
        tokio::task::yield_now().await;
        h.rt.hand_back(&h.me, task).await
    });
    let (done, refused): (Vec<_>, Vec<_>) = [a, b].into_iter().partition(Result::is_ok);
    assert_eq!(done.len(), 1, "{refused:?}");
    assert!(matches!(refused[0], Err(RuntimeError::Conflict(_))));
    let view = done.into_iter().next().unwrap().unwrap();
    assert!(
        view.caption.contains("changed 301 files"),
        "{}",
        view.caption
    );
    assert_eq!(yours(&h, task), 301);
    assert_eq!(h.tile(task), TileState::Working);
}

#[tokio::test]
async fn aw20_a_pause_landing_during_a_hand_back_isnt_lost() {
    let h = harness();
    let task = driving(&h).await;
    typed_many(&h);
    // The pause landed while the hand back worked out the person's edits,
    // then the hand back let the agent go on: Paused said, Working done.
    let (handed, paused) = tokio::join!(h.rt.hand_back(&h.me, task), async {
        tokio::task::yield_now().await;
        h.rt.pause(&h.me, task)
    });
    handed.unwrap();
    match paused {
        // A pause that answered Paused holds the agent.
        Ok(view) => {
            assert_eq!(view.state, TileState::Paused);
            assert_eq!(h.tile(task), TileState::Paused, "the pause was lost");
        }
        // Or the person still had control when it landed.
        Err(e) => assert!(matches!(e, RuntimeError::Conflict(_)), "{e}"),
    }
}

#[tokio::test]
async fn aw21_a_hand_back_waits_for_the_take_overs_snapshot() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let _agent = h.agent(task).await;
    // A hand back that lands before take over's snapshot found none, let
    // the agent go on, and the snapshot then stayed behind as the drive's.
    let (took, handed) = tokio::join!(h.rt.take_over(&h.me, task), h.rt.hand_back(&h.me, task));
    took.unwrap();
    let handed = handed.unwrap();
    assert_eq!(
        handed.caption, "You handed back without changing any files.",
        "the hand back compared with the take over's snapshot"
    );
    assert_eq!(h.tile(task), TileState::Working);
}

#[tokio::test]
async fn pm5_a_wake_leaves_a_task_you_took_over_while_it_slept() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let _agent = h.agent(task).await;
    assert_eq!(h.rt.suspend(&root()).await.unwrap().tasks, 1);
    assert_eq!(h.tile(task), TileState::Paused);
    h.rt.take_over(&h.me, task).await.unwrap();
    std::fs::write(h.home().join("Reports/by-hand.txt"), "typed by the person").unwrap();
    // The wake let go of every task sleep paused, this one too: the drive
    // ended without a hand back.
    h.rt.wake(&root()).unwrap();
    assert_eq!(h.tile(task), TileState::YouAreDriving);
    let view = h.rt.hand_back(&h.me, task).await.unwrap();
    assert!(view.caption.contains("changed 1 file"), "{}", view.caption);
    assert_eq!(view.state, TileState::Working);
}

#[tokio::test]
async fn aw22_an_ask_that_times_out_while_you_drive_leaves_you_driving() {
    let h = common::harness_with("");
    let text = glass_policy::DEFAULT_POLICY
        .replace("approval_timeout_secs = 900", "approval_timeout_secs = 1");
    let h = common::Harness {
        rt: common::rebuild(&h, &text),
        ..h
    };
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    let call = tool(
        &agent,
        "files_delete",
        json!({"path": "Reports/existing.txt"}),
    );
    let drive = async {
        eventually(|| h.tile(task) == TileState::NeedsYou).await;
        h.rt.take_over(&h.me, task).await.unwrap();
    };
    let (result, ()) = tokio::join!(call, drive);
    // The timed-out ask held a task the person drives as Paused: a resume
    // then ran the agent with the person's edits never recorded.
    assert!(result.is_error, "{}", result.text());
    assert_eq!(h.tile(task), TileState::YouAreDriving);
}
