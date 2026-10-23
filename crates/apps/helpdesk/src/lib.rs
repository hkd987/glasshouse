//! The `helpdesk` app: what the IT agent does when it can't fix something
//! (AE-4). It opens a ticket with the diagnostics attached, so IT doesn't
//! start from zero.
//!
//! A ticket leaves the machine, so it can't be undone and defaults to Ask.
//! Until the help desk integration lands (ServiceNow and the like, through
//! adapters), tickets are written to an outbox on this machine that only
//! root can read.

use std::path::{Path, PathBuf};

use glass_api::{AskLabels, ChangeKind};
use glass_platform::fs::ConfinedDir;
use glass_types::{ActionName, SideEffect, Tier, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext, parse_input,
};

#[derive(Debug)]
pub struct HelpdeskProvider {
    outbox: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketInput {
    summary: String,
    details: String,
    #[serde(default)]
    diagnostics: Value,
}

impl HelpdeskProvider {
    /// Tickets go to `outbox`, created (root-only) if missing.
    pub fn new(outbox: PathBuf) -> Self {
        Self { outbox }
    }

    fn validate(input: &TicketInput) -> Result<(), ActionError> {
        let summary = input.summary.trim();
        if summary.is_empty() || summary.len() > 200 {
            return Err(ActionError::InvalidInput(
                "a ticket needs a one-line summary (up to 200 characters)".into(),
            ));
        }
        if input.details.len() > 20_000 || input.diagnostics.to_string().len() > 200_000 {
            return Err(ActionError::InvalidInput("the ticket is too large".into()));
        }
        Ok(())
    }
}

impl ActionProvider for HelpdeskProvider {
    fn actions(&self) -> Vec<ActionDef> {
        vec![ActionDef {
            name: "helpdesk.ticket.create".parse().expect("static name"),
            title: "Open a help desk ticket".into(),
            description: "Open a ticket with IT's help desk, with diagnostics attached. Use it \
                          when a problem can't be fixed from the approved list."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string" },
                    "details": { "type": "string" },
                    "diagnostics": { "type": "object" }
                },
                "required": ["summary", "details"],
                "additionalProperties": false
            }),
            side_effect: SideEffect::Irreversible,
            default_tier: Tier::Ask,
            ask: AskLabels::new("Review ticket", "Approve and open"),
        }]
    }

    fn returns(&self, action: &ActionName) -> Option<Value> {
        (action.as_str() == "helpdesk.ticket.create").then(|| {
            json!({
                "type": "object",
                "properties": {
                    "ticket": { "type": "string", "description": "the ticket's id, such as HD-1A2B3C4D" }
                },
                "required": ["ticket"],
                "additionalProperties": false
            })
        })
    }

    fn preview(
        &self,
        _ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        if action.as_str() != "helpdesk.ticket.create" {
            return Err(ActionError::InvalidInput(format!(
                "helpdesk has no action {action}"
            )));
        }
        let ticket: TicketInput = parse_input(input)?;
        Self::validate(&ticket)?;
        let sections = ticket.diagnostics.as_object().map_or(0, |o| o.len());
        Ok(Preview {
            summary: format!("Open a help desk ticket: {}", ticket.summary.trim()),
            targets: vec!["IT help desk".into()],
            items: vec![
                ticket.summary.trim().to_owned(),
                format!("With diagnostics attached ({sections} sections)"),
            ],
            ..Preview::default()
        })
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        _action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        let ticket: TicketInput = parse_input(input)?;
        Self::validate(&ticket)?;
        std::fs::create_dir_all(&self.outbox)
            .map_err(|e| ActionError::io("help desk outbox", e))?;
        let outbox =
            ConfinedDir::open(&self.outbox).map_err(|e| ActionError::io("help desk outbox", e))?;
        // One id per ticket: the task's, numbered if it opens more than one.
        let base = format!("HD-{}", ctx.principal.task.short().to_uppercase());
        let id = std::iter::once(base.clone())
            .chain((2..).map(|n| format!("{base}-{n}")))
            .find(|id| !outbox.exists(Path::new(&format!("{id}.json"))))
            .unwrap_or(base);
        let record = json!({
            "id": id,
            "opened_at": Timestamp::now(),
            "opened_by": ctx.principal.attribution(),
            "user": ctx.principal.on_behalf_of.name,
            "summary": ticket.summary.trim(),
            "details": ticket.details,
            "diagnostics": ticket.diagnostics,
        });
        let body = serde_json::to_vec_pretty(&record)
            .map_err(|e| ActionError::Failed(format!("writing the ticket: {e}")))?;
        outbox
            .write(Path::new(&format!("{id}.json")), &body, None)
            .map_err(|e| ActionError::io("help desk outbox", e))?;
        Ok(ActionOutput {
            text: format!("Opened {id}: {}", ticket.summary.trim()),
            data: json!({ "ticket": id }),
            changes: vec![Change {
                kind: ChangeKind::Sent,
                target: format!("Help desk ticket {id}: {}", ticket.summary.trim()),
                reversible: false,
                paths: Vec::new(),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use glass_platform::fs::Owner;
    use glass_types::{AgentPrincipal, TaskId, UserRef};

    use super::*;

    #[test]
    fn tickets_carry_diagnostics_and_cannot_be_undone() {
        let dir = tempfile::tempdir().unwrap();
        let p = HelpdeskProvider::new(dir.path().join("outbox"));
        let ctx = TaskContext {
            principal: AgentPrincipal {
                agent: "it-agent".parse().unwrap(),
                on_behalf_of: UserRef {
                    name: "dana".into(),
                    uid: 1001,
                },
                task: TaskId::new(),
            },
            owner: Owner {
                uid: 1001,
                gid: 1001,
            },
            home: dir.path().into(),
            grants: Vec::new(),
            groups: Vec::new(),
            data_label: None,
        };
        let action: ActionName = "helpdesk.ticket.create".parse().unwrap();
        let input = json!({
            "summary": "Printer won't print",
            "details": "Checked the network and the queue.",
            "diagnostics": { "network": { "healthy": true } }
        });
        let preview = p.preview(&ctx, &action, &input).unwrap();
        assert_eq!(preview.items[1], "With diagnostics attached (1 sections)");
        let out = p.invoke(&ctx, &action, &input).unwrap();
        assert!(!out.changes[0].reversible);
        let id = out.data["ticket"].as_str().unwrap();
        let saved: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(format!("outbox/{id}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["diagnostics"]["network"]["healthy"], json!(true));
        assert!(saved["opened_by"].as_str().unwrap().contains("for dana"));
        assert!(
            p.preview(&ctx, &action, &json!({"summary": "", "details": ""}))
                .is_err()
        );
        let second = p.invoke(&ctx, &action, &input).unwrap();
        assert_eq!(
            second.data["ticket"],
            json!(format!("{id}-2")),
            "a second ticket gets its own id"
        );
    }

    #[test]
    fn ga1_what_each_action_returns_is_what_it_declares() {
        let dir = tempfile::tempdir().unwrap();
        let p = HelpdeskProvider::new(dir.path().join("outbox"));
        let ctx = TaskContext {
            principal: AgentPrincipal {
                agent: "it-agent".parse().unwrap(),
                on_behalf_of: UserRef {
                    name: "dana".into(),
                    uid: 1001,
                },
                task: TaskId::new(),
            },
            owner: Owner {
                uid: 1001,
                gid: 1001,
            },
            home: dir.path().into(),
            grants: Vec::new(),
            groups: Vec::new(),
            data_label: None,
        };
        for def in p.actions() {
            let input =
                json!({ "summary": "VPN drops every hour", "details": "Reconnected twice." });
            let mut data = p.invoke(&ctx, &def.name, &input).unwrap().data;
            if let Some(data) = data.as_object_mut() {
                data.remove(glass_actions::AUDIT_DETAIL);
            }
            let shape = p.returns(&def.name).unwrap();
            glass_actions::shape::conforms(&data, &shape)
                .unwrap_or_else(|e| panic!("{}: {e}", def.name));
        }
    }
}
