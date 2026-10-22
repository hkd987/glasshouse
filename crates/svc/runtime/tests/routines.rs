//! Routines through the pipeline (DA-4, ADR 0044): an agent saves one over
//! the task socket and the person approves it; it runs again as a task
//! with the scope it was saved with, when they start it or on its
//! schedule; undoing the save takes it away. The test plays the agent.

#![allow(clippy::unwrap_used)]

mod common;

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

    // On its schedule: in its minute on a weekday, as its person, and not
    // a minute later or on a Saturday.
    let me = |login: &str| (login == h.me.name).then(|| h.me.clone());
    let started = h.rt.run_due_routines(Weekday::Thursday, 9, 0, me);
    assert_eq!(started.len(), 1);
    let (which, scheduled) = &started[0];
    assert_eq!(which, &format!("{}/invoice-count", h.me.name));
    let scheduled = scheduled.as_ref().unwrap();
    assert_eq!(
        started_detail(&h, scheduled.id)["routine"]["scheduled"],
        true
    );
    assert!(
        h.rt.run_due_routines(Weekday::Thursday, 9, 1, me)
            .is_empty()
    );
    assert!(
        h.rt.run_due_routines(Weekday::Saturday, 9, 0, me)
            .is_empty()
    );
    // Someone it can't find has none.
    assert!(
        h.rt.run_due_routines(Weekday::Thursday, 9, 0, |_| None)
            .is_empty()
    );
    for task in [run.id, scheduled.id] {
        h.rt.stop(&h.me, task).unwrap();
    }

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
        h.rt.run_due_routines(Weekday::Thursday, 9, 0, me)
            .is_empty()
    );
}
