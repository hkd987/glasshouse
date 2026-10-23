//! Mail through the real pipeline (ADR 0037, EM0): the same actions on the
//! local store and on a real IMAP server (`mox localserve`, skipped, saying
//! so, when it isn't built: `tools/dev/mox.sh build`). An agent's send asks
//! and a decline sends nothing; a group's Never is a result it can read;
//! what leaves carries its by-line and the audit record its SHA-256, never
//! its text; offline, mail reads from the cache and a send says why it
//! waited; connecting mail is the person's own, in Mail.

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use glass_api::{
    AnswerApproval, AppCallResult, AppSession, ApprovalAnswer, ApprovalScope, ScopeRequest,
    TaskRequest, app_methods,
};
use glass_app_mail::Remote;
use glass_audit::{EventKind, Outcome};
use glass_gateway_providers::mail::testing::{self as mox, Mox};
use glass_managed::DirectoryRelease;
use glass_managed::Person;
use glass_managed::directory::Applied;
use glass_managed::mail::{MailLink, SyncLimits};
use glass_rpc::Client;
use glass_rpc::mcp::CallToolResult;
use glass_types::TaskId;
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness_with, tool};

/// `[mail]` for mox: its IMAPS and submissions ports. Policy wants a
/// domain name; the link the test connects reaches mox as `localhost`,
/// the name on its certificate.
fn imap_policy(extra: &str) -> String {
    format!(
        r#"
[mail]
service = "imap"
incoming = {{ host = "mail.localhost", port = {}, security = "tls" }}
outgoing = {{ host = "mail.localhost", port = {}, security = "tls" }}
{extra}"#,
        mox::IMAPS,
        mox::SUBMISSIONS
    )
}

/// The test user is `mox@localhost` in the directory: mox sends only as
/// the account signed in.
fn directory(h: &Harness) {
    h.directory.set(Applied {
        version: 1,
        release: DirectoryRelease {
            people: vec![Person {
                login: h.me.name.clone(),
                name: "Demo Person".into(),
                email: mox::USER.into(),
                title: String::new(),
                department: String::new(),
            }],
        },
    });
}

/// A harness whose mail goes through `mox`, the person connected in Mail
/// with their password, and Mail's connection to the app socket.
async fn on_mox(mox: &Mox, extra: &str) -> (Harness, Arc<Client>) {
    let h = harness_with(&imap_policy(extra));
    directory(&h);
    let server = |port| glass_managed::mail::Server {
        host: "localhost".into(),
        port,
        implicit_tls: true,
    };
    h.mail.connect(Remote {
        link: Arc::new(mox.link()),
        incoming: server(mox::IMAPS),
        outgoing: server(mox::SUBMISSIONS),
    });
    let apps = Arc::new(Client::connect(h.serve_apps()).await.unwrap());
    let session = open(&apps, "mail").await.session;
    let connected = call(
        &apps,
        session,
        "mail.connect",
        json!({ "password": mox::PASSWORD }),
    )
    .await;
    assert!(connected.ok, "{}", connected.text);
    (h, apps)
}

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

/// Claude Code, started for the test user, and its task socket.
async fn claude_code(h: &Harness) -> (TaskId, Arc<Client>) {
    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: "claude-code".parse().unwrap(),
                prompt: "Deal with my mail".into(),
                scope: ScopeRequest::default(),
                title: None,
            },
        )
        .unwrap();
    let socket = view.driver_socket.clone().unwrap();
    eventually(|| socket.exists()).await;
    let client = Client::connect(&socket).await.unwrap();
    let _: Value = client
        .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}))
        .await
        .unwrap();
    (view.id, Arc::new(client))
}

fn audited(h: &Harness, action: &str) -> Vec<glass_audit::AuditRecord> {
    glass_audit::read(&h.audit_path(), |r| {
        r.event.kind == EventKind::Action
            && r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == action)
    })
    .unwrap()
}

fn to_dana(subject: &str) -> Value {
    json!({ "messages": [{ "to": ["dana@northwind.example"], "subject": subject, "body": "Friday works for the renewal call." }] })
}

/// An agent's call that asks, answered `answer` on the desktop.
async fn answered(
    h: &Harness,
    agent: &Arc<Client>,
    name: &str,
    args: Value,
    answer: ApprovalAnswer,
) -> CallToolResult {
    let pending = {
        let agent = Arc::clone(agent);
        let name = name.to_owned();
        tokio::spawn(async move { tool(&agent, &name, args).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer,
        },
    )
    .unwrap();
    pending.await.unwrap()
}

fn inbox_message(subject: &str) -> Vec<u8> {
    format!(
        "From: Dana Ruiz <dana@northwind.example>\r\nTo: {}\r\nSubject: {subject}\r\n\
         Date: Mon, 19 Oct 2026 08:00:00 +0000\r\nMessage-ID: <{subject}@northwind.example>\r\n\r\n\
         Can we talk terms?\r\n",
        mox::USER
    )
    .into_bytes()
}

/// Where two results differ in shape: the same keys with values of the
/// same types, arrays compared by their first items, and `null` (a field
/// with nothing in it, such as a service's host with no service) fitting
/// any type.
fn differ(path: &str, a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => None,
        (Value::Object(x), Value::Object(y)) => {
            let keys = |m: &serde_json::Map<String, Value>| m.keys().cloned().collect::<Vec<_>>();
            if keys(x) != keys(y) {
                return Some(format!("{path}: keys {:?} vs {:?}", keys(x), keys(y)));
            }
            x.iter()
                .find_map(|(k, v)| differ(&format!("{path}.{k}"), v, &y[k]))
        }
        (Value::Array(x), Value::Array(y)) => match (x.first(), y.first()) {
            (Some(p), Some(q)) => differ(&format!("{path}[0]"), p, q),
            _ => None,
        },
        (Value::String(_), Value::String(_))
        | (Value::Number(_), Value::Number(_))
        | (Value::Bool(_), Value::Bool(_)) => None,
        _ => Some(format!("{path}: {a} vs {b}")),
    }
}

/// The same calls an agent makes, and what each returned.
async fn the_usual_calls(h: &Harness) -> Vec<(String, Value)> {
    let (_task, agent) = claude_code(h).await;
    let mut seen = Vec::new();
    let mut ok = |name: &str, result: CallToolResult| {
        assert!(!result.is_error, "{name}: {}", result.text());
        seen.push((
            name.to_owned(),
            result.structured_content.unwrap_or_default(),
        ));
    };
    ok("account", tool(&agent, "mail_account", json!({})).await);
    let inbox = tool(&agent, "mail_list", json!({ "folder": "inbox" })).await;
    let id = inbox.structured_content.as_ref().unwrap()["messages"][0]["id"].clone();
    ok("list", inbox);
    ok("read", tool(&agent, "mail_read", json!({ "id": id })).await);
    ok(
        "draft",
        tool(
            &agent,
            "mail_draft",
            json!({ "to": ["dana@northwind.example"], "subject": "Re: Renewal", "body": "Draft." }),
        )
        .await,
    );
    ok(
        "send",
        answered(
            h,
            &agent,
            "mail_send",
            to_dana("Renewal"),
            ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            },
        )
        .await,
    );
    ok(
        "sent",
        tool(&agent, "mail_list", json!({ "folder": "sent" })).await,
    );
    ok(
        "mark",
        tool(&agent, "mail_mark", json!({ "ids": [id], "read": true })).await,
    );
    ok(
        "move",
        tool(&agent, "mail_move", json!({ "ids": [id], "to": "archive" })).await,
    );
    ok(
        "archive",
        tool(&agent, "mail_list", json!({ "folder": "archive" })).await,
    );
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn in2_mail_actions_return_the_same_shapes_on_the_local_store_and_on_imap() {
    let Some(mox) = Mox::start() else { return };
    // The local store: no service, `~/Mail` is everything.
    let local = harness_with("");
    directory(&local);
    std::fs::create_dir_all(local.home().join("Mail/Inbox")).unwrap();
    std::fs::write(
        local.home().join("Mail/Inbox/1792400000-0-renewal.eml"),
        inbox_message("Renewal"),
    )
    .unwrap();
    let on_local = the_usual_calls(&local).await;

    mox.deliver(&inbox_message("Renewal"));
    let (imap, _apps) = on_mox(&mox, "").await;
    let on_imap = the_usual_calls(&imap).await;

    assert_eq!(on_local.len(), on_imap.len());
    for ((name, a), (_, b)) in on_local.iter().zip(&on_imap) {
        assert_eq!(differ(name, a, b), None, "{a}\n{b}");
    }
    // And it really went to the server, marked read before it was filed.
    let moved = on_imap.iter().find(|(name, _)| name == "move").unwrap();
    assert_eq!(moved.1["moved"], 1);
    assert_eq!(mox.uids("Archive").len(), 1);
    assert!(mox.unseen("Archive").is_empty());
    assert!(mox.uids("Inbox").is_empty());
    assert_eq!(mox.uids("Sent").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn in2_an_agents_send_asks_and_a_decline_sends_nothing() {
    let Some(mox) = Mox::start() else { return };
    let (h, _apps) = on_mox(&mox, "").await;
    let (_task, agent) = claude_code(&h).await;
    let pending = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move { tool(&agent, "mail_send", to_dana("Renewal")).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(ask.summary, "Send email: Renewal");
    assert_eq!(ask.labels.approve, "Approve and send");
    assert_eq!(ask.items, ["To dana@northwind.example: Renewal"]);
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Deny,
        },
    )
    .unwrap();
    let result = pending.await.unwrap();
    assert!(
        result.is_error && result.text().contains("declined"),
        "{}",
        result.text()
    );
    // Nothing in mox's queue, nothing filed in Sent, there or here.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(mox.queue().is_empty(), "{:?}", mox.queue());
    assert!(mox.uids("Sent").is_empty());
    assert_eq!(
        std::fs::read_dir(h.home().join("Mail/Sent"))
            .unwrap()
            .count(),
        0
    );
    let record = audited(&h, "mail.send").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Denied));
}

#[tokio::test(flavor = "multi_thread")]
async fn in2_a_group_rule_making_mail_send_never_is_a_readable_refusal() {
    let Some(mox) = Mox::start() else { return };
    let (h, apps) = on_mox(
        &mox,
        r#"
[[group]]
name = "legal-hold"
members = ["{me}"]
[group.tiers]
"mail.send" = "never"
"#,
    )
    .await;
    let (_task, agent) = claude_code(&h).await;
    let refused = tool(&agent, "mail_send", to_dana("Renewal")).await;
    assert!(refused.is_error, "a readable refusal, not a protocol error");
    assert!(
        refused.text().contains("group legal-hold rule mail.send"),
        "{}",
        refused.text()
    );
    assert!(h.rt.approvals(&h.me).is_empty());
    assert!(mox.queue().is_empty());
    let record = audited(&h, "mail.send").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Blocked));
    // The person's own Mail is held to it too.
    let session = open(&apps, "mail").await.session;
    let typed = call(&apps, session, "mail.send", to_dana("Renewal")).await;
    assert!(
        !typed.ok && typed.text.contains("Blocked by policy"),
        "{}",
        typed.text
    );
    assert!(mox.queue().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn em0_no_agent_connects_mail_and_the_password_is_never_recorded() {
    let Some(mox) = Mox::start() else { return };
    let (h, _apps) = on_mox(&mox, "").await;
    let (_task, agent) = claude_code(&h).await;
    let refused = tool(
        &agent,
        "mail_connect",
        json!({ "password": "agent-canary-4417" }),
    )
    .await;
    assert!(refused.is_error, "{}", refused.text());
    assert!(
        refused.text().contains("never an agent"),
        "{}",
        refused.text()
    );
    let log = std::fs::read_to_string(h.audit_path()).unwrap();
    assert!(!log.contains(mox::PASSWORD) && !log.contains("agent-canary-4417"));
    let connects = audited(&h, "mail.connect");
    assert_eq!(connects.len(), 2, "the person's and the agent's");
    assert_eq!(connects[0].event.outcome, Some(Outcome::Completed));
    assert_eq!(connects[1].event.outcome, Some(Outcome::Blocked));
    // Root only, in glassd's state, never in the home.
    let kept = h
        .dir
        .path()
        .join("state/mail")
        .join(&h.me.name)
        .join("account.json");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&kept).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut stack = vec![h.home()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(mox::PASSWORD),
                    "{}",
                    path.display()
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn au10_a_sent_message_carries_the_by_line_and_the_audit_its_sha256() {
    let Some(mox) = Mox::start() else { return };
    let (h, _apps) = on_mox(&mox, "").await;
    let (_task, agent) = claude_code(&h).await;
    let sent = answered(
        &h,
        &agent,
        "mail_send",
        to_dana("Renewal"),
        ApprovalAnswer::Approve {
            scope: ApprovalScope::Once,
        },
    )
    .await;
    assert!(!sent.is_error, "{}", sent.text());
    // The result carries no audit detail.
    let data = sent.structured_content.clone().unwrap();
    assert_eq!(data, json!({ "sent": 1 }));

    // What left, as mox filed it in Sent: the by-line, inside the message.
    let link = mox.link();
    let me = mox.account(&h.me.name);
    let filed = link
        .sync(
            &me,
            "Sent",
            None,
            &[],
            SyncLimits {
                newest: 10,
                max_bytes: 1 << 20,
            },
        )
        .unwrap();
    assert_eq!(filed.new.len(), 1);
    let raw = &filed.new[0].raw;
    let text = String::from_utf8_lossy(raw);
    assert!(
        text.contains(&format!(
            "X-Glasshouse-Attribution: by claude-code for {}\r\n",
            h.me.name
        )),
        "{text}"
    );
    assert!(
        mox.queue()
            .iter()
            .any(|m| m.contains("to:dana@northwind.example"))
    );

    // The audit record: its Message-ID, size, SHA-256 and the server's
    // reply, never its text.
    let record = audited(&h, "mail.send").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Completed));
    let detail = &record.event.detail["mail"][0];
    assert_eq!(detail["sha256"], glass_managed::sha256_hex(raw));
    assert_eq!(detail["size"], raw.len());
    assert_eq!(detail["recipients"], 1);
    assert!(
        detail["reply"].as_str().unwrap().starts_with("250 "),
        "{detail}"
    );
    let message_id = detail["message_id"].as_str().unwrap();
    assert!(
        text.contains(&format!("Message-ID: {message_id}")),
        "{text}"
    );
    let line = serde_json::to_string(&record).unwrap();
    assert!(!line.contains("Friday works"), "never the body: {line}");
    // And the copy here is byte for byte what left.
    let change = h.rt.get(&h.me, _task).unwrap().changes;
    let here = change
        .iter()
        .find(|c| c.action.as_str() == "mail.send")
        .unwrap();
    let path = h.home().join(here.paths[0].trim_start_matches("~/"));
    assert_eq!(&std::fs::read(path).unwrap(), raw);
}

#[tokio::test(flavor = "multi_thread")]
async fn dp30_offline_mail_reads_from_the_cache_and_a_send_says_why_it_waited() {
    let Some(server) = Mox::start() else { return };
    server.deliver(&inbox_message("Renewal"));
    let (h, _apps) = on_mox(&server, "").await;
    let (_task, agent) = claude_code(&h).await;
    let inbox = tool(&agent, "mail_list", json!({ "folder": "inbox" })).await;
    let id = inbox.structured_content.unwrap()["messages"][0]["id"].clone();

    // The server goes away; what this computer kept stays readable.
    drop(server);
    tokio::time::sleep(Duration::from_millis(5100)).await;
    let offline = tool(&agent, "mail_list", json!({ "folder": "inbox" })).await;
    assert!(!offline.is_error);
    assert!(offline.text().contains("offline"), "{}", offline.text());
    assert_eq!(
        offline.structured_content.unwrap()["messages"][0]["subject"],
        "Renewal"
    );
    let read = tool(&agent, "mail_read", json!({ "id": id })).await;
    assert!(!read.is_error, "{}", read.text());
    assert!(
        read.structured_content.unwrap()["message"]["body"]
            .as_str()
            .unwrap()
            .contains("Can we talk terms?")
    );

    // A send asks, then says why it didn't leave, and waits in Drafts.
    let waited = answered(
        &h,
        &agent,
        "mail_send",
        to_dana("Waiting"),
        ApprovalAnswer::Approve {
            scope: ApprovalScope::Once,
        },
    )
    .await;
    assert!(waited.is_error);
    let why = waited.text();
    assert!(
        why.contains("Not sent") && why.contains("offline") && why.contains("Drafts"),
        "{why}"
    );
    let drafts = tool(&agent, "mail_list", json!({ "folder": "drafts" })).await;
    assert_eq!(
        drafts.structured_content.unwrap()["messages"][0]["subject"],
        "Waiting"
    );
    let record = audited(&h, "mail.send").pop().expect("audited");
    assert!(matches!(record.event.outcome, Some(Outcome::Failed { .. })));
}

#[tokio::test(flavor = "multi_thread")]
async fn nv3_new_mail_wakes_the_mail_app_without_a_read() {
    let Some(mox) = Mox::start() else { return };
    let (h, apps) = on_mox(&mox, "").await;
    let session = open(&apps, "mail").await.session;
    assert!(
        call(&apps, session, "mail.list", json!({ "folder": "inbox" }))
            .await
            .ok
    );
    let now: glass_api::AppStatus = apps
        .call(app_methods::APP_STATUS, json!({ "session": session }))
        .await
        .unwrap();
    let waiting = {
        let apps = Arc::clone(&apps);
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let status: glass_api::AppStatus = apps
                .call(
                    app_methods::APP_STATUS,
                    json!({ "session": session, "wait_secs": 20, "since": now.cursor }),
                )
                .await
                .unwrap();
            (status, started.elapsed())
        })
    };
    // glassd's watcher: waiting on the Inbox (IMAP IDLE) as glassd does.
    let watcher = {
        let mail = h.mail.clone();
        let login = h.me.name.clone();
        std::thread::spawn(move || mail.watch(&login, Duration::from_secs(15)))
    };
    tokio::time::sleep(Duration::from_millis(700)).await;
    let delivered = std::time::Instant::now();
    mox.deliver(&inbox_message("Ping"));
    let (status, _) = waiting.await.unwrap();
    let took = delivered.elapsed();
    assert_eq!(status.changed, ["mail"]);
    assert!(took < Duration::from_secs(2), "{took:?}");
    watcher.join().unwrap().unwrap();
    // It's in ~/Mail already.
    let inbox = call(&apps, session, "mail.list", json!({ "folder": "inbox" })).await;
    assert_eq!(inbox.data["messages"][0]["subject"], "Ping");
}

/// The messages kept in `~/Mail/<folder>`, not its read markers (ADR 0050).
fn messages(h: &Harness, folder: &str) -> Vec<String> {
    std::fs::read_dir(h.home().join("Mail").join(folder))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn em3_undoing_a_move_puts_the_message_back_as_read_as_it_was() {
    // The mail model's counterexample (ADR 0051), through the real undo:
    // a move names the read markers too, so undo puts them back.
    let h = harness_with("");
    directory(&h);
    let name = "1792400000-0-renewal.eml";
    for dir in ["Mail/Inbox/.read", "Mail/Archive/.read"] {
        std::fs::create_dir_all(h.home().join(dir)).unwrap();
    }
    std::fs::write(
        h.home().join("Mail/Inbox").join(name),
        inbox_message("Renewal"),
    )
    .unwrap();
    // The person read it before the agent filed it away.
    std::fs::write(h.home().join("Mail/Inbox/.read").join(name), "").unwrap();
    let (task, agent) = claude_code(&h).await;
    let moved = tool(
        &agent,
        "mail_move",
        json!({ "ids": [format!("inbox/{name}")], "to": "archive" }),
    )
    .await;
    assert!(!moved.is_error, "{}", moved.text());
    assert!(h.home().join("Mail/Archive/.read").join(name).exists());

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
    assert!(h.home().join("Mail/Inbox").join(name).exists());
    assert!(
        h.home().join("Mail/Inbox/.read").join(name).exists(),
        "read, as it was"
    );
    assert!(!h.home().join("Mail/Archive/.read").join(name).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn em3_an_agents_mark_tells_the_person_and_undo_marks_it_back_on_the_server() {
    let Some(mox) = Mox::start() else { return };
    let uid = mox.deliver(&inbox_message("Renewal"));
    let (h, apps) = on_mox(&mox, "").await;
    let (task, agent) = claude_code(&h).await;
    let inbox = tool(&agent, "mail_list", json!({ "folder": "inbox" })).await;
    let listed = inbox.structured_content.unwrap();
    assert_eq!(listed["unread"], 1);
    let id = listed["messages"][0]["id"].clone();
    // An agent reading mail leaves it new to the person, there too.
    let read = tool(&agent, "mail_read", json!({ "id": id })).await;
    assert_eq!(read.structured_content.unwrap()["message"]["unread"], true);
    assert_eq!(mox.unseen("Inbox"), [uid]);

    let marked = tool(&agent, "mail_mark", json!({ "ids": [id], "read": true })).await;
    assert!(!marked.is_error, "{}", marked.text());
    assert!(h.rt.approvals(&h.me).is_empty(), "Notify: no ask");
    assert!(mox.unseen("Inbox").is_empty(), "read on the server");
    let record = audited(&h, "mail.mark").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Completed));
    assert!(record.event.snapshot.is_some(), "a snapshot first");

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
    assert_eq!(mox.unseen("Inbox"), [uid], "unread again on the server");

    // In Mail, opening it marks it read without a word (its [[app]] tier).
    let session = open(&apps, "mail").await.session;
    let opened = call(
        &apps,
        session,
        "mail.mark",
        json!({ "ids": [id], "read": true }),
    )
    .await;
    assert!(opened.ok, "{}", opened.text);
    assert!(mox.unseen("Inbox").is_empty());
    let list = call(&apps, session, "mail.list", json!({ "folder": "inbox" })).await;
    assert_eq!(list.data["unread"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn em0_an_agents_move_runs_at_notify_and_undo_moves_it_back_on_the_server() {
    let Some(mox) = Mox::start() else { return };
    mox.deliver(&inbox_message("Spammy"));
    let (h, _apps) = on_mox(&mox, "").await;
    let (task, agent) = claude_code(&h).await;
    let inbox = tool(&agent, "mail_list", json!({ "folder": "inbox" })).await;
    let id = inbox.structured_content.unwrap()["messages"][0]["id"].clone();
    let moved = tool(&agent, "mail_move", json!({ "ids": [id], "to": "junk" })).await;
    assert!(!moved.is_error, "{}", moved.text());
    assert!(h.rt.approvals(&h.me).is_empty(), "Notify: no ask");
    assert!(mox.uids("Inbox").is_empty());
    assert_eq!(mox.uids("Junk").len(), 1);
    let record = audited(&h, "mail.move").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Completed));
    assert!(record.event.snapshot.is_some(), "a snapshot first");

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
    assert!(mox.uids("Junk").is_empty(), "moved back on the server");
    let back = mox.uids("Inbox");
    assert_eq!(back.len(), 1);
    let names = messages(&h, "Inbox");
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(
        names[0].ends_with(&format!("-{:010}.eml", back[0])),
        "{names:?}"
    );
    assert_eq!(messages(&h, "Junk").len(), 0);
    let changes = h.rt.get(&h.me, task).unwrap().changes;
    assert!(
        changes.iter().all(|c| c.undone || !c.reversible),
        "{changes:?}"
    );
}
