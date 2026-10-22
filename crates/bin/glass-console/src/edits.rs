//! IT's edits to the policy draft, one control at a time (AD-2). Each
//! changes the draft `PolicyDocument` in place; the state validates the
//! result as a device would before keeping it.

use glass_policy::ActionPattern;
use std::collections::BTreeMap;

use glass_policy::document::{GroupPolicy, MailSettings, Managed, PolicyDocument, SupportRole};
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
    /// A group's device settings (UX-1), by id: each set as a default or
    /// locked, or `None` to leave it to the next group.
    Settings {
        group: String,
        settings: BTreeMap<String, Option<Managed>>,
    },
    /// Which of the org's groups win when a person's groups set a setting
    /// differently: higher first. Every group but `everyone`, once.
    GroupOrder { order: Vec<String> },
    /// A remote help role (CF-81), added or replaced by its id; `None`
    /// takes it out.
    SupportRole {
        id: String,
        role: Option<SupportRole>,
    },
    /// Whether agents may use the org's model credits (ADR 0039).
    Credits { on: bool },
    /// Whether an agent may ask AI models (`model.complete`, AD-2), on
    /// whatever routes the org's models allow. For every group.
    ModelCalls { agent: AgentId, on: bool },
}

/// The action an agent needs to ask a model (ADR 0018).
pub const MODEL_COMPLETE: &str = "model.complete";

/// The provider model credits run on, in policy (ADR 0039).
pub const CREDITS_PROVIDER: &str = "credits";
/// Its one model: OpenRouter's auto router, which picks per request.
pub const CREDITS_MODEL: &str = "auto";

/// Whether `doc` has model credits on.
pub fn credits_on(doc: &PolicyDocument) -> bool {
    doc.models
        .providers
        .iter()
        .any(|p| p.id == CREDITS_PROVIDER)
}

/// Credits on: the provider, its model, and the default for public and
/// internal data when the org has none. Confidential data goes to it only
/// if IT adds it to that route (GT-3). Off takes all of it out, wherever
/// it is named.
fn credits(doc: &mut PolicyDocument, on: bool) {
    use glass_policy::document::{ModelEntry, ProviderEntry, ProviderKind};
    let models = &mut doc.models;
    if on {
        if !models.providers.iter().any(|p| p.id == CREDITS_PROVIDER) {
            models.providers.push(ProviderEntry {
                id: CREDITS_PROVIDER.into(),
                kind: ProviderKind::Credits,
                name: "Glasshouse model credits".into(),
                region: None,
                endpoint: None,
            });
        }
        if !models.models.iter().any(|m| m.id == CREDITS_MODEL) {
            models.models.push(ModelEntry {
                id: CREDITS_MODEL.into(),
                provider: CREDITS_PROVIDER.into(),
                model: glass_gateway_providers::OPENROUTER_AUTO.into(),
                name: "The best model for each request".into(),
                usd_per_mtok_in: None,
                usd_per_mtok_out: None,
                usd_per_mtok_cache_read: None,
            });
        }
        models.default.get_or_insert_with(|| CREDITS_MODEL.into());
        return;
    }
    let gone: Vec<String> = models
        .models
        .iter()
        .filter(|m| m.provider == CREDITS_PROVIDER)
        .map(|m| m.id.clone())
        .collect();
    models.models.retain(|m| m.provider != CREDITS_PROVIDER);
    models.providers.retain(|p| p.id != CREDITS_PROVIDER);
    if models.default.as_ref().is_some_and(|d| gone.contains(d)) {
        models.default = None;
    }
    for route in models.routes.values_mut() {
        route.retain(|m| !gone.contains(m));
    }
    models.routes.retain(|_, route| !route.is_empty());
    for list in doc
        .groups
        .iter_mut()
        .filter_map(|g| g.models.as_mut())
        .chain(doc.agents.iter_mut().filter_map(|a| a.models.as_mut()))
    {
        list.retain(|m| !gone.contains(m));
    }
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
        settings: Default::default(),
    });
    doc.groups.last_mut().expect("just pushed")
}

/// Applies `edit` to `doc`, or says why it can't.
pub fn apply(doc: &mut PolicyDocument, edit: Edit) -> Result<(), String> {
    match edit {
        Edit::Credits { on } => credits(doc, on),
        Edit::Mail { mail } => {
            doc.mail = mail;
        }
        Edit::Settings {
            group: name,
            settings,
        } => {
            let g = group(doc, &name);
            for (id, managed) in settings {
                match managed {
                    Some(m) => {
                        g.settings.insert(id, m);
                    }
                    None => {
                        g.settings.remove(&id);
                    }
                }
            }
        }
        Edit::GroupOrder { order } => {
            let mut now: Vec<&str> = doc
                .groups
                .iter()
                .map(|g| g.name.as_str())
                .filter(|n| *n != crate::directory::EVERYONE)
                .collect();
            let mut asked: Vec<&str> = order.iter().map(String::as_str).collect();
            now.sort_unstable();
            asked.sort_unstable();
            if now != asked {
                return Err("list every group once, everyone left out".into());
            }
            doc.groups.sort_by_key(|g| {
                order
                    .iter()
                    .position(|n| *n == g.name)
                    .map_or(0, |at| at + 1)
            });
        }
        Edit::SupportRole { id, role } => {
            let at = doc.support.roles.iter().position(|r| r.id == id);
            match (role, at) {
                (Some(role), _) if role.id != id => {
                    return Err("a role keeps its id: add a new role instead".into());
                }
                (Some(role), Some(at)) => doc.support.roles[at] = role,
                (Some(role), None) => doc.support.roles.push(role),
                (None, Some(at)) => {
                    doc.support.roles.remove(at);
                }
                (None, None) => return Err(format!("no support role {id}")),
            }
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
        Edit::ModelCalls { agent, on } => {
            let rules = doc
                .agents
                .iter_mut()
                .find(|a| a.id == agent)
                .ok_or_else(|| format!("{agent} has no agent rules in this policy"))?;
            let action: glass_types::ActionName =
                MODEL_COMPLETE.parse().map_err(|e| format!("{e}"))?;
            let exact: ActionPattern = MODEL_COMPLETE.parse().map_err(|e| format!("{e}"))?;
            rules.allowed_actions.retain(|p| p != &exact);
            if on {
                rules.allowed_actions.push(exact);
            } else if let Some(wider) = rules.allowed_actions.iter().find(|p| p.matches(&action)) {
                return Err(format!(
                    "{agent}'s rules allow {wider}, which includes asking AI models"
                ));
            }
        }
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
    fn ad2_adr0039_an_agent_may_ask_models_once_it_is_turned_on() {
        let mut d = doc();
        let allows = |d: &PolicyDocument| {
            d.agents
                .iter()
                .find(|a| a.id.as_str() == "it-agent")
                .unwrap()
                .allowed_actions
                .iter()
                .filter(|p| p.as_str() == MODEL_COMPLETE)
                .count()
        };
        assert_eq!(allows(&d), 0);
        let on: Edit =
            serde_json::from_str(r#"{"edit":"model_calls","agent":"it-agent","on":true}"#).unwrap();
        apply(&mut d, on.clone()).unwrap();
        apply(&mut d, on).unwrap();
        assert_eq!(allows(&d), 1, "once, however often it's turned on");
        glass_policy::Policy::from_document(d.clone()).unwrap();
        apply(
            &mut d,
            Edit::ModelCalls {
                agent: "it-agent".parse().unwrap(),
                on: false,
            },
        )
        .unwrap();
        assert_eq!(allows(&d), 0);
        assert!(
            apply(
                &mut d,
                Edit::ModelCalls {
                    agent: "nobody".parse().unwrap(),
                    on: true,
                },
            )
            .is_err()
        );
        // A wider rule can't be turned off from here: the console says so.
        let rules = d
            .agents
            .iter_mut()
            .find(|a| a.id.as_str() == "it-agent")
            .unwrap();
        rules.allowed_actions.push("model.*".parse().unwrap());
        let err = apply(
            &mut d,
            Edit::ModelCalls {
                agent: "it-agent".parse().unwrap(),
                on: false,
            },
        )
        .unwrap_err();
        assert!(err.contains("model.*"), "{err}");
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

    #[test]
    fn ux1_settings_per_group_and_the_order_groups_win_in() {
        let mut d = doc();
        let edit: Edit = serde_json::from_str(
            r#"{"edit":"settings","group":"ap-ops","settings":{"lock.after_minutes":{"value":5,"locked":true},"clock.hour24":{"value":true}}}"#,
        )
        .unwrap();
        apply(&mut d, edit).unwrap();
        let ap = d.groups.iter().find(|g| g.name == "ap-ops").unwrap();
        assert!(ap.settings["lock.after_minutes"].locked);
        assert!(!ap.settings["clock.hour24"].locked);
        apply(
            &mut d,
            Edit::Settings {
                group: "ap-ops".into(),
                settings: [("clock.hour24".to_owned(), None)].into(),
            },
        )
        .unwrap();
        assert_eq!(
            d.groups
                .iter()
                .find(|g| g.name == "ap-ops")
                .unwrap()
                .settings
                .len(),
            1
        );
        glass_policy::Policy::from_document(d.clone()).unwrap();

        let names =
            |d: &PolicyDocument| d.groups.iter().map(|g| g.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&d), ["everyone", "it-admins", "ap-ops"]);
        apply(
            &mut d,
            Edit::GroupOrder {
                order: vec!["ap-ops".into(), "it-admins".into()],
            },
        )
        .unwrap();
        assert_eq!(names(&d), ["everyone", "ap-ops", "it-admins"]);
        assert!(
            apply(
                &mut d,
                Edit::GroupOrder {
                    order: vec!["ap-ops".into()]
                }
            )
            .is_err(),
            "every group, once"
        );
    }

    #[test]
    fn cf81_a_support_role_is_added_changed_and_taken_out_by_id() {
        let mut d = doc();
        let edit: Edit = serde_json::from_str(
            r#"{"edit":"support_role","id":"desk","role":{"id":"desk","name":"Desk","groups":["it-helpdesk"],"tools":["view"]}}"#,
        )
        .unwrap();
        apply(&mut d, edit.clone()).unwrap();
        assert_eq!(
            d.support.roles[0].people,
            ["everyone"],
            "everyone by default"
        );
        let Edit::SupportRole {
            role: Some(mut role),
            ..
        } = edit
        else {
            unreachable!()
        };
        role.tools
            .push(glass_policy::document::SupportTool::Control);
        apply(
            &mut d,
            Edit::SupportRole {
                id: "desk".into(),
                role: Some(role.clone()),
            },
        )
        .unwrap();
        assert_eq!(d.support.roles.len(), 1);
        assert_eq!(d.support.roles[0].tools.len(), 2);
        role.id = "other".into();
        assert!(
            apply(
                &mut d,
                Edit::SupportRole {
                    id: "desk".into(),
                    role: Some(role),
                },
            )
            .is_err(),
            "ids don't change"
        );
        apply(
            &mut d,
            Edit::SupportRole {
                id: "desk".into(),
                role: None,
            },
        )
        .unwrap();
        assert!(d.support.roles.is_empty());
        assert!(
            apply(
                &mut d,
                Edit::SupportRole {
                    id: "desk".into(),
                    role: None,
                },
            )
            .is_err()
        );
    }
}
