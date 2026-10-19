//! Preview impact (AD-3): what publishing the draft would change, compared
//! with what the broad ring runs, in words IT reads, and how many people
//! and agents it reaches ("this change affects 214 users and 3 agents").

use std::collections::{BTreeMap, BTreeSet};

use glass_policy::document::{AgentPolicy, GroupPolicy, PolicyDocument, Support, SupportRole};
use glass_policy::{ActionPattern, Policy};
use glass_types::{ActionName, SideEffect, Tier};
use serde::Serialize;

use crate::directory::{Directory, EVERYONE};

/// Plain-language names for actions, from policy's `[names]` and the
/// first-party apps' own titles.
pub struct Titles {
    titles: BTreeMap<String, String>,
    /// For `Policy::lint`: every action a device offers.
    pub lint_info: Vec<(ActionName, SideEffect, Tier)>,
}

impl Titles {
    pub fn first_party() -> Self {
        let registry = glass_first_party::registry(glass_first_party::Setup::offline())
            .expect("the first-party apps register");
        Self {
            titles: registry
                .defs()
                .map(|d| (d.name.to_string(), d.title.clone()))
                .collect(),
            lint_info: registry.lint_info(),
        }
    }

    pub fn of(&self, doc: &PolicyDocument, pattern: &ActionPattern) -> String {
        if let Some(name) = doc.names.get(pattern) {
            return name.clone();
        }
        let text = pattern.to_string();
        self.titles.get(&text).cloned().unwrap_or(text)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Impact {
    /// The version compared with: what the broad ring runs.
    pub base_version: Option<u64>,
    pub users: usize,
    pub agents: usize,
    pub agent_ids: Vec<String>,
    /// Groups whose rules changed, as IT names them.
    pub groups: Vec<String>,
    pub org_wide: bool,
    pub changes: Vec<String>,
    /// `Policy::lint` on the draft: irreversible actions below Ask and the
    /// like.
    pub warnings: Vec<String>,
}

fn tier(t: Tier) -> &'static str {
    match t {
        Tier::Auto => "Auto",
        Tier::Notify => "Notify",
        Tier::Ask => "Ask",
        Tier::Never => "Never",
    }
}

fn tier_lines(
    who: &str,
    old: &BTreeMap<ActionPattern, Tier>,
    new: &BTreeMap<ActionPattern, Tier>,
    draft: &PolicyDocument,
    titles: &Titles,
    out: &mut Vec<String>,
) {
    let patterns: BTreeSet<&ActionPattern> = old.keys().chain(new.keys()).collect();
    for p in patterns {
        let title = titles.of(draft, p);
        match (old.get(p), new.get(p)) {
            (Some(a), Some(b)) if a != b => {
                out.push(format!("{who}: {title}: {} → {}", tier(*a), tier(*b)));
            }
            (None, Some(b)) => out.push(format!("{who}: {title}: {}", tier(*b))),
            (Some(a), None) => out.push(format!(
                "{who}: {title}: back to the default (was {})",
                tier(*a)
            )),
            _ => {}
        }
    }
}

fn set_lines<T: Ord + std::fmt::Display>(
    who: &str,
    what: &str,
    old: &[T],
    new: &[T],
    out: &mut Vec<String>,
) {
    let old: BTreeSet<&T> = old.iter().collect();
    let new: BTreeSet<&T> = new.iter().collect();
    for added in new.difference(&old) {
        out.push(format!("{who}: {what} adds {added}"));
    }
    for removed in old.difference(&new) {
        out.push(format!("{who}: {what} removes {removed}"));
    }
}

fn without_members(g: &GroupPolicy) -> GroupPolicy {
    GroupPolicy {
        members: Vec::new(),
        ..g.clone()
    }
}

fn group_lines(
    name: &str,
    old: Option<&GroupPolicy>,
    new: Option<&GroupPolicy>,
    draft: &PolicyDocument,
    titles: &Titles,
    out: &mut Vec<String>,
) {
    let empty = GroupPolicy {
        name: name.to_owned(),
        members: Vec::new(),
        agents: Vec::new(),
        apps: Vec::new(),
        max_label: None,
        tiers: BTreeMap::new(),
        models: None,
    };
    let (o, n) = (old.unwrap_or(&empty), new.unwrap_or(&empty));
    tier_lines(name, &o.tiers, &n.tiers, draft, titles, out);
    set_lines(name, "allowed agents", &o.agents, &n.agents, out);
    set_lines(name, "apps in profiles", &o.apps, &n.apps, out);
    if o.max_label != n.max_label {
        out.push(format!(
            "{name}: data up to {}",
            n.max_label
                .map_or_else(|| "the org's limit".to_owned(), |l| l.to_string())
        ));
    }
    if o.models != n.models {
        out.push(format!(
            "{name}: models {}",
            n.models
                .as_ref()
                .map_or_else(|| "the org's list".to_owned(), |m| m.join(", "))
        ));
    }
}

fn agent_lines(
    id: &str,
    old: Option<&AgentPolicy>,
    new: Option<&AgentPolicy>,
    draft: &PolicyDocument,
    titles: &Titles,
    out: &mut Vec<String>,
) {
    let (Some(o), Some(n)) = (old, new) else {
        out.push(format!(
            "{id}: {}",
            if new.is_some() {
                "new agent rules"
            } else {
                "rules removed"
            }
        ));
        return;
    };
    let who = format!("Agent {id}");
    let actions = |a: &AgentPolicy| {
        a.allowed_actions
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    set_lines(&who, "allowed actions", &actions(o), &actions(n), out);
    tier_lines(&who, &o.tiers, &n.tiers, draft, titles, out);
    set_lines(&who, "network", &o.egress, &n.egress, out);
    if o.max_label != n.max_label {
        out.push(format!(
            "{who}: data up to {}",
            n.max_label
                .map_or_else(|| "the org's limit".to_owned(), |l| l.to_string())
        ));
    }
    if o.models != n.models {
        out.push(format!(
            "{who}: models {}",
            n.models
                .as_ref()
                .map_or_else(|| "the org's list".to_owned(), |m| m.join(", "))
        ));
    }
}

/// The agents `user` may run under `doc`, by any of their groups.
fn agents_for(doc: &PolicyDocument, directory: &dyn Directory, user: &str) -> Vec<String> {
    doc.groups
        .iter()
        .filter(|g| g.name == EVERYONE || directory.members(&g.name).contains(&user))
        .flat_map(|g| g.agents.iter().map(ToString::to_string))
        .collect()
}

/// What publishing `draft` would change compared with `base`.
pub fn preview(
    base: Option<(u64, &PolicyDocument)>,
    draft: &PolicyDocument,
    directory: &dyn Directory,
    titles: &Titles,
) -> Impact {
    let empty = PolicyDocument {
        version: glass_policy::document::SCHEMA_VERSION,
        org: Default::default(),
        groups: Vec::new(),
        agents: Vec::new(),
        labels: Vec::new(),
        names: BTreeMap::new(),
        fixes: Default::default(),
        models: Default::default(),
        mail: None,
        support: Default::default(),
    };
    let (base_version, old) = base.map_or((None, &empty), |(v, d)| (Some(v), d));
    let mut changes = Vec::new();

    // Org-wide rules reach everyone.
    tier_lines(
        "Everyone",
        &old.org.tiers,
        &draft.org.tiers,
        draft,
        titles,
        &mut changes,
    );
    let org_wide_before = changes.len();
    let mut org_wide = !changes.is_empty();
    if old.org.max_label != draft.org.max_label {
        changes.push(format!("Everyone: data up to {}", draft.org.max_label));
    }
    if old.org.approval_timeout_secs != draft.org.approval_timeout_secs {
        changes.push(format!(
            "Everyone: an unanswered ask pauses the task after {} minutes",
            draft.org.approval_timeout_secs / 60
        ));
    }
    if old.models != draft.models {
        changes.push("Everyone: models and where each label's data may go".into());
    }
    if old.labels != draft.labels {
        changes.push("Everyone: which folders are labeled".into());
    }
    if old.fixes != draft.fixes {
        changes.push("Everyone: what the IT agent may fix".into());
    }
    if old.mail != draft.mail {
        changes.push(match &draft.mail {
            Some(mail) => {
                let servers: Vec<String> = [&mail.incoming, &mail.outgoing]
                    .into_iter()
                    .flatten()
                    .map(|s| format!("{}:{}", s.host, s.port))
                    .chain(mail.tenant.clone())
                    .collect();
                format!(
                    "Everyone: mail through {}{}",
                    mail.service.title(),
                    if servers.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", servers.join(", "))
                    }
                )
            }
            None => "Everyone: no mail service from IT".into(),
        });
    }
    org_wide |= changes.len() > org_wide_before;
    let support_changed = support_lines(&old.support, &draft.support, directory, &mut changes);

    // Groups, without their members (the directory's, not IT's edit).
    let names: BTreeSet<&str> = old
        .groups
        .iter()
        .chain(&draft.groups)
        .map(|g| g.name.as_str())
        .collect();
    let mut changed_groups = Vec::new();
    for name in names {
        let o = old
            .groups
            .iter()
            .find(|g| g.name == name)
            .map(without_members);
        let n = draft
            .groups
            .iter()
            .find(|g| g.name == name)
            .map(without_members);
        if o != n {
            group_lines(
                &directory.group_display(name),
                o.as_ref(),
                n.as_ref(),
                draft,
                titles,
                &mut changes,
            );
            changed_groups.push(name);
        }
    }

    // Agents' own rules.
    let ids: BTreeSet<String> = old
        .agents
        .iter()
        .chain(&draft.agents)
        .map(|a| a.id.to_string())
        .collect();
    let mut rules_changed: BTreeSet<String> = BTreeSet::new();
    for id in &ids {
        let o = old.agents.iter().find(|a| a.id.as_str() == id);
        let n = draft.agents.iter().find(|a| a.id.as_str() == id);
        if o != n {
            agent_lines(id, o, n, draft, titles, &mut changes);
            rules_changed.insert(id.clone());
        }
    }

    // Who it reaches: everyone for an org-wide change; otherwise the
    // members of changed groups. And what: the agents those people may
    // run, when their rules changed (not just their profile), and every
    // agent whose own rules changed, with whoever may run it.
    let mut users: BTreeSet<&str> = BTreeSet::new();
    let mut agents = rules_changed.clone();
    if org_wide || changed_groups.contains(&EVERYONE) {
        users.extend(directory.members(EVERYONE));
    }
    if org_wide {
        agents.extend(draft.agents.iter().map(|a| a.id.to_string()));
    }
    for g in &changed_groups {
        let members = directory.members(g);
        users.extend(members.iter().copied());
        let rules = |doc: &PolicyDocument| {
            doc.groups
                .iter()
                .find(|x| x.name == *g)
                .map(|x| GroupPolicy {
                    members: Vec::new(),
                    apps: Vec::new(),
                    ..x.clone()
                })
        };
        let profile_only = rules(old) == rules(draft);
        if !profile_only {
            for member in members {
                for doc in [old, draft] {
                    agents.extend(agents_for(doc, directory, member));
                }
            }
        }
    }
    // A support role reaches the people who hold it and the people whose
    // computers it may ask for.
    for role in &support_changed {
        for group in role.groups.iter().chain(&role.people) {
            users.extend(directory.members(group));
        }
    }
    for group in &draft.groups {
        if group
            .agents
            .iter()
            .any(|a| rules_changed.contains(a.as_str()))
        {
            users.extend(directory.members(&group.name));
        }
    }

    let mut warnings = Policy::from_document(draft.clone())
        .map(|p| p.lint(&titles.lint_info))
        .unwrap_or_default();
    for role in draft
        .support
        .roles
        .iter()
        .filter(|r| !r.unattended.is_empty())
    {
        warnings.push(format!(
            "{} may reach {} computers with nobody there to answer",
            role.name,
            role.unattended.join(", ")
        ));
    }
    Impact {
        base_version,
        users: users.len(),
        agents: agents.len(),
        agent_ids: agents.into_iter().collect(),
        groups: changed_groups
            .iter()
            .map(|g| directory.group_display(g))
            .collect(),
        org_wide,
        changes,
        warnings,
    }
}

fn tool_list(tools: &[glass_policy::document::SupportTool]) -> String {
    let words: Vec<String> = tools.iter().map(|t| t.title().to_lowercase()).collect();
    words.join(", ")
}

/// Remote help's roles (CF-81) in words; returns the roles that changed,
/// as they were or now are.
fn support_lines<'a>(
    old: &'a Support,
    new: &'a Support,
    directory: &dyn Directory,
    changes: &mut Vec<String>,
) -> Vec<&'a SupportRole> {
    let groups = |r: &SupportRole| -> String {
        let names: Vec<String> = r
            .groups
            .iter()
            .map(|g| directory.group_display(g))
            .collect();
        names.join(", ")
    };
    let mut changed = Vec::new();
    for role in &new.roles {
        let Some(was) = old.roles.iter().find(|o| o.id == role.id) else {
            changes.push(format!(
                "Remote help: {} ({}) may {}",
                role.name,
                groups(role),
                tool_list(&role.tools)
            ));
            changed.push(role);
            continue;
        };
        if was == role {
            continue;
        }
        let gained: Vec<_> = role
            .tools
            .iter()
            .filter(|t| !was.tools.contains(t))
            .copied()
            .collect();
        let lost: Vec<_> = was
            .tools
            .iter()
            .filter(|t| !role.tools.contains(t))
            .copied()
            .collect();
        let mut parts = Vec::new();
        if !gained.is_empty() {
            parts.push(format!("may now {}", tool_list(&gained)));
        }
        if !lost.is_empty() {
            parts.push(format!("may no longer {}", tool_list(&lost)));
        }
        if was.groups != role.groups {
            parts.push(format!("held by {}", groups(role)));
        }
        if was.people != role.people {
            let people: Vec<String> = role
                .people
                .iter()
                .map(|g| directory.group_display(g))
                .collect();
            parts.push(format!("helps {}", people.join(", ")));
        }
        if was.max_minutes != role.max_minutes {
            parts.push(format!("sessions up to {} minutes", role.max_minutes));
        }
        if was.unattended != role.unattended {
            parts.push(if role.unattended.is_empty() {
                "no unattended access".to_owned()
            } else {
                format!("unattended on {}", role.unattended.join(", "))
            });
        }
        if was.name != role.name {
            parts.push(format!("renamed from {}", was.name));
        }
        changes.push(format!("Remote help: {} {}", role.name, parts.join("; ")));
        changed.extend([was, role]);
    }
    for gone in old
        .roles
        .iter()
        .filter(|o| !new.roles.iter().any(|r| r.id == o.id))
    {
        changes.push(format!(
            "Remote help: {} taken out; its people can't help anyone through it",
            gone.name
        ));
        changed.push(gone);
    }
    if old.answer_within_secs != new.answer_within_secs {
        changes.push(format!(
            "Everyone: {} seconds to answer a request for help",
            new.answer_within_secs
        ));
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::LocalDirectory;

    fn doc(text: &str) -> PolicyDocument {
        toml::from_str(text).unwrap()
    }

    fn with_ap(extra: &str) -> String {
        format!(
            "{}\n[[group]]\nname = \"ap-ops\"\nagents = [\"invoice-demo\"]\n{extra}",
            glass_policy::DEFAULT_POLICY
        )
    }

    #[test]
    fn ad3_a_group_change_reaches_its_members_and_their_agents() {
        let dir = LocalDirectory::starter("root");
        let titles = Titles::first_party();
        let base = doc(&with_ap(""));
        let draft = doc(&with_ap("tiers = { \"mail.send\" = \"never\" }\n"));
        let impact = preview(Some((4, &base)), &draft, &dir, &titles);
        assert_eq!(impact.base_version, Some(4));
        assert_eq!(impact.users, 2, "dana and sam");
        assert_eq!(
            impact.agent_ids,
            [
                "claude-code",
                "containment-probe",
                "invoice-demo",
                "it-agent"
            ],
            "every agent AP's people may run, by any of their groups"
        );
        assert_eq!(impact.groups, ["AP Operations"]);
        assert!(!impact.org_wide);
        assert_eq!(impact.changes.len(), 1, "{:?}", impact.changes);
        assert!(
            impact.changes[0].starts_with("AP Operations: "),
            "{:?}",
            impact.changes
        );
        assert!(
            impact.changes[0].ends_with(": Never"),
            "{:?}",
            impact.changes
        );
    }

    #[test]
    fn ad3_an_org_wide_change_reaches_everyone_and_nothing_changed_reaches_nobody() {
        let dir = LocalDirectory::starter("root");
        let titles = Titles::first_party();
        let base = doc(glass_policy::DEFAULT_POLICY);
        let same = preview(Some((1, &base)), &base, &dir, &titles);
        assert_eq!((same.users, same.agents, same.changes.len()), (0, 0, 0));

        let draft = doc(&glass_policy::DEFAULT_POLICY
            .replace("\"files.delete\" = \"ask\"", "\"files.delete\" = \"never\""));
        let impact = preview(Some((1, &base)), &draft, &dir, &titles);
        assert!(impact.org_wide);
        assert_eq!(impact.users, dir.users.len());
        assert_eq!(impact.agents, draft.agents.len());
        assert!(
            impact.changes[0].contains("Ask → Never"),
            "{:?}",
            impact.changes
        );
    }

    #[test]
    fn ad3_lint_warnings_come_with_the_preview() {
        let dir = LocalDirectory::starter("root");
        let titles = Titles::first_party();
        let base = doc(glass_policy::DEFAULT_POLICY);
        let draft = doc(&glass_policy::DEFAULT_POLICY
            .replace("\"mail.send\" = \"ask\"", "\"mail.send\" = \"auto\""));
        let impact = preview(Some((1, &base)), &draft, &dir, &titles);
        assert!(
            !impact.warnings.is_empty(),
            "sending mail without an ask is flagged"
        );
    }
}
