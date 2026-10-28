//! The folders the desktop agent reads when the person asks it without
//! naming any, from the command bar or in a meeting (ADR 0048): theirs to
//! choose (`agents.folders`, Documents by default), IT's to set or lock,
//! read only, and only those in their home. A request that names folders,
//! another agent and a routine's run keep exactly what they name.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use glass_api::settings::SettingChoice;
use glass_api::{ScopeRequest, TaskRequest};
use serde_json::json;

use crate::common::{Harness, harness, tool};

fn asked(agent: &str, prompt: &str) -> TaskRequest {
    TaskRequest {
        persona: None,
        agent: agent.parse().unwrap(),
        prompt: prompt.into(),
        scope: ScopeRequest::default(),
        title: None,
        computer: None,
    }
}

/// What the handoff card says the task reads and changes.
fn touches(h: &Harness, request: &TaskRequest) -> Vec<(PathBuf, String)> {
    h.rt.plan(&h.me, request)
        .unwrap()
        .touches
        .into_iter()
        .map(|l| (l.path, l.access))
        .collect()
}

fn choose(h: &Harness, folders: serde_json::Value) -> Result<(), String> {
    h.rt.choose_setting(
        &h.me,
        SettingChoice {
            id: "agents.folders".into(),
            value: Some(folders),
        },
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[tokio::test]
async fn adr0048_asked_without_folders_the_desktop_agent_reads_the_ones_the_person_chose() {
    let h = harness();
    let grab = asked(
        glass_api::DESKTOP_AGENT,
        "Grab the ABC report and send it to Susan",
    );
    // Documents by default, once there is one.
    assert_eq!(touches(&h, &grab), []);
    fs::create_dir_all(h.home().join("Documents")).unwrap();
    let documents = fs::canonicalize(h.home().join("Documents")).unwrap();
    assert_eq!(
        touches(&h, &grab),
        [(documents.clone(), "read only".to_owned())]
    );
    // The agent itself sees it among its folders.
    let task = h.rt.start(&h.me, grab.clone()).unwrap().id;
    let agent = h.agent(task).await;
    let listed = tool(&agent, "files_list", json!({ "path": "" })).await;
    assert!(!listed.is_error, "{}", listed.text());
    assert!(listed.text().contains("Documents"), "{}", listed.text());

    // The person's own choice; a folder that isn't there is left out.
    choose(&h, json!(["Reports", "Not there"])).unwrap();
    let reports = fs::canonicalize(h.home().join("Reports")).unwrap();
    assert_eq!(touches(&h, &grab), [(reports, "read only".to_owned())]);
    // Only folders in their home, by name.
    for bad in [json!(["../.."]), json!(["/etc"]), json!(["Reports/../.."])] {
        let err = choose(&h, bad.clone()).unwrap_err();
        assert!(err.contains("isn't a folder in the home"), "{bad}: {err}");
    }

    // A request that names folders gets those, and nothing more.
    let mut named = grab.clone();
    named.scope.read = vec![h.home().join("Invoices")];
    let invoices = fs::canonicalize(h.home().join("Invoices")).unwrap();
    assert_eq!(touches(&h, &named), [(invoices, "read only".to_owned())]);
    // Another agent asked the same way gets none.
    assert_eq!(touches(&h, &asked("invoice-demo", "Summarize")), []);
}

#[tokio::test]
async fn adr0048_it_locks_what_the_desktop_agent_reads() {
    let h = harness();
    fs::create_dir_all(h.home().join("Documents")).unwrap();
    // IT's: none, locked, for everyone.
    h.apply_policy(
        "[[group]]\nname = \"agents-locked\"\nmembers = [\"{me}\"]\n[group.settings]\n\"agents.folders\" = { value = [], locked = true }\n",
        "managed v2",
    );
    let grab = asked(glass_api::DESKTOP_AGENT, "Grab the ABC report");
    assert_eq!(touches(&h, &grab), []);
    let err = choose(&h, json!(["Documents"])).unwrap_err();
    assert!(
        err.contains("organization") || err.contains("locked"),
        "{err}"
    );
}
