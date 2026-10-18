//! Policy evaluation: from a validated document to the effective rules for
//! one (user, agent) pair, and from those to a tier for each action.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use glass_types::{ActionName, AgentId, Label, SideEffect, Tier};
use serde::{Deserialize, Serialize};

use crate::document::{AgentPolicy, GroupPolicy, PolicyDocument, SCHEMA_VERSION};
use crate::{ActionPattern, labels, pattern};

/// The group name that implicitly contains every user.
pub const EVERYONE: &str = "everyone";

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("policy is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("policy is invalid:\n  - {}", .0.join("\n  - "))]
    Invalid(Vec<String>),
}

/// A validated policy document, ready to evaluate.
#[derive(Debug, Clone)]
pub struct Policy {
    doc: PolicyDocument,
}

impl Policy {
    /// Parses and validates a policy document.
    pub fn from_toml(text: &str) -> Result<Self, PolicyError> {
        let doc: PolicyDocument = toml::from_str(text)?;
        Self::from_document(doc)
    }

    pub fn from_document(doc: PolicyDocument) -> Result<Self, PolicyError> {
        let mut errors = Vec::new();
        if doc.version != SCHEMA_VERSION {
            errors.push(format!(
                "unsupported policy version {}; this build reads version {SCHEMA_VERSION}",
                doc.version
            ));
        }
        if doc.org.approval_timeout_secs == 0 {
            errors.push("org.approval_timeout_secs must be greater than zero".into());
        }
        let mut seen_groups = std::collections::BTreeSet::new();
        for group in &doc.groups {
            if !seen_groups.insert(group.name.as_str()) {
                errors.push(format!("group {:?} is defined more than once", group.name));
            }
            for agent in &group.agents {
                if !doc.agents.iter().any(|a| &a.id == agent) {
                    errors.push(format!(
                        "group {:?} allows agent {agent:?}, which has no [[agent]] policy",
                        group.name
                    ));
                }
            }
        }
        let mut seen_agents = std::collections::BTreeSet::new();
        for agent in &doc.agents {
            if !seen_agents.insert(&agent.id) {
                errors.push(format!("agent {:?} is defined more than once", agent.id));
            }
            if agent.allowed_actions.is_empty() {
                errors.push(format!(
                    "agent {:?} has no allowed_actions; an agent that can do nothing is a mistake",
                    agent.id
                ));
            }
        }
        for rule in &doc.labels {
            if !rule.path.starts_with('/') {
                errors.push(format!("label rule path {:?} must be absolute", rule.path));
            }
        }
        if errors.is_empty() {
            Ok(Self { doc })
        } else {
            Err(PolicyError::Invalid(errors))
        }
    }

    pub fn document(&self) -> &PolicyDocument {
        &self.doc
    }

    /// The groups `user` belongs to, including `everyone` when defined.
    pub fn groups_of<'a>(&'a self, user: &'a str) -> impl Iterator<Item = &'a GroupPolicy> + 'a {
        self.doc
            .groups
            .iter()
            .filter(move |g| g.name == EVERYONE || g.members.iter().any(|m| m == user))
    }

    /// Agents `user` may run, in catalog order.
    pub fn agents_for(&self, user: &str) -> Vec<&AgentId> {
        self.doc
            .agents
            .iter()
            .map(|a| &a.id)
            .filter(|id| self.groups_of(user).any(|g| g.agents.contains(id)))
            .collect()
    }

    /// The effective rules for `agent` acting for `user`, or `None` when
    /// none of the user's groups allow that agent.
    pub fn effective(&self, user: &str, agent: &AgentId) -> Option<EffectivePolicy> {
        let agent_policy = self.doc.agents.iter().find(|a| &a.id == agent)?;
        let groups: Vec<&GroupPolicy> = self.groups_of(user).collect();
        if !groups.iter().any(|g| g.agents.contains(agent)) {
            return None;
        }
        Some(self.build(user, &groups, agent_policy))
    }

    fn build(&self, user: &str, groups: &[&GroupPolicy], agent: &AgentPolicy) -> EffectivePolicy {
        let max_label = groups
            .iter()
            .filter_map(|g| g.max_label)
            .chain(agent.max_label)
            .fold(self.doc.org.max_label, Label::min)
            // Restricted content is never opened by any agent.
            .min(Label::Confidential);
        EffectivePolicy {
            agent: agent.id.clone(),
            user: user.to_owned(),
            max_label,
            approval_timeout_secs: self.doc.org.approval_timeout_secs,
            egress: agent.egress.clone(),
            folder_labels: self.doc.labels.clone(),
            org: self.doc.org.tiers.clone(),
            groups: groups
                .iter()
                .map(|g| (g.name.clone(), g.tiers.clone()))
                .collect(),
            agent_rules: agent.clone(),
            restrictions: Vec::new(),
            names: self.doc.names.clone(),
        }
    }

    /// Warnings about rules that are legal but risky, given what the action
    /// registry declares. Shown in the admin console and at daemon start.
    ///
    /// Each agent is checked once per group that grants it, as a member of
    /// that group and `everyone` only.
    pub fn lint(&self, actions: &[(ActionName, SideEffect, Tier)]) -> Vec<String> {
        let everyone = self.doc.groups.iter().filter(|g| g.name == EVERYONE);
        let mut warnings = Vec::new();
        for agent in &self.doc.agents {
            for group in self
                .doc
                .groups
                .iter()
                .filter(|g| g.agents.contains(&agent.id))
            {
                let groups: Vec<&GroupPolicy> = everyone
                    .clone()
                    .filter(|g| g.name != group.name)
                    .chain([group])
                    .collect();
                let eff = self.build("", &groups, agent);
                for (name, effect, default) in actions {
                    let decision = eff.tier_for(name, *default);
                    if !effect.is_reversible() && decision.tier < Tier::Ask {
                        warnings.push(format!(
                            "agent {} in group {} runs irreversible action {name} at tier {} \
                             ({}); irreversible actions normally sit at ask",
                            agent.id, group.name, decision.tier, decision.source
                        ));
                    }
                }
            }
        }
        warnings
    }
}

/// Where a tier decision came from, recorded in the audit log so auditors can
/// see which rule allowed an action (AU-1, principle 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleSource {
    /// The action is not in the agent's allowed actions.
    NotAllowed,
    /// No rule matched; the app's declared default applies.
    ActionDefault,
    Org {
        pattern: String,
    },
    Group {
        group: String,
        pattern: String,
    },
    Agent {
        pattern: String,
    },
    /// A user or task-scope restriction made it stricter.
    Restriction {
        pattern: String,
    },
}

impl fmt::Display for RuleSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleSource::NotAllowed => f.write_str("not in the agent's allowed actions"),
            RuleSource::ActionDefault => f.write_str("app default"),
            RuleSource::Org { pattern } => write!(f, "org rule {pattern}"),
            RuleSource::Group { group, pattern } => write!(f, "group {group} rule {pattern}"),
            RuleSource::Agent { pattern } => write!(f, "agent rule {pattern}"),
            RuleSource::Restriction { pattern } => write!(f, "task restriction {pattern}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierDecision {
    pub tier: Tier,
    pub source: RuleSource,
}

/// Raised when a user or task scope tries to make a rule looser than policy
/// (DA-10: users can make tiers stricter, never looser).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{pattern} is already {current} by policy; a task can only make it stricter, not {requested}"
)]
pub struct LoosenRefused {
    pub pattern: String,
    pub current: Tier,
    pub requested: Tier,
}

/// The rules in force for one agent acting for one user, plus any task-level
/// restrictions. One of these is attached to every running task.
#[derive(Debug, Clone)]
pub struct EffectivePolicy {
    agent: AgentId,
    user: String,
    max_label: Label,
    approval_timeout_secs: u64,
    egress: Vec<String>,
    folder_labels: Vec<crate::document::FolderLabel>,
    org: BTreeMap<ActionPattern, Tier>,
    groups: Vec<(String, BTreeMap<ActionPattern, Tier>)>,
    agent_rules: AgentPolicy,
    restrictions: Vec<(ActionPattern, Tier)>,
    names: BTreeMap<ActionPattern, String>,
}

impl EffectivePolicy {
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn max_label(&self) -> Label {
        self.max_label
    }

    pub fn approval_timeout_secs(&self) -> u64 {
        self.approval_timeout_secs
    }

    pub fn egress(&self) -> &[String] {
        &self.egress
    }

    /// The tier `action` runs at, and which rule decided it.
    ///
    /// Precedence: the agent's allowed actions gate everything; then org
    /// baseline over the app default; then group and agent rules, which are
    /// both IT overrides of the baseline and combine by taking the stricter;
    /// finally task restrictions, which can only tighten.
    pub fn tier_for(&self, action: &ActionName, app_default: Tier) -> TierDecision {
        if !self
            .agent_rules
            .allowed_actions
            .iter()
            .any(|p| p.matches(action))
        {
            return TierDecision {
                tier: Tier::Never,
                source: RuleSource::NotAllowed,
            };
        }

        let mut decision = match pattern::most_specific(&self.org, action) {
            Some((p, t)) => TierDecision {
                tier: *t,
                source: RuleSource::Org {
                    pattern: p.to_string(),
                },
            },
            None => TierDecision {
                tier: app_default,
                source: RuleSource::ActionDefault,
            },
        };

        let overrides = self
            .groups
            .iter()
            .filter_map(|(group, rules)| {
                pattern::most_specific(rules, action).map(|(p, t)| TierDecision {
                    tier: *t,
                    source: RuleSource::Group {
                        group: group.clone(),
                        pattern: p.to_string(),
                    },
                })
            })
            .chain(
                pattern::most_specific(&self.agent_rules.tiers, action).map(|(p, t)| {
                    TierDecision {
                        tier: *t,
                        source: RuleSource::Agent {
                            pattern: p.to_string(),
                        },
                    }
                }),
            );
        if let Some(strictest) = overrides.max_by_key(|d| d.tier) {
            decision = strictest;
        }

        for (p, t) in &self.restrictions {
            if p.matches(action) && *t > decision.tier {
                decision = TierDecision {
                    tier: *t,
                    source: RuleSource::Restriction {
                        pattern: p.to_string(),
                    },
                };
            }
        }
        decision
    }

    /// Makes every action matching `pattern` at least `tier` for this task
    /// (DA-3, such as "block sends").
    ///
    /// Refuses when that would loosen any action the pattern covers, given
    /// the `known` actions and their app defaults. Restricting to a tier that
    /// is already met is a no-op that still records the user's intent.
    pub fn restrict(
        &mut self,
        pattern: ActionPattern,
        tier: Tier,
        known: &[(ActionName, Tier)],
    ) -> Result<(), LoosenRefused> {
        for (name, default) in known.iter().filter(|(n, _)| pattern.matches(n)) {
            let current = self.tier_for(name, *default).tier;
            if tier < current {
                return Err(LoosenRefused {
                    pattern: pattern.to_string(),
                    current,
                    requested: tier,
                });
            }
        }
        self.restrictions.push((pattern, tier));
        Ok(())
    }

    /// The label folder rules give `path`, if any.
    pub fn folder_label(&self, path: &Path) -> Option<Label> {
        labels::folder_label(&self.folder_labels, path)
    }

    /// Whether this agent may open content labeled `label`.
    pub fn may_open(&self, label: Label) -> bool {
        label.agent_may_open() && label <= self.max_label
    }

    /// Plain-language lines for the "Can't touch" panel (AW-14) and the
    /// handoff card's "Can't do · set by IT" (DA-2).
    ///
    /// Installed actions at Never are listed by `title`. Never rules that
    /// match no installed action are listed too, by their policy name, so the
    /// list shows IT's intent ("Payments") whatever apps are installed.
    /// Limits on data, network and scope come last.
    pub fn cant_touch(
        &self,
        known: &[(ActionName, Tier)],
        title: impl Fn(&ActionName) -> String,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut push = |line: String| {
            if !lines.contains(&line) {
                lines.push(line);
            }
        };
        for (name, default) in known {
            if self.tier_for(name, *default).tier == Tier::Never {
                push(title(name));
            }
        }
        for pattern in self.never_patterns() {
            if !known.iter().any(|(name, _)| pattern.matches(name)) {
                push(self.display_name(pattern));
            }
        }
        push(match self.max_label {
            Label::Public => "Anything labeled internal, confidential or restricted".into(),
            Label::Internal => "Anything labeled confidential or restricted".into(),
            _ => "Anything labeled restricted".into(),
        });
        if self.egress.is_empty() {
            push("The network (no destinations allowed)".into());
        }
        push("Files and folders outside this task's scope".into());
        lines
    }

    /// Every pattern some rule puts at Never for this task.
    fn never_patterns(&self) -> impl Iterator<Item = &ActionPattern> {
        let rules = self
            .org
            .iter()
            .chain(self.groups.iter().flat_map(|(_, rules)| rules.iter()))
            .chain(self.agent_rules.tiers.iter())
            .filter(|(_, tier)| **tier == Tier::Never)
            .map(|(pattern, _)| pattern);
        let restricted = self
            .restrictions
            .iter()
            .filter(|(_, tier)| *tier == Tier::Never)
            .map(|(pattern, _)| pattern);
        rules.chain(restricted)
    }

    /// The policy's name for a rule, or a readable form of the pattern.
    pub fn display_name(&self, pattern: &ActionPattern) -> String {
        if let Some(name) = self.names.get(pattern) {
            return name.clone();
        }
        let raw = pattern.as_str();
        if raw == "*" {
            return "Every other action".into();
        }
        let words = raw.trim_end_matches(".*").replace(['.', '_'], " ");
        let mut chars = words.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: &str = r#"
        version = 1

        [org]
        max_label = "confidential"
        approval_timeout_secs = 600
        [org.tiers]
        "files.read" = "auto"
        "files.*" = "notify"
        "mail.send" = "ask"
        "security.*" = "never"

        [[group]]
        name = "everyone"
        agents = ["probe"]

        [[group]]
        name = "ap-team"
        members = ["demo"]
        agents = ["invoice-agent"]
        max_label = "internal"
        [group.tiers]
        "files.delete" = "ask"

        [[group]]
        name = "auditors"
        members = ["demo"]
        [group.tiers]
        "files.delete" = "never"

        [[agent]]
        id = "invoice-agent"
        allowed_actions = ["files.*", "mail.*", "security.*"]
        [agent.tiers]
        "mail.draft" = "auto"
        "files.write" = "auto"

        [[agent]]
        id = "probe"
        allowed_actions = ["files.read"]

        [[label]]
        path = "/home/*/Finance"
        label = "confidential"
    "#;

    fn name(s: &str) -> ActionName {
        s.parse().unwrap()
    }

    fn invoice() -> EffectivePolicy {
        Policy::from_toml(POLICY)
            .unwrap()
            .effective("demo", &"invoice-agent".parse().unwrap())
            .unwrap()
    }

    #[test]
    fn group_membership_gates_agents() {
        let policy = Policy::from_toml(POLICY).unwrap();
        let ids = |u| {
            policy
                .agents_for(u)
                .into_iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("demo"), ["invoice-agent", "probe"]);
        assert_eq!(ids("stranger"), ["probe"]);
        assert!(
            policy
                .effective("stranger", &"invoice-agent".parse().unwrap())
                .is_none()
        );
    }

    #[test]
    fn org_baseline_then_overrides() {
        let eff = invoice();
        // Org exact rule.
        assert_eq!(
            eff.tier_for(&name("files.read"), Tier::Ask).tier,
            Tier::Auto
        );
        // Agent rule loosens the org subtree rule: IT said so explicitly.
        let write = eff.tier_for(&name("files.write"), Tier::Ask);
        assert_eq!(write.tier, Tier::Auto);
        assert_eq!(
            write.source,
            RuleSource::Agent {
                pattern: "files.write".into()
            }
        );
        // Org rule with no override.
        assert_eq!(
            eff.tier_for(&name("files.move"), Tier::Auto).tier,
            Tier::Notify
        );
        // No rule at all: the app default.
        let draft_default = eff.tier_for(&name("mail.archive"), Tier::Notify);
        assert_eq!(draft_default.source, RuleSource::ActionDefault);
    }

    #[test]
    fn conflicting_groups_take_the_stricter_rule() {
        let decision = invoice().tier_for(&name("files.delete"), Tier::Ask);
        assert_eq!(decision.tier, Tier::Never);
        assert_eq!(
            decision.source,
            RuleSource::Group {
                group: "auditors".into(),
                pattern: "files.delete".into()
            }
        );
    }

    #[test]
    fn actions_outside_the_allow_list_are_never() {
        let decision = invoice().tier_for(&name("payments.move"), Tier::Auto);
        assert_eq!(
            decision,
            TierDecision {
                tier: Tier::Never,
                source: RuleSource::NotAllowed
            }
        );
    }

    #[test]
    fn restrictions_only_tighten() {
        let mut eff = invoice();
        let known = [
            (name("mail.send"), Tier::Ask),
            (name("mail.draft"), Tier::Auto),
        ];
        eff.restrict("mail.send".parse().unwrap(), Tier::Never, &known)
            .unwrap();
        assert_eq!(
            eff.tier_for(&name("mail.send"), Tier::Ask).tier,
            Tier::Never
        );

        let err = eff
            .restrict("mail.*".parse().unwrap(), Tier::Auto, &known)
            .unwrap_err();
        assert_eq!(err.current, Tier::Never);
        assert_eq!(
            eff.tier_for(&name("mail.send"), Tier::Ask).tier,
            Tier::Never
        );
    }

    #[test]
    fn labels_take_the_strictest_limit_and_never_reach_restricted() {
        let eff = invoice();
        assert_eq!(eff.max_label(), Label::Internal);
        assert!(eff.may_open(Label::Internal));
        assert!(!eff.may_open(Label::Confidential));
        assert_eq!(
            eff.folder_label(Path::new("/home/demo/Finance/q3.csv")),
            Some(Label::Confidential)
        );
    }

    #[test]
    fn cant_touch_lists_blocked_actions_and_limits() {
        let known = [
            (name("security.settings"), Tier::Ask),
            (name("files.read"), Tier::Auto),
        ];
        let lines = invoice().cant_touch(&known, |n| format!("[{n}]"));
        assert_eq!(lines[0], "[security.settings]");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("confidential or restricted"))
        );
        assert!(lines.iter().any(|l| l.contains("network")));
    }

    #[test]
    fn cant_touch_shows_never_rules_without_installed_apps() {
        let text = format!("{POLICY}\n[names]\n\"security.*\" = \"Security settings\"\n");
        let policy = Policy::from_toml(&text).unwrap();
        let mut eff = policy
            .effective("demo", &"invoice-agent".parse().unwrap())
            .unwrap();
        let known = [
            (name("files.read"), Tier::Auto),
            (name("mail.send"), Tier::Ask),
        ];
        eff.restrict("payments.*".parse().unwrap(), Tier::Never, &known)
            .unwrap();
        let lines = eff.cant_touch(&known, |n| n.to_string());
        // Named by policy; humanized from a group rule and a task restriction.
        for expected in ["Security settings", "Files delete", "Payments"] {
            assert!(
                lines.contains(&expected.to_string()),
                "{expected} in {lines:?}"
            );
        }
        // A restriction on an installed action is listed by its title.
        eff.restrict("mail.send".parse().unwrap(), Tier::Never, &known)
            .unwrap();
        let titled = eff.cant_touch(&known, |n| format!("title:{n}"));
        assert!(titled.contains(&"title:mail.send".to_string()));
    }

    #[test]
    fn lint_flags_irreversible_actions_below_ask() {
        let policy = Policy::from_toml(POLICY).unwrap();
        let warnings = policy.lint(&[
            (name("mail.send"), SideEffect::Irreversible, Tier::Ask),
            (name("files.purge"), SideEffect::Irreversible, Tier::Ask),
        ]);
        // files.purge falls under the org files.* notify rule.
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("files.purge"));
    }

    #[test]
    fn validation_reports_every_problem() {
        let bad = r#"
            version = 2
            [[group]]
            name = "a"
            agents = ["ghost"]
            [[agent]]
            id = "lazy"
            allowed_actions = []
            [[label]]
            path = "relative"
            label = "internal"
        "#;
        let Err(PolicyError::Invalid(errors)) = Policy::from_toml(bad) else {
            panic!("expected validation errors");
        };
        assert_eq!(errors.len(), 4, "{errors:?}");
    }

    #[test]
    fn unquoted_dotted_keys_are_rejected() {
        let text = "version = 1\n[org.tiers]\nfiles.read = \"auto\"\n";
        assert!(matches!(
            Policy::from_toml(text),
            Err(PolicyError::Parse(_))
        ));
    }
}
