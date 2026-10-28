//! AD-7, role-based delivery: which apps a person gets is policy (per group),
//! enforced by glassd. An app outside someone's profile isn't offered to
//! them and doesn't open for them, and the refusal is audited.

#![allow(clippy::unwrap_used)]

use crate::common;

use glass_api::ProfileAppKind;
use glass_audit::EventKind;
use glass_runtime::RuntimeError;

use crate::common::harness;

fn ids(h: &common::Harness) -> Vec<(String, ProfileAppKind)> {
    h.rt.profile_apps(&h.me)
        .into_iter()
        .map(|a| (a.id.to_string(), a.kind))
        .collect()
}

#[test]
fn ad7_profiles_offer_everyone_their_apps_and_it_tools_only_to_its_role() {
    let h = harness();
    assert_eq!(
        ids(&h),
        [
            ("activity-monitor".into(), ProfileAppKind::Native),
            ("artifacts".into(), ProfileAppKind::Web),
            ("browser".into(), ProfileAppKind::Native),
            ("calendar".into(), ProfileAppKind::Web),
            ("chat".into(), ProfileAppKind::Web),
            ("docs".into(), ProfileAppKind::Web),
            ("mail".into(), ProfileAppKind::Web),
            ("meet".into(), ProfileAppKind::Web),
            ("routines".into(), ProfileAppKind::Web),
            ("settings".into(), ProfileAppKind::Web),
            ("store".into(), ProfileAppKind::Web),
            ("terminal".into(), ProfileAppKind::Native),
        ]
    );

    // IT's role adds the tools for managing computers: the admin console,
    // which needs a managed device to open, so it isn't offered here.
    h.apply_policy_replacing(
        "name = \"it-admins\"\n",
        "name = \"it-admins\"\nmembers = [\"{me}\"]\n",
        "",
    );
    let names: Vec<String> =
        h.rt.profile_apps(&h.me)
            .into_iter()
            .map(|a| a.name)
            .collect();
    assert_eq!(
        names,
        [
            "Activity Monitor",
            "Artifacts",
            "Browser",
            "Calendar",
            "Chat",
            "Docs",
            "Mail",
            "Meet",
            "Routines",
            "Settings",
            "Store",
            "Terminal"
        ]
    );

    // An app the profile names that isn't on this device isn't offered.
    h.apply_policy_replacing("\"terminal\"]", "\"terminal\", \"notes\"]", "");
    assert!(!ids(&h).iter().any(|(id, _)| id == "notes"));
}

#[tokio::test]
async fn ad7_an_app_outside_the_profile_doesnt_open_and_the_refusal_is_audited() {
    let h = harness();
    h.apply_policy_replacing("\"mail\", \"calendar\", ", "\"mail\", ", "");
    assert!(!ids(&h).iter().any(|(id, _)| id == "calendar"));
    let err =
        h.rt.open_app(&h.me, &"calendar".parse().unwrap())
            .unwrap_err();
    assert!(
        matches!(&err, RuntimeError::PermissionDenied(m) if m.contains("isn't in your profile")),
        "{err:?}"
    );
    assert!(h.rt.open_app(&h.me, &"mail".parse().unwrap()).is_ok());

    let refused =
        glass_audit::read(&h.audit_path(), |r| r.event.kind == EventKind::AppRefused).unwrap();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].event.user.as_deref(), Some(h.me.name.as_str()));
    assert_eq!(
        refused[0].event.agent.as_ref().map(|a| a.as_str()),
        Some("calendar")
    );
}

/// AD-3 (PolicyApply.tla): an open app's call that waited on the desktop
/// for an answer, while IT's release blocking it was applied, is decided
/// again once answered: approving it then sends nothing. The app's next
/// call is refused as before, and an agent's task keeps its own policy.
#[tokio::test]
async fn ad3_an_answer_given_after_it_blocked_the_action_sends_nothing() {
    use std::sync::Arc;

    use glass_api::{
        AnswerApproval, AppCallResult, AppSession, ApprovalAnswer, ApprovalScope, app_methods,
    };
    use glass_rpc::Client;
    use serde_json::json;

    let h = harness();
    let apps = Arc::new(Client::connect(h.serve_apps()).await.unwrap());
    let mail: AppSession = apps
        .call(app_methods::APP_OPEN, json!({ "app": "mail" }))
        .await
        .unwrap();
    let session = mail.session;
    let reply = json!({ "messages": [{ "to": ["dana@northwind.example"],
        "subject": "Re: Renewal terms", "body": "Tuesday works." }] });
    let sending = {
        let (apps, reply) = (Arc::clone(&apps), reply.clone());
        tokio::spawn(async move {
            let sent: AppCallResult = apps
                .call(
                    app_methods::APP_CALL,
                    json!({ "session": session, "action": "mail.send", "input": reply }),
                )
                .await
                .unwrap();
            sent
        })
    };
    common::eventually(|| !h.rt.approvals(&h.me).is_empty()).await;
    // While the person decides, IT's release makes sending mail Never.
    h.apply_policy(
        r#"
[[group]]
name = "frontdesk"
members = ["{me}"]
agents = []
[group.tiers]
"mail.send" = "never"
"#,
        "managed v2",
    );
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
    let sent = sending.await.unwrap();
    assert!(!sent.ok, "sent after IT blocked it: {}", sent.text);
    assert!(sent.text.starts_with("Blocked by policy"), "{}", sent.text);
    let sent_dir = h.home().join("Mail/Sent");
    let kept = std::fs::read_dir(&sent_dir).map_or(0, Iterator::count);
    assert_eq!(kept, 0, "nothing left");
    let blocked = glass_audit::read(&h.audit_path(), |r| {
        r.event.outcome == Some(glass_audit::Outcome::Blocked)
            && r.event.action.as_ref().map(|a| a.as_str()) == Some("mail.send")
    })
    .unwrap();
    assert_eq!(blocked.len(), 1, "the refusal is on the record");
}

/// ADR 0070: IT turns an app off for the org, and a group turns it back on
/// or off, off winning. Off, it isn't offered and doesn't open, and no
/// agent acting for the person reaches it: its tools aren't listed, and a
/// call is refused with why, on the record.
#[tokio::test]
async fn adr0070_an_app_it_turned_off_is_off_for_their_agents_too() {
    use glass_rpc::mcp::ListToolsResult;
    use serde_json::json;

    let h = harness();
    let mail_off = "[org]\napps_off = [\"mail\"]\n";
    h.apply_policy_replacing("[org]\n", mail_off, "");
    assert!(!ids(&h).iter().any(|(id, _)| id == "mail"));
    assert!(ids(&h).iter().any(|(id, _)| id == "calendar"));
    assert!(matches!(
        h.rt.open_app(&h.me, &"mail".parse().unwrap()),
        Err(RuntimeError::PermissionDenied(_))
    ));

    let task = h.rt.start(&h.me, h.request(&["Invoices"], &[])).unwrap().id;
    let agent = h.agent(task).await;
    let tools: ListToolsResult = agent.call("tools/list", json!({})).await.unwrap();
    assert!(
        !tools.tools.iter().any(|t| t.name.starts_with("mail_")),
        "{:?}",
        tools.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
    assert!(tools.tools.iter().any(|t| t.name == "files_read"));
    let refused = common::tool(&agent, "mail_send", common::send_args(1)).await;
    assert!(refused.is_error);
    assert!(
        refused
            .text()
            .contains("IT turned the mail app off for you"),
        "{}",
        refused.text()
    );
    let blocked = glass_audit::read(&h.audit_path(), |r| {
        r.event.task == Some(task)
            && r.event
                .action
                .as_ref()
                .is_some_and(|a| a.as_str() == "mail.send")
    })
    .unwrap();
    assert!(!blocked.is_empty(), "the refusal is on the record");
    assert!(
        blocked
            .iter()
            .all(|r| r.event.outcome != Some(glass_audit::Outcome::Completed)),
        "nothing was sent"
    );

    // A group turns it back on for its members...
    let finance = "[[group]]\nname = \"finance\"\nmembers = [\"{me}\"]\napps_on = [\"mail\"]\n";
    h.apply_policy_replacing("[org]\n", mail_off, finance);
    assert!(ids(&h).iter().any(|(id, _)| id == "mail"));
    assert!(h.rt.open_app(&h.me, &"mail".parse().unwrap()).is_ok());
    // ...but a group that turns it off wins, and the open Mail closes.
    let contractors = format!(
        "{finance}\n[[group]]\nname = \"contractors\"\nmembers = [\"{{me}}\"]\napps_off = [\"mail\"]\n"
    );
    h.apply_policy_replacing("[org]\n", mail_off, &contractors);
    assert!(!ids(&h).iter().any(|(id, _)| id == "mail"));
    assert!(h.rt.open_app(&h.me, &"mail".parse().unwrap()).is_err());
}
