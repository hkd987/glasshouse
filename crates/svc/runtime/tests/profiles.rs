//! AD-7, role-based delivery: which apps a person gets is policy (per group),
//! enforced by glassd. An app outside someone's profile isn't offered to
//! them and doesn't open for them, and the refusal is audited.

#![allow(clippy::unwrap_used)]

mod common;

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
            ("calendar".into(), ProfileAppKind::Web),
            ("mail".into(), ProfileAppKind::Web)
        ]
    );

    // Given IT's role, the tools for managing a computer join the profile.
    // The admin console needs a managed device to open, so it isn't here.
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
    assert_eq!(names, ["Activity Monitor", "Calendar", "Mail", "Terminal"]);
}

#[tokio::test]
async fn ad7_an_app_outside_the_profile_doesnt_open_and_the_refusal_is_audited() {
    let h = harness();
    h.apply_policy_replacing("apps = [\"mail\", \"calendar\"]", "apps = [\"mail\"]", "");
    assert_eq!(ids(&h), [("mail".into(), ProfileAppKind::Web)]);
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
