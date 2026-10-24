//! Routines through the pipeline (DA-4, ADR 0044): an agent saves one over
//! the task socket and the person approves it; it runs again as a task
//! with the scope it was saved with, when they start it or on its
//! schedule; undoing the save takes it away. The test plays the agent.

#![allow(clippy::unwrap_used)]

use crate::common;

use glass_api::{AnswerApproval, ApprovalAnswer, ApprovalScope, TaskRequest, UndoRequest};
use glass_app_routines::Weekday;
use glass_runtime::RuntimeError;
use glass_types::TaskId;
use serde_json::json;

use crate::common::{Harness, eventually, harness_with, tool};

/// An agent that may save and read routines, for a person who may run it.
const POLICY: &str = r#"
[[group]]
name = "writers"
members = ["{me}"]
agents = ["writer"]

[[agent]]
id = "writer"
allowed_actions = ["files.*", "routines.*"]
"#;

const SCRIPT: &str = r#"const names = call("files_list", { path: "Invoices" }).text.split("\n");
return names.length + " invoices";"#;

fn writer_request(h: &Harness) -> TaskRequest {
    let mut request = h.request(&["Invoices"], &["Reports"]);
    request.agent = "writer".parse().unwrap();
    request.prompt = "Tell me how many invoices there are, every weekday".into();
    request
}

fn started_detail(h: &Harness, task: TaskId) -> serde_json::Value {
    glass_audit::read(&h.audit_path(), |r| {
        r.event.task == Some(task) && r.event.summary.starts_with("Started")
    })
    .unwrap()
    .remove(0)
    .event
    .detail
}

#[tokio::test]
async fn da4_a_saved_routine_runs_again_with_its_scope_when_started_and_on_its_schedule() {
    let h = harness_with(POLICY);
    let saving = h.rt.start(&h.me, writer_request(&h)).unwrap().id;
    let agent = std::sync::Arc::new(h.agent(saving).await);

    // The agent saves it; the person sees the script and approves.
    let pending = {
        let agent = std::sync::Arc::clone(&agent);
        tokio::spawn(async move {
            tool(
                &agent,
                "routines_save",
                json!({
                    "name": "Invoice count",
                    "request": "Tell me how many invoices there are.",
                    "script": SCRIPT,
                    "schedule": "weekdays 09:00",
                    "note": "First version",
                }),
            )
            .await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(ask.summary, "Save the routine Invoice count");
    assert!(
        ask.items
            .iter()
            .any(|i| i == "  return names.length + \" invoices\";"),
        "{:?}",
        ask.items
    );
    assert!(ask.items.iter().any(|i| i.starts_with("Calls: files_list")));
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            },
        },
    )
    .unwrap();
    let saved = pending.await.unwrap();
    assert!(!saved.is_error, "{}", saved.text());

    let mine = h.rt.routines_of(&h.me);
    assert_eq!(mine.len(), 1);
    let routine = &mine[0];
    assert_eq!(
        (routine.routine.as_str(), routine.runs.as_str()),
        ("invoice-count", "every weekday at 09:00")
    );
    // The scope of the task that saved it (DA-4).
    assert_eq!(routine.read, [h.home().join("Invoices")]);
    assert_eq!(routine.write, [h.home().join("Reports")]);

    // Started by the person: a task of its own, as the agent that saved
    // it, with that scope and its request.
    let run = h.rt.run_routine(&h.me, "invoice-count").unwrap();
    assert_eq!(run.title, "Invoice count");
    assert_eq!(run.agent.as_str(), "writer");
    let detail = started_detail(&h, run.id);
    assert_eq!(
        detail["routine"],
        json!({ "id": "invoice-count", "version": 1, "scheduled": false })
    );
    let mut grants: Vec<(String, bool)> = detail["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["name"].as_str().unwrap().to_owned(),
                g["writable"].as_bool().unwrap(),
            )
        })
        .collect();
    grants.sort();
    assert_eq!(
        grants,
        [("Invoices".into(), false), ("Reports".into(), true)]
    );
    let shown = h.rt.get(&h.me, run.id).unwrap();
    assert_eq!(shown.prompt, "Tell me how many invoices there are.");
    assert!(
        shown
            .activity
            .iter()
            .any(|a| a.text.ends_with("(routine invoice-count version 1)")),
        "{:?}",
        shown.activity
    );
    assert!(matches!(
        h.rt.run_routine(&h.me, "not-mine"),
        Err(RuntimeError::NotFound(_))
    ));
    // One run at a time (ADR 0051).
    h.rt.stop(&h.me, run.id).unwrap();
    eventually(|| !h.rt.get(&h.me, run.id).unwrap().view.state.is_live()).await;

    // On its schedule: in its minute on a weekday, as its person, and not
    // a minute later or on a Saturday.
    let me = |login: &str| (login == h.me.name).then(|| h.me.clone());
    let started = h.rt.run_due_routines(Weekday::Thursday, 9, 0, 1000, me);
    assert_eq!(started.len(), 1);
    let (which, scheduled) = &started[0];
    assert_eq!(which, &format!("{}/invoice-count", h.me.name));
    let scheduled = scheduled.as_ref().unwrap();
    assert_eq!(
        started_detail(&h, scheduled.id)["routine"]["scheduled"],
        true
    );
    assert!(
        h.rt.run_due_routines(Weekday::Thursday, 9, 1, 1001, me)
            .is_empty()
    );
    assert!(
        h.rt.run_due_routines(Weekday::Saturday, 9, 0, 1002, me)
            .is_empty()
    );
    // Someone it can't find has none.
    assert!(
        h.rt.run_due_routines(Weekday::Thursday, 9, 0, 1003, |_| None)
            .is_empty()
    );
    h.rt.stop(&h.me, scheduled.id).unwrap();

    // Undoing the task that saved it takes the routine away.
    let report =
        h.rt.undo(
            &h.me,
            UndoRequest {
                task: saving,
                to: None,
                changes: vec![],
                dry_run: false,
            },
        )
        .await
        .unwrap();
    assert!(report.not_undone.is_empty(), "{report:?}");
    assert!(h.rt.routines_of(&h.me).is_empty());
    assert!(h.rt.run_routine(&h.me, "invoice-count").is_err());
    assert!(
        h.rt.run_due_routines(Weekday::Thursday, 9, 0, 2000, me)
            .is_empty()
    );
}

/// `name` kept as one of the test user's routines, as an approved save
/// keeps it: run by `writer`, on `schedule`.
fn kept(h: &Harness, name: &str, schedule: Option<&str>) -> String {
    let routines = common::routines(h.dir.path());
    let draft = glass_app_routines::Draft {
        routine: None,
        name: Some(name.into()),
        request: "Tell me how many invoices there are.".into(),
        script: SCRIPT.into(),
        schedule: schedule.map(str::to_owned),
        note: "First version".into(),
    };
    let scope = glass_app_routines::Scope {
        agent: "writer".parse().unwrap(),
        read: vec![h.home().join("Invoices")],
        write: vec![h.home().join("Reports")],
        task: TaskId::new(),
    };
    let routine = routines.plan(&h.me.name, &h.home(), draft, scope).unwrap();
    let owner = glass_platform::fs::Owner {
        uid: h.me.uid,
        gid: h.me.gid,
    };
    routines.keep(routine, &h.home(), owner).unwrap().id
}

#[tokio::test]
async fn da4_a_routine_runs_one_at_a_time() {
    // ADR 0051: Run now twice ran it twice at once, and so did its schedule
    // while a run was going.
    let h = harness_with(POLICY);
    let id = kept(&h, "Invoice count", Some("weekdays 09:00"));
    let first = h.rt.run_routine(&h.me, &id).unwrap();
    let again = h.rt.run_routine(&h.me, &id).unwrap_err();
    assert!(matches!(again, RuntimeError::Conflict(_)), "{again}");
    assert!(again.to_string().contains("running now"), "{again}");
    let me = |login: &str| (login == h.me.name).then(|| h.me.clone());
    let started = h.rt.run_due_routines(Weekday::Thursday, 9, 0, 1000, me);
    assert_eq!(started.len(), 1);
    let skipped = started[0].1.as_ref().unwrap_err();
    assert!(matches!(skipped, RuntimeError::Conflict(_)), "{skipped}");
    h.rt.stop(&h.me, first.id).unwrap();
    eventually(|| !h.rt.get(&h.me, first.id).unwrap().view.state.is_live()).await;
    let next = h.rt.run_routine(&h.me, &id).unwrap();
    h.rt.stop(&h.me, next.id).unwrap();
}

#[tokio::test]
async fn da4_a_scheduled_minute_starts_its_routines_once_whatever_glassd_or_the_clock_do() {
    // ADR 0051: glassd restarting in the minute a routine started on its
    // schedule started it again.
    let h = harness_with(POLICY);
    kept(&h, "Invoice count", Some("weekdays 09:00"));
    let me = |login: &str| (login == h.me.name).then(|| h.me.clone());
    let thursday_nine = 20_000;
    let first =
        h.rt.run_due_routines(Weekday::Thursday, 9, 0, thursday_nine, me);
    assert_eq!(first.len(), 1);
    let task = first[0].1.as_ref().unwrap().id;
    h.rt.stop(&h.me, task).unwrap();
    eventually(|| !h.rt.get(&h.me, task).unwrap().view.state.is_live()).await;
    // The same minute again: a restarted glassd's first tick, or the clock
    // set back a minute.
    let again =
        h.rt.run_due_routines(Weekday::Thursday, 9, 0, thursday_nine, me);
    assert!(again.is_empty(), "{again:?}");
    // A minute that comes again when daylight saving ends: an hour on, the
    // clock shows 09:00 again, but its local number is the earlier one's.
    let later =
        h.rt.run_due_routines(Weekday::Thursday, 9, 0, thursday_nine - 1, me);
    assert!(later.is_empty(), "{later:?}");
    // A new store over the same state, as after a restart, holds the claim.
    let restarted = glass_app_routines::Routines::new(h.dir.path().join("state/routines"));
    assert!(!restarted.claim_minute(thursday_nine).unwrap());
    // The next week's 09:00 runs.
    let next_week =
        h.rt.run_due_routines(Weekday::Thursday, 9, 0, thursday_nine + 7 * 24 * 60, me);
    let next = next_week[0].1.as_ref().unwrap().id;
    h.rt.stop(&h.me, next).unwrap();
}

#[tokio::test]
async fn da4_a_run_a_restart_cut_off_is_kept() {
    // ADR 0051: a run going when glassd stopped was never kept: its end
    // never came in the process that started it.
    let h = harness_with(POLICY);
    let id = kept(&h, "Invoice count", None);
    let run = h.rt.run_routine(&h.me, &id).unwrap();
    // glassd stops here; the next one starts over the same state.
    let restarted = glass_app_routines::Routines::new(h.dir.path().join("state/routines"));
    assert_eq!(restarted.recover(), 1);
    let runs = restarted.runs(&h.me.name, &id);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].task, run.id);
    assert_eq!(runs[0].outcome, glass_app_routines::Outcome::Failed);
    assert_eq!(runs[0].said, glass_app_routines::store::CUT_OFF);
    // Kept once: a second start finds nothing more to keep.
    assert_eq!(restarted.recover(), 0);
    // And a run that ends in the process that started it is kept once,
    // as it went.
    h.rt.stop(&h.me, run.id).unwrap();
    eventually(|| {
        restarted.runs(&h.me.name, &id)[0].outcome == glass_app_routines::Outcome::Stopped
    })
    .await;
    assert_eq!(restarted.runs(&h.me.name, &id).len(), 1);
    assert_eq!(restarted.recover(), 0);
}
