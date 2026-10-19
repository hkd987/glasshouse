//! Device settings (UX-1, ADR 0029) through the real sockets: the desktop
//! reads its person's settings over the control socket and hears when they
//! change; the Settings app changes the ones IT left to the person and is
//! refused, in words, for the ones IT locked; agents may read them but not
//! change them.

#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use glass_api::settings::{SettingFrom, SettingsView};
use glass_api::{
    AppCallResult, AppSession, Event, ScopeRequest, TaskRequest, app_methods, methods,
};
use glass_rpc::Client;
use glass_rpc::mcp::ListToolsResult;
use glass_types::TaskId;
use serde_json::{Value, json};

use crate::common::{eventually, harness, harness_with, tool};

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

/// Waits for the next settings change, past the tiles' own updates (app
/// sessions are tasks too).
async fn settings_changed(client: &Client) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let note = client.next_notification().await.unwrap();
            if serde_json::from_value::<Event>(note.params.unwrap()).unwrap()
                == Event::SettingsChanged
            {
                return;
            }
        }
    })
    .await
    .expect("a settings change");
}

const AP_LOCKS: &str = r#"
[[group]]
name = "ap-ops"
members = ["{me}"]
[group.settings]
"lock.after_minutes" = { value = 5, locked = true }
"clock.hour24" = { value = true }
"#;

#[tokio::test(flavor = "multi_thread")]
async fn ux1_the_desktop_reads_its_settings_hears_of_changes_and_the_app_changes_only_what_it_may()
{
    let h = harness();
    let desktop = Client::connect(h.serve_control()).await.unwrap();
    let before: SettingsView = desktop
        .call(methods::SETTINGS_GET, json!(null))
        .await
        .unwrap();
    assert_eq!(
        before.number("lock.after_minutes", 0),
        10,
        "the computer's default"
    );
    assert!(
        before
            .settings
            .iter()
            .all(|s| s.from == SettingFrom::Default)
    );
    let () = desktop
        .call(methods::EVENTS_SUBSCRIBE, json!(null))
        .await
        .unwrap();

    // IT locks the lock-after for AP and makes the 24-hour clock their default.
    h.apply_policy(AP_LOCKS, "managed v2");
    settings_changed(&desktop).await;
    let now: SettingsView = desktop
        .call(methods::SETTINGS_GET, json!(null))
        .await
        .unwrap();
    assert_eq!(now.number("lock.after_minutes", 0), 5);
    assert!(
        now.settings
            .iter()
            .any(|s| s.id == "lock.after_minutes" && s.locked)
    );
    assert!(now.flag("clock.hour24", false));

    // The person turns the 24-hour clock back off in Settings.
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let settings = open(&apps, "settings").await;
    let set = call(
        &apps,
        settings.session,
        "settings.set",
        json!({ "id": "clock.hour24", "value": false }),
    )
    .await;
    assert!(set.ok, "{}", set.text);
    settings_changed(&desktop).await;
    let mine: SettingsView = desktop
        .call(methods::SETTINGS_GET, json!(null))
        .await
        .unwrap();
    assert!(!mine.flag("clock.hour24", true));
    assert!(
        mine.settings
            .iter()
            .any(|s| s.id == "clock.hour24" && s.from == SettingFrom::You)
    );

    // IT's lock holds, and says so as a result the app can show.
    let refused = call(
        &apps,
        settings.session,
        "settings.set",
        json!({ "id": "lock.after_minutes", "value": 60 }),
    )
    .await;
    assert!(!refused.ok);
    assert!(
        refused.text.contains("IT locked Lock after at 5 minutes"),
        "{}",
        refused.text
    );

    let list = call(&apps, settings.session, "settings.list", json!({})).await;
    assert!(list.ok, "{}", list.text);
    assert_eq!(list.text, "9 settings; IT locked 1.");
    assert!(
        list.data["device"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == "browser.home_page")
    );

    // Mail doesn't declare settings, so it can't change them.
    let mail = open(&apps, "mail").await;
    let undeclared = call(
        &apps,
        mail.session,
        "settings.set",
        json!({ "id": "clock.hour24", "value": true }),
    )
    .await;
    assert!(!undeclared.ok);
}

#[tokio::test(flavor = "multi_thread")]
async fn ux1_an_agent_reads_settings_but_cant_change_them() {
    let h = harness_with(
        r#"
[[group]]
name = "ap-ops"
members = ["{me}"]
agents = ["claude-code"]
[group.settings]
"lock.after_minutes" = { value = 5, locked = true }
"#,
    );
    let view =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: "claude-code".parse().unwrap(),
                prompt: "Why does my screen lock so fast?".into(),
                scope: ScopeRequest::default(),
                title: None,
            },
        )
        .unwrap();
    let socket = view.driver_socket.clone().unwrap();
    eventually(|| socket.exists()).await;
    let agent = Client::connect(&socket).await.unwrap();
    let _: Value = agent
        .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}))
        .await
        .unwrap();
    let tools: ListToolsResult = agent.call("tools/list", json!({})).await.unwrap();
    assert!(tools.tools.iter().any(|t| t.name == "settings_list"));
    assert!(!tools.tools.iter().any(|t| t.name == "settings_set"));

    let read = tool(&agent, "settings_list", json!({})).await;
    assert!(!read.is_error, "{}", read.text());
    assert!(read.text().contains("IT locked 1"), "{}", read.text());
    assert!(h.rt.approvals(&h.me).is_empty(), "Auto: nobody is asked");

    let refused = tool(
        &agent,
        "settings_set",
        json!({ "id": "clock.hour24", "value": true }),
    )
    .await;
    assert!(refused.is_error, "a readable refusal, not a protocol error");
    let mine: SettingsView = h.rt.settings_view(&h.me);
    assert!(!mine.flag("clock.hour24", false), "nothing changed");
}
