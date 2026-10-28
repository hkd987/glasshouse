//! Take over on an ended task: carry on in the user's own harness, such as
//! Claude Code, through `glass-mcp`. An actions agent has no screen to hand
//! over (AW-20, AW-22), so the user gets the task instead: a new task with
//! the same request and scope, attributed to the harness, whose
//! instructions say what the first task did and why it stopped. The harness
//! is an attended agent: glassd starts no program for it, and every call it
//! makes still goes through the action pipeline, approvals, audit and undo.

use std::sync::Arc;

use glass_api::{ActivityKind, TaskRequest, TaskView, TileState};
use glass_types::TaskId;

use crate::error::Result;
use crate::runtime::Origin;
use crate::task::{Continued, Gate, Task};
use crate::{Caller, Runtime, RuntimeError};

/// Steps and changes listed for the harness; the rest are counted.
const LISTED: usize = 20;

impl Runtime {
    /// Continues an ended task in the user's harness (`task.continue`).
    pub fn continue_task(self: &Arc<Self>, caller: &Caller, id: TaskId) -> Result<TaskView> {
        let task = self.authorized(caller, id)?;
        if task.user.uid != caller.uid {
            return Err(RuntimeError::PermissionDenied(format!(
                "only {} can take over their own task",
                task.user.name
            )));
        }
        if task.lock().tile.is_live() {
            return Err(RuntimeError::Conflict(
                "that task is still running; take over pauses it and gives you control".into(),
            ));
        }
        let harness = self.config.harness.clone().ok_or_else(|| {
            RuntimeError::Conflict("this computer has no harness set up for taking over".into())
        })?;
        let request = TaskRequest {
            agent: harness,
            prompt: task.prompt.clone(),
            scope: task.scope.clone(),
            title: Some(task.lock().title.clone()),
            computer: task.sandbox.computer.clone(),
        };
        self.start_task(caller, request, Origin::Continues(continued(&task)))
    }

    /// The user's harness said hello on the task socket.
    pub(crate) fn harness_connected(&self, task: &Task) {
        {
            let mut state = task.lock();
            if !state.tile.is_live() || state.connected {
                return;
            }
            state.connected = true;
            state.caption = if task.entry.driver.is_some() {
                format!("{} started", task.entry.name)
            } else {
                format!("You're driving in {}", task.entry.name)
            };
            state.note(
                ActivityKind::State,
                format!("{} connected", task.entry.name),
            );
        }
        self.state_change(task, &format!("{} connected", task.entry.name));
    }

    /// Hand back on an attended task: the user is done driving, and the task
    /// ends as Done with its receipt.
    pub(crate) fn finish_attended(&self, task: &Task) -> TaskView {
        {
            let mut state = task.lock();
            state.tile = TileState::Done;
            // A driver that answered (ADR 0042) keeps its answer's caption.
            if state.answer.is_none() {
                state.caption = "You finished in your harness".into();
            }
            state.progress = None;
            state.sleeping = None;
            // With the tile, under its lock, as end_task does.
            task.gate.send_replace(Gate::Ended);
        }
        self.desk.end_task(task.id);
        if let Some(stop) = task.stop.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = stop.send(());
        }
        self.state_change(task, "Handed back from the harness");
        task.view(None)
    }
}

/// What the harness is told about the task it carries on from.
fn continued(task: &Task) -> Continued {
    let state = task.lock();
    let ended = match state.tile {
        TileState::Failed => format!(
            "failed: {}",
            state.failure.as_deref().unwrap_or("for an unknown reason")
        ),
        TileState::Stopped => "was stopped".into(),
        _ => "finished".into(),
    };
    let done: Vec<String> = state
        .activity
        .iter()
        .filter(|a| a.kind == ActivityKind::Action)
        .map(|a| a.text.clone())
        .collect();
    let changes: Vec<String> = state
        .changes
        .iter()
        .filter(|c| !c.undone)
        .map(|c| {
            format!(
                "{} ({})",
                c.target,
                if c.reversible {
                    "undoable"
                } else {
                    "can't be undone"
                }
            )
        })
        .collect();
    Continued {
        id: task.id,
        agent: task.entry.name.clone(),
        ended,
        done: listed(done),
        changes: listed(changes),
    }
}

fn listed(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() > LISTED {
        let more = lines.len() - LISTED;
        lines.truncate(LISTED);
        lines.push(format!("…and {more} more"));
    }
    lines
}
