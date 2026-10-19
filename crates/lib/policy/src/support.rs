//! Checks the `[support]` section (CF-81, ADR 0028): what a device would
//! refuse, the console refuses to sign.

use std::collections::BTreeSet;

use crate::document::{PolicyDocument, SupportTool};

fn plain(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub(crate) fn validate(doc: &PolicyDocument, errors: &mut Vec<String>) {
    let support = &doc.support;
    if !(10..=600).contains(&support.answer_within_secs) {
        errors.push("support.answer_within_secs: 10 to 600 seconds".into());
    }
    let mut ids = BTreeSet::new();
    for role in &support.roles {
        let at = format!("support.role {:?}", role.id);
        if !plain(&role.id, 64) {
            errors.push(format!("{at}: ids are lowercase letters, digits, - and _"));
        }
        if !ids.insert(role.id.as_str()) {
            errors.push(format!("{at}: two roles have this id"));
        }
        if role.name.trim().is_empty()
            || role.name.len() > 80
            || role.name.chars().any(char::is_control)
        {
            errors.push(format!("{at}: the name must be one short line"));
        }
        if role.groups.is_empty() || role.groups.iter().any(|g| !plain(g, 64)) {
            errors.push(format!("{at}: name the directory groups that hold it"));
        }
        if role.groups.iter().any(|g| g == crate::EVERYONE) {
            errors.push(format!(
                "{at}: everyone can't hold a support role: name IT's groups"
            ));
        }
        let tools: BTreeSet<SupportTool> = role.tools.iter().copied().collect();
        if tools.is_empty() {
            errors.push(format!("{at}: give it at least one tool"));
        }
        if tools.len() != role.tools.len() {
            errors.push(format!("{at}: a tool is listed twice"));
        }
        if tools.contains(&SupportTool::Control) && !tools.contains(&SupportTool::View) {
            errors.push(format!(
                "{at}: control needs view: nobody controls a screen they can't see"
            ));
        }
        if role.people.is_empty() || role.people.iter().any(|g| !plain(g, 64)) {
            errors.push(format!(
                "{at}: name the groups whose computers it may help with"
            ));
        }
        if role.unattended.iter().any(|t| !plain(t, 32)) {
            errors.push(format!(
                "{at}: device types are lowercase letters, digits, - and _"
            ));
        }
        if !(1..=480).contains(&role.max_minutes) {
            errors.push(format!("{at}: max_minutes is 1 to 480"));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::document::SupportTool;
    use crate::{DEFAULT_POLICY, Policy, SUPPORT_TIERS};

    #[test]
    fn cf81_the_starter_tiers_are_help_desk_desktop_support_and_security() {
        let policy = Policy::from_toml(&format!("{DEFAULT_POLICY}\n{SUPPORT_TIERS}")).unwrap();
        let roles = &policy.support().roles;
        let ids: Vec<&str> = roles.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["help-desk", "desktop-support", "security"]);
        assert_eq!(roles[0].tools, [SupportTool::View, SupportTool::Control]);
        assert!(roles[1].tools.contains(&SupportTool::InstallApps));
        assert!(roles[2].tools.contains(&SupportTool::Isolate));
        assert!(
            roles.iter().all(|r| r.unattended.is_empty()),
            "nobody unattended by default"
        );
        assert!(
            Policy::from_toml(DEFAULT_POLICY)
                .unwrap()
                .support()
                .roles
                .is_empty()
        );
    }

    #[test]
    fn cf81_support_roles_are_checked() {
        let role = |extra: &str| {
            Policy::from_toml(&format!(
                "{DEFAULT_POLICY}\n[[support.role]]\nid = \"desk\"\nname = \"Desk\"\ngroups = [\"it\"]\n{extra}\n"
            ))
            .map_err(|e| e.to_string())
        };
        assert!(role("tools = [\"view\"]").is_ok());
        for (bad, says) in [
            ("tools = []", "at least one tool"),
            ("tools = [\"control\"]", "control needs view"),
            ("tools = [\"view\", \"view\"]", "listed twice"),
            ("tools = [\"view\"]\nmax_minutes = 0", "max_minutes"),
            (
                "tools = [\"view\"]\nunattended = [\"Kiosk Room\"]",
                "device types",
            ),
            ("tools = [\"teleport\"]", "teleport"),
        ] {
            let refused = role(bad).unwrap_err();
            assert!(refused.contains(says), "{bad}: {refused}");
        }
        let everyone = Policy::from_toml(&format!(
            "{DEFAULT_POLICY}\n[[support.role]]\nid = \"all\"\nname = \"All\"\ngroups = [\"everyone\"]\ntools = [\"view\"]\n"
        ))
        .unwrap_err()
        .to_string();
        assert!(everyone.contains("everyone can't"), "{everyone}");
        let twice = Policy::from_toml(&format!(
            "{DEFAULT_POLICY}\n{SUPPORT_TIERS}\n[[support.role]]\nid = \"security\"\nname = \"Again\"\ngroups = [\"x\"]\ntools = [\"view\"]\n"
        ))
        .unwrap_err()
        .to_string();
        assert!(twice.contains("two roles"), "{twice}");
    }
}
