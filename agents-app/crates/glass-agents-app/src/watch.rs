//! Who needs the person, for the tray and the system's notifications: what
//! the browser can't do for the web app (ADR 0065 §12). The app follows
//! the person's events as the web app does (a long poll), and after each
//! change looks at the questions waiting and the agents' states.

use std::collections::{HashMap, HashSet};

use glass_agents_api::{AgentStatus, AgentView, PendingApproval};

/// Something worth a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum News {
    /// An agent asks the person something. Only the ask's summary is told:
    /// its detail can be confidential (labels, ADR 0018).
    Asks { agent: String, summary: String },
    /// An agent finished what it was doing.
    Done { agent: String, doing: String },
}

impl News {
    /// The notification's title and body.
    pub fn words(&self) -> (String, String) {
        match self {
            News::Asks { agent, summary } => (format!("{agent} needs you"), summary.clone()),
            News::Done { agent, doing } => (format!("{agent} is done"), doing.clone()),
        }
    }
}

/// What the app has already told the person of.
#[derive(Debug, Default)]
pub struct Seen {
    looked: bool,
    asks: HashSet<String>,
    status: HashMap<String, AgentStatus>,
}

impl Seen {
    /// What's new since the last look. The first look tells nothing: the
    /// tray counts what's waiting, and opening the app shows it.
    pub fn look(&mut self, asks: &[PendingApproval], agents: &[AgentView]) -> Vec<News> {
        let mut news = Vec::new();
        for ask in asks {
            if self.looked && !self.asks.contains(&ask.approval.id) {
                news.push(News::Asks {
                    agent: ask.agent_name.clone(),
                    summary: ask.approval.summary.clone(),
                });
            }
        }
        self.asks = asks.iter().map(|a| a.approval.id.clone()).collect();
        for agent in agents {
            let before = self.status.insert(agent.id.clone(), agent.status);
            let was_busy = matches!(before, Some(AgentStatus::Working | AgentStatus::NeedsYou));
            if self.looked && was_busy && agent.status == AgentStatus::Idle {
                news.push(News::Done {
                    agent: agent.name.clone(),
                    doing: agent.doing.clone(),
                });
            }
        }
        self.looked = true;
        news
    }
}

/// The tray's words for how many questions wait.
pub fn waiting_words(n: usize) -> String {
    match n {
        0 => "Nothing is waiting on you".into(),
        1 => "1 question is waiting on you".into(),
        n => format!("{n} questions are waiting on you"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(id: &str, agent: &str) -> PendingApproval {
        serde_json::from_value(serde_json::json!({
            "agent": agent.to_lowercase(),
            "agent_name": agent,
            "approval": {
                "id": id, "action": "mail.send", "summary": "Send email: INV-101 approved",
                "detail": "To ap@northwind.example: the whole letter", "yes_label": "Approve and send",
                "state": { "state": "pending" }, "confidential": false
            }
        }))
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn agent(id: &str, status: &str, doing: &str) -> AgentView {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "Reconciler", "role": "Finance", "template": "desktop-agent",
            "status": status, "doing": doing, "updated_at": "", "pending": 0,
            "computer": { "state": "ready", "kind": "Linux namespaces", "contained": true }
        }))
        .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn adr0071_the_person_hears_of_a_new_ask_and_a_finish_once() {
        let mut seen = Seen::default();
        // What was already waiting when the app opened isn't news.
        let first = seen.look(
            &[ask("a1", "Reconciler")],
            &[agent("reconciler", "needs_you", "")],
        );
        assert!(first.is_empty());
        // A new ask is, once; never its detail.
        let news = seen.look(
            &[ask("a1", "Reconciler"), ask("a2", "Reconciler")],
            &[agent("reconciler", "needs_you", "")],
        );
        assert_eq!(
            news,
            [News::Asks {
                agent: "Reconciler".into(),
                summary: "Send email: INV-101 approved".into()
            }]
        );
        assert!(!format!("{news:?}").contains("whole letter"));
        assert!(
            seen.look(
                &[ask("a2", "Reconciler")],
                &[agent("reconciler", "working", "")]
            )
            .is_empty()
        );
        // Busy, then idle: done.
        let done = seen.look(&[], &[agent("reconciler", "idle", "Done: Match March")]);
        assert_eq!(
            done,
            [News::Done {
                agent: "Reconciler".into(),
                doing: "Done: Match March".into()
            }]
        );
        assert_eq!(done[0].words().0, "Reconciler is done");
        // Idle to asleep and back isn't.
        assert!(
            seen.look(&[], &[agent("reconciler", "asleep", "")])
                .is_empty()
        );
        assert!(
            seen.look(&[], &[agent("reconciler", "idle", "")])
                .is_empty()
        );
    }

    #[test]
    fn adr0071_the_tray_counts_what_waits() {
        assert_eq!(waiting_words(0), "Nothing is waiting on you");
        assert_eq!(waiting_words(1), "1 question is waiting on you");
        assert_eq!(waiting_words(3), "3 questions are waiting on you");
    }
}
