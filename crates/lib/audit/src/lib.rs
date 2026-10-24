//! Tamper-evident audit log (PRD "Audit, attribution and undo", AU-1 to AU-3).
//!
//! Every agent action and broker decision becomes one [`AuditRecord`] in an
//! append-only, hash-chained file on the device. Auditors rebuild any task end
//! to end from it (AU-3) and [`verify`] proves nothing was edited, dropped or
//! reordered. Streaming to the org's SIEM (AU-2) reads the same file.

mod event;
mod log;

pub use event::{ApprovalRef, AuditEvent, EventKind, Outcome, PolicyDecision};
pub use log::{
    AuditError, AuditLog, AuditRecord, Durability, GENESIS, VerifyReport, read, read_since, verify,
};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use glass_types::TaskId;

    use super::*;

    fn event(summary: &str) -> AuditEvent {
        AuditEvent::new(EventKind::Action, summary)
    }

    fn write_three(path: &std::path::Path) {
        let log = AuditLog::open(path, Durability::Buffered).unwrap();
        for s in ["one", "two", "three"] {
            log.append(event(s)).unwrap();
        }
    }

    #[test]
    fn appends_chain_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        write_three(&path);

        let report = verify(&path).unwrap();
        assert_eq!(report.records, 3);

        let records = read(&path, |_| true).unwrap();
        assert_eq!(records[0].prev, GENESIS);
        assert_eq!(records.iter().map(|r| r.seq).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(records[2].event.summary, "three");
    }

    #[test]
    fn a_tasks_records_come_from_the_index_as_a_whole_read_finds_them() {
        // The speed audit (ADR 0051): a task's records without reading the
        // whole log, the same as a whole read, before and after appends.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let (a, b) = (TaskId::new(), TaskId::new());
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        let on = |task: TaskId, summary: &str| {
            let mut e = event(summary);
            e.task = Some(task);
            e
        };
        for i in 0..5 {
            log.append(on(a, &format!("a{i}"))).unwrap();
            log.append(on(b, &format!("b{i}"))).unwrap();
            log.append(event("no task")).unwrap();
        }
        let whole = |task: TaskId| read(&path, |r| r.event.task == Some(task)).unwrap();
        assert_eq!(log.task_records(a).unwrap(), whole(a));
        // Appends after the index is built are found too.
        log.append(on(b, "b5")).unwrap();
        let found = log.task_records(b).unwrap();
        assert_eq!(found, whole(b));
        assert_eq!(found.last().unwrap().event.summary, "b5");
        assert!(log.task_records(TaskId::new()).unwrap().is_empty());
        // Reopened, the index is built afresh from the file.
        drop(log);
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        log.append(on(a, "a5")).unwrap();
        assert_eq!(log.task_records(a).unwrap(), whole(a));
        assert_eq!(log.task_records(a).unwrap().len(), 6);
    }

    #[test]
    fn the_last_day_is_read_from_the_end_as_a_whole_read_finds_it() {
        // The speed audit (ADR 0051): glassd's report counts the last day's
        // blocked actions every minute; across many chunks, the same as a
        // read of the whole log.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        for i in 0..3_000 {
            let mut e = event(&format!("record {i} {}", "x".repeat(i % 97)));
            if i % 7 == 0 {
                e.outcome = Some(Outcome::Blocked);
            }
            log.append(e).unwrap();
        }
        assert!(std::fs::metadata(&path).unwrap().len() > 4 * 64 * 1024);
        let blocked = |r: &AuditRecord| r.event.outcome == Some(Outcome::Blocked);
        let since = read(&path, |_| true).unwrap()[0].ts.unix_seconds();
        assert_eq!(
            read_since(&path, since, blocked).unwrap(),
            read(&path, blocked).unwrap()
        );
        assert_eq!(read_since(&path, since, |_| true).unwrap().len(), 3_000);
        assert!(
            read_since(&path, since + 3_600, |_| true)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reopening_continues_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        write_three(&path);
        let log = AuditLog::open(&path, Durability::Sync).unwrap();
        let record = log.append(event("four")).unwrap();
        assert_eq!(record.seq, 3);
        assert_eq!(verify(&path).unwrap().records, 4);
    }

    #[test]
    fn editing_a_record_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        write_three(&path);
        let text = fs::read_to_string(&path)
            .unwrap()
            .replace("\"two\"", "\"TWO\"");
        fs::write(&path, text).unwrap();
        let err = verify(&path).unwrap_err();
        assert!(matches!(err, AuditError::Corrupt { line: 2, .. }), "{err}");
    }

    #[test]
    fn dropping_a_record_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        write_three(&path);
        let text = fs::read_to_string(&path).unwrap();
        let without_second: Vec<&str> = text
            .lines()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, l)| l)
            .collect();
        fs::write(&path, without_second.join("\n") + "\n").unwrap();
        let err = verify(&path).unwrap_err();
        assert!(matches!(err, AuditError::Corrupt { line: 2, .. }), "{err}");
    }

    #[test]
    fn rehashing_an_edited_record_still_breaks_the_chain() {
        // An attacker who edits a body and recomputes its hash still breaks
        // the next record's `prev` link.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        write_three(&path);
        let text = fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        let v: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        let mut body = v["body"].clone();
        body["event"]["summary"] = "forged".into();
        let body_text = serde_json::to_string(&body).unwrap();
        let hash = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(body_text.as_bytes()))
        };
        lines[1] = format!("{{\"hash\":\"{hash}\",\"body\":{body_text}}}");
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        let err = verify(&path).unwrap_err();
        assert!(matches!(err, AuditError::Corrupt { line: 3, .. }), "{err}");
    }

    #[test]
    fn torn_tail_is_truncated_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        write_three(&path);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"hash\":\"abc\",\"bo")
            .unwrap();

        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        log.append(event("after recovery")).unwrap();
        let records = read(&path, |_| true).unwrap();
        assert_eq!(records.len(), 5);
        assert_eq!(records[3].event.kind, EventKind::LogRecovered);
        assert_eq!(records[3].event.detail["dropped_bytes"], 17);
    }

    // AuditLog.tla, AU2_VerifyNeverCallsAGoodLogCorrupt and
    // AU2_TheReportReadsAGoodLog: a write under way, or one a full disk or
    // a crash cut short, ends the file in part of a line. Every reader took
    // it for corruption: `glassctl verify` failed, and glassd reported no
    // blocked actions to the console.
    #[test]
    fn au2_a_record_still_being_written_is_not_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        write_three(&path);
        for unfinished in [&br#"{"hash":"9a"#[..], &[b'x'; 70_000][..]] {
            let records = verify(&path).unwrap().records;
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(unfinished).unwrap();
            drop(file);

            let report = verify(&path).unwrap();
            assert_eq!(report.records, records);
            assert_eq!(report.unfinished, unfinished.len() as u64);
            let all = read(&path, |_| true).unwrap();
            assert_eq!(all.len() as u64, records);
            assert_eq!(read_since(&path, 0, |_| true).unwrap(), all);
            // Opening it for writing cuts it off, as before.
            AuditLog::open(&path, Durability::Buffered).unwrap();
            let report = verify(&path).unwrap();
            assert_eq!(report.unfinished, 0);
            assert_eq!(
                read(&path, |_| true).unwrap().last().unwrap().event.kind,
                EventKind::LogRecovered
            );
        }
    }

    #[test]
    fn read_filters_by_task() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        let task = TaskId::new();
        for i in 0..4 {
            let mut e = event(&format!("e{i}"));
            if i % 2 == 0 {
                e.task = Some(task);
            }
            log.append(e).unwrap();
        }
        let mine = read(&path, |r| r.event.task == Some(task)).unwrap();
        assert_eq!(mine.len(), 2);
    }

    #[test]
    fn large_logs_reopen_from_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        let mut e = event("padding");
        e.detail = serde_json::json!({ "pad": "x".repeat(10_000) });
        for _ in 0..20 {
            log.append(e.clone()).unwrap();
        }
        drop(log);
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        assert_eq!(log.append(event("next")).unwrap().seq, 20);
        assert_eq!(verify(&path).unwrap().records, 21);
    }
}
