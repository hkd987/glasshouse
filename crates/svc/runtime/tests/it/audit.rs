//! The audit log when the disk fills (AU-1, ADR 0053, `specs/tla/AuditLog.tla`):
//! nothing an agent asks runs while the log can't record it, reads
//! included; what a read found reaches the agent only once it's on the
//! record; a record the log couldn't take waits, in order, and is written
//! once there's room, without glassd restarting. What changes something is
//! on the record before it runs, so glassd stopping can't leave it off
//! (ADR 0054).

#![allow(clippy::unwrap_used)]

use std::fs;

use glass_audit::{AuditRecord, EventKind};
use serde_json::json;

use crate::common::{self, Harness, eventually, harness, tool};

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
    // Nor is a call that would only be refused looked at: its refusal
    // would be one more record waiting, and an agent can make these
    // without end.
    for _ in 0..3 {
        let outside = tool(&agent, "files_read", json!({"path": "../etc/passwd"})).await;
        assert!(
            outside.text().contains("can't take records"),
            "{}",
            outside.text()
        );
    }

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
    // And the calls refused unseen while the log was down left none.
    assert!(
        !records
            .iter()
            .any(|r| r.event.summary.starts_with("Out of scope")),
        "{records:?}"
    );
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

    // The disk fills after the next change's record ahead of it and its
    // snapshot, before its outcome: the change happened, so the agent
    // hears so, and its outcome waits.
    h.audit.fail_writes_for_tests(2, u32::MAX);
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

fn kind(h: &Harness, kind: EventKind) -> Vec<AuditRecord> {
    glass_audit::read(&h.audit_path(), |r| r.event.kind == kind).unwrap()
}

#[tokio::test]
async fn au1_a_change_is_on_the_record_before_it_runs_so_a_stop_cant_leave_it_off() {
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
    assert!(!write("total\n").await.is_error);
    let first = kind(&h, EventKind::Action);
    let started = kind(&h, EventKind::ActionStarted);
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(first[0].event.detail["started"], started[0].seq);

    // The disk fills after the next change's record ahead of it and its
    // snapshot: the change runs, and its outcome can't be written.
    h.audit.fail_writes_for_tests(2, u32::MAX);
    assert!(!write("total,505.75\n").await.is_error);
    // glassd stops now, losing the outcome waiting in memory: what's on
    // disk still says the change was made, by whom, and that its outcome
    // wasn't recorded (no outcome names it).
    let started = kind(&h, EventKind::ActionStarted);
    assert_eq!(started.len(), 2, "{started:?}");
    let last = &started[1];
    assert_eq!(last.event.task, Some(task));
    assert_eq!(last.event.action.as_ref().unwrap().as_str(), "files.write");
    assert_eq!(last.event.targets, ["Reports/summary.csv"]);
    assert!(
        !kind(&h, EventKind::Action)
            .iter()
            .any(|r| r.event.detail["started"] == last.seq)
    );

    // Without a stop, the outcome goes once there's room, naming it.
    h.audit.fail_writes_for_tests(0, 0);
    assert!(
        !tool(&agent, "files_list", json!({"path": "Reports"}))
            .await
            .is_error
    );
    let writes: Vec<_> = kind(&h, EventKind::Action)
        .into_iter()
        .filter(|r| {
            r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == "files.write")
        })
        .collect();
    assert_eq!(writes[1].event.detail["started"], last.seq);
    // Reads write nothing ahead: what they find waits for their record.
    assert_eq!(kind(&h, EventKind::ActionStarted).len(), 2);
}

#[tokio::test]
async fn au1_an_undo_is_on_the_record_before_it_changes_files_and_waits_for_the_log() {
    let h = harness();
    let task =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = h.agent(task).await;
    let wrote = tool(
        &agent,
        "files_write",
        json!({"path": "Reports/summary.csv", "content": "total\n"}),
    )
    .await;
    assert!(!wrote.is_error, "{}", wrote.text());
    let undo = |dry_run| glass_api::UndoRequest {
        task,
        to: None,
        changes: Vec::new(),
        dry_run,
    };

    // The log can't take records: nothing is undone.
    h.audit.fail_writes_for_tests(0, u32::MAX);
    let refused = h.rt.undo(&h.me, undo(false)).await.unwrap_err();
    assert!(
        refused.to_string().contains("Nothing was undone"),
        "{refused}"
    );
    assert!(h.home().join("Reports/summary.csv").exists());

    h.audit.fail_writes_for_tests(0, 0);
    let report = h.rt.undo(&h.me, undo(false)).await.unwrap();
    assert!(report.not_undone.is_empty(), "{report:?}");
    assert!(!h.home().join("Reports/summary.csv").exists());
    let started = kind(&h, EventKind::RollbackStarted);
    assert_eq!(started.len(), 1, "{started:?}");
    // The refused undo is on the record too, written once there was room.
    let rollback = kind(&h, EventKind::Rollback);
    assert_eq!(rollback.len(), 2, "{rollback:?}");
    assert!(rollback[0].event.summary.starts_with("Not done:"));
    assert_eq!(rollback[1].event.detail["started"], started[0].seq);
    assert!(started[0].seq < rollback[1].seq);
}

#[tokio::test]
async fn au1_a_change_refused_for_a_full_log_keeps_it_down_and_is_recorded_once_there_is_room() {
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
    assert!(!write("total\n").await.is_error);
    // The disk fills: the next change's record ahead of it can't be
    // written, so it doesn't run.
    h.audit.fail_writes_for_tests(0, u32::MAX);
    let refused = write("total,505.75\n").await;
    assert!(
        refused.text().contains("can't take records"),
        "{}",
        refused.text()
    );
    assert_eq!(
        fs::read_to_string(h.home().join("Reports/summary.csv")).unwrap(),
        "total\n"
    );
    // The log is down until that refusal is written: reads don't run.
    let read = tool(
        &agent,
        "files_read",
        json!({"path": "Invoices/inv-001.txt"}),
    )
    .await;
    assert!(
        read.text().contains("can't take records"),
        "{}",
        read.text()
    );

    h.audit.fail_writes_for_tests(0, 0);
    let read = tool(
        &agent,
        "files_read",
        json!({"path": "Invoices/inv-001.txt"}),
    )
    .await;
    assert!(!read.is_error, "{}", read.text());
    let writes: Vec<_> = kind(&h, EventKind::Action)
        .into_iter()
        .filter(|r| {
            r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == "files.write")
        })
        .collect();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(
        writes[1].event.summary.starts_with("Not done:"),
        "{writes:?}"
    );
    assert!(writes[1].event.detail.get("happened_at").is_some());
    assert_eq!(kind(&h, EventKind::ActionStarted).len(), 1);
}

fn note(text: &str) -> glass_audit::AuditEvent {
    glass_audit::AuditEvent::new(EventKind::TaskStateChanged, text)
}

#[test]
fn au1_records_asked_for_at_once_share_runs_and_each_is_there_once_in_its_order() {
    // Audit 9.1: eight writers, 25 records each, on a disk whose sync
    // takes 5 ms. One sync a record, one after another, is 200 runs and a
    // second; sharing them is a few dozen.
    let h = harness();
    h.audit.slow_syncs_for_tests(5);
    let before = h.audit.runs_for_tests();
    let writers: Vec<_> = (0..8)
        .map(|w| {
            let rt = std::sync::Arc::clone(&h.rt);
            std::thread::spawn(move || {
                for n in 0..25 {
                    // Back only once the record is in the log.
                    rt.append_audit(note(&format!("writer {w} record {n}")));
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    h.audit.slow_syncs_for_tests(0);
    let runs = h.audit.runs_for_tests() - before;
    assert!(
        runs < 100,
        "200 records took {runs} runs: they don't share them"
    );
    assert_eq!(h.rt.audit_waiting_for_tests(), 0);

    let said: Vec<String> = kind(&h, EventKind::TaskStateChanged)
        .into_iter()
        .map(|r| r.event.summary)
        .filter(|s| s.starts_with("writer "))
        .collect();
    assert_eq!(said.len(), 200, "every record once");
    for w in 0..8 {
        let theirs: Vec<&String> = said
            .iter()
            .filter(|s| s.starts_with(&format!("writer {w} ")))
            .collect();
        let expected: Vec<String> = (0..25).map(|n| format!("writer {w} record {n}")).collect();
        assert_eq!(
            theirs,
            expected.iter().collect::<Vec<_>>(),
            "writer {w}'s, in its order"
        );
    }
    assert_eq!(glass_audit::verify(&h.audit_path()).unwrap().unfinished, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn au1_a_run_that_fails_refuses_the_change_in_it_and_every_record_is_there_once_after() {
    let h = harness();
    let task =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = h.agent(task).await;
    let wait_for = |what: &dyn Fn() -> bool| {
        for _ in 0..3000 {
            if what() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("never happened");
    };

    // One record is at the disk, which is slow to answer.
    h.audit.hold_syncs_for_tests(true);
    let runs = h.audit.runs_for_tests();
    let first = {
        let rt = std::sync::Arc::clone(&h.rt);
        std::thread::spawn(move || rt.append_audit(note("first")))
    };
    wait_for(&|| h.audit.runs_for_tests() == runs + 1);
    // Meanwhile a change is asked for: its start record waits behind it.
    // And another record after that.
    let change = tokio::spawn(async move {
        tool(
            &agent,
            "files_write",
            json!({"path": "Reports/summary.csv", "content": "total\n"}),
        )
        .await
    });
    wait_for(&|| h.rt.audit_waiting_for_tests() == 2);
    let second = {
        let rt = std::sync::Arc::clone(&h.rt);
        std::thread::spawn(move || rt.append_audit(note("second")))
    };
    wait_for(&|| h.rt.audit_waiting_for_tests() == 3);
    // The disk fills before the next run, and then answers.
    h.audit.fail_writes_for_tests(0, u32::MAX);
    h.audit.hold_syncs_for_tests(false);
    first.join().unwrap();
    second.join().unwrap();

    // The change's run failed, so the change didn't run.
    let refused = change.await.unwrap();
    assert!(
        refused.text().contains("can't take records"),
        "{}",
        refused.text()
    );
    assert!(!h.home().join("Reports/summary.csv").exists());
    // "first" is in the log; nothing of the failed run is.
    assert_eq!(
        kind(&h, EventKind::TaskStateChanged)
            .iter()
            .filter(|r| r.event.summary == "first" || r.event.summary == "second")
            .map(|r| r.event.summary.as_str())
            .collect::<Vec<_>>(),
        ["first"]
    );
    assert!(kind(&h, EventKind::ActionStarted).is_empty());

    // Room again: what waited is written, in the order it was asked, the
    // change as one that wasn't done.
    h.audit.fail_writes_for_tests(0, 0);
    h.rt.append_audit(note("third"));
    let all = glass_audit::read(&h.audit_path(), |r| {
        matches!(
            r.event.kind,
            EventKind::TaskStateChanged | EventKind::Action | EventKind::ActionStarted
        ) && (r.event.task == Some(task)
            || ["first", "second", "third"].contains(&r.event.summary.as_str()))
    })
    .unwrap();
    let said: Vec<(EventKind, &str)> = all
        .iter()
        .map(|r| (r.event.kind, r.event.summary.as_str()))
        .filter(|(kind, summary)| {
            *kind != EventKind::TaskStateChanged || ["first", "second", "third"].contains(summary)
        })
        .collect();
    assert_eq!(said.len(), 4, "{said:?}");
    assert_eq!(said[0], (EventKind::TaskStateChanged, "first"));
    assert_eq!(said[1].0, EventKind::Action);
    assert!(said[1].1.starts_with("Not done: "), "{said:?}");
    assert_eq!(said[2], (EventKind::TaskStateChanged, "second"));
    assert_eq!(said[3], (EventKind::TaskStateChanged, "third"));
    assert!(all[1].event.detail.get("happened_at").is_some());
    let report = glass_audit::verify(&h.audit_path()).unwrap();
    assert_eq!(report.unfinished, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn au3_records_past_its_retention_rule_go_on_the_record_and_the_log_still_verifies() {
    use glass_audit::{AuditEvent, Segments};
    use glass_types::Timestamp;
    let h = harness();
    // IT's rule: thirty days. And a log closed into a segment every few
    // records, as a month of use does.
    let rule = |days: u32| {
        let policy = glass_policy::DEFAULT_POLICY.replace(
            "audit_retention_days = 400",
            &format!("audit_retention_days = {days}"),
        );
        assert_ne!(policy, glass_policy::DEFAULT_POLICY);
        policy
    };
    let segments = Segments {
        bytes: 1500,
        age: std::time::Duration::from_secs(3600),
    };
    let rt = common::rebuild_segmenting(&h, &rule(30), segments);
    let root = glass_runtime::Caller {
        name: "root".into(),
        uid: 0,
        gid: 0,
        home: "/root".into(),
    };

    // A task whose records start in the log's first segment and end in
    // its last.
    let task = rt
        .start(&h.me, h.request(&["Invoices"], &["Reports"]))
        .unwrap()
        .id;
    for n in 0..40 {
        rt.append_audit(AuditEvent::new(
            EventKind::DaemonStarted,
            format!("something else on the record, {n}"),
        ));
    }
    rt.stop(&h.me, task).unwrap();
    eventually(|| !rt.get(&h.me, task).unwrap().view.state.is_live()).await;
    eventually(|| {
        glass_audit::read(&h.audit_path(), |r| r.event.kind == EventKind::TaskFinished)
            .unwrap()
            .len()
            == 1
    })
    .await;

    let whole = |task| glass_audit::read(&h.audit_path(), |r| r.event.task == Some(task)).unwrap();
    let before = rt.audit_verify(&root).unwrap();
    assert!(before.files > 3, "{before:?}");
    assert_eq!(before.first_seq, 0);
    let mine = rt.audit_task(&h.me, task).unwrap();
    assert!(mine.len() >= 3, "{mine:?}");
    assert_eq!(mine, whole(task));

    // Today nothing is thirty days old.
    assert_eq!(rt.trim_audit(), None);
    assert_eq!(rt.audit_verify(&root).unwrap(), before);

    // Thirty-one days on, every segment closed by now is past the rule.
    let later = Timestamp::from_unix_seconds(Timestamp::now().unix_seconds() + 31 * 24 * 3600);
    let trimmed = rt.trim_audit_as_of(later.unwrap()).unwrap();
    assert!(trimmed.records > 30, "{trimmed:?}");
    let after = rt.audit_verify(&root).unwrap();
    assert_eq!(after.first_seq, trimmed.through_seq + 1);
    assert!(after.files < before.files, "{after:?}");
    // What went is on the record, with the rule it went under.
    let all = glass_audit::read(&h.audit_path(), |_| true).unwrap();
    let note = all.last().unwrap();
    assert_eq!(note.event.kind, EventKind::LogTrimmed);
    assert!(
        note.event.summary.contains("older than 30 days"),
        "{}",
        note.event.summary
    );
    assert_eq!(note.event.detail["through_seq"], trimmed.through_seq);
    // The task's records are those still kept.
    let kept = rt.audit_task(&h.me, task).unwrap();
    assert!(kept.len() < mine.len());
    assert_eq!(kept, whole(task));
    // The record of a removal can be what fills the file, which is then
    // closed, and (in this test, where everything is "old") goes at the
    // next pass. After that there is nothing more to remove, and nothing
    // more is said.
    let mut passes = 0;
    while rt.trim_audit_as_of(later.unwrap()).is_some() {
        passes += 1;
        assert!(passes < 5);
    }
    let after = rt.audit_verify(&root).unwrap();
    assert_eq!(rt.trim_audit_as_of(later.unwrap()), None);
    assert_eq!(rt.audit_verify(&root).unwrap(), after);

    // An org that keeps everything: nothing goes, however old.
    let rt = common::rebuild_segmenting(&h, &rule(0), segments);
    for n in 0..20 {
        rt.append_audit(AuditEvent::new(
            EventKind::DaemonStarted,
            format!("kept for good, {n}"),
        ));
    }
    let ages_on = Timestamp::from_unix_seconds(Timestamp::now().unix_seconds() + 4000 * 24 * 3600);
    assert_eq!(rt.trim_audit_as_of(ages_on.unwrap()), None);
    let report = rt.audit_verify(&root).unwrap();
    assert_eq!(report.first_seq, after.first_seq);
    assert!(report.records > after.records);
}
