//! Skills' evals through the real pipeline (ADR 0034, SK-5): the
//! skill-evals agent reads the skill, asks the model through the gateway
//! with the skill as its instructions, and keeps the scores on this
//! computer; nothing else may report scores; a confidential skill's evals
//! go only where policy routes confidential data. The test plays the
//! agent over the task socket, with the scripted model.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use glass_actions::ActionProvider;
use glass_api::{ScopeRequest, TaskRequest, app_methods};
use glass_app_chat::{ChatHandle, ChatProvider};
use glass_app_skills::SkillsProvider;
use glass_audit::{EventKind, Outcome};
use glass_crypto::SecretKey;
use glass_managed::chat::testing::MemoryLink;
use glass_managed::directory::Applied;
use glass_managed::skills::Bundle;
use glass_managed::{ChatRelease, DirectoryRelease, Person};
use glass_rpc::Client;
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness_with, tool};

/// Echo answers internal data; nothing is approved for confidential.
const MODELS: &str = r#"
[models]
default = "echo"
max_output_tokens = 2000

[models.routes]
internal = ["echo"]

[[models.provider]]
id = "scripted"
kind = "scripted"
name = "Scripted"

[[models.model]]
id = "echo"
provider = "scripted"
model = "echo-1"
name = "Echo"
"#;

fn files(policy: &str) -> Value {
    let evals = json!({
        "runs": 1,
        "cases": [{ "id": "window", "prompt": "How long do refunds take?", "graders": [{ "type": "contains", "text": "30 days" }] }]
    });
    json!({
        "SKILL.md": format!("---\nname: Refunds\ndescription: Answers questions about refunds\n---\n{policy}\n"),
        "evals.json": evals.to_string(),
    })
}

struct Org {
    priya: SkillsProvider,
    priya_chat: ChatHandle,
    _dir: tempfile::TempDir,
}

fn org(h: &Harness) -> Org {
    let person = |login: &str, name: &str| Person {
        login: login.into(),
        name: name.into(),
        email: format!("{login}@acme.example"),
        title: String::new(),
        department: String::new(),
    };
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
    Org {
        priya: SkillsProvider::new(shared, h.directory.clone(), dir.path().join("skills")),
        priya_chat,
        _dir: dir,
    }
}

impl Org {
    /// Priya, in Skills on her own computer.
    fn priya(&self, action: &str, input: Value) -> Value {
        self.priya_chat.sync_once(0).unwrap();
        let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
        ctx.principal.on_behalf_of.name = "priya".into();
        ctx.principal.agent = "skills".parse().unwrap();
        self.priya
            .invoke(&ctx, &action.parse().unwrap(), &input)
            .unwrap()
            .data
    }
}

/// The person has keys (Artifacts made them), and Priya shares a skill
/// labeled `label` with them; their computer has synced. Its id.
async fn shared_with_me(h: &Harness, org: &Org, label: &str) -> String {
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let session: glass_api::AppSession = apps
        .call(app_methods::APP_OPEN, json!({ "app": "artifacts" }))
        .await
        .unwrap();
    let _: Value = apps
        .call(
            app_methods::APP_CALL,
            json!({ "session": session.session, "action": "artifact.list", "input": {} }),
        )
        .await
        .unwrap();
    org.priya("skill.list", json!({}));
    let shared = org.priya(
        "skill.share",
        json!({ "files": files("Refunds take 14 days."), "with": [h.me.name], "label": label }),
    );
    h.chat.sync_once(0).unwrap();
    shared["id"].as_str().unwrap().to_owned()
}

async fn agent(h: &Harness, id: &str, prompt: &str) -> Arc<Client> {
    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: id.parse().unwrap(),
                prompt: prompt.into(),
                scope: ScopeRequest::default(),
                title: None,
            },
        )
        .unwrap();
    // Claude Code connects through its driver socket; the others are
    // played on their task socket.
    let Some(socket) = view.driver_socket.clone() else {
        return Arc::new(h.agent(view.id).await);
    };
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
async fn sk5_evals_run_through_the_gateway_with_the_skill_as_instructions() {
    let h = harness_with(MODELS);
    let org = org(&h);
    let id = shared_with_me(&h, &org, "internal").await;

    let evals = agent(&h, "skill-evals", &id).await;
    let read = tool(&evals, "skill_read", json!({ "skill": id })).await;
    assert!(!read.is_error, "{}", read.text());
    let data = read.structured_content.clone().unwrap();
    let bundle: Bundle = serde_json::from_value(json!({ "files": data["files"] })).unwrap();
    let case = &bundle.check().unwrap().evals.cases[0];
    let (system, user) = bundle.case_request(case);
    let answer = tool(
        &evals,
        "model_complete",
        json!({ "system": system, "messages": [{ "role": "user", "content": user }], "max_tokens": 500 }),
    )
    .await;
    assert!(!answer.is_error, "{}", answer.text());
    // The skill went as the model's instructions, through the gateway.
    let seen = h.model.seen().pop().unwrap();
    assert!(seen.system.unwrap().contains("Refunds take 14 days."));
    assert_eq!(answer.structured_content.as_ref().unwrap()["model"], "echo");
    assert_eq!(
        case.graders[0].grade(&answer.text()),
        Some(false),
        "it says 14"
    );
    let kept = tool(
        &evals,
        "skill_results",
        json!({ "skill": id, "version": 1, "model": "echo", "runner": "skill-evals 1", "cases": [{ "id": "window", "passed": 0, "runs": 1 }] }),
    )
    .await;
    assert!(!kept.is_error, "{}", kept.text());
    assert_eq!(kept.text(), "Kept on this computer: 0 of 1 case (0%).");

    // Claude Code can't report scores: the tool isn't its to call.
    let claude = agent(&h, "claude-code", "Score the refunds skill").await;
    let claimed = tool(
        &claude,
        "skill_results",
        json!({ "skill": id, "model": "m", "runner": "r", "cases": [] }),
    )
    .await;
    assert!(claimed.is_error, "{}", claimed.text());
    assert!(audited(&h, "skill.results").iter().all(|r| {
        r.event
            .agent
            .as_ref()
            .is_some_and(|a| a.as_str() == "skill-evals")
            || r.event.outcome != Some(Outcome::Completed)
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn sk5_a_confidential_skills_evals_go_only_where_confidential_data_may() {
    let h = harness_with(MODELS);
    h.apply_policy_replacing(
        "the PRD data labels).\nmax_label = \"internal\"",
        "the PRD data labels).\nmax_label = \"confidential\"",
        "",
    );
    let org = org(&h);
    let id = shared_with_me(&h, &org, "confidential").await;
    let evals = agent(&h, "skill-evals", &id).await;
    let read = tool(&evals, "skill_read", json!({ "skill": id })).await;
    assert!(!read.is_error, "{}", read.text());
    // Reading it made the task's data confidential: no model is approved.
    let answer = tool(
        &evals,
        "model_complete",
        json!({ "system": "x", "messages": [{ "role": "user", "content": "How long do refunds take?" }], "max_tokens": 100 }),
    )
    .await;
    assert!(answer.is_error, "{}", answer.text());
    assert!(answer.text().contains("confidential"), "{}", answer.text());
    assert!(h.model.seen().is_empty(), "nothing left the computer");
    let record = audited(&h, "model.complete").pop().expect("audited");
    assert_ne!(record.event.outcome, Some(Outcome::Completed));
}
