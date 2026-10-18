//! IT-defined agent policy (PRD "Delegation and approvals", AD-2).
//!
//! - [`document`] is the on-disk schema (`policy.toml`).
//! - [`Policy`] validates a document and resolves the [`EffectivePolicy`] for
//!   one agent acting for one user.
//! - [`EffectivePolicy`] answers the questions the broker asks on every
//!   action: which tier, why, which labels, and what the agent can't touch.

pub mod document;
mod engine;
mod labels;
mod pattern;

pub use engine::{
    EVERYONE, EffectivePolicy, LoosenRefused, Policy, PolicyError, RuleSource, TierDecision,
};
pub use pattern::{ActionPattern, InvalidPattern, Specificity};

/// The policy a new org ships with. Also what `glassd` enforces when no
/// policy file is installed.
pub const DEFAULT_POLICY: &str = include_str!("../default-policy.toml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_valid_and_safe() {
        let policy = Policy::from_toml(DEFAULT_POLICY).expect("default policy must validate");
        let eff = policy
            .effective("anyone", &"invoice-demo".parse().unwrap())
            .unwrap();
        let tier = |n: &str| {
            eff.tier_for(&n.parse().unwrap(), glass_types::Tier::Auto)
                .tier
        };
        use glass_types::Tier::*;
        assert_eq!(tier("files.read"), Auto);
        assert_eq!(tier("files.write"), Notify);
        assert_eq!(tier("files.delete"), Ask);
        assert_eq!(tier("mail.send"), Ask);
        assert_eq!(tier("security.firewall"), Never);
        assert!(eff.egress().is_empty());
        assert_eq!(eff.max_label(), glass_types::Label::Internal);
        assert_eq!(
            tier("system.service.restart"),
            Never,
            "system actions are the IT agent's alone"
        );
    }

    #[test]
    fn the_it_agent_checks_quietly_and_asks_before_changing_the_machine() {
        // AE-4
        let policy = Policy::from_toml(DEFAULT_POLICY).unwrap();
        assert!(policy.fixes().services.iter().any(|s| s == "openvpn"));
        let eff = policy
            .effective("anyone", &"it-agent".parse().unwrap())
            .unwrap();
        let tier = |n: &str| {
            eff.tier_for(&n.parse().unwrap(), glass_types::Tier::Auto)
                .tier
        };
        use glass_types::Tier::*;
        assert_eq!(tier("system.network.check"), Auto);
        assert_eq!(tier("system.storage.clean_cache"), Notify);
        assert_eq!(tier("system.service.restart"), Ask);
        assert_eq!(tier("helpdesk.ticket.create"), Ask);
        assert_eq!(tier("security.firewall"), Never, "never security settings");
        assert_eq!(tier("files.read"), Never, "no user files");
    }

    #[test]
    fn fix_lists_name_services_not_commands() {
        let bad =
            DEFAULT_POLICY.replace(r#"services = ["cron","#, r#"services = ["cron; reboot","#);
        assert!(Policy::from_toml(&bad).is_err());
    }
}
