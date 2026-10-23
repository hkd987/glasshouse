//! What a task did, as code this computer writes (NV-21, DA-4, ADR 0046):
//! a finished task's calls compile into replay code; the same request done
//! the same way again says so; the person saves it as a routine with no
//! model writing it; an agent saves one from what its task did, and the
//! approval shows the code. The test plays the agent.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use glass_api::{AnswerApproval, ApprovalAnswer, ApprovalScope, RoutineFromTask, TileState};
use glass_runtime::{Caller, RuntimeError};
use glass_types::TaskId;
use serde_json::json;

use crate::common::{Harness, eventually, harness_with, tool};

const POLICY: &str = r#"
[[group]]
name = "repeaters"
members = ["{me}"]
agents = ["repeater"]

[[agent]]
id = "repeater"
allowed_actions = ["files.*", "routines.*"]
"#;

const PROMPT: &str = "Read my invoices and note in Reports how many there are";

/// Starts the request and plays the agent: lists the invoices, reads each
/// and notes the count. Returns the task and its socket's client.
async fn note_the_invoices(h: &Harness, prompt: &str) -> (TaskId, glass_rpc::Client) {
    let mut request = h.request(&["Invoices"], &["Reports"]);
    request.agent = "repeater".parse().unwrap();
    request.prompt = prompt.into();
    let task = h.rt.start(&h.me, request).unwrap().id;
    (task, play(h, task).await)
}

/// Plays the agent of `task`: the same calls.
async fn play(h: &Harness, task: TaskId) -> glass_rpc::Client {
    let agent = h.agent(task).await;
    let listed = tool(&agent, "files_list", json!({ "path": "Invoices" })).await;
    let names: Vec<String> = listed.structured_content.unwrap()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_owned())
        .collect();
    for name in &names {
        let read = tool(
            &agent,
            "files_read",
            json!({ "path": format!("Invoices/{name}") }),
        )
        .await;
        assert!(!read.is_error, "{}", read.text());
    }
    let wrote = tool(
        &agent,
        "files_write",
        json!({ "path": "Reports/count.txt", "content": format!("{} invoices", names.len()) }),
    )
    .await;
    assert!(!wrote.is_error, "{}", wrote.text());
    agent
}

async fn done(h: &Harness, task: TaskId) {
    eventually(|| h.rt.get(&h.me, task).unwrap().view.state == TileState::Done).await;
}

fn notes(h: &Harness, task: TaskId) -> Vec<String> {
    h.rt.get(&h.me, task)
        .unwrap()
        .activity
        .into_iter()
        .map(|a| a.text)
        .collect()
}

#[tokio::test]
async fn nv21_a_request_done_the_same_way_twice_becomes_a_routine_this_computer_wrote() {
    let h = harness_with(POLICY);
    let (first, _agent) = note_the_invoices(&h, PROMPT).await;
    done(&h, first).await;
    assert!(
        !notes(&h, first)
            .iter()
            .any(|n| n.contains("The same steps")),
        "the first time is just the first time"
    );

    // Its calls as code: a loop over the listing, then the note.
    let draft = h.rt.routine_draft(&h.me, first).unwrap();
    assert_eq!(draft.request, PROMPT);
    assert!(
        draft
            .script
            .contains("for (const item of r0.data.entries || []) {\n  const s0 = need(call(\"files_read\", {\"path\": \"Invoices/\" + item.name})"),
        "{}",
        draft.script
    );
    assert!(
        draft.script.contains(
            "call(\"files_write\", {\"content\": \"3 invoices\", \"path\": \"Reports/count.txt\"})"
        ),
        "{}",
        draft.script
    );
    assert_eq!(draft.steps.len(), 3);
    assert_eq!(draft.read, [h.home().join("Invoices")]);

    // The same request again, done the same way: it says so.
    let (second, _agent) = note_the_invoices(
        &h,
        "  read my Invoices and note in Reports how many there are.",
    )
    .await;
    done(&h, second).await;
    let said = notes(&h, second);
    assert!(
        said.iter().any(|n| n.starts_with(&format!(
            "The same steps as task {} for this request",
            first.short()
        ))),
        "{said:?}"
    );
    // The record of its end, written just after the tile says Done.
    let finished = || {
        glass_audit::read(&h.audit_path(), |r| {
            r.event.task == Some(second) && r.event.kind == glass_audit::EventKind::TaskFinished
        })
        .unwrap()
    };
    eventually(|| !finished().is_empty()).await;
    assert_eq!(finished()[0].event.detail["repeats"], json!(first));

    // The person saves it: their own act, no ask, on the record.
    let saved =
        h.rt.routine_from_task(
            &h.me,
            RoutineFromTask {
                task: second,
                name: "Invoice count".into(),
                schedule: Some("weekdays 09:00".into()),
            },
        )
        .unwrap();
    assert_eq!(
        (saved.routine.as_str(), saved.runs.as_str()),
        ("invoice-count", "every weekday at 09:00")
    );
    assert_eq!(saved.calls, ["files_list", "files_read", "files_write"]);
    assert_eq!(
        (saved.read.clone(), saved.write.clone()),
        (
            vec![h.home().join("Invoices")],
            vec![h.home().join("Reports")]
        )
    );
    let kept = common::routines(h.dir.path())
        .record(&h.me.name, "invoice-count", 1)
        .unwrap();
    assert_eq!(
        kept.script,
        h.rt.routine_draft(&h.me, second).unwrap().script
    );
    assert_eq!(kept.script, draft.script, "the same code both times");
    let on_record = glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == glass_audit::EventKind::RoutineSaved
    })
    .unwrap();
    assert_eq!(on_record[0].event.task, Some(second));
    // Its run is a routine already: it isn't one to suggest.
    let run = h.rt.run_routine(&h.me, "invoice-count").unwrap().id;
    let _agent = play(&h, run).await;
    done(&h, run).await;
    assert!(
        !notes(&h, run).iter().any(|n| n.contains("The same steps")),
        "{:?}",
        notes(&h, run)
    );

    // Only the person's own, and only once it's done.
    let stranger = Caller {
        name: "mallory".into(),
        uid: 4242,
        gid: 4242,
        home: PathBuf::from("/home/mallory"),
    };
    assert!(matches!(
        h.rt.routine_draft(&stranger, first),
        Err(RuntimeError::NotFound(_))
    ));
    let (running, _agent) = note_the_invoices(&h, PROMPT).await;
    let err =
        h.rt.routine_from_task(
            &h.me,
            RoutineFromTask {
                task: running,
                name: "Too soon".into(),
                schedule: None,
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("still running"), "{err}");
    done(&h, running).await;
}

#[tokio::test]
async fn nv21_an_agent_saves_what_its_task_did_and_the_person_sees_that_code() {
    let h = harness_with(POLICY);
    let (task, agent) = note_the_invoices(&h, PROMPT).await;
    let agent = std::sync::Arc::new(agent);
    let pending = {
        let agent = std::sync::Arc::clone(&agent);
        tokio::spawn(async move {
            tool(
                &agent,
                "routines_save",
                json!({
                    "name": "Invoice count",
                    "request": "Note in Reports how many invoices there are.",
                    "from_this_task": true,
                    "schedule": "daily 18:00",
                    "note": "From what I just did",
                }),
            )
            .await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert!(
        ask.items
            .iter()
            .any(|i| i == "  for (const item of r0.data.entries || []) {"),
        "the code this computer wrote: {:?}",
        ask.items
    );
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
    assert_eq!(mine[0].runs, "every day at 18:00");
    done(&h, task).await;

    // A task that only read has nothing to replay, and says why.
    let mut request = h.request(&["Invoices"], &[]);
    request.agent = "repeater".parse().unwrap();
    request.prompt = "How many invoices are there?".into();
    let reader = h.rt.start(&h.me, request).unwrap().id;
    let agent = h.agent(reader).await;
    tool(&agent, "files_list", json!({ "path": "Invoices" })).await;
    let refused = tool(
        &agent,
        "routines_save",
        json!({ "name": "Count", "request": "Count them", "from_this_task": true, "note": "x" }),
    )
    .await;
    assert!(refused.is_error);
    assert!(refused.text().contains("only read"), "{}", refused.text());
    done(&h, reader).await;
    let err = h.rt.routine_draft(&h.me, reader).unwrap_err();
    assert!(err.to_string().contains("only read"), "{err}");
}
