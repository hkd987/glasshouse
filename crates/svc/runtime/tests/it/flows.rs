//! What a task did, as code this computer writes (NV-21, DA-4, ADR 0046):
//! a finished task's calls compile into replay code; the same request done
//! the same way again says so; the person saves it as a routine with no
//! model writing it; an agent saves one from what its task did, and the
//! approval shows the code. The test plays the agent.

#![allow(clippy::unwrap_used)]

use crate::common;

use std::path::PathBuf;

use glass_api::{
    AnswerApproval, AppCallResult, AppSession, ApprovalAnswer, ApprovalScope, RoutineFromTask,
    TileState, app_methods,
};
use glass_rpc::Client;
use glass_runtime::{Caller, RuntimeError};
use glass_types::TaskId;
use serde_json::{Value, json};

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

/// The Routines app (ADR 0047): a session on the app socket, as its page
/// has through its bridge.
struct RoutinesApp {
    client: Client,
    session: AppSession,
}

impl RoutinesApp {
    async fn open(h: &Harness) -> Self {
        let client = Client::connect(h.serve_apps()).await.unwrap();
        let session: AppSession = client
            .call(app_methods::APP_OPEN, json!({ "app": "routines" }))
            .await
            .unwrap();
        Self { client, session }
    }

    async fn call(&self, action: &str, input: Value) -> AppCallResult {
        self.client
            .call(
                app_methods::APP_CALL,
                json!({ "session": self.session.session, "action": action, "input": input }),
            )
            .await
            .unwrap()
    }

    async fn ok(&self, action: &str, input: Value) -> Value {
        let result = self.call(action, input).await;
        assert!(result.ok, "{action}: {}", result.text);
        result.data
    }

    /// `app.status`: with `since`, waiting up to 10 s for the person's routines to change.
    async fn status(&self, since: Option<u64>) -> glass_api::AppStatus {
        let wait = since.map_or(
            json!({}),
            |since| json!({ "wait_secs": 10, "since": since }),
        );
        let mut request = json!({ "session": self.session.session });
        request
            .as_object_mut()
            .unwrap()
            .extend(wait.as_object().unwrap().clone());
        self.client
            .call(app_methods::APP_STATUS, request)
            .await
            .unwrap()
    }

    /// Waits for the next change to the person's routines after `since`; its cursor.
    async fn changed(&self, since: u64) -> u64 {
        let status = self.status(Some(since)).await;
        assert_eq!(
            status.changed,
            ["routines"],
            "woken by a change to routines"
        );
        status.cursor
    }
}

#[tokio::test]
async fn adr0047_the_person_keeps_runs_pauses_and_reschedules_what_they_did_twice_in_routines() {
    let h = harness_with(POLICY);
    let (first, _agent) = note_the_invoices(&h, PROMPT).await;
    done(&h, first).await;
    let (second, _agent) = note_the_invoices(&h, PROMPT).await;
    done(&h, second).await;

    // Done the same way twice: offered, with the code this computer wrote.
    let app = RoutinesApp::open(&h).await;
    let repeats = app.ok("routines.repeats", json!({})).await["repeats"].clone();
    assert_eq!(repeats.as_array().unwrap().len(), 1, "{repeats}");
    assert_eq!(repeats[0]["task"], json!(second));
    assert_eq!(repeats[0]["times"], 2);
    assert!(
        repeats[0]["script"]
            .as_str()
            .unwrap()
            .contains("for (const item of r0.data.entries || [])"),
        "{repeats}"
    );

    // Kept from Routines: the person's own act, no ask.
    let kept = app
        .ok(
            "routines.keep",
            json!({ "task": second, "name": "Invoice count", "schedule": "fri 17:00" }),
        )
        .await;
    assert_eq!(kept["routine"], "invoice-count");
    assert!(h.rt.approvals(&h.me).is_empty());
    let repeats = app.ok("routines.repeats", json!({})).await;
    assert_eq!(repeats["repeats"], json!([]), "a routine now, not offered");
    let listed = app.ok("routines.list", json!({})).await["routines"][0].clone();
    assert_eq!(
        (&listed["runs"], &listed["paused"], &listed["last_run"]),
        (&json!("every Friday at 17:00"), &json!(false), &Value::Null)
    );
    assert!(listed["next_run_at"].is_string(), "{listed}");

    // Run now: a task of its own, and how it went is kept beside it.
    // Routines' window hears it start and end, without asking again.
    let cursor = app.status(None).await.cursor;
    let started = app
        .ok("routines.run", json!({ "routine": "invoice-count" }))
        .await;
    let run: TaskId = serde_json::from_value(started["task"].clone()).unwrap();
    let cursor = app.changed(cursor).await;
    let listed = app.ok("routines.list", json!({})).await["routines"][0].clone();
    assert_eq!(listed["running"], json!(run), "running now");
    let _agent = play(&h, run).await;
    done(&h, run).await;
    app.changed(cursor).await;
    eventually(|| {
        h.rt.get(&h.me, run).is_ok()
            && common::routines(h.dir.path())
                .runs(&h.me.name, "invoice-count")
                .first()
                .is_some_and(|r| r.task == run)
    })
    .await;
    let read = app
        .ok("routines.read", json!({ "routine": "invoice-count" }))
        .await;
    assert_eq!(read["last_run"]["task"], json!(run));
    assert_eq!(read["last_run"]["outcome"], "done");
    assert_eq!(read["last_run"]["model"], false, "no model asked");
    assert_eq!(read["last_run"]["scheduled"], false);
    assert_eq!(read["runs_kept"].as_array().unwrap().len(), 1);

    // Paused, its schedule doesn't start it; resumed, it does.
    app.ok(
        "routines.pause",
        json!({ "routine": "invoice-count", "paused": true }),
    )
    .await;
    let listed = app.ok("routines.list", json!({})).await["routines"][0].clone();
    assert_eq!(
        (&listed["paused"], &listed["next_run_at"]),
        (&json!(true), &Value::Null)
    );
    let me = h.me.clone();
    // Fridays at 17:00, a week apart: each its own minute.
    let due = |rt: &std::sync::Arc<glass_runtime::Runtime>, week: i64| {
        rt.run_due_routines(
            glass_app_routines::Weekday::Friday,
            17,
            0,
            week * 7 * 24 * 60,
            |_| Some(me.clone()),
        )
    };
    assert!(due(&h.rt, 1).is_empty(), "paused");
    app.ok(
        "routines.pause",
        json!({ "routine": "invoice-count", "paused": false }),
    )
    .await;
    let scheduled = due(&h.rt, 2);
    assert_eq!(scheduled.len(), 1);
    let scheduled = scheduled[0].1.as_ref().unwrap().id;
    let _agent = play(&h, scheduled).await;
    done(&h, scheduled).await;

    // A new time: its next version, the same code and scope.
    let moved = app
        .ok(
            "routines.schedule",
            json!({ "routine": "invoice-count", "schedule": "weekdays 09:00" }),
        )
        .await;
    assert_eq!(
        (&moved["version"], &moved["runs"]),
        (&json!(2), &json!("every weekday at 09:00"))
    );
    let kept = common::routines(h.dir.path());
    assert_eq!(
        kept.record(&h.me.name, "invoice-count", 2).unwrap().script,
        kept.record(&h.me.name, "invoice-count", 1).unwrap().script
    );

    // Back to the first version: on Fridays again, the person's own choice.
    let cursor = app.status(None).await.cursor;
    let used = app
        .ok(
            "routines.use",
            json!({ "routine": "invoice-count", "version": 1 }),
        )
        .await;
    assert_eq!(
        (&used["version"], &used["runs"]),
        (&json!(1), &json!("every Friday at 17:00"))
    );
    assert!(h.rt.approvals(&h.me).is_empty(), "no ask in Routines");
    app.changed(cursor).await;
    let read = app
        .ok("routines.read", json!({ "routine": "invoice-count" }))
        .await;
    assert_eq!(read["version"], 1);
    assert_eq!(read["versions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn adr0047_no_agent_starts_or_keeps_routines_and_a_repeat_can_be_let_go() {
    let h = harness_with(POLICY);
    let (first, _agent) = note_the_invoices(&h, PROMPT).await;
    done(&h, first).await;
    let (second, _agent) = note_the_invoices(&h, PROMPT).await;
    done(&h, second).await;

    // An agent allowed routines.* still can't take the person's own.
    let (task, agent) = note_the_invoices(&h, "Count the invoices again").await;
    for (name, args) in [
        ("routines_repeats", json!({})),
        (
            "routines_keep",
            json!({ "task": second, "name": "Mine now" }),
        ),
        ("routines_dismiss", json!({ "task": second })),
        ("routines_run", json!({ "routine": "anything" })),
    ] {
        let refused = tool(&agent, name, args).await;
        assert!(refused.is_error, "{name}");
        assert!(
            refused.text().contains("only the person"),
            "{name}: {}",
            refused.text()
        );
    }
    done(&h, task).await;

    // Not now: it isn't offered again.
    let app = RoutinesApp::open(&h).await;
    let repeats = app.ok("routines.repeats", json!({})).await;
    assert_eq!(repeats["repeats"].as_array().unwrap().len(), 1);
    app.ok("routines.dismiss", json!({ "task": second })).await;
    let repeats = app.ok("routines.repeats", json!({})).await;
    assert_eq!(repeats["repeats"], json!([]));
}

#[tokio::test]
async fn adr0046_the_files_it_picked_by_size_are_picked_again_by_a_round_number() {
    let h = harness_with(POLICY);
    // Two scans among the small invoices.
    for (name, size) in [("scan-01.pdf", 6_000), ("scan-02.pdf", 9_000)] {
        std::fs::write(h.home().join("Invoices").join(name), vec![b'x'; size]).unwrap();
    }
    let mut request = h.request(&["Invoices"], &["Reports"]);
    request.agent = "repeater".parse().unwrap();
    request.prompt = "Note in Reports each big file in Invoices".into();
    let task = h.rt.start(&h.me, request).unwrap().id;
    let agent = h.agent(task).await;
    let listed = tool(&agent, "files_list", json!({ "path": "Invoices" })).await;
    let entries = listed.structured_content.unwrap()["entries"].clone();
    for entry in entries.as_array().unwrap() {
        if entry["size"].as_u64().unwrap() > 1_000 {
            let name = entry["name"].as_str().unwrap();
            let noted = tool(
                &agent,
                "files_write",
                json!({ "path": format!("Reports/{name}"), "content": "Big: check it" }),
            )
            .await;
            assert!(!noted.is_error, "{}", noted.text());
        }
    }
    done(&h, task).await;

    // The code keeps the files over a round size, whatever they're called.
    let draft = h.rt.routine_draft(&h.me, task).unwrap();
    assert!(
        draft
            .script
            .contains("if (!(Number(item.size) > 1000)) continue;"),
        "{}",
        draft.script
    );
    assert!(
        draft
            .steps
            .iter()
            .any(|s| s.contains("whose size is over 1,000")),
        "{:?}",
        draft.steps
    );
}
