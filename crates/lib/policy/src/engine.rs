//! Policy evaluation: from a validated document to the effective rules for
//! one (user, agent) pair, and from those to a tier for each action.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use glass_types::{ActionName, AgentId, Label, SideEffect, Tier};
use serde::{Deserialize, Serialize};

use crate::document::{AgentPolicy, Fixes, GroupPolicy, PolicyDocument, SCHEMA_VERSION};
use crate::models::{self, ModelRules};
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
            for app in &group.apps {
                if doc.agents.iter().any(|a| &a.id == app) {
                    errors.push(format!(
                        "group {:?} lists {app:?} as an app, but it is an agent; apps and agents \
                         share one namespace, so list it under agents",
                        group.name
                    ));
                }
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
        let mut seen_apps = std::collections::BTreeSet::new();
        for app in &doc.apps {
            if !seen_apps.insert(&app.id) {
                errors.push(format!("app {:?} has more than one [[app]]", app.id));
            }
            if doc.agents.iter().any(|a| a.id == app.id) {
                errors.push(format!(
                    "[[app]] {:?} is an agent; apps and agents share one namespace",
                    app.id
                ));
            }
        }
        // Ids that start my- are the apps people make for themselves (ADR
        // 0032): policy names none, so none takes on another's rules.
        let personal = doc
            .apps
            .iter()
            .map(|a| &a.id)
            .chain(doc.agents.iter().map(|a| &a.id))
            .chain(
                doc.groups
                    .iter()
                    .flat_map(|g| g.apps.iter().chain(&g.agents)),
            )
            .filter(|id| id.is_personal());
        for id in personal {
            errors.push(format!(
                "{id:?}: ids that start {:?} are people's own apps, which policy doesn't name",
                glass_types::PERSONAL_PREFIX
            ));
        }
        // Meetings are for people (CF-21, ADR 0030): no rule that reaches
        // agents may name them. The org's baseline and an app's own
        // [[app]] tiers may.
        for group in &doc.groups {
            for pattern in group.tiers.keys().filter(|p| names_people_only(p)) {
                errors.push(format!(
                    "group {:?} has a rule for {pattern}: {PEOPLE_ONLY_WHY}",
                    group.name
                ));
            }
        }
        for agent in &doc.agents {
            let named = agent
                .allowed_actions
                .iter()
                .chain(agent.tiers.keys())
                .filter(|p| names_people_only(p));
            for pattern in named {
                errors.push(format!(
                    "agent {} names {pattern}: {PEOPLE_ONLY_WHY}",
                    agent.id
                ));
            }
        }
        // So is connecting mail (ADR 0037): a rule naming it could only
        // mislead, since no agent ever does it. Patterns such as `mail.*`
        // name it with the rest and stay valid.
        for group in &doc.groups {
            for pattern in group.tiers.keys().filter(|p| is_persons_own(p.as_str())) {
                errors.push(format!(
                    "group {:?} has a rule for {pattern}: {PERSONS_OWN_WHY}",
                    group.name
                ));
            }
        }
        for agent in &doc.agents {
            let named = agent
                .allowed_actions
                .iter()
                .chain(agent.tiers.keys())
                .filter(|p| is_persons_own(p.as_str()));
            for pattern in named {
                errors.push(format!(
                    "agent {} names {pattern}: {PERSONS_OWN_WHY}",
                    agent.id
                ));
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
        models::validate(&doc, &mut errors);
        crate::mail::validate(&doc, &mut errors);
        crate::support::validate(&doc, &mut errors);
        crate::settings::validate(&doc, &mut errors);
        for service in &doc.fixes.services {
            let plain = !service.is_empty()
                && service
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if !plain {
                errors.push(format!("fixes.services: {service:?} is not a service name"));
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

    /// IT's approved fix list for the IT self-service agent (AE-4).
    pub fn fixes(&self) -> &Fixes {
        &self.doc.fixes
    }

    /// `user`'s device settings (UX-1): each from their first group that
    /// sets it, then everyone, then the computer's default.
    pub fn settings_for(&self, user: &str) -> Vec<crate::settings::Resolved> {
        crate::settings::resolve(&self.doc, user)
    }

    /// The computer's own settings, such as the browser's (UX-1), with
    /// `seat` signed in at the screen or nobody (ADR 0031).
    pub fn computer_settings(&self, seat: Option<&str>) -> Vec<crate::settings::Resolved> {
        crate::settings::computer(&self.doc, seat)
    }

    /// Whether people may remix apps on this org's computers (TL-6, ADR
    /// 0032): everyone may unless IT turned `apps.remix` off.
    pub fn remixing(&self) -> bool {
        !self
            .computer_settings(None)
            .iter()
            .any(|r| r.id == "apps.remix" && r.value == crate::document::SettingValue::Bool(false))
    }

    /// Remote help's roles (CF-81, ADR 0028).
    pub fn support(&self) -> &crate::document::Support {
        &self.doc.support
    }

    /// The org's mail service, if IT set one (NV-3).
    pub fn mail(&self) -> Option<&crate::document::MailSettings> {
        self.doc.mail.as_ref()
    }

    /// The groups `user` belongs to, including `everyone` when defined.
    pub fn groups_of<'a>(&'a self, user: &'a str) -> impl Iterator<Item = &'a GroupPolicy> + 'a {
        self.doc
            .groups
            .iter()
            .filter(move |g| g.name == EVERYONE || g.members.iter().any(|m| m == user))
    }

    /// The apps in `user`'s profile: every one their groups list
    /// (role-based delivery). glassd opens no other app for them, and the
    /// launcher offers no other.
    pub fn apps_for<'a>(&'a self, user: &'a str) -> std::collections::BTreeSet<&'a AgentId> {
        self.groups_of(user).flat_map(|g| g.apps.iter()).collect()
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

    /// The effective rules for an app acting for `user` (UI-5, ADR 0024):
    /// declaration ∩ policy (DV-3, ADR 0008). The app may call only the
    /// actions it `declares`, each at the tier org and group rules give
    /// it, with the app's own `[[app]]` tiers as its rules (ADR 0030), so
    /// a group's stricter tier still wins; it opens nothing above
    /// `max_label` or the user's groups' limits; it has no network and no
    /// models.
    pub fn for_app(
        &self,
        user: &str,
        app: &AgentId,
        declares: &[ActionName],
        max_label: Label,
    ) -> EffectivePolicy {
        let groups: Vec<&GroupPolicy> = self.groups_of(user).collect();
        let declared = AgentPolicy {
            id: app.clone(),
            allowed_actions: declares
                .iter()
                .filter_map(|name| name.as_str().parse().ok())
                .collect(),
            // An app a person made never has the rules policy gives the
            // app it remixes, or any other (ADR 0032): it asks where they
            // would be asked.
            tiers: self
                .doc
                .apps
                .iter()
                .find(|a| &a.id == app && !app.is_personal())
                .map(|a| a.tiers.clone())
                .unwrap_or_default(),
            max_label: Some(max_label),
            egress: Vec::new(),
            models: Some(Vec::new()),
        };
        let mut effective = self.build(user, &groups, &declared);
        effective.app = true;
        effective
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
            models: ModelRules::new(&self.doc.models, groups, agent.models.as_deref()),
            app: false,
        }
    }

    /// Warnings about rules that are legal but risky, given what the action
    /// registry declares. Shown in the admin console and at daemon start.
    ///
    /// Each agent is checked once per group that grants it, as a member of
    /// that group and `everyone` only.
    ///
    /// `[[app]]` tiers aren't linted: the default policy's own Chat rule
    /// puts posting below Ask on purpose, held by presence (ADR 0030).
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
                    // Disclosures (model calls) are governed by labels and
                    // routing, not by asking each time (ADR 0018).
                    if *effect == SideEffect::Irreversible && decision.tier < Tier::Ask {
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

/// Apps whose actions only people take, in their own apps (CF-21): an
/// agent's call is Never, whatever its rules say.
pub const PEOPLE_ONLY: &[&str] = &["meet"];

const PEOPLE_ONLY_WHY: &str = "meetings are for people, never agents (ADR 0030); \
     the Meet and Calendar apps' own [[app]] tiers say what the person does there";

/// What agents may still take in a people-only app: reading a transcribed
/// meeting's transcript, never joining it (ADR 0045). Policy rules name it
/// like any action.
pub const AGENTS_TOO: &[&str] = &["meet.transcript"];

fn in_people_only_app(name: &str) -> bool {
    !AGENTS_TOO.contains(&name)
        && PEOPLE_ONLY.iter().any(|app| {
            name.strip_prefix(app)
                .is_some_and(|rest| rest.starts_with('.'))
        })
}

/// Whether `action` is one only people take ([`PEOPLE_ONLY`]).
pub fn people_only(action: &ActionName) -> bool {
    in_people_only_app(action.as_str())
}

/// Actions only the person takes, in their own app, because they carry a
/// secret only the person has (ADR 0037): an agent's call is Never,
/// whatever its rules say.
pub const PERSONS_OWN: &[&str] = &[
    "mail.connect",
    "settings.models_connect",
    // A Wi-Fi network's password (CF-12, ADR 0059).
    "settings.wifi_join",
    // A paired keyboard types into everything (CF-13, ADR 0062).
    "settings.bluetooth_pair",
    // An OS update and a kernel patch change what the whole computer runs
    // (ADR 0057, ADR 0061): installing one, or taking a patch out, is the
    // person's at the screen; IT's on a managed computer.
    "settings.update_install",
    "settings.update_restart",
    "settings.patch_remove",
    "catalog.patch_install",
    // Starting a routine runs a task with the scope it was saved with,
    // which the agent asking may not have; and what the person did twice
    // is theirs to keep (ADR 0047).
    "routines.run",
    "routines.repeats",
    "routines.keep",
    "routines.dismiss",
];

const PERSONS_OWN_WHY: &str = "only the person does it, in their own app: connecting their mail \
     or their AI models with their password or key, in Mail or Settings (ADR 0037, ADR 0039), and \
     starting their routines or keeping new ones, in Routines (ADR 0047), and installing this \
     computer's OS updates or kernel patches, in Settings and the Store (ADR 0061); no agent \
     ever does, and those apps' own [[app]] tiers say what the person does there";

/// What an agent does only with the person's yes, each time: at least Ask
/// whatever the rules say, and never under an approval for the rest of the
/// task. Kernel code runs outside every sandbox (ADR 0057, PA-3).
pub const EVERY_TIME: &[&str] = &["hardware.patch_propose"];

/// Whether `action` asks the person every time ([`EVERY_TIME`]).
pub fn every_time(action: &ActionName) -> bool {
    EVERY_TIME.contains(&action.as_str())
}

fn is_persons_own(name: &str) -> bool {
    PERSONS_OWN.contains(&name)
}

/// Whether `action` is the person's own ([`PERSONS_OWN`]): Never for every
/// agent, and for apps people make, which may carry it from the app they
/// remix but never run it (only the shipped app's [[app]] tiers do).
pub fn persons_own(action: &ActionName) -> bool {
    is_persons_own(action.as_str())
}

/// Whether `pattern` names a people-only app's actions: `meet.*` or one of
/// them. `*` names no app.
fn names_people_only(pattern: &ActionPattern) -> bool {
    in_people_only_app(pattern.as_str())
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
    /// Only people do this, in their own apps; never an agent (CF-21).
    PeopleOnly,
    /// The person's own, with a secret only they have (ADR 0037): never an
    /// agent.
    PersonsOwn,
    /// The person says yes every time (ADR 0057): rules can't lower it
    /// below Ask.
    EveryTime,
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
            RuleSource::PeopleOnly => f.write_str("meetings are for people, never agents"),
            RuleSource::PersonsOwn => {
                f.write_str("only the person does this, in their own app, never an agent")
            }
            RuleSource::EveryTime => f.write_str("the person says yes every time"),
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
    models: ModelRules,
    /// An app's session (the person in that app's window), not an agent.
    app: bool,
}

impl EffectivePolicy {
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    /// The groups this policy was built from: the user's, `everyone`
    /// included when policy defines it.
    pub fn groups(&self) -> impl Iterator<Item = &str> {
        self.groups.iter().map(|(name, _)| name.as_str())
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

    /// The approved models and where this task's data may go (DP-2).
    pub fn models(&self) -> &ModelRules {
        &self.models
    }

    /// Keeps only the models in `keep` for this task; never adds one
    /// (ADR 0014).
    pub fn narrow_models(&mut self, keep: &[String]) {
        self.models.narrow(keep);
    }

    /// The tier `action` runs at, and which rule decided it.
    ///
    /// Precedence: the agent's allowed actions gate everything; then org
    /// baseline over the app default; then group and agent rules, which are
    /// both IT overrides of the baseline and combine by taking the stricter;
    /// finally task restrictions, which can only tighten.
    pub fn tier_for(&self, action: &ActionName, app_default: Tier) -> TierDecision {
        // Whatever a pattern such as `*` allows, an agent never joins a
        // meeting (CF-21, ADR 0030).
        if !self.app && people_only(action) {
            return TierDecision {
                tier: Tier::Never,
                source: RuleSource::PeopleOnly,
            };
        }
        // Nor connects the person's mail with their password (ADR 0037).
        if !self.app && is_persons_own(action.as_str()) {
            return TierDecision {
                tier: Tier::Never,
                source: RuleSource::PersonsOwn,
            };
        }
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
        if !self.app && every_time(action) && decision.tier < Tier::Ask {
            decision = TierDecision {
                tier: Tier::Ask,
                source: RuleSource::EveryTime,
            };
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
        self.narrow(pattern, tier);
        Ok(())
    }

    /// Makes every action matching `pattern` at least `tier`, without
    /// [`Self::restrict`]'s check. Restrictions only ever raise a tier, so
    /// where policy is already stricter this does nothing. For limits saved
    /// earlier, such as a tailoring layer's (TL-10), whose policy may since
    /// have tightened.
    pub fn narrow(&mut self, pattern: ActionPattern, tier: Tier) {
        self.restrictions.push((pattern, tier));
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
    /// Actions an agent simply isn't given (outside its `allowed_actions`,
    /// and under no Never rule) are one counted line: an agent with narrow
    /// rules would otherwise list dozens, burying IT's. Limits on data,
    /// network and scope come last.
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
        let mut not_given = 0;
        for (name, default) in known {
            let decision = self.tier_for(name, *default);
            if decision.tier != Tier::Never {
                continue;
            }
            if decision.source == RuleSource::NotAllowed
                && !self.never_patterns().any(|p| p.matches(name))
            {
                not_given += 1;
            } else {
                push(title(name));
            }
        }
        for pattern in self.never_patterns() {
            if !known.iter().any(|(name, _)| pattern.matches(name)) {
                push(self.display_name(pattern));
            }
        }
        match not_given {
            0 => {}
            1 => push("1 other action it isn't given".into()),
            n => push(format!("{n} other actions it isn't given")),
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
    fn dv3_apps_get_what_they_declare_at_policys_tiers() {
        let policy = Policy::from_toml(POLICY).unwrap();
        let app: AgentId = "mail".parse().unwrap();
        let declared = [name("mail.send"), name("files.delete")];
        let eff = policy.for_app("demo", &app, &declared, Label::Confidential);
        // Org baseline, then the user's groups, stricter wins.
        assert_eq!(eff.tier_for(&name("mail.send"), Tier::Auto).tier, Tier::Ask);
        assert_eq!(
            eff.tier_for(&name("files.delete"), Tier::Auto).tier,
            Tier::Never
        );
        // Anything undeclared is out, whatever its default.
        let undeclared = eff.tier_for(&name("mail.draft"), Tier::Auto);
        assert_eq!(undeclared.tier, Tier::Never);
        assert_eq!(undeclared.source, RuleSource::NotAllowed);
        // The groups' label limit still applies to an app that declares more.
        assert_eq!(eff.max_label(), Label::Internal);
        assert_eq!(eff.models().allowed().count(), 0, "apps call no models");
        let stranger = policy.for_app("stranger", &app, &declared, Label::Public);
        assert_eq!(stranger.max_label(), Label::Public);
    }

    #[test]
    fn cf91_an_apps_own_tiers_apply_to_its_sessions_and_a_stricter_group_still_wins() {
        let text = format!(
            "{}\n[[group]]\nname = \"legal\"\nmembers = [\"priya\"]\n[group.tiers]\n\"chat.post\" = \"ask\"\n",
            crate::DEFAULT_POLICY
        );
        let policy = Policy::from_toml(&text).unwrap();
        let chat: AgentId = "chat".parse().unwrap();
        let declared = [name("chat.post"), name("chat.read")];
        let tier = |user: &str, app: &AgentId| {
            policy
                .for_app(user, app, &declared, Label::Internal)
                .tier_for(&name("chat.post"), Tier::Ask)
                .tier
        };
        // What the person types in Chat doesn't ask; in another app that
        // declares chat.post, the org's Ask holds.
        assert_eq!(tier("dana", &chat), Tier::Auto);
        assert_eq!(tier("dana", &"mail".parse().unwrap()), Tier::Ask);
        // A group that asks for every message wins over the app's rule.
        assert_eq!(tier("priya", &chat), Tier::Ask);

        let twice = format!("{}\n[[app]]\nid = \"chat\"\n", crate::DEFAULT_POLICY);
        assert!(Policy::from_toml(&twice).is_err());
        let agent = format!("{}\n[[app]]\nid = \"claude-code\"\n", crate::DEFAULT_POLICY);
        assert!(Policy::from_toml(&agent).is_err());
    }

    /// ADR 0032: a person's own app never gets an app's rules, and policy
    /// can't name one to give it some.
    #[test]
    fn tl5_a_persons_app_has_no_apps_rules_and_policy_names_none() {
        let policy = Policy::from_toml(crate::DEFAULT_POLICY).unwrap();
        let declares = [name("chat.post")];
        let tier = |app: &str| {
            policy
                .for_app("demo", &app.parse().unwrap(), &declares, Label::Internal)
                .tier_for(&name("chat.post"), Tier::Ask)
                .tier
        };
        assert_eq!(tier("chat"), Tier::Auto);
        assert_eq!(tier("my-chat"), Tier::Ask);
        for extra in [
            "[[app]]\nid = \"my-chat\"\ntiers = { \"chat.post\" = \"auto\" }\n",
            "[[group]]\nname = \"sales\"\nmembers = [\"dana\"]\napps = [\"my-chat\"]\n",
        ] {
            let Err(PolicyError::Invalid(errors)) = with_default(extra) else {
                panic!("{extra} must not validate");
            };
            assert!(errors[0].contains("people's own apps"), "{errors:?}");
        }
    }

    fn with_default(extra: &str) -> Result<Policy, PolicyError> {
        Policy::from_toml(&format!("{}\n{extra}", crate::DEFAULT_POLICY))
    }

    #[test]
    fn cf21_a_group_or_agent_rule_naming_meetings_fails_validation() {
        let group = "[[group]]\nname = \"sales\"\nmembers = [\"dana\"]\n\
                     [group.tiers]\n\"meet.join\" = \"auto\"\n";
        let Err(PolicyError::Invalid(errors)) = with_default(group) else {
            panic!("a group rule naming meet.join must not validate");
        };
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("group \"sales\" has a rule for meet.join")
                && errors[0].contains("never agents"),
            "{errors:?}"
        );
        for agent in [
            "allowed_actions = [\"files.read\", \"meet.*\"]",
            "allowed_actions = [\"files.read\"]\n[agent.tiers]\n\"meet.keys\" = \"never\"",
        ] {
            let text = format!("[[agent]]\nid = \"note-taker\"\n{agent}\n");
            let err = with_default(&text).unwrap_err().to_string();
            assert!(err.contains("agent note-taker names meet."), "{err}");
        }
        // `*` names no app, and "meeting.*" isn't meetings.
        with_default("[[agent]]\nid = \"anything\"\nallowed_actions = [\"*\", \"meeting.*\"]\n")
            .unwrap();
    }

    #[test]
    fn adr0057_a_kernel_patch_is_proposed_with_the_persons_yes_every_time() {
        let everything = "[[agent]]\nid = \"anything\"\nallowed_actions = [\"*\"]\n\
                          [agent.tiers]\n\"hardware.*\" = \"auto\"\n\
                          [[group]]\nname = \"all\"\nmembers = [\"dana\"]\nagents = [\"anything\"]\n\
                          [group.tiers]\n\"*\" = \"auto\"\n";
        let policy = with_default(everything).unwrap();
        let agent = policy
            .effective("dana", &"anything".parse().unwrap())
            .unwrap();
        let propose = agent.tier_for(&name("hardware.patch_propose"), Tier::Auto);
        assert_eq!(
            (propose.tier, propose.source),
            (Tier::Ask, RuleSource::EveryTime)
        );
        // Building one, or reading the report, stays as the rules say.
        assert_eq!(
            agent
                .tier_for(&name("hardware.patch_build"), Tier::Auto)
                .tier,
            Tier::Auto
        );
        // Never stays never.
        let never = with_default(
            "[[agent]]\nid = \"anything\"\nallowed_actions = [\"*\"]\n\
             [agent.tiers]\n\"hardware.patch_propose\" = \"never\"\n\
             [[group]]\nname = \"all\"\nmembers = [\"dana\"]\nagents = [\"anything\"]\n",
        )
        .unwrap();
        let agent = never
            .effective("dana", &"anything".parse().unwrap())
            .unwrap();
        assert_eq!(
            agent
                .tier_for(&name("hardware.patch_propose"), Tier::Ask)
                .tier,
            Tier::Never
        );
    }

    #[test]
    fn adr0039_no_agent_connects_ai_models_whatever_its_rules_and_settings_does() {
        let everything = "[[agent]]\nid = \"anything\"\nallowed_actions = [\"*\", \"settings.*\"]\n\
                          [[group]]\nname = \"all\"\nmembers = [\"dana\"]\nagents = [\"anything\"]\n\
                          [group.tiers]\n\"*\" = \"auto\"\n";
        let policy = with_default(everything).unwrap();
        let agent = policy
            .effective("dana", &"anything".parse().unwrap())
            .unwrap();
        let connect = agent.tier_for(&name("settings.models_connect"), Tier::Auto);
        assert_eq!(
            (connect.tier, connect.source),
            (Tier::Never, RuleSource::PersonsOwn)
        );
        let err = with_default(
            "[[group]]\nname = \"sales\"\nmembers = [\"dana\"]\n[group.tiers]\n\"settings.models_connect\" = \"auto\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("settings.models_connect"), "{err}");
        let policy = Policy::from_toml(crate::DEFAULT_POLICY).unwrap();
        let settings = policy.for_app(
            "dana",
            &"settings".parse().unwrap(),
            &[name("settings.models_connect"), name("settings.models")],
            Label::Internal,
        );
        assert_eq!(
            settings
                .tier_for(&name("settings.models_connect"), Tier::Never)
                .tier,
            Tier::Auto
        );
    }

    #[test]
    fn adr0047_no_agent_starts_or_keeps_routines_and_the_routines_app_does() {
        // The desktop agent's "routines.*" still leaves starting a routine
        // and keeping what the person did twice at Never; pausing and
        // rescheduling stay its, at their tiers.
        let policy = Policy::from_toml(crate::DEFAULT_POLICY).unwrap();
        let agent = policy
            .effective("dana", &"desktop-agent".parse().unwrap())
            .unwrap();
        for action in [
            "routines.run",
            "routines.repeats",
            "routines.keep",
            "routines.dismiss",
        ] {
            let decision = agent.tier_for(&name(action), Tier::Auto);
            assert_eq!(
                (decision.tier, decision.source),
                (Tier::Never, RuleSource::PersonsOwn),
                "{action}"
            );
        }
        assert_eq!(
            agent.tier_for(&name("routines.pause"), Tier::Notify).tier,
            Tier::Notify
        );
        assert_eq!(
            agent.tier_for(&name("routines.schedule"), Tier::Ask).tier,
            Tier::Ask
        );
        let declared = [
            "routines.list",
            "routines.run",
            "routines.keep",
            "routines.repeats",
            "routines.pause",
        ]
        .map(name);
        let app = policy.for_app(
            "dana",
            &"routines".parse().unwrap(),
            &declared,
            Label::Internal,
        );
        assert_eq!(
            app.tier_for(&name("routines.run"), Tier::Notify).tier,
            Tier::Auto
        );
        assert_eq!(
            app.tier_for(&name("routines.keep"), Tier::Notify).tier,
            Tier::Auto
        );
        assert_eq!(
            app.tier_for(&name("routines.repeats"), Tier::Auto).tier,
            Tier::Auto
        );
    }

    #[test]
    fn em0_no_agent_connects_mail_whatever_its_rules_and_the_mail_app_does() {
        // ADR 0037: the password is the person's. A group's "*" = auto and
        // an agent's "mail.*" still leave mail.connect at Never.
        let everything = "[[agent]]\nid = \"anything\"\nallowed_actions = [\"*\", \"mail.*\"]\n\
                          [[group]]\nname = \"all\"\nmembers = [\"dana\"]\nagents = [\"anything\"]\n\
                          [group.tiers]\n\"*\" = \"auto\"\n\"mail.*\" = \"auto\"\n";
        let policy = with_default(everything).unwrap();
        let agent = policy
            .effective("dana", &"anything".parse().unwrap())
            .unwrap();
        let connect = agent.tier_for(&name("mail.connect"), Tier::Auto);
        assert_eq!(connect.tier, Tier::Never);
        assert_eq!(connect.source, RuleSource::PersonsOwn);
        assert_eq!(
            agent.tier_for(&name("mail.move"), Tier::Ask).tier,
            Tier::Auto
        );
        // A rule naming it doesn't validate.
        for extra in [
            "[[group]]\nname = \"sales\"\nmembers = [\"dana\"]\n[group.tiers]\n\"mail.connect\" = \"auto\"\n",
            "[[agent]]\nid = \"x\"\nallowed_actions = [\"mail.connect\"]\n",
        ] {
            let err = with_default(extra).unwrap_err().to_string();
            assert!(
                err.contains("mail.connect") && err.contains("no agent ever does"),
                "{err}"
            );
        }
        // In Mail the person connects without being asked; agents in the
        // default policy never do.
        let policy = Policy::from_toml(crate::DEFAULT_POLICY).unwrap();
        let mail = policy.for_app(
            "dana",
            &"mail".parse().unwrap(),
            &[name("mail.connect"), name("mail.move"), name("mail.send")],
            Label::Internal,
        );
        assert_eq!(
            mail.tier_for(&name("mail.connect"), Tier::Never).tier,
            Tier::Auto
        );
        assert_eq!(
            mail.tier_for(&name("mail.move"), Tier::Notify).tier,
            Tier::Auto
        );
        assert_eq!(mail.tier_for(&name("mail.send"), Tier::Ask).tier, Tier::Ask);
        let claude = policy
            .effective("dana", &"claude-code".parse().unwrap())
            .unwrap();
        assert_eq!(
            claude.tier_for(&name("mail.connect"), Tier::Never).tier,
            Tier::Never
        );
        assert_eq!(
            claude.tier_for(&name("mail.move"), Tier::Notify).tier,
            Tier::Notify
        );
    }

    #[test]
    fn cf21_agents_never_join_meetings_whatever_their_rules_allow() {
        let everything = "[[agent]]\nid = \"anything\"\nallowed_actions = [\"*\"]\n\
                          [[group]]\nname = \"all\"\nmembers = [\"dana\"]\nagents = [\"anything\"]\n\
                          [group.tiers]\n\"*\" = \"auto\"\n";
        let policy = with_default(everything).unwrap();
        let agent = policy
            .effective("dana", &"anything".parse().unwrap())
            .unwrap();
        let join = agent.tier_for(&name("meet.join"), Tier::Auto);
        assert_eq!(
            join,
            TierDecision {
                tier: Tier::Never,
                source: RuleSource::PeopleOnly
            }
        );
        assert_eq!(
            join.source.to_string(),
            "meetings are for people, never agents"
        );
        assert_eq!(
            agent.tier_for(&name("files.read"), Tier::Ask).tier,
            Tier::Auto,
            "the group's * reaches everything else"
        );

        // The person joins in Meet and sees their meetings in Calendar; any
        // other app declaring them gets the org's Never.
        let policy = Policy::from_toml(crate::DEFAULT_POLICY).unwrap();
        let declared = [name("meet.list"), name("meet.join")];
        let tier = |app: &str, action: &str| {
            policy
                .for_app("dana", &app.parse().unwrap(), &declared, Label::Internal)
                .tier_for(&name(action), Tier::Ask)
                .tier
        };
        assert_eq!(tier("meet", "meet.join"), Tier::Auto);
        assert_eq!(tier("calendar", "meet.list"), Tier::Auto);
        assert_eq!(tier("calendar", "meet.join"), Tier::Never);
        assert_eq!(tier("mail", "meet.list"), Tier::Never);
        // Claude Code's calendar.* doesn't reach them either.
        let claude = policy
            .effective("dana", &"claude-code".parse().unwrap())
            .unwrap();
        assert_eq!(
            claude.tier_for(&name("meet.list"), Tier::Auto).tier,
            Tier::Never
        );
    }

    #[test]
    fn adr0045_agents_read_a_transcript_but_never_join_or_add_to_one() {
        let policy = Policy::from_toml(crate::DEFAULT_POLICY).unwrap();
        let desktop = policy
            .effective("dana", &"desktop-agent".parse().unwrap())
            .unwrap();
        assert_eq!(
            desktop.tier_for(&name("meet.transcript"), Tier::Auto).tier,
            Tier::Auto
        );
        assert_eq!(
            desktop.tier_for(&name("mail.send"), Tier::Ask).tier,
            Tier::Ask,
            "sending what it was asked to always asks"
        );
        for action in ["meet.transcribe", "meet.join", "meet.keys"] {
            assert_eq!(
                desktop.tier_for(&name(action), Tier::Auto).source,
                RuleSource::PeopleOnly,
                "{action}"
            );
        }
        // Meet adds the person's own words; an agent's "*" doesn't.
        let declared = [name("meet.transcribe")];
        let meet = policy.for_app("dana", &"meet".parse().unwrap(), &declared, Label::Internal);
        assert_eq!(
            meet.tier_for(&name("meet.transcribe"), Tier::Ask).tier,
            Tier::Auto
        );
        // A rule may name the transcript, as any action: IT can keep a
        // group's agents out of it.
        with_default(
            "[[group]]\nname = \"legal\"\nmembers = [\"dana\"]\nagents = [\"desktop-agent\"]\n\
             [group.tiers]\n\"meet.transcript\" = \"never\"\n",
        )
        .unwrap();
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
    fn aw14_actions_an_agent_isnt_given_are_counted_not_listed() {
        let known = [
            (name("files.read"), Tier::Auto),
            (name("calendar.list"), Tier::Auto),
            (name("calendar.add"), Tier::Ask),
            (name("chat.post"), Tier::Ask),
            (name("security.settings"), Tier::Ask),
        ];
        // The probe may read files only; security.* is also IT's Never.
        let probe = Policy::from_toml(POLICY)
            .unwrap()
            .effective("demo", &"probe".parse().unwrap())
            .unwrap();
        let lines = probe.cant_touch(&known, |n| format!("[{n}]"));
        assert_eq!(lines[0], "[security.settings]", "IT's rule, by name");
        assert!(
            lines.contains(&"3 other actions it isn't given".to_string()),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("calendar")), "{lines:?}");
        // An agent given everything it could touch has no such line.
        let lines = invoice().cant_touch(&known[..1], |n| n.to_string());
        assert!(
            !lines.iter().any(|l| l.contains("isn't given")),
            "{lines:?}"
        );
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
