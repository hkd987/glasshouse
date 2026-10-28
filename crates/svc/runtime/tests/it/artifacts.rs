//! Artifacts (AR-1, ADR 0033) through the real pipeline, with an in-memory
//! console: an agent's share asks and says who will see it, and once
//! approved the colleague opens the page; what the person does in the
//! Artifacts app doesn't ask; every read is audited without the page; an
//! artifact's label holds like a file's (ADR 0034 §5).

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use glass_actions::ActionProvider;
use glass_api::{
    AnswerApproval, AppCallResult, AppSession, ApprovalAnswer, ApprovalScope, ScopeRequest,
    TaskRequest, app_methods,
};
use glass_app_artifacts::ArtifactsProvider;
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

const PAGE: &str = "<main><h1>Q3 forecast</h1><p>Canary-5521: Northwind at 42k</p></main>";

/// Priya's computer, in one org with the test's through an in-memory
/// console.
struct Org {
    link: Arc<MemoryLink>,
    priya_chat: ChatHandle,
    priya: ArtifactsProvider,
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
    let org = Org {
        link,
        priya: ArtifactsProvider::new(shared, h.directory.clone()),
        priya_chat,
        _dir: dir,
    };
    // Priya has opened Artifacts once, which made her keys.
    org.priya("artifact.list", json!({}));
    org
}

impl Org {
    /// Priya, in Artifacts on her own computer, after it synced.
    fn priya(&self, action: &str, input: Value) -> Value {
        self.priya_chat.sync_once(0).unwrap();
        let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
        ctx.principal.on_behalf_of.name = "priya".into();
        ctx.principal.agent = "artifacts".parse().unwrap();
        self.priya
            .invoke(&ctx, &action.parse().unwrap(), &input)
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

async fn claude_code(h: &Harness) -> Arc<Client> {
    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                persona: None,
                agent: "claude-code".parse().unwrap(),
                prompt: "Share the Q3 forecast with Priya".into(),
                scope: ScopeRequest::default(),
                title: None,
                computer: None,
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
    Arc::new(client)
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
async fn ar1_an_agents_share_asks_who_sees_it_and_the_colleague_opens_it() {
    let h = harness_with("");
    let org = org(&h);
    // The person opened Artifacts once, which made their keys.
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "artifacts").await.session;
    assert!(call(&apps, session, "artifact.list", json!({})).await.ok);

    let agent = claude_code(&h).await;
    let share = {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            tool(
                &agent,
                "artifact_share",
                json!({ "title": "Q3 forecast", "html": PAGE, "with": ["priya"] }),
            )
            .await
        })
    };
    eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    let ask = h.rt.approvals(&h.me).remove(0);
    assert_eq!(ask.summary, "Share Q3 forecast");
    assert_eq!(ask.labels.approve, "Share");
    assert!(
        ask.items.iter().any(|i| i == "Shared with Priya Shah"),
        "{:?}",
        ask.items
    );
    assert!(org.link.blobs().is_empty(), "nothing leaves before the yes");
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
    let shared = share.await.unwrap();
    assert!(!shared.is_error, "{}", shared.text());
    let record = audited(&h, "artifact.share").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Completed));
    assert_eq!(record.event.decision.as_ref().unwrap().tier, Tier::Ask);

    // Priya opens it and comments.
    let list = org.priya("artifact.list", json!({}));
    let id = list["artifacts"][0]["id"].as_str().unwrap().to_owned();
    let read = org.priya("artifact.read", json!({ "artifact": id }));
    assert_eq!(read["html"], PAGE);
    assert_eq!(
        read["versions"][0]["attribution"],
        format!("by claude-code for {}", h.me.name)
    );
    org.priya(
        "artifact.comment",
        json!({ "artifact": id, "text": "It's 48.5k now", "anchor": { "path": "main:nth-of-type(1) > p:nth-of-type(1)" } }),
    );

    // The person reads it in Artifacts and answers there: nobody is asked.
    h.chat.sync_once(0).unwrap();
    let mine = call(&apps, session, "artifact.read", json!({ "artifact": id })).await;
    assert!(mine.ok, "{}", mine.text);
    assert_eq!(mine.data["comments"][0]["text"], "It's 48.5k now");
    let comment = mine.data["comments"][0]["id"].clone();
    let resolved = call(
        &apps,
        session,
        "artifact.resolve",
        json!({ "artifact": id, "comment": comment }),
    )
    .await;
    assert!(resolved.ok, "{}", resolved.text);
    assert!(h.rt.approvals(&h.me).is_empty(), "the person did it");
    let record = audited(&h, "artifact.resolve").pop().expect("audited");
    assert_eq!(record.event.decision.as_ref().unwrap().tier, Tier::Auto);

    // Reads are audited; the page isn't in the log.
    assert!(!audited(&h, "artifact.read").is_empty());
    let log = std::fs::read_to_string(h.audit_path()).unwrap();
    assert!(!log.contains("Canary-5521"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ar1_a_group_rule_making_sharing_never_holds_in_the_app_too() {
    let h = harness_with(
        r#"
[[group]]
name = "legal-hold"
members = ["{me}"]
[group.tiers]
"artifact.share" = "never"
"#,
    );
    let org = org(&h);
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "artifacts").await.session;
    assert!(call(&apps, session, "artifact.list", json!({})).await.ok);
    let shared = call(
        &apps,
        session,
        "artifact.share",
        json!({ "title": "Q3 forecast", "html": PAGE, "with": ["priya"] }),
    )
    .await;
    assert!(
        !shared.ok && shared.text.contains("Blocked by policy"),
        "{}",
        shared.text
    );
    assert!(org.link.blobs().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0034_an_artifacts_label_holds_like_a_files() {
    let h = harness_with("");
    // An org that handles confidential data; Claude Code's own rule still
    // keeps it to internal.
    h.apply_policy_replacing(
        "the PRD data labels).\nmax_label = \"internal\"",
        "the PRD data labels).\nmax_label = \"confidential\"",
        "",
    );
    let org = org(&h);
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session = open(&apps, "artifacts").await.session;
    assert!(call(&apps, session, "artifact.list", json!({})).await.ok);
    // Priya shares a confidential page with the person.
    let shared = org.priya(
        "artifact.share",
        json!({ "title": "Board deck", "html": PAGE, "with": [h.me.name], "label": "confidential" }),
    );
    let id = shared["id"].as_str().unwrap().to_owned();
    h.chat.sync_once(0).unwrap();

    // Claude Code may open internal data at most: refused, readably.
    let agent = claude_code(&h).await;
    let refused = tool(&agent, "artifact_read", json!({ "artifact": id })).await;
    assert!(refused.is_error, "{}", refused.text());
    assert!(
        refused
            .text()
            .contains("Board deck is labeled confidential"),
        "{}",
        refused.text()
    );
    let record = audited(&h, "artifact.read").pop().expect("audited");
    assert_eq!(record.event.outcome, Some(Outcome::Blocked));

    // The person opens it in Artifacts.
    let read = call(&apps, session, "artifact.read", json!({ "artifact": id })).await;
    assert!(read.ok, "{}", read.text);
    assert_eq!(read.data["label"], "confidential");
}
