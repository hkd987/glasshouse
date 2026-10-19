//! IT's edits to the policy draft, one control at a time (AD-2). Each
//! changes the draft `PolicyDocument` in place; the state validates the
//! result as a device would before keeping it.

use glass_policy::ActionPattern;
use glass_policy::document::{GroupPolicy, MailSettings, PolicyDocument};
use glass_types::{AgentId, Label, Tier};
use serde::Deserialize;

use crate::views::CLASSES;

/// One edit, as the policy page sends it.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    /// A tier for every action in a class, for a group; `None` goes back
    /// to the org's.
    Tier {
        group: String,
        class: String,
        tier: Option<Tier>,
    },
    /// Whether a group's members may run an agent.
    Agent {
        group: String,
        agent: AgentId,
        allowed: bool,
    },
    /// The most sensitive data a group's agents may open; `None` is the
    /// org's limit.
    MaxLabel { group: String, label: Option<Label> },
    /// Network destinations an agent's sandbox may reach (`host:port`).
    Egress { agent: AgentId, egress: Vec<String> },
    /// Whether an app is in a group's members' profiles.
    App {
        group: String,
        app: AgentId,
        on: bool,
    },
    /// The org's mail service (NV-3); `None` takes it out.
    Mail { mail: Option<MailSettings> },
}

fn group<'a>(doc: &'a mut PolicyDocument, name: &str) -> &'a mut GroupPolicy {
    if let Some(at) = doc.groups.iter().position(|g| g.name == name) {
        return &mut doc.groups[at];
    }
    doc.groups.push(GroupPolicy {
        name: name.to_owned(),
        members: Vec::new(),
        agents: Vec::new(),
        apps: Vec::new(),
        max_label: None,
        tiers: Default::default(),
        models: None,
    });
    doc.groups.last_mut().expect("just pushed")
}

/// Applies `edit` to `doc`, or says why it can't.
pub fn apply(doc: &mut PolicyDocument, edit: Edit) -> Result<(), String> {
    match edit {
        Edit::Mail { mail } => {
            doc.mail = mail;
        }
        Edit::Tier {
            group: name,
            class,
            tier,
        } => {
            let (_, _, patterns) = CLASSES
                .iter()
                .find(|(id, _, _)| *id == class)
                .ok_or_else(|| format!("no action class {class}"))?;
            let g = group(doc, &name);
            for p in *patterns {
                let pattern: ActionPattern = p.parse().map_err(|e| format!("{e}"))?;
                match tier {
                    Some(t) => {
                        g.tiers.insert(pattern, t);
                    }
                    None => {
                        g.tiers.remove(&pattern);
                    }
                }
            }
        }
        Edit::Agent {
            group: name,
            agent,
            allowed,
        } => {
            if !doc.agents.iter().any(|a| a.id == agent) {
                return Err(format!("{agent} has no agent rules in this policy"));
            }
            let g = group(doc, &name);
            g.agents.retain(|a| a != &agent);
            if allowed {
                g.agents.push(agent);
            }
        }
        Edit::MaxLabel { group: name, label } => group(doc, &name).max_label = label,
        Edit::Egress { agent, egress } => {
            for destination in &egress {
                let valid = destination
                    .rsplit_once(':')
                    .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok())
                    && !destination.contains(['/', ' ', '@']);
                if !valid {
                    return Err(format!("{destination:?} is not host:port"));
                }
            }
            let rules = doc
                .agents
                .iter_mut()
                .find(|a| a.id == agent)
                .ok_or_else(|| format!("{agent} has no agent rules in this policy"))?;
            rules.egress = egress;
        }
        Edit::App {
            group: name,
            app,
            on,
        } => {
            let g = group(doc, &name);
            g.apps.retain(|a| a != &app);
            if on {
                g.apps.push(app);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> PolicyDocument {
        toml::from_str(glass_policy::DEFAULT_POLICY).unwrap()
    }

    #[test]
    fn ad2_a_class_tier_sets_every_action_in_it_for_the_group() {
        let mut d = doc();
        let edit: Edit = serde_json::from_str(
            r#"{"edit":"tier","group":"ap-ops","class":"send","tier":"never"}"#,
        )
        .unwrap();
        apply(&mut d, edit).unwrap();
        let g = d.groups.iter().find(|g| g.name == "ap-ops").unwrap();
        assert_eq!(g.tiers.len(), 2);
        assert!(g.tiers.values().all(|t| *t == Tier::Never));
        apply(
            &mut d,
            Edit::Tier {
                group: "ap-ops".into(),
                class: "send".into(),
                tier: None,
            },
        )
        .unwrap();
        assert!(
            d.groups
                .iter()
                .find(|g| g.name == "ap-ops")
                .unwrap()
                .tiers
                .is_empty()
        );
    }

    #[test]
    fn ad2_agents_egress_and_profiles_per_group() {
        let mut d = doc();
        apply(
            &mut d,
            Edit::Agent {
                group: "ap-ops".into(),
                agent: "invoice-demo".parse().unwrap(),
                allowed: true,
            },
        )
        .unwrap();
        assert!(
            apply(
                &mut d,
                Edit::Agent {
                    group: "ap-ops".into(),
                    agent: "nobody".parse().unwrap(),
                    allowed: true,
                },
            )
            .is_err()
        );
        apply(
            &mut d,
            Edit::Egress {
                agent: "invoice-demo".parse().unwrap(),
                egress: vec!["erp.example:443".into()],
            },
        )
        .unwrap();
        assert!(
            apply(
                &mut d,
                Edit::Egress {
                    agent: "invoice-demo".parse().unwrap(),
                    egress: vec!["https://x/".into()],
                },
            )
            .is_err()
        );
        apply(
            &mut d,
            Edit::App {
                group: "ap-ops".into(),
                app: "terminal".parse().unwrap(),
                on: true,
            },
        )
        .unwrap();
        glass_policy::Policy::from_document(d).unwrap();
    }
}
