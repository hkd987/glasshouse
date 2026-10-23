//! Apps people make (ADR 0032, TL-5, GA-3, GA-5): an agent remixes an
//! installed app through the studio's actions; the remix is its maker's
//! alone, in their launcher, served from what glassd built, without the
//! rules policy gives the app it remixes, gone when that app leaves their
//! profile, and undone like any change.

#![allow(clippy::unwrap_used)]

mod common;

use glass_api::{AppFiles, AppSession, ScopeRequest, TaskRequest, UndoRequest, app_methods};
use glass_rpc::Client;
use glass_runtime::{Caller, RuntimeError};
use glass_types::{TaskId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, eventually, harness, tool};

/// Starts the person's own harness (attended) and connects to its socket.
async fn claude_code(h: &Harness) -> (TaskId, Client) {
    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: "claude-code".parse().unwrap(),
                prompt: "Make Chat ask before every message I send".into(),
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
    (view.id, client)
}

/// Chat's source and manifest, saved as the person's own Chat.
async fn remix_chat(client: &Client) -> Value {
    let source = tool(client, "apps_source", json!({ "app": "chat" })).await;
    assert!(!source.is_error, "{}", source.text());
    let data = source.structured_content.unwrap();
    assert_eq!(data["name"], "Chat");
    let saved = tool(
        client,
        "apps_save",
        json!({
            "base": "chat",
            "manifest": data["manifest"],
            "files": data["files"],
            "note": "The same Chat, mine to change",
        }),
    )
    .await;
    assert!(!saved.is_error, "{}", saved.text());
    saved.structured_content.unwrap()
}

async fn open(client: &Client, app: &str) -> Result<AppSession, glass_rpc::ClientError> {
    client
        .call(app_methods::APP_OPEN, json!({ "app": app }))
        .await
}

fn tier(session: &AppSession, action: &str) -> Tier {
    session
        .actions
        .iter()
        .find(|a| a.name.as_str() == action)
        .unwrap()
        .tier
}

fn someone_else(h: &Harness) -> Caller {
    let home = h.dir.path().join("elsewhere");
    std::fs::create_dir_all(&home).unwrap();
    Caller {
        name: "priya".into(),
        uid: h.me.uid + 1,
        gid: h.me.gid,
        home,
    }
}

#[tokio::test]
async fn tl5_ga5_a_remix_is_its_makers_alone_and_never_has_its_bases_rules() {
    let h = harness();
    let (task, client) = claude_code(&h).await;
    let saved = remix_chat(&client).await;
    assert_eq!(saved["app"], "my-chat");
    assert_eq!(saved["based_on"]["id"], "chat");
    assert!(
        h.rt.approvals(&h.me).is_empty(),
        "Notify: saving their own version asks nothing"
    );

    // In their launcher, after the org's apps.
    let profile = h.rt.profile_apps(&h.me);
    let mine = profile.iter().find(|a| a.id.as_str() == "my-chat").unwrap();
    assert_eq!(mine.name, "My Chat");
    assert_eq!(mine.based_on.as_ref().map(|b| b.as_str()), Some("chat"));
    assert_eq!(profile.last().unwrap().id.as_str(), "my-chat");

    // Opened, it posts only after asking: Chat's own rule is Chat's.
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let chat = open(&apps, "chat").await.unwrap();
    let remix = open(&apps, "my-chat").await.unwrap();
    assert_eq!(tier(&chat, "chat.post"), Tier::Auto);
    assert_eq!(tier(&remix, "chat.post"), Tier::Ask);

    // The bridge serves what glassd built, for its maker only.
    let files: AppFiles = apps
        .call(app_methods::APP_FILES, json!({ "app": "my-chat" }))
        .await
        .unwrap();
    assert_eq!(files.version, 1);
    assert!(files.files["index.html"].contains("<title>My Chat</title>"));
    let other = someone_else(&h);
    assert!(matches!(
        h.rt.app_files(&other, &"my-chat".parse().unwrap()),
        Err(RuntimeError::NotFound(_))
    ));
    assert!(matches!(
        h.rt.open_app(&other, &"my-chat".parse().unwrap()),
        Err(RuntimeError::NotFound(_))
    ));
    assert!(
        apps.call::<AppFiles>(app_methods::APP_FILES, json!({ "app": "chat" }))
            .await
            .is_err(),
        "installed apps' files are the bridge's to read"
    );

    // Undo the harness's task: the save is undone, and the app is gone
    // from the launcher.
    let report =
        h.rt.undo(
            &h.me,
            UndoRequest {
                task,
                to: None,
                changes: vec![],
                dry_run: false,
            },
        )
        .await
        .unwrap();
    assert!(report.not_undone.is_empty(), "{report:?}");
    assert!(
        !h.rt
            .profile_apps(&h.me)
            .iter()
            .any(|a| a.id.as_str() == "my-chat")
    );
}

#[tokio::test]
async fn em0_a_remix_of_mail_saves_as_is_and_never_takes_the_mail_password() {
    // ADR 0037: Mail's connect flow comes along in a remix, inert. Only
    // the shipped Mail runs mail.connect; a page an agent wrote can't
    // collect the person's password.
    let h = harness();
    let (_task, client) = claude_code(&h).await;
    let source = tool(&client, "apps_source", json!({ "app": "mail" })).await;
    let data = source.structured_content.unwrap();
    assert!(
        data["manifest"]["actions"]
            .as_array()
            .unwrap()
            .contains(&json!("mail.connect"))
    );
    let saved = tool(
        &client,
        "apps_save",
        json!({ "base": "mail", "manifest": data["manifest"], "files": data["files"], "note": "Mine" }),
    )
    .await;
    assert!(!saved.is_error, "{}", saved.text());
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let mail = open(&apps, "mail").await.unwrap();
    let remix = open(&apps, "my-mail").await.unwrap();
    assert_eq!(tier(&mail, "mail.connect"), Tier::Auto);
    assert_eq!(tier(&remix, "mail.connect"), Tier::Never);
    let tried: glass_api::AppCallResult = apps
        .call(
            app_methods::APP_CALL,
            json!({ "session": remix.session, "action": "mail.connect", "input": { "password": "canary-9931" } }),
        )
        .await
        .unwrap();
    assert!(
        !tried.ok && tried.text.contains("Blocked by policy"),
        "{}",
        tried.text
    );
}

#[tokio::test]
async fn tl5_a_remix_goes_when_the_app_it_remixes_leaves_the_profile() {
    let h = harness();
    let (_task, client) = claude_code(&h).await;
    remix_chat(&client).await;
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let remix = open(&apps, "my-chat").await.unwrap();

    // IT takes Chat out of everyone's apps: the remix closes and won't open.
    h.apply_policy_replacing(
        r#""calendar", "chat", "artifacts","#,
        r#""calendar", "artifacts","#,
        "",
    );
    eventually(|| {
        !h.rt
            .list(&h.me)
            .iter()
            .any(|t| t.id == remix.session && t.state.is_live())
    })
    .await;
    assert!(
        !h.rt
            .profile_apps(&h.me)
            .iter()
            .any(|a| a.id.as_str() == "my-chat")
    );
    let err = open(&apps, "my-chat").await.unwrap_err().to_string();
    assert!(err.contains("isn't in your profile"), "{err}");
}

#[tokio::test]
async fn ga5_policy_decides_what_an_app_someone_makes_may_declare() {
    // Agents may not use meetings, and no app but Meet may join one: a
    // remix of Calendar keeps its Join links only if IT allows it.
    let h = harness();
    let (_task, client) = claude_code(&h).await;
    let source = tool(&client, "apps_source", json!({ "app": "calendar" })).await;
    let data = source.structured_content.unwrap();
    let saved = tool(
        &client,
        "apps_save",
        json!({ "base": "calendar", "manifest": data["manifest"], "files": data["files"], "note": "Mine" }),
    )
    .await;
    assert!(saved.is_error);
    assert!(
        saved
            .text()
            .contains("meet.list: your organization's policy doesn't allow it in apps you make"),
        "{}",
        saved.text()
    );
}

/// TL-6: IT turns remixing off for the org's computers. People's own apps
/// close and won't open, the studio refuses, and their versions are kept:
/// back on, they open again.
#[tokio::test]
async fn tl6_it_turns_remixing_off_and_peoples_apps_wait_for_it() {
    let h = harness();
    let (_task, client) = claude_code(&h).await;
    remix_chat(&client).await;
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let remix = open(&apps, "my-chat").await.unwrap();

    let line = r#""meet", "store", "settings", "routines", "activity-monitor", "terminal"]"#;
    let off =
        format!("{line}\n[group.settings]\n\"apps.remix\" = {{ value = false, locked = true }}");
    h.apply_policy_replacing(line, &off, "");
    eventually(|| {
        !h.rt
            .list(&h.me)
            .iter()
            .any(|t| t.id == remix.session && t.state.is_live())
    })
    .await;
    let mine = |h: &Harness| {
        h.rt.profile_apps(&h.me)
            .iter()
            .any(|a| a.id.as_str() == "my-chat")
    };
    assert!(!mine(&h));
    assert!(open(&apps, "my-chat").await.is_err());
    let refused = tool(&client, "apps_source", json!({ "app": "chat" })).await;
    assert!(refused.is_error);
    assert!(
        refused.text().contains("IT turned remixing apps off"),
        "{}",
        refused.text()
    );

    h.apply_policy_replacing(line, line, "");
    assert!(mine(&h), "kept while off, back when on");
    assert!(open(&apps, "my-chat").await.is_ok());
}

fn listed_actions(listed: &Value) -> impl Iterator<Item = &Value> {
    listed["actions"].as_array().into_iter().flatten()
}

#[tokio::test]
async fn ga1_an_agent_making_an_app_reads_what_each_action_takes_and_returns() {
    let h = harness();
    let (_task, client) = claude_code(&h).await;
    let listed = tool(&client, "apps_actions", json!({})).await;
    assert!(!listed.is_error, "{}", listed.text());
    let listed = listed.structured_content.unwrap();
    let names: Vec<String> = listed_actions(&listed)
        .map(|a| a["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"calendar.read".to_owned()), "{names:?}");
    // Every one says what its data is, so no app is built on a guess.
    let undescribed: Vec<&str> = listed_actions(&listed)
        .filter(|a| a["described"] != true)
        .filter_map(|a| a["name"].as_str())
        .collect();
    assert!(undescribed.is_empty(), "{undescribed:?}");
    // Never Mail's password, nor what policy keeps from apps people make.
    assert!(!names.contains(&"mail.connect".to_owned()), "{names:?}");
    assert!(!names.contains(&"meet.list".to_owned()), "{names:?}");

    // In full, as Calendar declares it: the attendees are in data.event.
    let read = tool(
        &client,
        "apps_actions",
        json!({ "actions": ["calendar.read"] }),
    )
    .await;
    assert!(
        read.text().contains("result.data is:\n{\n  event: {\n"),
        "{}",
        read.text()
    );
    assert!(
        read.text()
            .contains("attendees: string[]; // email addresses, without the organizer"),
        "{}",
        read.text()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0051_two_remixes_made_at_once_are_two_apps() {
    // ADR 0032, GA-3 (specs/tla/StudioVersions.tla): two tasks of one
    // person remixing Chat at once each get an app of their own, My Chat
    // and My Chat 2, never one app's two versions. Their saves take turns,
    // so the second is planned after the first is in the person's apps.
    let h = harness();
    let ((_, a), (_, b)) = (claude_code(&h).await, claude_code(&h).await);
    let (one, two) = tokio::join!(remix_chat(&a), remix_chat(&b));
    let mut apps = [one["app"].clone(), two["app"].clone()];
    apps.sort_by_key(|a| a.to_string());
    assert_eq!(apps, [json!("my-chat"), json!("my-chat-2")], "{one} {two}");
    assert_eq!(
        (one["version"].clone(), two["version"].clone()),
        (json!(1), json!(1))
    );
}
