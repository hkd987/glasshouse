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
    }
}
