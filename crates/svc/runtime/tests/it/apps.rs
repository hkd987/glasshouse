//! The app bridge (UI-5, DV-3, ADR 0024): an installed app's session over
//! the app socket acts for the user who opened it, calls only what its
//! manifest declares, and runs every call through the pipeline, attributed
//! "by [app] for [user]". The test plays the app runtime.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::sync::Arc;

use glass_api::{
    AnswerApproval, AppCallResult, AppSession, AppStatus, ApprovalAnswer, ApprovalScope, TileState,
    app_methods,
};
use glass_audit::{EventKind, Outcome};
use glass_rpc::{Client, ClientError};
use glass_types::{TaskId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness, harness_with, tool};

async fn open(client: &Client, app: &str) -> AppSession {
    client
        .call(app_methods::APP_OPEN, json!({ "app": app }))
        .await
        .unwrap()
}

async fn call(client: &Client, session: TaskId, action: &str, input: Value) -> AppCallResult {
    client
        .call(
            app_methods::APP_CALL,
            json!({ "session": session, "action": action, "input": input }),
        )
        .await
        .unwrap()
}

async fn status(client: &Client, session: TaskId) -> AppStatus {
    client
        .call(app_methods::APP_STATUS, json!({ "session": session }))
        .await
        .unwrap()
}

fn reply() -> Value {
    json!({ "to": ["dana@northwind.example"], "subject": "Re: Renewal terms", "body": "Tuesday works." })
}

/// A session's records, but those its changes write before they run
/// (ADR 0054).
fn records(h: &Harness, session: TaskId) -> Vec<glass_audit::AuditRecord> {
    glass_audit::read(&h.audit_path(), |r| {
        r.event.task == Some(session) && r.event.kind != glass_audit::EventKind::ActionStarted
    })
    .unwrap()
}

// The user's decision of 2026-11-11: how long an app shows what it kept to
// work offline is IT's to set, and unset it is kept.
#[tokio::test]
async fn ui5_an_app_is_told_how_long_it_may_show_what_it_kept_offline() {
    let h = harness();
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    assert_eq!(open(&apps, "mail").await.offline_days, None);
    // Not named at all where there is no rule: an app built on an SDK from
    // before the setting reads its session as it always did.
    let plain: Value = apps
        .call(app_methods::APP_OPEN, json!({ "app": "calendar" }))
        .await
        .unwrap();
    assert!(plain.get("offline_days").is_none(), "{plain}");

    h.apply_policy_replacing("offline_cache_days = 0", "offline_cache_days = 7", "");
    assert_eq!(open(&apps, "mail").await.offline_days, Some(7));
}

#[tokio::test]
async fn ui5_au10_an_app_acts_for_its_user_by_app_for_user() {
    let h = harness();
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let mail = open(&apps, "mail").await;
    assert_eq!(
        mail.user, h.me.name,
        "the kernel's uid says who it acts for"
    );
    let tiers: Vec<(String, Tier)> = mail
        .actions
        .iter()
        .map(|a| (a.name.to_string(), a.tier))
        .collect();
    assert!(tiers.contains(&("mail.list".into(), Tier::Auto)));
    assert!(
        tiers.contains(&("mail.send".into(), Tier::Ask)),
        "{tiers:?}"
    );
    assert!(
        h.home().join("Mail/Inbox").is_dir(),
        "its folders exist before its first call"
    );

    let drafted = call(&apps, mail.session, "mail.draft", reply()).await;
    assert!(drafted.ok, "{}", drafted.text);
    let drafts = call(
        &apps,
        mail.session,
        "mail.list",
        json!({ "folder": "drafts" }),
    )
    .await;
    let draft = &drafts.data["messages"][0];
    assert_eq!(draft["subject"], "Re: Renewal terms");
    assert_eq!(draft["attribution"]["by"], "mail");
    assert_eq!(draft["attribution"]["for"], h.me.name.as_str());

    // Audited like an agent's call, with the app as the principal.
    let records = records(&h, mail.session);
    let draft = records
        .iter()
        .find(|r| {
            r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == "mail.draft")
        })
        .expect("the draft is audited");
    assert_eq!(draft.event.agent.as_ref().unwrap().as_str(), "mail");
    assert_eq!(draft.event.user.as_deref(), Some(h.me.name.as_str()));
    assert_eq!(draft.event.outcome, Some(Outcome::Completed));
    assert!(
        records
            .iter()
            .any(|r| r.event.kind == EventKind::TaskStarted && r.event.summary == "Opened Mail")
    );
    // An app is a window, not a tile, while it doesn't need the user.
    assert!(h.rt.list(&h.me).is_empty());
}

#[tokio::test]
async fn dv3_an_app_calls_only_what_its_manifest_declares() {
    let h = harness();
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let calendar = open(&apps, "calendar").await;
    for action in [
        "mail.send",
        "files.delete",
        "helpdesk.ticket.create",
        "nothing.here",
    ] {
        let refused = call(&apps, calendar.session, action, json!({})).await;
        assert!(!refused.ok, "{action}");
        assert!(
            refused.text.contains("doesn't declare") && refused.text.contains(action),
            "{}",
            refused.text
        );
    }
    // Each refusal is audited, so an app probing for more shows.
    let blocked: Vec<_> = records(&h, calendar.session)
        .into_iter()
        .filter(|r| r.event.outcome == Some(Outcome::Blocked))
        .collect();
    assert_eq!(blocked.len(), 4);
    assert!(blocked.iter().all(|r| {
        r.event
            .decision
            .as_ref()
            .unwrap()
            .rule
            .starts_with("app manifest calendar")
    }));
    assert!(
        !h.home().join("Mail/Sent").exists()
            || fs::read_dir(h.home().join("Mail/Sent")).unwrap().count() == 0
    );

    // Apps that aren't installed don't open.
    let err = apps
        .call_value(app_methods::APP_OPEN, json!({ "app": "invoice-demo" }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no app invoice-demo"), "{err}");
}

#[tokio::test]
async fn dv3_policy_still_bounds_what_an_app_declares() {
    // A group rule makes holds and events Never for this user: the app's
    // declaration doesn't widen it.
    let h = harness_with(
        r#"
[[group]]
name = "frontdesk"
members = ["{me}"]
agents = []
[group.tiers]
"calendar.create" = "never"
"#,
    );
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let calendar = open(&apps, "calendar").await;
    let create = calendar
        .actions
        .iter()
        .find(|a| a.name.as_str() == "calendar.create")
        .unwrap();
    assert_eq!(create.tier, Tier::Never);
    let refused = call(
        &apps,
        calendar.session,
        "calendar.create",
        json!({ "title": "Standup", "start": "2026-10-20T09:00", "end": "2026-10-20T09:15" }),
    )
    .await;
    assert!(!refused.ok);
    assert!(
        refused.text.starts_with("Blocked by policy"),
        "{}",
        refused.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nv3_an_open_app_hears_what_changed_across_a_restart_of_glassd() {
    // The live-updates model's counterexample (ADR 0051): a restarted
    // glassd counted from 0 again, so the cursor a page kept from before
    // was ahead of the changes after, and a change the last run made and
    // never said was lost; the page waited on, out of date.
    let before = harness();
    let socket = before.serve_apps();
    let apps = Client::connect(&socket).await.unwrap();
    let session = open(&apps, "calendar").await.session;
    for _ in 0..3 {
        before.rt.app_changed(&before.me.name, "calendar");
    }
    let kept = status(&apps, session).await.cursor;
    drop(apps);
    drop(before);

    // glassd again; the bridge opens the page's session again and relays
    // its wait as it was.
    let after = harness();
    let socket = after.serve_apps();
    let apps = Client::connect(&socket).await.unwrap();
    let session = open(&apps, "calendar").await.session;
    after.rt.app_changed(&after.me.name, "calendar");
    let started = std::time::Instant::now();
    let woke: AppStatus = apps
        .call(
            app_methods::APP_STATUS,
            json!({ "session": session, "wait_secs": 5, "since": kept }),
        )
        .await
        .unwrap();
    assert_eq!(woke.changed, ["calendar"]);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(woke.cursor > kept, "its next wait hears what comes next");
    // From there it waits as before: nothing new, nothing said.
    let quiet: AppStatus = apps
        .call(
            app_methods::APP_STATUS,
            json!({ "session": session, "wait_secs": 1, "since": woke.cursor }),
        )
        .await
        .unwrap();
    assert!(quiet.changed.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn nv3_an_open_app_hears_what_an_agent_changed_and_not_what_it_did_itself() {
    let h = harness();
    let socket = h.serve_apps();
    let apps = Client::connect(&socket).await.unwrap();
    let calendar = open(&apps, "calendar").await;
    let mail = open(&apps, "mail").await;
    let start = status(&apps, calendar.session).await.cursor;
    let news = async |since: u64| -> AppStatus {
        apps.call(
            app_methods::APP_STATUS,
            json!({ "session": calendar.session, "wait_secs": 1, "since": since }),
        )
        .await
        .unwrap()
    };

    // What Calendar's own window does is no news to it: it would read the
    // week again after every event it added, and Chat its conversation
    // after every draft it saved.
    let standup =
        json!({ "title": "Standup", "start": "2026-10-20T09:00", "end": "2026-10-20T09:15" });
    let added = call(&apps, calendar.session, "calendar.create", standup).await;
    assert!(added.ok, "{}", added.text);
    let quiet = news(start).await;
    assert!(quiet.changed.is_empty(), "{:?}", quiet.changed);

    // A hold an agent makes is: the open week shows it now, where it
    // showed at the next change of week.
    let mut request = h.request(&[], &[]);
    request.agent = "claude-code".parse().unwrap();
    let driver = h.rt.start(&h.me, request).unwrap().driver_socket.unwrap();
    eventually(|| driver.exists()).await;
    let agent = Client::connect(&driver).await.unwrap();
    let _: Value = agent
        .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}))
        .await
        .unwrap();
    let held = tool(
        &agent,
        "calendar_hold",
        json!({ "title": "Prep for Northwind", "start": "2026-10-20T14:00", "end": "2026-10-20T15:00" }),
    )
    .await;
    assert!(!held.is_error, "{}", held.text());
    let woke = news(quiet.cursor).await;
    assert_eq!(woke.changed, ["calendar"]);
    // Calendar's news, nobody else's: Mail's window waits on.
    let other: AppStatus = apps
        .call(
            app_methods::APP_STATUS,
            json!({ "session": mail.session, "wait_secs": 0, "since": start }),
        )
        .await
        .unwrap();
    assert!(other.changed.is_empty(), "{:?}", other.changed);
    // A look alone is no change: the agent reading the calendar wakes nobody.
    let read = tool(
        &agent,
        "calendar_list",
        json!({ "from": "2026-10-19", "to": "2026-10-26" }),
    )
    .await;
    assert!(!read.is_error, "{}", read.text());
    let after = news(woke.cursor).await;
    assert!(after.changed.is_empty(), "{:?}", after.changed);
}

#[tokio::test(flavor = "multi_thread")]
async fn ad3_dv3_a_policy_it_publishes_reaches_an_app_already_open() {
    // Calendar is open, as it stays all day; IT then makes adding events
    // Never for this person's group. The open app's next call is refused,
    // with no reopening (ADR 0023, amended).
    let h = harness();
    let socket = h.serve_apps();
    let apps = Client::connect(&socket).await.unwrap();
    let calendar = open(&apps, "calendar").await;
    let standup =
        json!({ "title": "Standup", "start": "2026-10-20T09:00", "end": "2026-10-20T09:15" });
    let before = call(&apps, calendar.session, "calendar.create", standup.clone()).await;
    assert!(before.ok, "{}", before.text);

    let tier = |status: &AppStatus| {
        status
            .actions
            .iter()
            .find(|a| a.name.as_str() == "calendar.create")
            .map(|a| a.tier)
    };
    let opened = status(&apps, calendar.session).await;
    assert_eq!(tier(&opened), Some(glass_types::Tier::Notify));
    // The app waits for news; the new policy wakes it with its tiers.
    let waiting = {
        let apps = Client::connect(&socket).await.unwrap();
        let session = open(&apps, "calendar").await.session;
        let cursor = status(&apps, session).await.cursor;
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let woke: AppStatus = apps
                .call(
                    app_methods::APP_STATUS,
                    json!({ "session": session, "wait_secs": 20, "since": cursor }),
                )
                .await
                .unwrap();
            (woke, started.elapsed())
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    h.apply_policy(
        r#"
[[group]]
name = "frontdesk"
members = ["{me}"]
agents = []
[group.tiers]
"calendar.create" = "never"
"#,
        "managed v2",
    );
    let (woke, took) = waiting.await.unwrap();
    assert!(
        took < std::time::Duration::from_secs(5),
        "woke after {took:?}"
    );
    assert_eq!(tier(&woke), Some(glass_types::Tier::Never));
    assert_eq!(
        tier(&status(&apps, calendar.session).await),
        Some(glass_types::Tier::Never),
        "the app's labels follow the policy it runs under"
    );
    let after = call(&apps, calendar.session, "calendar.create", standup.clone()).await;
    assert!(!after.ok);
    assert!(
        after.text.starts_with("Blocked by policy"),
        "{}",
        after.text
    );

    // And back: IT lifts it, and the same window can add events again.
    h.apply_policy("", "managed v3");
    let again = call(&apps, calendar.session, "calendar.create", standup.clone()).await;
    assert!(again.ok, "{}", again.text);

    // IT takes Calendar out of everyone's profile: the open one closes.
    h.apply_policy_replacing("\"mail\", \"calendar\", ", "\"mail\", ", "");
    let closed: Result<Value, ClientError> = apps
        .call(
            app_methods::APP_CALL,
            json!({ "session": calendar.session, "action": "calendar.create", "input": standup }),
        )
        .await;
    let refused = match closed {
        Ok(v) => serde_json::from_value::<AppCallResult>(v).map_or(true, |r| !r.ok),
        Err(_) => true,
    };
    assert!(refused, "a closed app's calls go nowhere");
}

#[tokio::test]
async fn ui5_an_apps_ask_waits_on_the_desktop_and_the_app_cannot_answer_it() {
    let h = harness();
    let apps = Arc::new(Client::connect(h.serve_apps()).await.unwrap());
    let mail = open(&apps, "mail").await;
    let session = mail.session;
    let sending = {
        let apps = Arc::clone(&apps);
        tokio::spawn(async move {
            call(
                &apps,
                session,
                "mail.send",
                json!({ "messages": [reply()] }),
            )
            .await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;

    // The app can say it is waiting, and on what.
    let waiting = status(&apps, session).await.waiting.expect("waiting");
    assert!(waiting.summary.contains("Send"), "{}", waiting.summary);
    assert!(waiting.items[0].contains("dana@northwind.example"));
    // The ask is a tile on the desktop, asked as the app.
    let tiles = h.rt.list(&h.me);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].state, TileState::NeedsYou);
    assert_eq!(tiles[0].agent.as_str(), "mail");
    // The app socket answers no approvals: that stays with the user.
    let ask = h.rt.approvals(&h.me).remove(0);
    for method in ["approval.answer", "approval.list", "task.list"] {
        let err = apps
            .call_value(
                method,
                json!({ "approval": ask.id, "answer": { "approve": { "scope": "once" } } }),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::Rpc(_)), "{method}: {err}");
    }
    assert!(!sending.is_finished());

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
    let sent = sending.await.unwrap();
    assert!(sent.ok, "{}", sent.text);
    assert_eq!(fs::read_dir(h.home().join("Mail/Sent")).unwrap().count(), 1);
    assert!(status(&apps, session).await.waiting.is_none());
    assert!(h.rt.list(&h.me).is_empty(), "back to being a window");

    // Declined on the desktop: a result the app can show.
    let sending = {
        let apps = Arc::clone(&apps);
        tokio::spawn(async move {
            call(
                &apps,
                session,
                "mail.send",
                json!({ "messages": [reply()] }),
            )
            .await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Deny,
        },
    )
    .unwrap();
    let declined = sending.await.unwrap();
    assert!(!declined.ok);
    assert!(
        declined.text.starts_with("Declined on the desktop"),
        "{}",
        declined.text
    );
    let denied = records(&h, session)
        .into_iter()
        .filter(|r| r.event.outcome == Some(Outcome::Denied))
        .count();
    assert_eq!(denied, 1);
}

#[tokio::test]
async fn pa2_a_session_belongs_to_its_connection_and_ends_with_it() {
    let h = harness();
    let socket = h.serve_apps();
    let first = Arc::new(Client::connect(&socket).await.unwrap());
    let other = Client::connect(&socket).await.unwrap();
    let mail = open(&first, "mail").await;

    // Another connection can't call through it, even as the same user.
    let err = other
        .call_value(
            app_methods::APP_CALL,
            json!({ "session": mail.session, "action": "mail.list", "input": { "folder": "inbox" } }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no app session"), "{err}");

    // An ask still open when the app goes away is withdrawn with it.
    let session = mail.session;
    let pending = {
        let first = Arc::clone(&first);
        tokio::spawn(async move {
            let _ = first
                .call_value(
                    app_methods::APP_CALL,
                    json!({ "session": session, "action": "mail.send", "input": { "messages": [reply()] } }),
                )
                .await;
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    pending.abort();
    drop(first);
    eventually(|| h.rt.approvals(&h.me).is_empty()).await;
    eventually(|| h.rt.list(&h.me).is_empty()).await;
    let finished = records(&h, session)
        .into_iter()
        .any(|r| r.event.kind == EventKind::TaskFinished && r.event.summary == "Closed");
    assert!(finished, "the session is recorded closed");
    assert_eq!(
        fs::read_dir(h.home().join("Mail/Sent")).unwrap().count(),
        0,
        "nothing was sent"
    );

    // Closing explicitly works too, and a closed session takes no calls.
    let calendar = open(&other, "calendar").await;
    let _: Value = other
        .call(
            app_methods::APP_CLOSE,
            json!({ "session": calendar.session }),
        )
        .await
        .unwrap();
    let err = other
        .call_value(
            app_methods::APP_STATUS,
            json!({ "session": calendar.session }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no app session"), "{err}");
}
