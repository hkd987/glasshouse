//! What the web app, the CLI and Teams read, made from what glassd says:
//! the agents on a person's computer, each one's tasks a conversation
//! (ADR 0066, ADR 0068). No I/O here.

use std::collections::BTreeMap;

use glass_agents_api as api;
use glass_api::{
    ActivityKind, AgentSummary, ApprovalView, ComputerState, ComputerView, PersonaView, TaskDetail,
    TaskView, TileState,
};

/// How an answer was given, kept by the front: glassd records that it was,
/// not from where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    pub yes: bool,
    pub by: String,
    pub at: String,
    pub via: api::Channel,
}

/// RFC 3339, as the API carries times.
pub fn time(at: &glass_types::Timestamp) -> String {
    serde_json::to_value(at)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// One of the person's agents, from the computer it works on (theirs) and
/// the tasks it ran. `tasks` are its own, any order.
pub fn agent(
    computer: &ComputerView,
    persona: &PersonaView,
    tasks: &[&TaskView],
) -> api::AgentView {
    let live: Vec<&&TaskView> = tasks.iter().filter(|t| t.state.is_live()).collect();
    let status = if computer.taken_over {
        api::AgentStatus::TakenOver
    } else if computer.state == ComputerState::Asleep {
        api::AgentStatus::Asleep
    } else if live.iter().any(|t| t.state == TileState::NeedsYou) {
        api::AgentStatus::NeedsYou
    } else if live.iter().any(|t| t.state == TileState::Starting) {
        api::AgentStatus::Starting
    } else if !live.is_empty() {
        api::AgentStatus::Working
    } else {
        api::AgentStatus::Idle
    };
    let latest = tasks.iter().max_by_key(|t| t.updated_at.unix_seconds());
    let doing = match (live.first(), latest) {
        (Some(task), _) => task.caption.clone(),
        (None, Some(task)) if task.state == TileState::Done => {
            format!("Done: {}", task.title)
        }
        (None, Some(task)) if task.state == TileState::Failed => {
            format!("Couldn't finish: {}", task.title)
        }
        _ => String::new(),
    };
    api::AgentView {
        id: persona.id.clone(),
        name: persona.name.clone(),
        role: persona.role.clone(),
        template: persona.agent.to_string(),
        status,
        doing,
        updated_at: latest
            .map(|t| time(&t.updated_at))
            .unwrap_or_else(|| time(&persona.created_at)),
        pending: live.iter().filter(|t| t.needs.is_some()).count() as u32,
        computer: computer_view(computer),
    }
}

/// One of the person's schedules (ADR 0069).
pub fn schedule(view: &glass_api::ScheduleView) -> api::Schedule {
    api::Schedule {
        id: view.id.clone(),
        agent: view.persona.clone(),
        prompt: view.prompt.clone(),
        when: view.when.clone(),
        describe: view.describe.clone(),
        zone: view.zone.clone(),
        enabled: view.enabled,
        disabled_why: view.disabled_why.clone(),
        next_run: view.next_run.as_ref().map(time),
        failures: view.failures,
        last: view.last.as_ref().map(|last| api::ScheduleRun {
            at: time(&last.at),
            outcome: last.outcome.clone(),
            task: last.task.map(|t| t.to_string()),
            note: last.note.clone(),
        }),
    }
}

pub fn computer_state(state: ComputerState, live: bool) -> api::ComputerState {
    match state {
        ComputerState::Asleep => api::ComputerState::Asleep,
        ComputerState::Running => api::ComputerState::Running,
        ComputerState::Ready if live => api::ComputerState::Running,
        ComputerState::Ready => api::ComputerState::Ready,
    }
}

fn computer_view(computer: &ComputerView) -> api::ComputerView {
    api::ComputerView {
        state: computer_state(computer.state, !computer.live_tasks.is_empty()),
        kind: computer.kind.clone(),
        contained: computer.contained,
        disk_bytes: computer.disk_bytes,
        stored: computer.stored.as_ref().map(|s| api::Stored {
            bytes: s.bytes,
            at: time(&s.at),
            place: s.place.clone(),
        }),
        display: computer.display,
    }
}

/// A question an agent asked, as people see it.
pub fn approval(view: &ApprovalView, answered: Option<&Answered>) -> api::ApprovalView {
    use glass_api::ApprovalState as S;
    let detail = if view.texts.is_empty() {
        view.items.join("\n")
    } else {
        view.items
            .iter()
            .zip(&view.texts)
            .map(|(item, texts)| {
                if texts.is_empty() {
                    item.clone()
                } else {
                    format!("{item}: {}", texts.join(" "))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let state = match (view.state, answered) {
        (S::Pending, _) => api::ApprovalState::Pending,
        (S::Approved { .. }, Some(a)) => api::ApprovalState::Approved {
            by: a.by.clone(),
            at: a.at.clone(),
            via: a.via,
        },
        (S::Denied, Some(a)) => api::ApprovalState::Denied {
            by: a.by.clone(),
            at: a.at.clone(),
            via: a.via,
        },
        (S::Approved { .. }, None) => api::ApprovalState::Approved {
            by: String::new(),
            at: String::new(),
            via: api::Channel::Desktop,
        },
        (S::Denied, None) => api::ApprovalState::Denied {
            by: String::new(),
            at: String::new(),
            via: api::Channel::Desktop,
        },
        (S::TimedOut | S::Withdrawn, _) => api::ApprovalState::Expired,
    };
    api::ApprovalView {
        id: view.id.to_string(),
        action: view.action.to_string(),
        summary: view.summary.clone(),
        detail,
        yes_label: view.labels.approve.clone(),
        state,
        // Labels on asks come later: until glassd says, nothing is held
        // back from Teams by label (ADR 0065 §13).
        confidential: false,
    }
}

/// An agent's conversation: each task, oldest first, as the person's ask
/// and what came of it.
pub fn messages(
    details: &[TaskDetail],
    answered: &BTreeMap<String, Answered>,
) -> Vec<api::Message> {
    let mut out = Vec::new();
    for detail in details {
        let task = &detail.view;
        let id = task.id.to_string();
        let message = |suffix: &str, at: String, from: api::From, body: api::Body| api::Message {
            id: format!("{id}/{suffix}"),
            at,
            from,
            task: id.clone(),
            body,
        };
        out.push(message(
            "ask",
            time(&task.started_at),
            api::From::Person,
            api::Body::Text {
                text: detail.prompt.clone(),
                files: Vec::new(),
            },
        ));
        // What it did, for people: its model calls are on the record, not
        // here ("Ask Claude Sonnet via Amazon Bedrock: 3 messages, …").
        let steps: Vec<String> = detail
            .activity
            .iter()
            .filter(|a| matches!(a.kind, ActivityKind::Action | ActivityKind::Blocked))
            .filter(|a| !(a.text.starts_with("Ask ") && a.text.contains(" via ")))
            .map(|a| a.text.clone())
            .collect();
        if !steps.is_empty() {
            let at = detail
                .activity
                .last()
                .map(|a| time(&a.at))
                .unwrap_or_else(|| time(&task.updated_at));
            out.push(message(
                "steps",
                at,
                api::From::Agent,
                api::Body::Progress { steps },
            ));
        }
        // How its questions went; the one waiting is its card below.
        for (n, line) in detail
            .activity
            .iter()
            .filter(|a| a.kind == ActivityKind::Approval)
            .filter(|a| !(task.needs.is_some() && a.text.starts_with("Needs you: ")))
            .enumerate()
        {
            out.push(message(
                &format!("asked-{n}"),
                time(&line.at),
                api::From::Agent,
                api::Body::Note {
                    text: line.text.clone(),
                },
            ));
        }
        if let Some(needs) = &task.needs {
            out.push(message(
                "needs",
                time(&needs.requested_at),
                api::From::Agent,
                api::Body::Approval(approval(needs, answered.get(&needs.id.to_string()))),
            ));
        }
        let ended = time(&task.updated_at);
        match task.state {
            TileState::Done => {
                let changed: Vec<String> = detail
                    .changes
                    .iter()
                    .filter(|c| !c.undone)
                    .map(|c| c.target.clone())
                    .collect();
                let undoable = detail.changes.iter().any(|c| c.reversible && !c.undone);
                let text = detail
                    .answer
                    .clone()
                    .filter(|a| !a.trim().is_empty())
                    .unwrap_or_else(|| task.caption.clone());
                out.push(message(
                    "done",
                    ended,
                    api::From::Agent,
                    api::Body::Done {
                        text,
                        changed,
                        undoable,
                    },
                ));
            }
            TileState::Failed => out.push(message(
                "failed",
                ended,
                api::From::Agent,
                api::Body::Note {
                    text: format!(
                        "Couldn't finish: {}",
                        task.failure.as_deref().unwrap_or("it stopped")
                    ),
                },
            )),
            TileState::Stopped => out.push(message(
                "stopped",
                ended,
                api::From::Agent,
                api::Body::Note {
                    text: "Stopped".into(),
                },
            )),
            _ => {}
        }
    }
    out
}

/// Its record: the latest steps on the record across its tasks, newest
/// last.
pub fn record(details: &[TaskDetail], most: usize) -> Vec<api::RecordLine> {
    let mut lines: Vec<(i64, api::RecordLine)> = details
        .iter()
        .flat_map(|d| d.activity.iter())
        .map(|a| {
            (
                a.at.unix_seconds(),
                api::RecordLine {
                    at: time(&a.at),
                    text: a.text.clone(),
                },
            )
        })
        .collect();
    lines.sort_by_key(|(at, _)| *at);
    let skip = lines.len().saturating_sub(most);
    lines.into_iter().skip(skip).map(|(_, l)| l).collect()
}

/// What an agent may reach, said for people: its computer, then what policy
/// lets it do and asks about.
pub fn reach(computer: &ComputerView, card: Option<&glass_api::HandoffCard>) -> Vec<api::Reach> {
    let mut out = vec![
        api::Reach {
            // The person's, which all their agents share (ADR 0068).
            text: "A computer of your own".into(),
            sub: if computer.contained {
                format!(
                    "Shared by your agents: root there, nothing else: {}",
                    computer.kind
                )
            } else {
                "Development only: not contained".into()
            },
            ok: computer.contained,
        },
        api::Reach {
            text: "No internet from your computer".into(),
            sub: "Anything leaving goes through the rules below".into(),
            ok: true,
        },
    ];
    if let Some(card) = card {
        out.extend(card.touches.iter().map(|t| api::Reach {
            text: t.path.display().to_string(),
            sub: t.access.clone(),
            ok: true,
        }));
        out.extend(card.asks_before.iter().map(|a| api::Reach {
            text: format!("Asks you before: {a}"),
            sub: String::new(),
            ok: true,
        }));
        // The first few things it may not do, then how many more: the
        // rest is on the handoff card.
        const SHOWN: usize = 4;
        out.extend(card.cant_touch.iter().take(SHOWN).map(|c| api::Reach {
            text: c.clone(),
            sub: "Not allowed".into(),
            ok: false,
        }));
        if card.cant_touch.len() > SHOWN {
            out.push(api::Reach {
                text: format!(
                    "{} more things it may not do",
                    card.cant_touch.len() - SHOWN
                ),
                sub: "Set by your company's rules".into(),
                ok: false,
            });
        }
    }
    out
}

/// An agent IT approved, as a template to make one from.
pub fn template(agent: &AgentSummary) -> api::Template {
    api::Template {
        id: agent.id.to_string(),
        name: agent.name.clone(),
        team: String::new(),
        does: agent.description.clone(),
        reach: Vec::new(),
    }
}

/// An id for a new agent from its name: `Contract reviewer` →
/// `contract-reviewer`, made unique against `taken`.
pub fn new_id(name: &str, taken: &[String]) -> String {
    let mut base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let mut base = base.trim_matches('-').to_owned();
    base.truncate(32);
    let base = base.trim_end_matches('-');
    let base = if base.is_empty() { "agent" } else { base };
    if !taken.iter().any(|t| t == base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(id))
        .unwrap_or_else(|| base.to_owned())
}

#[cfg(test)]
mod tests {
    use glass_api::{ActivityEntry, Receipt};
    use glass_types::{TaskId, Timestamp};

    use super::*;

    fn persona() -> PersonaView {
        PersonaView {
            id: "reconciler".into(),
            name: "Reconciler".into(),
            role: "Matches invoices".into(),
            agent: "desktop-agent".parse().unwrap(),
            created_at: Timestamp::from_unix_seconds(1_700_000_000).unwrap(),
        }
    }

    fn computer() -> ComputerView {
        ComputerView {
            id: glass_api::PERSON_COMPUTER.into(),
            name: "Your computer".into(),
            role: String::new(),
            agent: "desktop-agent".parse().unwrap(),
            state: ComputerState::Ready,
            kind: "Linux namespaces".into(),
            contained: true,
            disk_bytes: 4096,
            live_tasks: Vec::new(),
            taken_over: false,
            display: false,
            stored: None,
            created_at: Timestamp::from_unix_seconds(1_700_000_000).unwrap(),
            personas: vec![persona()],
        }
    }

    fn task(state: TileState, caption: &str) -> TaskView {
        let at = Timestamp::from_unix_seconds(1_700_000_100).unwrap();
        TaskView {
            id: TaskId::new(),
            agent: "desktop-agent".parse().unwrap(),
            agent_name: "Desktop agent".into(),
            user: "dana".into(),
            mode: glass_api::AgentMode::Actions,
            state,
            title: "March invoices".into(),
            caption: caption.into(),
            progress: None,
            feed: Vec::new(),
            needs: None,
            receipt: Some(Receipt::default()),
            failure: None,
            started_at: at,
            updated_at: at,
            driver_socket: None,
            continues: None,
            computer: Some(glass_api::PERSON_COMPUTER.into()),
            persona: Some("reconciler".into()),
        }
    }

    #[test]
    fn adr0066_an_agents_status_reads_from_its_computer_and_tasks() {
        let idle = agent(&computer(), &persona(), &[]);
        assert_eq!(idle.status, api::AgentStatus::Idle);
        assert_eq!(idle.computer.state, api::ComputerState::Ready);
        assert_eq!(idle.updated_at, "2023-11-14T22:13:20Z");

        let done = task(TileState::Done, "Matched 207 of 214");
        let working = task(TileState::Working, "Matching invoices");
        let view = agent(&computer(), &persona(), &[&done, &working]);
        assert_eq!(view.status, api::AgentStatus::Working);
        assert_eq!(view.doing, "Matching invoices");

        let view = agent(&computer(), &persona(), &[&done]);
        assert_eq!(view.status, api::AgentStatus::Idle);
        assert_eq!(view.doing, "Done: March invoices");

        let mut taken = computer();
        taken.taken_over = true;
        assert_eq!(
            agent(&taken, &persona(), &[&working]).status,
            api::AgentStatus::TakenOver
        );
        let mut asleep = computer();
        asleep.state = ComputerState::Asleep;
        assert_eq!(
            agent(&asleep, &persona(), &[]).status,
            api::AgentStatus::Asleep
        );
    }

    #[test]
    fn adr0066_a_conversation_is_each_ask_and_what_came_of_it() {
        let mut done = task(TileState::Done, "Matched 207 of 214");
        done.updated_at = Timestamp::from_unix_seconds(1_700_000_200).unwrap();
        let detail = TaskDetail {
            view: done.clone(),
            prompt: "Match March's invoices".into(),
            plan: Vec::new(),
            now: String::new(),
            activity: vec![
                ActivityEntry {
                    at: done.started_at,
                    kind: ActivityKind::State,
                    text: "Agent working on its own computer".into(),
                },
                ActivityEntry {
                    at: done.started_at,
                    kind: ActivityKind::Action,
                    text: "Read 214 invoices".into(),
                },
            ],
            changes: Vec::new(),
            cant_touch: Vec::new(),
            snapshots: Vec::new(),
            answer: Some("Seven don't match.".into()),
        };
        let out = messages(std::slice::from_ref(&detail), &BTreeMap::new());
        let kinds: Vec<&str> = out
            .iter()
            .map(|m| m.id.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(kinds, ["ask", "steps", "done"]);
        assert_eq!(out[0].from, api::From::Person);
        assert!(
            matches!(&out[2].body, api::Body::Done { text, undoable: false, .. } if text == "Seven don't match.")
        );

        let lines = record(&[detail], 1);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Read 214 invoices");
    }

    #[test]
    fn adr0066_new_agents_get_ids_from_their_names() {
        assert_eq!(new_id("Contract reviewer", &[]), "contract-reviewer");
        assert_eq!(new_id("  Réconciler!! ", &[]), "r-conciler");
        assert_eq!(new_id("???", &[]), "agent");
        let taken = vec!["reconciler".to_owned(), "reconciler-2".to_owned()];
        assert_eq!(new_id("Reconciler", &taken), "reconciler-3");
        assert!(glass_agents_api::valid_id(&new_id(&"x".repeat(80), &[])));
    }
}
