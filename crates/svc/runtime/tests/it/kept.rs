//! A task and its undo outlive the glassd that ran it (ADR 0020 item 3,
//! FD-4): its record is on disk, and a glassd started on the same state
//! reads it back as an ended task. "A restart" here is a second runtime on
//! the first one's state, as `rebuild` makes.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use glass_api::{TileState, UndoReport, UndoRequest};
use glass_audit::EventKind;
use glass_policy::DEFAULT_POLICY;
use glass_rpc::Client;
use glass_runtime::{Restored, Runtime};
use glass_types::TaskId;
use serde_json::json;

use crate::common::{Harness, eventually, harness, rebuild, tool};

const CHANGED_SINCE: &str = "changed since this task, so it was left as it is";
const RESTARTED: &str = "the system restarted while it was working";

async fn write(agent: &Client, path: &str, content: &str) {
    let done = tool(
        agent,
        "files_write",
        json!({"path": path, "content": content}),
    )
    .await;
    assert!(!done.is_error, "{}", done.text());
}

fn record(h: &Harness, task: TaskId) -> PathBuf {
    h.dir
        .path()
        .join("state/tasks")
        .join(task.to_string())
        .join("task.json")
}

/// The record on disk has `changes` changes: written within a second of
/// the last of them.
async fn kept(h: &Harness, task: TaskId, changes: usize) {
    let file = record(h, task);
    eventually(|| {
        fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|kept| kept["changes"].as_array().map(Vec::len) == Some(changes))
    })
    .await;
}

async fn undo(rt: &Arc<Runtime>, h: &Harness, task: TaskId, dry_run: bool) -> UndoReport {
    let request = UndoRequest {
        task,
        to: None,
        changes: Vec::new(),
        dry_run,
    };
    rt.undo(&h.me, request).await.unwrap()
}

fn read(h: &Harness, path: &str) -> String {
    fs::read_to_string(h.home().join(path)).unwrap()
}

#[tokio::test]
async fn fd4_a_task_that_ended_is_back_after_a_restart_and_its_undo_works() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    write(&agent, "Reports/new.csv", "a").await;
    write(&agent, "Reports/existing.txt", "clobbered").await;
    h.rt.stop(&h.me, task).unwrap();
    eventually(|| h.tile(task) == TileState::Stopped).await;
    kept(&h, task, 2).await;
    // The person carries on after the task.
    fs::write(h.home().join("Reports/mine.txt"), "mine\n").unwrap();

    let again = rebuild(&h, DEFAULT_POLICY);
    assert!(
        again.get(&h.me, task).is_err(),
        "nothing is in memory at a start"
    );
    assert_eq!(
        again.restore_tasks(),
        Restored {
            tasks: 1,
            interrupted: 0,
            unread: 0
        }
    );
    // As it ended: the same tile, activity, changes and markers.
    let before = h.rt.get(&h.me, task).unwrap();
    let after = again.get(&h.me, task).unwrap();
    assert_eq!(after.view.state, TileState::Stopped);
    assert_eq!(after.changes, before.changes);
    assert_eq!(after.snapshots, before.snapshots);
    assert_eq!(after.activity, before.activity);
    assert_eq!(after.view.receipt, before.view.receipt);
    // It is the same person's, and nobody else's.
    let other = glass_runtime::Caller {
        uid: h.me.uid + 1,
        ..h.me.clone()
    };
    assert!(again.get(&other, task).is_err());

    let report = undo(&again, &h, task, false).await;
    assert_eq!(report.restored, ["~/Reports/existing.txt"]);
    assert_eq!(report.removed, ["~/Reports/new.csv"]);
    assert!(report.not_undone.is_empty(), "{:?}", report.not_undone);
    assert_eq!(read(&h, "Reports/existing.txt"), "original report\n");
    assert!(!h.home().join("Reports/new.csv").exists());
    assert_eq!(read(&h, "Reports/mine.txt"), "mine\n");

    // What is undone is kept too: after another restart it isn't offered,
    // or done, a second time.
    let third = rebuild(&h, DEFAULT_POLICY);
    assert_eq!(third.restore_tasks().tasks, 1);
    let changes = third.get(&h.me, task).unwrap().changes;
    assert!(changes.iter().all(|c| c.undone), "{changes:?}");
    fs::write(
        h.home().join("Reports/new.csv"),
        "the person's own, since\n",
    )
    .unwrap();
    let nothing = undo(&third, &h, task, false).await;
    assert!(
        nothing.restored.is_empty() && nothing.removed.is_empty(),
        "{nothing:?}"
    );
    assert_eq!(read(&h, "Reports/new.csv"), "the person's own, since\n");
}

#[tokio::test]
async fn fd4_a_task_cut_short_comes_back_failed_saying_so_with_its_undo() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    write(&agent, "Reports/existing.txt", "clobbered").await;
    // No stop and no end: glassd is gone from one moment to the next. What
    // the task changed is on disk within a second of it.
    kept(&h, task, 1).await;

    let again = rebuild(&h, DEFAULT_POLICY);
    assert_eq!(
        again.restore_tasks(),
        Restored {
            tasks: 1,
            interrupted: 1,
            unread: 0
        }
    );
    let detail = again.get(&h.me, task).unwrap();
    assert_eq!(detail.view.state, TileState::Failed);
    assert_eq!(detail.view.failure.as_deref(), Some(RESTARTED));
    assert_eq!(detail.now, format!("Stopped because {RESTARTED}."));
    // Its end is on the audit record, where its records used to just stop.
    let ends = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == EventKind::TaskFinished && r.event.task == Some(task)
    })
    .unwrap();
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(ends[0].event.detail["interrupted_by_restart"], true);
    assert_eq!(ends[0].event.user.as_deref(), Some(h.me.name.as_str()));

    let report = undo(&again, &h, task, false).await;
    assert_eq!(report.restored, ["~/Reports/existing.txt"]);
    assert_eq!(read(&h, "Reports/existing.txt"), "original report\n");

    // The next start finds it ended: it isn't "interrupted" twice.
    let third = rebuild(&h, DEFAULT_POLICY);
    assert_eq!(third.restore_tasks().interrupted, 0);
    let ends = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == EventKind::TaskFinished && r.event.task == Some(task)
    })
    .unwrap();
    assert_eq!(ends.len(), 1);
}

#[tokio::test]
async fn fd4_a_record_behind_its_task_leaves_the_later_changes_alone() {
    // glassd stops in the second after a change, before its record is
    // written: the record knows the first change only.
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    write(&agent, "Reports/existing.txt", "first").await;
    kept(&h, task, 1).await;
    let behind = fs::read(record(&h, task)).unwrap();
    write(&agent, "Reports/existing.txt", "second").await;
    write(&agent, "Reports/new.csv", "a").await;
    kept(&h, task, 3).await;
    fs::write(record(&h, task), behind).unwrap();

    let again = rebuild(&h, DEFAULT_POLICY);
    assert_eq!(again.restore_tasks().tasks, 1);
    let report = undo(&again, &h, task, false).await;
    // The file isn't as the record says the task left it, so it is left:
    // nothing is put back over a version the record doesn't know.
    assert!(
        report.restored.is_empty() && report.removed.is_empty(),
        "{report:?}"
    );
    assert_eq!(
        report.not_undone,
        [format!("~/Reports/existing.txt — {CHANGED_SINCE}")]
    );
    assert_eq!(read(&h, "Reports/existing.txt"), "second");
    assert_eq!(read(&h, "Reports/new.csv"), "a");
}

#[tokio::test]
async fn fd4_a_dismissed_task_stays_gone_and_a_damaged_record_is_passed_over() {
    let h = harness();
    let gone = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(gone).await;
    write(&agent, "Reports/new.csv", "a").await;
    h.rt.stop(&h.me, gone).unwrap();
    eventually(|| h.tile(gone) == TileState::Stopped).await;
    kept(&h, gone, 1).await;
    h.rt.dismiss(&h.me, gone).unwrap();
    assert!(!record(&h, gone).exists());

    // A record that is no record, and one in a folder that isn't its
    // task's: neither is a task, and neither stops the start.
    let stays = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(stays).await;
    write(&agent, "Reports/other.csv", "b").await;
    kept(&h, stays, 1).await;
    let tasks = h.dir.path().join("state/tasks");
    let damaged = tasks.join(TaskId::new().to_string());
    fs::create_dir_all(&damaged).unwrap();
    fs::write(damaged.join("task.json"), "{ not a record").unwrap();
    let moved = tasks.join(TaskId::new().to_string());
    fs::create_dir_all(&moved).unwrap();
    fs::copy(record(&h, stays), moved.join("task.json")).unwrap();

    let again = rebuild(&h, DEFAULT_POLICY);
    assert_eq!(
        again.restore_tasks(),
        Restored {
            tasks: 1,
            interrupted: 1,
            unread: 2
        }
    );
    assert!(again.get(&h.me, gone).is_err());
    assert!(again.get(&h.me, stays).is_ok());
    // Read once: a second look at the same folders adds nothing.
    assert_eq!(again.restore_tasks().tasks, 0);
}
