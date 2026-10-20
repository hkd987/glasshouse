//! Docs (DC, ADR 0035) through the real pipeline, with an in-memory
//! console: the person makes and opens a doc in the Docs app without being
//! asked, types into it live (`app.live`) with no audited call per edit,
//! hears a colleague's edits through a waiting pull, and the session's
//! close is audited once with what it changed (ADR 0035 §3). An agent's
//! write into the doc asks.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;
use std::sync::Arc;

use glass_actions::{ActionProvider, Live};
use glass_api::{
    AnswerApproval, AppCallResult, AppLiveResult, AppSession, ApprovalAnswer, ApprovalScope,
    ScopeRequest, TaskRequest, app_methods,
};
use glass_app_chat::{ChatHandle, ChatProvider};
use glass_app_docs::{DocsProvider, markdown, ydoc};
use glass_audit::{EventKind, Outcome};
use glass_crypto::SecretKey;
use glass_managed::base64;
use glass_managed::chat::testing::MemoryLink;
use glass_managed::directory::Applied;
use glass_managed::{ChatRelease, DirectoryRelease, Person};
use glass_rpc::{Client, ClientError};
use glass_types::{TaskId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness_with, tool};

/// Priya's computer, in one org with the test's through an in-memory
/// console.
struct Org {
    priya_chat: ChatHandle,
    priya: DocsProvider,
    ctx: glass_actions::TaskContext,
    _dir: tempfile::TempDir,
}

fn person(login: &str, name: &str) -> Person {
    Person {
        login: login.into(),
        name: name.into(),
        email: format!("{login}@acme.example"),
        title: String::new(),
        department: String::new(),
    }
}

fn org(h: &Harness) -> Org {
    h.directory.set(Applied {
        version: 1,
        release: DirectoryRelease {
            people: vec![
                person(&h.me.name, "Demo Person"),
                person("priya", "Priya Shah"),
            ],
        },
    });
    let it = SecretKey::generate().unwrap();
    let link = Arc::new(MemoryLink::new(&it.public().unwrap()));
    let release = ChatRelease::new(&it.public().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let priya_chat = ChatHandle::new(dir.path().join("chat"));
    for (chat, device) in [(&h.chat, "dev-me"), (&priya_chat, "dev-priya")] {
        chat.connect("acme", device, link.clone());
        chat.set_compliance(&release.compliance, &release.fingerprint)
            .unwrap();
    }
    let shared = Arc::new(ChatProvider::new(priya_chat.clone(), h.directory.clone()));
    let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
    ctx.principal.on_behalf_of.name = "priya".into();
    ctx.principal.agent = "docs".parse().unwrap();
    let org = Org {
        priya: DocsProvider::new(shared, h.directory.clone()),
        priya_chat,
        ctx,
        _dir: dir,
    };
    // Priya has opened Docs once, which made her keys.
    org.priya("doc.list", json!({}));
    org
}

impl Org {
    /// Priya, in Docs on her own computer, after it synced.
    fn priya(&self, action: &str, input: Value) -> Value {
        self.priya_chat.sync_once(0).unwrap();
        self.priya
            .invoke(&self.ctx, &action.parse().unwrap(), &input)
            .unwrap()
            .data
    }
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

async fn live(client: &Client, session: TaskId, request: Value) -> Result<AppLiveResult, String> {
    let mut params = request;
    params["session"] = json!(session);
    client
        .call(app_methods::APP_LIVE, params)
        .await
        .map_err(|e| match e {
            ClientError::Rpc(e) => e.message,
            other => other.to_string(),
        })
}

/// An edit typed after `md` in a doc whose state is `state` (base64), as
/// the page's editor would make it: the update, base64.
fn typed(state: &str, md: &str) -> String {
    let doc = ydoc::doc_from(&[base64::decode(state).unwrap()]).unwrap();
    let update = ydoc::write(&doc, &markdown::parse(md), &ydoc::At::End, "").unwrap();
    base64::encode(&update)
}

fn records(h: &Harness, kind: EventKind) -> Vec<glass_audit::AuditRecord> {
    glass_audit::read(&h.audit_path(), |r| r.event.kind == kind).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn dc3_live_edits_reach_a_colleague_and_the_close_is_audited_once() {
    let h = harness_with("");
    let org = org(&h);
    let apps = Arc::new(Client::connect(h.serve_apps()).await.unwrap());
    let session = open(&apps, "docs").await.session;

    // The person makes a doc with Priya in Docs: nobody is asked.
    let made = call(
        &apps,
        session,
        "doc.create",
        json!({ "title": "Q3 plan", "with": ["priya"], "markdown": "# Q3 plan\n\nDraft.\n" }),
    )
    .await;
    assert!(made.ok, "{}", made.text);
    assert!(h.rt.approvals(&h.me).is_empty(), "the person did it");
    let id = made.data["id"].as_str().unwrap().to_owned();
    let opened = call(&apps, session, "doc.open", json!({ "doc": id })).await;
    assert!(opened.ok, "{}", opened.text);
    assert_eq!(opened.data["role"], "owner");
    let actions_before = records(&h, EventKind::Action).len();

    // They type: two live pushes, no audited call for either.
    let state = opened.data["state"].as_str().unwrap();
    for line in ["Ship Northwind by Friday.", "Hire two engineers."] {
        let pushed = live(
            &apps,
            session,
            json!({ "op": "push", "item": id, "client": "page-1", "update": typed(state, line) }),
        )
        .await;
        assert!(pushed.is_ok(), "{pushed:?}");
    }
    assert_eq!(records(&h, EventKind::Action).len(), actions_before);
    let read = org.priya("doc.read", json!({ "doc": id }));
    let text = read["markdown"].as_str().unwrap();
    assert!(text.contains("Ship Northwind by Friday."), "{text}");
    assert!(text.contains("Hire two engineers."), "{text}");
    // Another window of Docs shares the session and hears them.
    let other = live(
        &apps,
        session,
        json!({ "op": "pull", "item": id, "client": "page-2", "since": opened.data["cursor"] }),
    )
    .await
    .unwrap();
    assert_eq!(other.updates.len(), 2, "{other:?}");
    assert!(other.updates.iter().all(|u| u["by"] == h.me.name.as_str()));

    // The page waits for others' edits, not hearing its own; Priya's
    // arrives and wakes it.
    let since = opened.data["cursor"].as_u64().unwrap();
    let pull = {
        let (apps, id) = (Arc::clone(&apps), id.clone());
        tokio::spawn(async move {
            live(
                &apps,
                session,
                json!({ "op": "pull", "item": id, "client": "page-1", "since": since, "wait_secs": 20 }),
            )
            .await
        })
    };
    let priyas = org.priya("doc.open", json!({ "doc": made.data["id"] }));
    assert_eq!(priyas["role"], "editor");
    let edit = typed(
        priyas["state"].as_str().unwrap(),
        "Priya: and a launch date.",
    );
    org.priya
        .push(&org.ctx, made.data["id"].as_str().unwrap(), "", &edit)
        .unwrap();
    h.chat.sync_once(0).unwrap();
    h.rt.app_changed(&h.me.name, "docs");
    let pulled = tokio::time::timeout(std::time::Duration::from_secs(15), pull)
        .await
        .expect("the pull wakes")
        .unwrap()
        .unwrap();
    assert_eq!(pulled.updates.len(), 1, "{pulled:?}");
    assert_eq!(pulled.updates[0]["by"], "priya");
    assert_eq!(pulled.updates[0]["by_name"], "Priya Shah");

    // Closing writes one record: what the session changed.
    let _: Value = apps
        .call(app_methods::APP_CLOSE, json!({ "session": session }))
        .await
        .unwrap();
    let edited = records(&h, EventKind::Edited);
    assert_eq!(edited.len(), 1, "{edited:?}");
    let event = &edited[0].event;
    assert_eq!(event.outcome, Some(Outcome::Completed));
    assert_eq!(event.summary, "Q3 plan: 2 changes in 1 minute");
    assert_eq!(event.task, Some(session));
    assert_eq!(event.agent.as_ref().unwrap().as_str(), "docs");
    assert_eq!(event.user.as_deref(), Some(h.me.name.as_str()));
    // The doc's text isn't in the log.
    let log = std::fs::read_to_string(h.audit_path()).unwrap();
    assert!(!log.contains("Northwind"));
}

#[tokio::test(flavor = "multi_thread")]
async fn dc4_live_edits_hold_to_what_the_session_opened() {
    let h = harness_with("");
    let org = org(&h);
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "docs").await.session;

    // The person opened Docs once, which made their keys; Priya shares a
    // doc with them to view only.
    assert!(call(&apps, session, "doc.list", json!({})).await.ok);
    let made = org.priya(
        "doc.create",
        json!({ "title": "Board notes", "viewers": [h.me.name], "markdown": "Notes.\n" }),
    );
    let id = made["id"].as_str().unwrap().to_owned();
    h.chat.sync_once(0).unwrap();

    // Not opened in this window yet: refused.
    let state = org.priya("doc.open", json!({ "doc": id }))["state"]
        .as_str()
        .unwrap()
        .to_owned();
    let push = json!({ "op": "push", "item": id, "update": typed(&state, "Mine now.") });
    let refused = live(&apps, session, push.clone()).await.unwrap_err();
    assert!(refused.contains("isn't open in this window"), "{refused}");

    // Opened, but as a viewer: still refused.
    let opened = call(&apps, session, "doc.open", json!({ "doc": id })).await;
    assert!(opened.ok, "{}", opened.text);
    assert_eq!(opened.data["role"], "viewer");
    let refused = live(&apps, session, push.clone()).await.unwrap_err();
    assert!(refused.contains("not edit it"), "{refused}");
    // Not an update at all.
    let refused = live(
        &apps,
        session,
        json!({ "op": "push", "item": id, "update": base64::encode(b"<script>") }),
    )
    .await
    .unwrap_err();
    assert!(!refused.is_empty());
    // A client id is an id, not a place to put text.
    let mut odd = push.clone();
    odd["client"] = json!("<b>page</b>");
    assert!(live(&apps, session, odd).await.is_err());

    // Priya makes them an editor: opened again, the window edits.
    org.priya("doc.roles", json!({ "doc": id, "editors": [h.me.name] }));
    h.chat.sync_once(0).unwrap();
    let opened = call(&apps, session, "doc.open", json!({ "doc": id })).await;
    assert_eq!(opened.data["role"], "editor");
    live(&apps, session, push.clone()).await.unwrap();

    // A closed session edits nothing more.
    let _: Value = apps
        .call(app_methods::APP_CLOSE, json!({ "session": session }))
        .await
        .unwrap();
    assert!(live(&apps, session, push).await.is_err());
    // Each refusal is on record, and then the edit.
    let edited = records(&h, EventKind::Edited);
    assert_eq!(edited.len(), 4, "{edited:?}");
    assert!(
        edited[..3]
            .iter()
            .all(|r| r.event.outcome == Some(Outcome::Blocked) && r.event.targets == [id.clone()])
    );
    assert_eq!(edited[3].event.summary, "Board notes: 1 change in 1 minute");
    let text = org.priya("doc.read", json!({ "doc": id }))["markdown"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        text.contains("Notes.") && text.contains("Mine now."),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dc6_an_agents_write_into_a_doc_asks() {
    let h = harness_with("");
    let _org = org(&h);
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "docs").await.session;
    let made = call(
        &apps,
        session,
        "doc.create",
        json!({ "title": "Q3 plan", "markdown": "# Q3 plan\n\n## Risks\n\nNone yet.\n" }),
    )
    .await;
    let id = made.data["id"].as_str().unwrap().to_owned();

    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: "claude-code".parse().unwrap(),
                prompt: "Add the risks to the Q3 plan".into(),
                scope: ScopeRequest::default(),
                title: None,
            },
        )
        .unwrap();
    let socket = view.driver_socket.clone().unwrap();
    eventually(|| socket.exists()).await;
    let agent = Arc::new(Client::connect(&socket).await.unwrap());
    let _: Value = agent
        .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}))
        .await
        .unwrap();
    let write = {
        let (agent, id) = (Arc::clone(&agent), id.clone());
        tokio::spawn(async move {
            tool(
                &agent,
                "doc_write",
                json!({ "doc": id, "markdown": "Supplier delay.", "at": { "after": "Risks" } }),
            )
            .await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert!(ask.summary.contains("Q3 plan"), "{}", ask.summary);
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
    let written = write.await.unwrap();
    assert!(!written.is_error, "{}", written.text());
    let record = glass_audit::read(&h.audit_path(), |r| {
        r.event
            .action
            .as_ref()
            .is_some_and(|a| a.as_str() == "doc.write")
    })
    .unwrap()
    .pop()
    .expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Completed));
    assert_eq!(record.event.decision.as_ref().unwrap().tier, Tier::Ask);

    // The open Docs window hears it.
    let read = call(&apps, session, "doc.read", json!({ "doc": id })).await;
    assert!(
        read.data["markdown"]
            .as_str()
            .unwrap()
            .contains("Supplier delay."),
        "{}",
        read.data
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dc3_a_session_stopped_from_the_desktop_still_records_its_edits() {
    let h = harness_with("");
    let _org = org(&h);
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "docs").await.session;
    let made = call(&apps, session, "doc.create", json!({ "title": "Offsite" })).await;
    let id = made.data["id"].as_str().unwrap().to_owned();
    let opened = call(&apps, session, "doc.open", json!({ "doc": id })).await;
    let update = typed(opened.data["state"].as_str().unwrap(), "Lisbon, in May.");
    live(
        &apps,
        session,
        json!({ "op": "push", "item": id, "update": update }),
    )
    .await
    .unwrap();

    h.rt.stop(&h.me, session).unwrap();
    let edited = records(&h, EventKind::Edited);
    assert_eq!(edited.len(), 1, "{edited:?}");
    assert_eq!(edited[0].event.summary, "Offsite: 1 change in 1 minute");
}
