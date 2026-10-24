//! The speed audit (the owner, 2026-10-23: "a full speed audit on all
//! features to ensure the OS is as fast as it can be"): each feature timed
//! through the whole path an agent's call takes (the task socket, policy,
//! the audit record, the snapshot a write takes first, the provider), on
//! this machine's CPU, with data the size a person has: a 500-message
//! inbox, a 2,000-file folder, a month of meetings, a 2,000-person
//! directory. The dev VM's `glass-e2e perf` holds the budgets; this finds
//! what grows with the data and where the time goes.
//!
//! Ignored by `make test`. Run with the shipping optimization:
//! `cargo test --profile vm -p glass-runtime --test it speed:: -- --ignored --nocapture --test-threads 1`

#![allow(clippy::unwrap_used, clippy::print_stdout)]

use std::fs;
use std::time::{Duration, Instant};

use crate::common::*;
use glass_managed::directory::Applied;
use glass_managed::{DirectoryRelease, Person};
use serde_json::{Value, json};

const N: usize = 20;

struct Row {
    what: String,
    samples: Vec<Duration>,
}

fn row(what: impl Into<String>, mut samples: Vec<Duration>) -> Row {
    let what = what.into();
    eprintln!("  measured: {what}");
    samples.sort();
    Row { what, samples }
}

fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

fn print(rows: &[Row]) {
    println!(
        "\n{:<58} {:>4} {:>9} {:>9} {:>9}",
        "feature", "n", "p50 ms", "p95 ms", "max ms"
    );
    for r in rows {
        let n = r.samples.len();
        let at = |q: f64| r.samples[((n as f64 - 1.0) * q).round() as usize];
        println!(
            "{:<58} {:>4} {:>9} {:>9} {:>9}",
            r.what,
            n,
            ms(at(0.5)),
            ms(at(0.95)),
            ms(r.samples[n - 1])
        );
    }
}

/// `n` timed calls of `call` after two untimed ones.
async fn timed<F, Fut>(n: usize, mut call: F) -> Vec<Duration>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for i in 0..2 {
        call(i).await;
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let started = Instant::now();
        call(i + 2).await;
        out.push(started.elapsed());
    }
    out
}

async fn ok(agent: &glass_rpc::Client, name: &str, args: Value) -> Value {
    let result = tool(agent, name, args).await;
    assert!(!result.is_error, "{name}: {}", result.text());
    result.structured_content.unwrap_or_default()
}

fn seed(h: &Harness) {
    let home = h.home();
    // A big folder, and a large file.
    fs::create_dir_all(home.join("Big")).unwrap();
    for i in 0..2_000 {
        fs::write(
            home.join(format!("Big/note-{i:05}.txt")),
            "a line of notes\n".repeat(128),
        )
        .unwrap();
    }
    fs::write(home.join("Big/large.csv"), "a,b,c,d\n".repeat(128 * 1024)).unwrap();
    // A 500-message inbox, as the local store keeps it.
    fs::create_dir_all(home.join("Mail/Inbox")).unwrap();
    for i in 0..500 {
        let body = format!(
            "Hello,\n\n{}\nThanks,\nSam\n",
            "Some words about the quarter. ".repeat(60)
        );
        fs::write(
            home.join(format!("Mail/Inbox/{}-{i}-note.eml", 1_792_000_000 + i * 600)),
            format!(
                "From: Sam Lee <sam{i}@example.com>\nTo: demo@corp.example\nSubject: Note {i}\nDate: 2026-10-{:02}T08:00:00Z\nMessage-ID: <{i}@example.com>\n\n{body}",
                1 + i % 28
            ),
        )
        .unwrap();
    }
    // A month of meetings.
    fs::create_dir_all(home.join("Calendar/Events")).unwrap();
    for i in 0..300 {
        let day = 1 + i % 30;
        let hour = 8 + (i / 30) % 9;
        let id = format!("ev-{i:04}");
        fs::write(
            home.join(format!("Calendar/Events/{id}.json")),
            json!({
                "id": id, "title": format!("Meeting {i}"), "kind": "event",
                "start": format!("2026-10-{day:02}T{hour:02}:00"), "end": format!("2026-10-{day:02}T{hour:02}:30"),
                "location": "Room 2B", "notes": "", "attendees": ["sam@example.com", "mei@example.com"],
                "organizer": h.me.name, "attribution": ""
            })
            .to_string(),
        )
        .unwrap();
    }
    // An org of 2,000.
    h.directory.set(Applied {
        version: 1,
        release: DirectoryRelease {
            people: (0..2_000)
                .map(|i| Person {
                    login: format!("person{i}"),
                    name: format!("Person {i} Example"),
                    email: format!("person{i}@corp.example"),
                    title: "Analyst".into(),
                    department: ["Finance", "Legal", "Sales", "IT"][i % 4].into(),
                })
                .collect(),
        },
    });
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "the speed audit: run by hand, with --profile vm"]
async fn speed_audit_every_feature_on_a_persons_worth_of_data() {
    let h = harness();
    seed(&h);
    let mut rows = Vec::new();

    // Starting a task: the request to the agent's socket answering. The
    // person's own Claude Code (attended), whose policy reaches every app.
    let mut request = h.request(&["Big", "Invoices"], &["Reports"]);
    request.agent = "claude-code".parse().unwrap();
    let starts = timed(5, |_| {
        let (h, request) = (&h, request.clone());
        async move {
            let view = h.rt.start(&h.me, request).unwrap();
            let socket = view.driver_socket.clone().unwrap();
            eventually(|| socket.exists()).await;
            h.rt.stop(&h.me, view.id).unwrap();
        }
    })
    .await;
    rows.push(row("task: start to the agent's socket", starts));

    let view = h.rt.start(&h.me, request.clone()).unwrap();
    let task = view.id;
    let socket = view.driver_socket.clone().unwrap();
    eventually(|| socket.exists()).await;
    let agent = glass_rpc::Client::connect(&socket).await.unwrap();
    let _: Value = agent
        .call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "speed", "version": "0"}}),
        )
        .await
        .unwrap();
    rows.push(row(
        "task.list (control API)",
        timed(N, |_| async { drop(h.rt.list(&h.me)) }).await,
    ));
    rows.push(row(
        "task.get (control API)",
        timed(N, |_| async { drop(h.rt.get(&h.me, task).unwrap()) }).await,
    ));

    // Files.
    rows.push(row(
        "files.list, 2,000 files",
        timed(N, |_| {
            ok(&agent, "files_list", json!({ "path": "Big" })).map_unit()
        })
        .await,
    ));
    rows.push(row(
        "files.read, 2 KB",
        timed(N, |i| {
            ok(
                &agent,
                "files_read",
                json!({ "path": format!("Big/note-{:05}.txt", i) }),
            )
            .map_unit()
        })
        .await,
    ));
    rows.push(row(
        "files.read, 1 MB",
        timed(N, |_| {
            ok(&agent, "files_read", json!({ "path": "Big/large.csv" })).map_unit()
        })
        .await,
    ));
    rows.push(row(
        "files.write (a snapshot first)",
        timed(N, |i| {
            ok(
                &agent,
                "files_write",
                json!({ "path": format!("Reports/out-{i}.txt"), "content": "written\n" }),
            )
            .map_unit()
        })
        .await,
    ));

    // Undo of everything that task wrote: 22 writes.
    let started = Instant::now();
    h.rt.undo(
        &h.me,
        glass_api::UndoRequest {
            task,
            to: None,
            changes: Vec::new(),
            dry_run: true,
        },
    )
    .await
    .unwrap();
    rows.push(row("undo preview, 22 writes", vec![started.elapsed()]));
    let started = Instant::now();
    let report =
        h.rt.undo(
            &h.me,
            glass_api::UndoRequest {
                task,
                to: None,
                changes: Vec::new(),
                dry_run: false,
            },
        )
        .await
        .unwrap();
    assert!(report.not_undone.is_empty(), "{report:?}");
    rows.push(row("undo, 22 writes", vec![started.elapsed()]));
    // A rollback holds the task until the person resumes it (AW-22).
    h.rt.resume(&h.me, task).unwrap();

    // Mail on the local store.
    let listed = ok(
        &agent,
        "mail_list",
        json!({ "folder": "inbox", "limit": 50 }),
    )
    .await;
    let ids: Vec<String> = listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect();
    rows.push(row(
        "mail.list, 500-message inbox, 50 listed",
        timed(N, |_| {
            ok(
                &agent,
                "mail_list",
                json!({ "folder": "inbox", "limit": 50 }),
            )
            .map_unit()
        })
        .await,
    ));
    rows.push(row(
        "mail.list, 500-message inbox, 1 listed (a count)",
        timed(N, |_| {
            ok(
                &agent,
                "mail_list",
                json!({ "folder": "inbox", "limit": 1 }),
            )
            .map_unit()
        })
        .await,
    ));
    rows.push(row(
        "mail.read",
        timed(N, |i| {
            ok(&agent, "mail_read", json!({ "id": ids[i % ids.len()] })).map_unit()
        })
        .await,
    ));
    rows.push(row(
        "mail.mark (a snapshot first)",
        timed(N, |i| {
            ok(
                &agent,
                "mail_mark",
                json!({ "ids": [ids[i % ids.len()]], "read": i % 2 == 0 }),
            )
            .map_unit()
        })
        .await,
    ));
    rows.push(row(
        "mail.draft (a snapshot first)",
        timed(N, |i| {
            ok(
                &agent,
                "mail_draft",
                json!({ "to": ["sam@example.com"], "subject": format!("Re: Note {i}"), "body": "Thanks." }),
            )
            .map_unit()
        })
        .await,
    ));

    // Calendar.
    rows.push(row(
        "calendar.list, a week of a 300-meeting month",
        timed(N, |_| {
            ok(
                &agent,
                "calendar_list",
                json!({ "from": "2026-10-12", "to": "2026-10-19" }),
            )
            .map_unit()
        })
        .await,
    ));
    rows.push(row(
        "calendar.read",
        timed(N, |i| {
            ok(
                &agent,
                "calendar_read",
                json!({ "id": format!("ev-{:04}", i) }),
            )
            .map_unit()
        })
        .await,
    ));

    // The directory and what an app can do.
    rows.push(row(
        "people.search, 2,000 people",
        timed(N, |i| {
            ok(
                &agent,
                "people_search",
                json!({ "query": format!("person {}", 100 + i) }),
            )
            .map_unit()
        })
        .await,
    ));
    rows.push(row(
        "apps.actions, the list",
        timed(N, |_| ok(&agent, "apps_actions", json!({})).map_unit()).await,
    ));
    rows.push(row(
        "apps.actions, 12 in full",
        timed(N, |_| {
            ok(
                &agent,
                "apps_actions",
                json!({ "actions": ["mail.list", "mail.read", "mail.draft", "mail.send", "mail.move", "mail.mark",
                                    "calendar.list", "calendar.read", "calendar.hold", "files.list", "files.read", "people.search"] }),
            )
            .map_unit()
        })
        .await,
    ));

    // The audit trail of a computer used for a while: 20,000 records of
    // other tasks around this one's.
    for i in 0..20_000 {
        let mut event = glass_audit::AuditEvent::new(
            glass_audit::EventKind::Action,
            format!("an earlier task's step {i}"),
        );
        event.task = Some(glass_types::TaskId::new());
        event.detail = json!({ "elapsed_ms": 3, "path": "Reports/out.txt" });
        h.rt.append_audit(event);
    }
    let records = h.rt.audit_task(&h.me, task).unwrap().len();
    rows.push(row(
        format!("audit, a task's records ({records})"),
        timed(5, |_| async { drop(h.rt.audit_task(&h.me, task).unwrap()) }).await,
    ));
    let started = Instant::now();
    let verified = h.rt.audit_verify(&h.me).unwrap();
    rows.push(row(
        format!("audit verify, the whole log ({} records)", verified.records),
        vec![started.elapsed()],
    ));

    print(&rows);
}

trait MapUnit {
    fn map_unit(self) -> impl std::future::Future<Output = ()>;
}

impl<F: std::future::Future<Output = Value>> MapUnit for F {
    async fn map_unit(self) {
        self.await;
    }
}
