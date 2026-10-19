//! The Store (DV-10, DV-11): what the org's catalog offers a person, and
//! their choices from it. An item the catalog makes *available* reaches
//! them only once they install it; a *required* one always; one another
//! group gets, or that's blocked, never. Installing only ever narrows:
//! policy still decides who may run anything (ADR 0023).

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use glass_api::{AppCallResult, AppSession, TaskRequest, app_methods};
use glass_app_catalog::{Applied, INSTALLS};
use glass_managed::{CatalogItem, CatalogRelease, Declared, InstallMode, ItemKind};
use glass_rpc::Client;
use glass_runtime::RuntimeError;
use glass_types::{Label, TaskId};
use serde_json::{Value, json};

use crate::common::{Harness, harness};

fn item(id: &str, kind: ItemKind, groups: &[(&str, InstallMode)]) -> CatalogItem {
    CatalogItem {
        id: id.parse().unwrap(),
        kind,
        name: id.into(),
        version: "1.0.0".into(),
        publisher: "Acme IT".into(),
        publisher_verified: true,
        internal: true,
        summary: format!("The {id}"),
        keywords: Vec::new(),
        declares: Declared {
            actions: vec!["mail.*".into()],
            default_tiers: BTreeMap::new(),
            max_label: Label::Internal,
            egress: Vec::new(),
        },
        groups: groups.iter().map(|(g, m)| ((*g).to_owned(), *m)).collect(),
        pinned: false,
        provenance: Default::default(),
    }
}

fn publish(h: &Harness, items: Vec<CatalogItem>) {
    h.catalog.set(Applied {
        version: 2,
        release: CatalogRelease { items },
    });
}

fn agents(h: &Harness) -> Vec<String> {
    h.rt.agents_for(&h.me)
        .into_iter()
        .map(|a| a.id.to_string())
        .collect()
}

fn apps(h: &Harness) -> Vec<String> {
    h.rt.profile_apps(&h.me)
        .into_iter()
        .map(|a| a.id.to_string())
        .collect()
}

fn install_by_hand(h: &Harness, ids: &[&str]) {
    let path = h.home().join(INSTALLS);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, json!({ "installed": ids }).to_string()).unwrap();
}

#[tokio::test]
async fn dv11_available_reaches_a_person_once_installed_and_never_beyond_policy() {
    use InstallMode::*;
    let h = harness();
    // No catalog: policy alone decides, as on an unmanaged device.
    assert!(agents(&h).contains(&"invoice-demo".to_owned()));
    publish(
        &h,
        vec![
            item("invoice-demo", ItemKind::Agent, &[("everyone", Available)]),
            item("it-agent", ItemKind::Agent, &[("everyone", Required)]),
            // Required by the catalog, but policy doesn't let anyone run it.
            item("writer", ItemKind::Agent, &[("everyone", Required)]),
            item("mail", ItemKind::App, &[("everyone", Available)]),
            item("calendar", ItemKind::App, &[("finance", Required)]),
        ],
    );
    let agents_now = agents(&h);
    assert!(agents_now.contains(&"it-agent".to_owned()), "required");
    assert!(
        agents_now.contains(&"claude-code".to_owned()),
        "not in the catalog: policy's"
    );
    assert!(
        !agents_now.contains(&"invoice-demo".to_owned()),
        "available, not installed"
    );
    assert!(
        !agents_now.contains(&"writer".to_owned()),
        "never beyond policy"
    );
    let apps_now = apps(&h);
    assert!(
        !apps_now.contains(&"mail".to_owned()),
        "available, not installed"
    );
    assert!(
        !apps_now.contains(&"calendar".to_owned()),
        "another group's"
    );
    assert!(
        apps_now.contains(&"terminal".to_owned()),
        "not in the catalog"
    );

    // Not installed, it doesn't start either.
    let err =
        h.rt.start(
            &h.me,
            TaskRequest {
                agent: "invoice-demo".parse().unwrap(),
                prompt: "Total the invoices".into(),
                scope: Default::default(),
                title: None,
            },
        )
        .unwrap_err();
    assert!(
        matches!(err, RuntimeError::PermissionDenied(ref why) if why.contains("Store")),
        "{err}"
    );

    // Installed (even by editing the file), it reaches them; policy still
    // keeps out what it doesn't allow.
    install_by_hand(&h, &["invoice-demo", "mail", "writer", "calendar"]);
    let agents_now = agents(&h);
    assert!(agents_now.contains(&"invoice-demo".to_owned()));
    assert!(
        !agents_now.contains(&"writer".to_owned()),
        "installing never widens policy"
    );
    let apps_now = apps(&h);
    assert!(apps_now.contains(&"mail".to_owned()));
    assert!(
        !apps_now.contains(&"calendar".to_owned()),
        "installing never widens the catalog"
    );

    // Blocked for their group, installed or not, it's gone.
    publish(
        &h,
        vec![item(
            "invoice-demo",
            ItemKind::Agent,
            &[("everyone", Available), ("everyone", Blocked)],
        )],
    );
    assert!(!agents(&h).contains(&"invoice-demo".to_owned()));
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

#[tokio::test]
async fn dv11_the_store_lists_installs_and_removes_through_the_pipeline() {
    use InstallMode::*;
    let h = harness();
    let apps = Client::connect(h.serve_apps()).await.unwrap();
    let store = open(&apps, "store").await;

    // Before IT's catalog arrives, the Store says so.
    let empty = call(&apps, store.session, "catalog.list", json!({})).await;
    assert!(empty.ok, "{}", empty.text);
    assert_eq!(empty.data["catalog_version"], Value::Null);

    publish(
        &h,
        vec![
            item("invoice-demo", ItemKind::Agent, &[("everyone", Available)]),
            item("it-agent", ItemKind::Agent, &[("everyone", Required)]),
            item("finance-tool", ItemKind::App, &[("finance", Available)]),
        ],
    );
    let listed = call(&apps, store.session, "catalog.list", json!({})).await;
    let items = listed.data["items"].as_array().unwrap();
    let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        ["invoice-demo", "it-agent"],
        "only what their groups get"
    );
    assert_eq!(items[0]["install"], json!("available"));
    assert_eq!(items[0]["installed"], json!(false));
    assert_eq!(items[1]["installed"], json!(true), "required is theirs");

    let installed = call(
        &apps,
        store.session,
        "catalog.install",
        json!({ "id": "invoice-demo" }),
    )
    .await;
    assert!(installed.ok, "{}", installed.text);
    assert!(agents(&h).contains(&"invoice-demo".to_owned()));

    // What their groups don't get can't be installed, nor required ones.
    let other = call(
        &apps,
        store.session,
        "catalog.install",
        json!({ "id": "finance-tool" }),
    )
    .await;
    assert!(!other.ok, "{}", other.text);
    let required = call(
        &apps,
        store.session,
        "catalog.remove",
        json!({ "id": "it-agent" }),
    )
    .await;
    assert!(!required.ok, "{}", required.text);

    let removed = call(
        &apps,
        store.session,
        "catalog.remove",
        json!({ "id": "invoice-demo" }),
    )
    .await;
    assert!(removed.ok, "{}", removed.text);
    assert!(!agents(&h).contains(&"invoice-demo".to_owned()));
}
