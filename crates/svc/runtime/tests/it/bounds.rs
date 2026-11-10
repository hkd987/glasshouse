//! What an agent can make as large as it likes has a size in glassd: the
//! text of a call that is refused (kept in the task's activity and the
//! audit log), what its program prints (the task's log, and the line a
//! failure shows), and a file undo brings back. Each of these was once as
//! large as the agent chose, in memory or on the disk the audit log is on.

#![allow(clippy::unwrap_used)]

use std::fs;

use glass_api::{TileState, UndoRequest};
use glass_audit::EventKind;
use glass_runtime::CatalogEntry;
use serde_json::json;

use crate::common::{self, eventually, harness, tool};

#[tokio::test]
async fn a_refused_calls_text_is_a_line_long_in_the_activity_and_the_record() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = h.agent(task).await;
    // A megabyte of path, out of scope: the refusal used to quote all of
    // it into the audit log and the task's activity, once per call.
    let outside = format!("../{}", "A".repeat(3000));
    let refused = tool(&agent, "files_read", json!({ "path": outside })).await;
    assert!(refused.is_error);
    // Longer than a path may be: refused before it is looked at.
    let long = format!("Invoices/{}", "B".repeat(1024 * 1024));
    let refused = tool(&agent, "files_read", json!({ "path": long })).await;
    assert!(
        refused.text().contains("longer than a path may be"),
        "{:.200}",
        refused.text()
    );

    let detail = h.rt.get(&h.me, task).unwrap();
    let longest = detail
        .activity
        .iter()
        .map(|a| a.text.chars().count())
        .max()
        .unwrap();
    assert!(longest < 600, "an activity line of {longest} characters");
    assert!(
        detail
            .activity
            .iter()
            .any(|a| a.text.starts_with("Blocked:") && a.text.contains("more characters")),
        "{:?}",
        detail.activity
    );
    let records = h.rt.audit_task(&h.me, task).unwrap();
    let blocked = records
        .iter()
        .find(|r| r.event.kind == EventKind::Action && r.event.summary.starts_with("Out of scope"))
        .expect("the refusal is on the record");
    let size = serde_json::to_string(&blocked.event).unwrap().len();
    assert!(size < 2000, "a record of {size} bytes");
}

#[tokio::test]
async fn a_tasks_activity_keeps_its_newest_lines_however_many_there_were() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = h.agent(task).await;
    // An agent calling in a loop: every call is a line.
    for n in 0..2400 {
        let refused = tool(&agent, "files_read", json!({ "path": format!("../{n}") })).await;
        assert!(refused.is_error);
    }
    let activity = h.rt.get(&h.me, task).unwrap().activity;
    assert!(activity.len() <= 2250, "{} lines kept", activity.len());
    assert!(activity.len() >= 2000, "{} lines kept", activity.len());
    let last = activity.last().unwrap();
    assert!(last.text.contains("../2399"), "{}", last.text);
    // Every one of them is on the record, which is where they're kept.
    let on_record =
        h.rt.audit_task(&h.me, task)
            .unwrap()
            .iter()
            .filter(|r| r.event.summary.starts_with("Out of scope"))
            .count();
    assert_eq!(on_record, 2400);
}

/// An agent that prints 20 MB, says why it gives up, and exits 3: the
/// invoice demo's id, which the default policy lets people run.
fn noisy() -> CatalogEntry {
    CatalogEntry {
        id: "invoice-demo".parse().unwrap(),
        name: "Noisy".into(),
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "head -c 20000000 /dev/zero | tr '\\0' 'x'
             echo
             echo 'fatal: out of ideas' >&2
             exit 3"
                .into(),
        ],
        plan: Vec::new(),
        ..common::repeater()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_an_agent_prints_is_kept_to_a_size_and_its_last_line_is_why_it_failed() {
    let h = harness();
    let rt = common::rebuild_with(&h, glass_policy::DEFAULT_POLICY, vec![noisy()]);
    let task = rt.start(&h.me, h.request(&[], &[])).unwrap().id;
    eventually(|| {
        rt.get(&h.me, task)
            .is_ok_and(|d| d.view.state == TileState::Failed)
    })
    .await;
    let view = rt.get(&h.me, task).unwrap().view;
    // The last line it printed, not megabytes of it; the whole log was
    // once read into memory to find this.
    assert_eq!(
        view.failure.as_deref(),
        Some("the agent exited (exit status: 3): fatal: out of ideas")
    );
    let log = h.dir.path().join(format!("state/tasks/{task}/agent.log"));
    let size = fs::metadata(&log).unwrap().len();
    assert!(
        (16 * 1024 * 1024..17 * 1024 * 1024).contains(&size),
        "a log of {size} bytes"
    );
    let text = fs::read_to_string(&log).unwrap();
    assert!(
        text.starts_with("xxxx"),
        "it starts with what was printed first"
    );
    assert!(text.contains("more bytes of output weren't kept"));
    assert!(
        text.ends_with("fatal: out of ideas\n"),
        "and ends with the last"
    );
    // Dismissed, its log goes with it.
    rt.dismiss(&h.me, task).unwrap();
    assert!(!log.exists());
}

#[tokio::test]
async fn old_output_of_tasks_no_longer_there_is_removed_and_a_live_tasks_is_kept() {
    let h = harness();
    let task = h.rt.start(&h.me, h.request(&[], &[])).unwrap().id;
    let _agent = h.agent(task).await;
    let logs = h.dir.path().join("state/tasks");
    let live = logs.join(task.to_string());
    assert!(live.join("agent.log").exists());
    // Left by a glassd that restarted: a task nobody has now.
    let gone = logs.join(glass_types::TaskId::new().to_string());
    fs::create_dir_all(&gone).unwrap();
    fs::write(gone.join("agent.log"), "from before").unwrap();
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 3600);
    for dir in [&gone, &live] {
        fs::File::open(dir).unwrap().set_modified(long_ago).unwrap();
    }
    let week = std::time::Duration::from_secs(7 * 24 * 3600);
    assert_eq!(h.rt.prune_task_logs(week), 1);
    assert!(!gone.exists());
    assert!(
        live.join("agent.log").exists(),
        "a task still here keeps its log"
    );
}

#[tokio::test]
async fn undo_brings_back_a_large_file_without_holding_it_whole() {
    // AU-21: a file comes back whatever its size. It was read whole into
    // memory first, so one the size of the computer's memory ended glassd
    // mid-rollback.
    let h = harness();
    // Larger than what undo keeps a digest of (1 MiB), and not all alike.
    let content: Vec<u8> = (0..5_000_000u32).map(|n| (n % 251) as u8).collect();
    let path = h.home().join("Reports/recording.bin");
    fs::write(&path, &content).unwrap();
    let task = h.rt.start(&h.me, h.request(&[], &["Reports"])).unwrap().id;
    let agent = h.agent(task).await;
    let written = tool(
        &agent,
        "files_write",
        json!({ "path": "Reports/recording.bin", "content": "written over" }),
    )
    .await;
    assert!(!written.is_error, "{}", written.text());
    assert_eq!(fs::read(&path).unwrap(), b"written over");

    let request = UndoRequest {
        task,
        to: None,
        changes: Vec::new(),
        dry_run: false,
    };
    let report = h.rt.undo(&h.me, request.clone()).await.unwrap();
    assert_eq!(report.restored, ["~/Reports/recording.bin"]);
    assert!(report.not_undone.is_empty(), "{:?}", report.not_undone);
    assert_eq!(fs::read(&path).unwrap(), content);
    // Undone once: a second undo has nothing left to do and changes nothing.
    let again = h.rt.undo(&h.me, request).await.unwrap();
    assert!(again.restored.is_empty(), "{:?}", again.restored);
    assert_eq!(fs::read(&path).unwrap(), content);
}
