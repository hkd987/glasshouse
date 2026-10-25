use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use glass_api::{ApprovalAnswer, ApprovalScope, ApprovalState, ApprovalView, DisplaySafe};
use glass_types::{ActionName, ApprovalId, TaskId, Timestamp};
use tokio::sync::oneshot;

/// How an ask ended, as the waiting call sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Approved(ApprovalScope),
    Denied,
    TimedOut,
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeskError {
    /// Unknown, or not the caller's to answer. The two are indistinguishable
    /// on purpose: users can't probe for other users' approvals.
    #[error("no such pending approval")]
    NotFound,
    #[error("this approval was already answered ({0})")]
    AlreadyAnswered(String),
}

/// Answered approvals kept for display.
const HISTORY: usize = 200;

/// Tasks the desk ended, remembered so that a call on its way as its task
/// ended can't open an ask after (PA-2): such a call is on its way for
/// moments, not for thousands of tasks.
const ENDED_KEPT: usize = 4096;

/// Every open and recently answered approval on the machine.
#[derive(Debug, Default)]
pub struct ApprovalDesk {
    state: Mutex<DeskState>,
}

#[derive(Debug, Default)]
struct DeskState {
    pending: HashMap<ApprovalId, Pending>,
    /// Actions approved "for the rest of this task" (DA-11).
    task_grants: HashMap<TaskId, HashSet<ActionName>>,
    history: VecDeque<ApprovalView>,
    /// Tasks ended here, oldest first (ENDED_KEPT of them).
    ended: HashSet<TaskId>,
    ended_order: VecDeque<TaskId>,
}

#[derive(Debug)]
struct Pending {
    view: ApprovalView,
    owner_uid: u32,
    reply: oneshot::Sender<Outcome>,
}

impl ApprovalDesk {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DeskState> {
        // A poisoned desk only means another thread panicked mid-update; the
        // maps are still structurally valid, so keep serving.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether the user already allowed `action` for the rest of `task`.
    pub fn has_task_grant(&self, task: TaskId, action: &ActionName) -> bool {
        self.lock()
            .task_grants
            .get(&task)
            .is_some_and(|set| set.contains(action))
    }

    /// Opens an ask. Only the user with `owner_uid` may answer it.
    ///
    /// Every view the desk hands out started here, so this is where an
    /// ask's summary and review list (the agent's arguments: recipients,
    /// subjects, file names) become display text that can't forge or hide
    /// part of what the user is approving.
    ///
    /// An ask on a task that has ended is withdrawn at once: its call was on
    /// its way as the task ended (the Approvals model, specs/tla).
    pub fn open(&self, mut view: ApprovalView, owner_uid: u32) -> oneshot::Receiver<Outcome> {
        view.make_display_safe();
        let (reply, rx) = oneshot::channel();
        let pending = Pending {
            view,
            owner_uid,
            reply,
        };
        let mut state = self.lock();
        if state.ended.contains(&pending.view.task) {
            Self::close(
                &mut state,
                pending,
                Outcome::Withdrawn,
                ApprovalState::Withdrawn,
            );
        } else {
            state.pending.insert(pending.view.id, pending);
        }
        rx
    }

    /// Answers an ask on behalf of `caller_uid`.
    pub fn answer(
        &self,
        id: ApprovalId,
        answer: ApprovalAnswer,
        caller_uid: u32,
    ) -> Result<ApprovalView, DeskError> {
        let mut state = self.lock();
        match state.pending.get(&id) {
            Some(p) if p.owner_uid == caller_uid => {}
            Some(_) => return Err(DeskError::NotFound),
            None => {
                return match state.history.iter().find(|v| v.id == id) {
                    Some(v) if state_is_final(&v.state) => {
                        Err(DeskError::AlreadyAnswered(describe(&v.state)))
                    }
                    _ => Err(DeskError::NotFound),
                };
            }
        }
        let pending = state.pending.remove(&id).ok_or(DeskError::NotFound)?;
        let (outcome, new_state) = match answer {
            ApprovalAnswer::Approve { scope } => {
                (Outcome::Approved(scope), ApprovalState::Approved { scope })
            }
            ApprovalAnswer::Deny => (Outcome::Denied, ApprovalState::Denied),
        };
        // What the person says yes to every time is never granted for the
        // rest of the task (ADR 0057).
        if let ApprovalAnswer::Approve {
            scope: ApprovalScope::Task,
        } = answer
            && !glass_policy::every_time(&pending.view.action)
        {
            state
                .task_grants
                .entry(pending.view.task)
                .or_default()
                .insert(pending.view.action.clone());
        }
        Ok(Self::close(&mut state, pending, outcome, new_state))
    }

    /// Marks an ask timed out (DA-13). `None` if it was answered first.
    pub fn expire(&self, id: ApprovalId) -> Option<ApprovalView> {
        let mut state = self.lock();
        let pending = state.pending.remove(&id)?;
        Some(Self::close(
            &mut state,
            pending,
            Outcome::TimedOut,
            ApprovalState::TimedOut,
        ))
    }

    /// Withdraws every open ask on `task` and forgets its task grants. Called
    /// when the task ends, so no permission outlives it (PA-2).
    pub fn end_task(&self, task: TaskId) -> Vec<ApprovalView> {
        let mut state = self.lock();
        state.task_grants.remove(&task);
        if state.ended.insert(task) {
            state.ended_order.push_back(task);
            while state.ended_order.len() > ENDED_KEPT {
                if let Some(old) = state.ended_order.pop_front() {
                    state.ended.remove(&old);
                }
            }
        }
        let ids: Vec<ApprovalId> = state
            .pending
            .values()
            .filter(|p| p.view.task == task)
            .map(|p| p.view.id)
            .collect();
        let mut withdrawn = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(pending) = state.pending.remove(&id) {
                withdrawn.push(Self::close(
                    &mut state,
                    pending,
                    Outcome::Withdrawn,
                    ApprovalState::Withdrawn,
                ));
            }
        }
        withdrawn
    }

    /// Open asks, for one user or (`None`) everyone, oldest first.
    pub fn pending(&self, owner_uid: Option<u32>) -> Vec<ApprovalView> {
        let state = self.lock();
        let mut views: Vec<ApprovalView> = state
            .pending
            .values()
            .filter(|p| owner_uid.is_none_or(|uid| p.owner_uid == uid))
            .map(|p| p.view.clone())
            .collect();
        views.sort_by_key(|v| v.requested_at);
        views
    }

    pub fn pending_for_task(&self, task: TaskId) -> Option<ApprovalView> {
        self.lock()
            .pending
            .values()
            .find(|p| p.view.task == task)
            .map(|p| p.view.clone())
    }

    fn close(
        state: &mut DeskState,
        pending: Pending,
        outcome: Outcome,
        new_state: ApprovalState,
    ) -> ApprovalView {
        let mut view = pending.view;
        view.state = new_state;
        // The waiting call may be gone (task stopped); that's fine.
        let _ = pending.reply.send(outcome);
        state.history.push_back(view.clone());
        while state.history.len() > HISTORY {
            state.history.pop_front();
        }
        view
    }
}

fn state_is_final(state: &ApprovalState) -> bool {
    !matches!(state, ApprovalState::Pending)
}

fn describe(state: &ApprovalState) -> String {
    match state {
        ApprovalState::Pending => "pending".into(),
        ApprovalState::Approved {
            scope: ApprovalScope::Once,
        } => "approved once".into(),
        ApprovalState::Approved {
            scope: ApprovalScope::Task,
        } => "approved for the rest of the task".into(),
        ApprovalState::Denied => "denied".into(),
        ApprovalState::TimedOut => "timed out".into(),
        ApprovalState::Withdrawn => "withdrawn".into(),
    }
}

/// A fresh pending view with the default answer labels, for callers
/// building asks.
pub fn new_view(
    task: TaskId,
    agent: glass_types::AgentId,
    action: ActionName,
    summary: String,
    items: Vec<String>,
    rule: String,
    timeout_secs: u64,
) -> ApprovalView {
    let now = Timestamp::now();
    ApprovalView {
        id: ApprovalId::new(),
        task,
        agent,
        action,
        summary,
        items,
        texts: Vec::new(),
        rule,
        requested_at: now,
        expires_at: Timestamp::from_unix_seconds(now.unix_seconds() + timeout_secs as i64)
            .unwrap_or(now),
        state: ApprovalState::Pending,
        labels: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(task: TaskId) -> ApprovalView {
        new_view(
            task,
            "invoice-demo".parse().unwrap(),
            "mail.send".parse().unwrap(),
            "Send 2 emails".into(),
            vec!["a".into(), "b".into()],
            "org rule mail.send".into(),
            60,
        )
    }

    #[tokio::test]
    async fn approve_once_does_not_grant_the_task() {
        let desk = ApprovalDesk::new();
        let task = TaskId::new();
        let v = view(task);
        let rx = desk.open(v.clone(), 1001);
        let answered = desk
            .answer(
                v.id,
                ApprovalAnswer::Approve {
                    scope: ApprovalScope::Once,
                },
                1001,
            )
            .unwrap();
        assert_eq!(
            answered.state,
            ApprovalState::Approved {
                scope: ApprovalScope::Once
            }
        );
        assert_eq!(rx.await.unwrap(), Outcome::Approved(ApprovalScope::Once));
        assert!(!desk.has_task_grant(task, &v.action));
    }

    /// A kernel patch is a yes every time (ADR 0057): "for the rest of the
    /// task" approves this one only.
    #[tokio::test]
    async fn adr0057_a_kernel_patch_is_never_granted_for_the_task() {
        let desk = ApprovalDesk::new();
        let task = TaskId::new();
        let mut v = view(task);
        v.action = "hardware.patch_propose".parse().unwrap();
        let rx = desk.open(v.clone(), 1001);
        desk.answer(
            v.id,
            ApprovalAnswer::Approve {
                scope: ApprovalScope::Task,
            },
            1001,
        )
        .unwrap();
        assert_eq!(rx.await.unwrap(), Outcome::Approved(ApprovalScope::Task));
        assert!(!desk.has_task_grant(task, &v.action));
    }

    #[tokio::test]
    async fn approve_for_task_grants_until_the_task_ends() {
        let desk = ApprovalDesk::new();
        let task = TaskId::new();
        let v = view(task);
        let _rx = desk.open(v.clone(), 1001);
        desk.answer(
            v.id,
            ApprovalAnswer::Approve {
                scope: ApprovalScope::Task,
            },
            1001,
        )
        .unwrap();
        assert!(desk.has_task_grant(task, &v.action));
        assert!(
            !desk.has_task_grant(TaskId::new(), &v.action),
            "grants never cross tasks"
        );
        desk.end_task(task);
        assert!(
            !desk.has_task_grant(task, &v.action),
            "grants expire with the task"
        );
    }

    #[tokio::test]
    async fn only_the_owner_can_answer_and_only_once() {
        let desk = ApprovalDesk::new();
        let v = view(TaskId::new());
        let _rx = desk.open(v.clone(), 1001);
        assert_eq!(
            desk.answer(v.id, ApprovalAnswer::Deny, 1002),
            Err(DeskError::NotFound)
        );
        assert_eq!(
            desk.answer(v.id, ApprovalAnswer::Deny, 0),
            Err(DeskError::NotFound)
        );
        desk.answer(v.id, ApprovalAnswer::Deny, 1001).unwrap();
        assert!(matches!(
            desk.answer(v.id, ApprovalAnswer::Deny, 1001),
            Err(DeskError::AlreadyAnswered(_))
        ));
    }

    #[tokio::test]
    async fn expiry_and_withdrawal_reach_the_waiting_call() {
        let desk = ApprovalDesk::new();
        let task = TaskId::new();
        let a = view(task);
        let b = view(task);
        let rx_a = desk.open(a.clone(), 1001);
        let rx_b = desk.open(b.clone(), 1001);
        assert_eq!(desk.expire(a.id).unwrap().state, ApprovalState::TimedOut);
        assert_eq!(rx_a.await.unwrap(), Outcome::TimedOut);
        assert!(desk.expire(a.id).is_none());
        assert_eq!(desk.end_task(task).len(), 1);
        assert_eq!(rx_b.await.unwrap(), Outcome::Withdrawn);
        assert!(desk.pending(None).is_empty());
    }

    /// PA-2, found by the Approvals model (specs/tla): a call on its way
    /// when its task ended opens its ask after the desk withdrew the task's
    /// asks. It is withdrawn at once, and no answer can grant the task
    /// anything.
    #[tokio::test]
    async fn pa2_an_ask_opened_after_its_task_ended_is_withdrawn_at_once() {
        let desk = ApprovalDesk::new();
        let task = TaskId::new();
        desk.end_task(task);
        let late = view(task);
        let rx = desk.open(late.clone(), 1001);
        assert!(desk.pending(None).is_empty(), "nothing left open");
        assert_eq!(rx.await.unwrap(), Outcome::Withdrawn);
        assert_eq!(
            desk.answer(
                late.id,
                ApprovalAnswer::Approve {
                    scope: ApprovalScope::Task,
                },
                1001,
            ),
            Err(DeskError::AlreadyAnswered("withdrawn".into()))
        );
        assert!(!desk.has_task_grant(task, &late.action));
        // Other tasks ask as before.
        let other = view(TaskId::new());
        let _rx = desk.open(other.clone(), 1001);
        assert_eq!(desk.pending(None).len(), 1);
    }

    /// What the user approves is what they read (DA-12): an agent's
    /// arguments can't clear the screen, hide a line or reorder an item.
    #[tokio::test]
    async fn da12_asks_show_agent_text_as_plain_lines() {
        let desk = ApprovalDesk::new();
        let task = TaskId::new();
        let mut v = view(task);
        v.summary = "Send 1 email\x1b[2J\x1b[HApproved".into();
        v.items = vec!["To a@example.com\rTo boss@example.com: Raise".into()];
        v.labels.approve = "Approve\u{202E}".into();
        let _rx = desk.open(v.clone(), 1001);
        let shown = desk.pending_for_task(task).unwrap();
        assert_eq!(shown.summary, "Send 1 email\u{FFFD}[2J\u{FFFD}[HApproved");
        assert_eq!(shown.items, ["To a@example.com To boss@example.com: Raise"]);
        assert_eq!(shown.labels.approve, "Approve\u{FFFD}");
        let answered = desk.answer(v.id, ApprovalAnswer::Deny, 1001).unwrap();
        assert_eq!(answered.summary, shown.summary);
    }
}
