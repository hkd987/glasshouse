//! The org's chat (CF-91, ADR 0030) through the real pipeline, with an
//! in-memory console: agents read without asking and each read is audited
//! without its text; an agent's post asks, a decline or a group's Never is
//! a result it can read, and nothing is posted; what the person types in
//! Chat is posted without asking unless a group says otherwise; an agent's
//! message says who wrote it, inside the seal (AU-10); with the console out
//! of reach a post is refused, not queued.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;
use std::sync::Arc;

use glass_actions::ActionProvider;
use glass_api::{
    AnswerApproval, AppCallResult, AppSession, ApprovalAnswer, ApprovalScope, ScopeRequest,
    TaskRequest, app_methods,
};
use glass_app_chat::{ChatHandle, ChatProvider};
use glass_audit::{EventKind, Outcome};
use glass_crypto::SecretKey;
use glass_managed::chat::testing::MemoryLink;
use glass_managed::directory::Applied;
use glass_managed::{ChatRelease, DirectoryRelease, Person};
use glass_rpc::Client;
use glass_types::{TaskId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness_with, tool};

const CANARY: &str = "Canary-7731: wire the Northwind deposit Friday";

/// The test's computer and Priya's, in one org with an in-memory console.
struct Org {
    link: Arc<MemoryLink>,
    priya: ChatProvider,
    priya_chat: ChatHandle,
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
    Org {
        link,
        priya: ChatProvider::new(priya_chat.clone(), h.directory.clone()),
        priya_chat,
        _dir: dir,
    }
}

impl Org {
    /// Priya, in Chat on her own computer.
    fn priya(&self, action: &str, input: Value) -> Value {
        let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
        ctx.principal.on_behalf_of.name = "priya".into();
        ctx.principal.agent = "chat".parse().unwrap();
        self.priya
            .invoke(&ctx, &action.parse().unwrap(), &input)
            .unwrap()
            .data
    }

    /// The newest message in Priya's only conversation, as she reads it.
    fn priya_reads(&self) -> Value {
        self.priya_chat.sync_once(0).unwrap();
        let list = self.priya("chat.list", json!({}));
        let id = list["conversations"][0]["id"].clone();
        let read = self.priya("chat.read", json!({ "conversation": id }));
        read["messages"].as_array().unwrap().last().unwrap().clone()
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

/// The test user opens Chat, which makes their key.
async fn opens_chat(h: &Harness) {
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "chat").await.session;
    let listed = call(&apps, session, "chat.list", json!({})).await;
    assert!(listed.ok, "{}", listed.text);
}

/// Claude Code, started for the test user, and its task socket.
async fn claude_code(h: &Harness) -> (TaskId, Arc<Client>) {
    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: "claude-code".parse().unwrap(),
                prompt: "Catch me up on chat".into(),
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

fn post_to_priya(text: &str) -> Value {
    json!({ "posts": [{ "to": ["priya"], "text": text }] })
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

#[tokio::test(flavor = "multi_thread")]
async fn cf91_agents_read_chat_at_auto_and_each_read_is_audited_without_text() {
    let h = harness_with("");
    let org = org(&h);
    let (_task, agent) = claude_code(&h).await;
    // An agent can't start chat for the person: no key is made for them
    // until they open Chat themselves.
    let first = tool(&agent, "chat_list", json!({})).await;
    assert!(!first.is_error, "{}", first.text());
    assert!(
        first.text().contains("hasn't opened Chat"),
        "{}",
        first.text()
    );
    assert!(!h.dir.path().join("state/chat").join(&h.me.name).exists());
    opens_chat(&h).await;
    org.priya("chat.list", json!({}));
    org.priya(
        "chat.post",
        json!({ "posts": [{ "to": [h.me.name], "text": CANARY }] }),
    );
    h.chat.sync_once(0).unwrap();

    let list = tool(&agent, "chat_list", json!({})).await;
    assert!(list.text().contains("Priya Shah"), "{}", list.text());
    assert!(list.text().contains("1 unread"), "{}", list.text());
    let id = list.structured_content.as_ref().unwrap()["conversations"][0]["id"].clone();
    let read = tool(&agent, "chat_read", json!({ "conversation": id })).await;
    assert!(!read.is_error, "{}", read.text());
    assert!(read.text().contains(CANARY), "{}", read.text());
    assert!(h.rt.approvals(&h.me).is_empty(), "Auto: nobody is asked");
    // An agent reading for the person leaves it unread for them.
    let again = tool(&agent, "chat_list", json!({})).await;
    assert!(again.text().contains("1 unread"), "{}", again.text());

    let record = audited(&h, "chat.read").pop().expect("audited");
    assert_eq!(record.event.decision.as_ref().unwrap().tier, Tier::Auto);
    assert_eq!(record.event.outcome, Some(Outcome::Completed));
    assert_eq!(record.event.summary, "Read Priya Shah");
    let log = std::fs::read_to_string(h.audit_path()).unwrap();
    assert!(
        !log.contains("Canary-7731"),
        "the audit log never holds message text"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cf91_an_agents_chat_post_asks_and_a_decline_is_a_readable_result() {
    let h = harness_with("");
    let org = org(&h);
    org.priya("chat.list", json!({}));
    let (_task, agent) = claude_code(&h).await;
    let call = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move { tool(&agent, "chat_post", post_to_priya(CANARY)).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(ask.summary, "Message Priya Shah");
    assert_eq!(ask.labels.approve, "Approve and post");
    assert_eq!(ask.items, [format!("Priya Shah: {CANARY}")]);
    h.rt.answer(
        &h.me,
        AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Deny,
        },
    )
    .unwrap();
    let result = call.await.unwrap();
    assert!(
        result.is_error && result.text().contains("declined"),
        "{}",
        result.text()
    );
    assert!(org.link.messages().is_empty(), "nothing was posted");
    let record = audited(&h, "chat.post").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Denied));
}

#[tokio::test(flavor = "multi_thread")]
async fn cf91_a_group_rule_making_chat_post_never_is_a_readable_refusal() {
    let h = harness_with(
        r#"
[[group]]
name = "legal-hold"
members = ["{me}"]
[group.tiers]
"chat.post" = "never"
"#,
    );
    let org = org(&h);
    org.priya("chat.list", json!({}));
    let (_task, agent) = claude_code(&h).await;
    let refused = tool(&agent, "chat_post", post_to_priya("hello")).await;
    assert!(refused.is_error, "a readable refusal, not a protocol error");
    assert!(
        refused.text().contains("group legal-hold rule chat.post"),
        "{}",
        refused.text()
    );
    assert!(h.rt.approvals(&h.me).is_empty());
    assert!(org.link.messages().is_empty());
    let record = audited(&h, "chat.post").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Blocked));
    // The person's own Chat is held to it too.
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "chat").await.session;
    let typed = call(&apps, session, "chat.post", post_to_priya("hello")).await;
    assert!(
        !typed.ok && typed.text.contains("Blocked by policy"),
        "{}",
        typed.text
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cf91_the_chat_app_posts_what_the_person_typed_without_asking() {
    let h = harness_with("");
    let org = org(&h);
    org.priya("chat.list", json!({}));
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "chat").await.session;
    let sent = call(&apps, session, "chat.post", post_to_priya("See you at 3")).await;
    assert!(sent.ok, "{}", sent.text);
    assert!(h.rt.approvals(&h.me).is_empty(), "nobody is asked");
    let message = org.priya_reads();
    assert_eq!(message["text"], "See you at 3");
    assert!(message["attribution"].is_null(), "the person typed it");
    let record = audited(&h, "chat.post").pop().expect("audited");
    assert_eq!(record.event.decision.as_ref().unwrap().tier, Tier::Auto);
}

#[tokio::test(flavor = "multi_thread")]
async fn cf91_a_group_rule_makes_the_chat_app_ask_too() {
    let h = harness_with(
        r#"
[[group]]
name = "trading"
members = ["{me}"]
[group.tiers]
"chat.post" = "ask"
"#,
    );
    let org = org(&h);
    org.priya("chat.list", json!({}));
    let apps = Arc::new(Client::connect(h.serve_apps()).await.unwrap());
    let session = open(&apps, "chat").await.session;
    let sent = {
        let apps = Arc::clone(&apps);
        tokio::spawn(async move { call(&apps, session, "chat.post", post_to_priya("Buy")).await })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(ask.summary, "Message Priya Shah");
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
    let sent = sent.await.unwrap();
    assert!(sent.ok, "{}", sent.text);
    assert_eq!(org.link.messages().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn au10_an_agents_message_carries_by_agent_for_user_inside_the_seal() {
    let h = harness_with("");
    let org = org(&h);
    org.priya("chat.list", json!({}));
    opens_chat(&h).await;
    let (_task, agent) = claude_code(&h).await;
    let call = {
        let agent = Arc::clone(&agent);
        tokio::spawn(
            async move { tool(&agent, "chat_post", post_to_priya("Draft attached")).await },
        )
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
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
    let posted = call.await.unwrap();
    assert!(!posted.is_error, "{}", posted.text());
    let message = org.priya_reads();
    assert_eq!(message["text"], "Draft attached");
    assert_eq!(
        message["attribution"],
        format!("by claude-code for {}", h.me.name)
    );
    // The by-line is sealed: the console's copy doesn't show it.
    let relayed = &org.link.messages()[0];
    assert!(!relayed.header.contains("claude-code"));
    assert!(!relayed.ct.contains(&hex_of("claude-code")));
}

fn hex_of(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn cf91_without_the_console_a_post_is_a_readable_refusal_and_not_queued() {
    let h = harness_with("");
    let org = org(&h);
    org.priya("chat.list", json!({}));
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "chat").await.session;
    // Opened once while the console answered, so the key is registered.
    assert!(call(&apps, session, "chat.list", json!({})).await.ok);
    org.link.offline(true);
    let refused = call(&apps, session, "chat.post", post_to_priya("Are you there?")).await;
    assert!(!refused.ok);
    assert!(
        refused.text.contains("can't be reached") && refused.text.contains("Nothing was queued"),
        "{}",
        refused.text
    );
    org.link.offline(false);
    // Back online, nothing went out on its own.
    h.chat.sync_once(0).unwrap();
    assert!(org.link.messages().is_empty());
    // And reading still works from what this computer kept.
    assert!(call(&apps, session, "chat.list", json!({})).await.ok);
}
