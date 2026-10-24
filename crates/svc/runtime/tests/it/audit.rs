//! The audit log when the disk fills (AU-1, ADR 0053, `specs/tla/AuditLog.tla`):
//! nothing an agent asks runs while the log can't record it, reads
//! included; what a read found reaches the agent only once it's on the
//! record; a record the log couldn't take waits, in order, and is written
//! once there's room, without glassd restarting.

#![allow(clippy::unwrap_used)]

use std::fs;

use glass_audit::{AuditRecord, EventKind};
use serde_json::json;

use crate::common::{Harness, harness, tool};

fn actions(h: &Harness, task: glass_types::TaskId) -> Vec<AuditRecord> {
    h.rt.audit_task(&h.me, task)
        .unwrap()
        .into_iter()
        .filter(|r| r.event.kind == EventKind::Action)
        .collect()
}

#[tokio::test]
async fn au1_a_read_the_log_cant_record_is_withheld_and_calls_wait_for_room() {
    let h = harness();
    let task =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = h.agent(task).await;

    // The disk fills: the read runs, but its record can't be written.
    h.audit.fail_writes_for_tests(0, u32::MAX);
    let read = tool(
        &agent,
        "files_read",
        json!({"path": "Invoices/inv-003.txt"}),
    )
    .await;
    assert!(read.is_error, "{}", read.text());
    assert!(
        read.text().contains("couldn't record this read") && !read.text().contains("310.25"),
        "{}",
        read.text()
    );
    // Nothing runs while the log is down, reads included.
    let write = tool(
        &agent,
        "files_write",
        json!({"path": "Reports/summary.csv", "content": "total\n"}),
    )
    .await;
    assert!(
        write.text().contains("can't take records"),
        "{}",
        write.text()
    );
    assert!(!h.home().join("Reports/summary.csv").exists());
    let again = tool(
        &agent,
        "files_read",
        json!({"path": "Invoices/inv-001.txt"}),
    )
    .await;
    assert!(
        again.text().contains("can't take records"),
        "{}",
        again.text()
    );

    // There's room again: the record waiting goes first, then the call.
    h.audit.fail_writes_for_tests(0, 0);
    let read = tool(
        &agent,
        "files_read",
        json!({"path": "Invoices/inv-003.txt"}),
    )
    .await;
    assert!(
        !read.is_error && read.text().contains("310.25"),
        "{}",
        read.text()
    );
    let records = actions(&h, task);
    let reads: Vec<_> = records
        .iter()
        .filter(|r| {
            r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == "files.read")
        })
        .collect();
    assert_eq!(reads.len(), 2, "{records:?}");
    assert!(reads[0].event.detail.get("happened_at").is_some());
    assert!(reads[1].event.detail.get("happened_at").is_none());
    // The write never ran: no record of it.
    assert!(!records.iter().any(|r| {
        r.event
            .action
            .as_ref()
            .is_some_and(|a| a.as_str() == "files.write")
    }));
    // The chain holds, with the part of a line the full disk left cut off
    // and noted.
    let report = glass_audit::verify(&h.audit_path()).unwrap();
    assert_eq!(report.unfinished, 0);
    let all = glass_audit::read(&h.audit_path(), |_| true).unwrap();
    assert!(all.iter().any(|r| r.event.kind == EventKind::LogRecovered));
}

#[tokio::test]
async fn au1_a_change_the_log_cant_record_is_recorded_once_there_is_room() {
    let h = harness();
    let task =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = h.agent(task).await;
    let write = |content: &'static str| {
        tool(
            &agent,
            "files_write",
            json!({"path": "Reports/summary.csv", "content": content}),
        )
    };
    // The task's first change takes its snapshot, on the record.
    assert!(!write("total\n").await.is_error);

    // The disk fills after the next change's snapshot, before its record:
    // the change happened, so the agent hears so, and its record waits.
    h.audit.fail_writes_for_tests(1, u32::MAX);
    let second = write("total,505.75\n").await;
    assert!(!second.is_error, "{}", second.text());
    assert_eq!(
        fs::read_to_string(h.home().join("Reports/summary.csv")).unwrap(),
        "total,505.75\n"
    );
    // A snapshot's record fails too: nothing more runs.
    let refused = tool(&agent, "files_list", json!({"path": "Reports"})).await;
    assert!(
        refused.text().contains("can't take records"),
        "{}",
        refused.text()
    );

    h.audit.fail_writes_for_tests(0, 0);
    let listing = tool(&agent, "files_list", json!({"path": "Reports"})).await;
    assert!(!listing.is_error, "{}", listing.text());
    let records = actions(&h, task);
    let names: Vec<&str> = records
        .iter()
        .filter_map(|r| r.event.action.as_ref().map(|a| a.as_str()))
        .collect();
    assert_eq!(
        names,
        ["files.write", "files.write", "files.list"],
        "{records:?}"
    );
    assert!(records[0].event.detail.get("happened_at").is_none());
    assert!(records[1].event.detail.get("happened_at").is_some());
}
