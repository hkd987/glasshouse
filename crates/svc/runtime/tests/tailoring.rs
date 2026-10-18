//! Personal layers on catalog agents (PRD "Tailoring catalog items"), through
//! the runtime: what can change (TL-1), versions and going back (TL-2),
//! permissions that only narrow (TL-10), and a new agent version re-checking
//! the layer (TL-13).

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use glass_api::{CheckResult, LayerChange, LayerRevert, LayerStatus};
use glass_audit::EventKind;
use glass_rpc::mcp::ListToolsResult;
use glass_runtime::RuntimeError;
use glass_types::{AgentId, Tier};
use serde_json::{Value, json};

use crate::common::{Harness, harness, invoice_demo, rebuild_with};

fn demo() -> AgentId {
    "invoice-demo".parse().unwrap()
}

fn change(settings: &[(&str, Value)], restrict: &[(&str, Tier)], note: &str) -> LayerChange {
    LayerChange {
        agent: demo(),
        settings: settings
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
        restrict: restrict
            .iter()
            .map(|(p, t)| ((*p).to_owned(), *t))
            .collect(),
        note: note.into(),
    }
}

fn invalid(result: Result<impl std::fmt::Debug, RuntimeError>) -> String {
    match result {
        Err(RuntimeError::Invalid(message)) => message,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn tl1_only_declared_settings_can_change() {
    let h = harness();
    let view = h.rt.layer_get(&h.me, &demo()).unwrap();
    assert_eq!(view.status, LayerStatus::Off);
    let keys: Vec<_> = view.points.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["flag_over", "remind_vendors"]);

    let message = invalid(h.rt.layer_set(
        &h.me,
        change(
            &[("model", json!("other")), ("flag_over", json!("big"))],
            &[],
            "",
        ),
    ));
    assert!(message.contains("model can't be changed"), "{message}");
    assert!(
        message.contains("Flag invoices over: must be a number"),
        "{message}"
    );
    assert_eq!(
        h.rt.layer_get(&h.me, &demo()).unwrap().status,
        LayerStatus::Off,
        "nothing was saved"
    );
}

#[test]
fn tl10_a_layer_can_only_narrow_permissions() {
    let h = harness();
    // Sends are Ask by default; a layer can't make them Notify.
    let message = invalid(h.rt.layer_set(
        &h.me,
        change(&[], &[("mail.send", Tier::Notify)], "send without asking"),
    ));
    assert!(message.contains("Permissions only narrow"), "{message}");

    let view =
        h.rt.layer_set(
            &h.me,
            change(&[], &[("mail.send", Tier::Never)], "no sends"),
        )
        .unwrap();
    assert_eq!(view.status, LayerStatus::On);
    let narrows = view
        .checks
        .iter()
        .find(|c| c.name == "Permissions only narrow");
    assert_eq!(narrows.unwrap().result, CheckResult::Passed);
    // TL-11 needs evaluation suites, and says so rather than passing.
    let bar = view
        .checks
        .iter()
        .find(|c| c.name == "Release bar")
        .unwrap();
    assert_eq!(bar.result, CheckResult::NotAvailable);

    let card =
        h.rt.plan(&h.me, &h.request(&["Invoices"], &["Reports"]))
            .unwrap();
    assert!(card.asks_before.iter().all(|a| a != "Send emails"));
    assert!(
        card.cant_touch.iter().any(|c| c.contains("Send emails")),
        "{:?}",
        card.cant_touch
    );
}

#[tokio::test]
async fn tl2_a_layer_reaches_the_agent_and_every_version_can_be_gone_back_to() {
    let h = harness();
    h.rt.layer_set(
        &h.me,
        change(&[("flag_over", json!(100))], &[], "flag big ones"),
    )
    .unwrap();
    let view =
        h.rt.layer_set(
            &h.me,
            change(
                &[("flag_over", json!(250)), ("remind_vendors", json!(false))],
                &[("mail.send", Tier::Never)],
                "and no reminders",
            ),
        )
        .unwrap();
    assert_eq!(view.active, Some(2));
    assert_eq!(view.history.len(), 2);
    assert_eq!(
        view.history[0].summary,
        [
            "Flag invoices over: 250",
            "Remind vendors: off",
            "Send emails: never"
        ]
    );

    let card =
        h.rt.plan(&h.me, &h.request(&["Invoices"], &["Reports"]))
            .unwrap();
    let line = card.layer.unwrap();
    assert_eq!((line.version, line.paused), (2, None));

    let task =
        h.rt.start(&h.me, h.request(&["Invoices"], &["Reports"]))
            .unwrap()
            .id;
    let agent = h.agent(task).await;
    let init: Value = agent
        .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}))
        .await
        .unwrap();
    let instructions = init["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("Flag invoices over (flag_over): 250"),
        "{instructions}"
    );
    let tools: ListToolsResult = agent.call("tools/list", json!({})).await.unwrap();
    assert!(
        tools.tools.iter().all(|t| t.name != "mail_send"),
        "the layer took sends away"
    );
    let detail = h.rt.get(&h.me, task).unwrap();
    assert!(detail.activity.iter().any(|a| {
        a.text
            .starts_with("Using your layer v2: Flag invoices over: 250")
    }));
    h.rt.stop(&h.me, task).unwrap();

    // Going back is switching versions; history stays.
    let view =
        h.rt.layer_revert(
            &h.me,
            LayerRevert {
                agent: demo(),
                to: Some(1),
            },
        )
        .unwrap();
    assert_eq!((view.active, view.history.len()), (Some(1), 2));
    let card =
        h.rt.plan(&h.me, &h.request(&["Invoices"], &["Reports"]))
            .unwrap();
    assert!(
        card.asks_before.iter().any(|a| a == "Send emails"),
        "sends are back to Ask"
    );
    let view =
        h.rt.layer_revert(
            &h.me,
            LayerRevert {
                agent: demo(),
                to: None,
            },
        )
        .unwrap();
    assert_eq!(view.status, LayerStatus::Off);
    assert!(
        h.rt.plan(&h.me, &h.request(&[], &[]))
            .unwrap()
            .layer
            .is_none()
    );

    let started =
        glass_audit::read(&h.audit_path(), |r| r.event.kind == EventKind::TaskStarted).unwrap();
    assert_eq!(started[0].event.detail["layer"], json!(2));
    assert_eq!(
        started[0].event.detail["settings"]["remind_vendors"],
        json!(false)
    );
    let changes =
        glass_audit::read(&h.audit_path(), |r| r.event.kind == EventKind::LayerChanged).unwrap();
    assert_eq!(changes.len(), 4, "two saves, a switch and turning it off");
}

#[test]
fn tl13_a_new_agent_version_rechecks_the_layer_and_pauses_it_if_it_no_longer_fits() {
    let h = harness();
    h.rt.layer_set(&h.me, change(&[("flag_over", json!(100))], &[], ""))
        .unwrap();

    // 0.2 keeps the setting: the layer carries on.
    let mut next = invoice_demo();
    next.version = "0.2".into();
    let h = Harness {
        rt: rebuild_with(&h, glass_policy::DEFAULT_POLICY, vec![next.clone()]),
        ..h
    };
    h.rt.recheck_layers();
    assert_eq!(
        h.rt.layer_get(&h.me, &demo()).unwrap().status,
        LayerStatus::On
    );

    // 0.3 drops it: the layer is paused and tasks run the published agent.
    next.version = "0.3".into();
    next.customize.retain(|p| p.key != "flag_over");
    let h = Harness {
        rt: rebuild_with(&h, glass_policy::DEFAULT_POLICY, vec![next]),
        ..h
    };
    h.rt.recheck_layers();
    let view = h.rt.layer_get(&h.me, &demo()).unwrap();
    let LayerStatus::Paused { reason } = view.status else {
        panic!("expected a paused layer, got {:?}", view.status);
    };
    assert!(
        reason.contains("version 0.3") && reason.contains("flag_over"),
        "{reason}"
    );
    let card = h.rt.plan(&h.me, &h.request(&[], &[])).unwrap();
    assert!(card.layer.unwrap().paused.is_some());

    // Going back to a version that doesn't fit is refused; turning it off works.
    let message = invalid(h.rt.layer_revert(
        &h.me,
        LayerRevert {
            agent: demo(),
            to: Some(1),
        },
    ));
    assert!(message.contains("no longer fits"), "{message}");
    h.rt.layer_revert(
        &h.me,
        LayerRevert {
            agent: demo(),
            to: None,
        },
    )
    .unwrap();

    let records =
        glass_audit::read(&h.audit_path(), |r| r.event.kind == EventKind::LayerChanged).unwrap();
    let summaries: Vec<_> = records.iter().map(|r| r.event.summary.as_str()).collect();
    assert!(
        summaries.iter().any(|s| s.contains("still fits")),
        "{summaries:?}"
    );
    assert!(
        summaries.iter().any(|s| s.starts_with("Paused your layer")),
        "{summaries:?}"
    );
}

#[test]
fn layers_are_per_user_and_need_the_agent_allowed() {
    let h = harness();
    h.rt.layer_set(&h.me, change(&[("flag_over", json!(100))], &[], ""))
        .unwrap();
    let other = glass_runtime::Caller {
        name: "mallory".into(),
        uid: h.me.uid + 4242,
        gid: h.me.gid,
        home: "/home/mallory".into(),
    };
    // Everyone may run the invoice agent; Mallory's layer is her own.
    let theirs = h.rt.layer_get(&other, &demo()).unwrap();
    assert_eq!(theirs.status, LayerStatus::Off);
    assert!(theirs.history.is_empty());
    assert_eq!(h.rt.layer_get(&h.me, &demo()).unwrap().active, Some(1));
    let missing = h.rt.layer_get(&h.me, &"no-such-agent".parse().unwrap());
    assert!(matches!(missing, Err(RuntimeError::NotFound(_))));
}
