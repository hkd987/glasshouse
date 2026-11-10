//! Tasks kept on disk, so they outlive the glassd that ran them (ADR 0020,
//! item 3; FD-4).
//!
//! Everything glassd knew of a task was in its memory. Stopping glassd, a
//! crash or a power cut took every task with it, and with them the undo of
//! all they had changed, though the snapshots to undo from were still on
//! disk: `glassctl undo` said "no task". A task's record
//! ([`crate::task::Kept`]) is now written beside its agent's output, as
//! `<state>/tasks/<id>/task.json`, readable by root alone:
//!
//! - within [`KEEP_WITHIN`] of a change it records, so a task cut short
//!   keeps the undo of what it had done;
//! - after an undo, and when the person's own edits are recorded as they
//!   hand back;
//! - when it ends.
//!
//! At start glassd reads the records back as ended tasks
//! ([`Runtime::restore_tasks`]), before anything prunes snapshots: a
//! snapshot is kept for as long as a task in memory could undo from it.
//! One that was still working comes back as failed, saying the system
//! restarted, and its end goes on the audit record, where the task's
//! records used to just stop.
//!
//! A record can be behind its task by the changes of its last second. An
//! undo after a restart knows nothing of such a change and so leaves its
//! files alone: a file the record says the task left one way, and that is
//! another way now, was "changed since" and is never put back over. Being
//! behind only ever leaves more as it is. The audit log has every step.
//!
//! A record goes with its task: `task.dismiss`, the retention of ended
//! tasks, and an app's session that changed nothing remove it.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use glass_audit::EventKind;
use glass_types::TaskId;

use crate::Runtime;
use crate::task::{Kept, RESTARTED, Task};

/// The record's name in the task's folder.
const FILE: &str = "task.json";

/// How long after a change its task's record is on disk, at the latest.
/// One write then covers every change of that second: a task renaming a
/// thousand files rewrites its record a few times, not a thousand.
pub(crate) const KEEP_WITHIN: Duration = Duration::from_secs(1);

/// What reading the records back at start found.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Restored {
    /// Tasks back in memory, ended.
    pub tasks: usize,
    /// Of those, the ones that were still working when glassd stopped.
    pub interrupted: usize,
    /// Records that couldn't be read, or are from a glassd this one doesn't
    /// read the records of. They are left where they are.
    pub unread: usize,
}

impl Runtime {
    fn kept_file(&self, id: TaskId) -> PathBuf {
        self.task_folder(id).join(FILE)
    }

    /// Writes `task`'s record now. A failure is logged and the task goes
    /// on: its undo then doesn't outlive this glassd, as none did before.
    pub(crate) fn keep(&self, task: &Task) {
        if task.gone.load(Ordering::SeqCst) {
            return;
        }
        // An app's session that changed nothing is forgotten as it ends.
        if task.is_app() && task.lock().changes.is_empty() {
            return;
        }
        let path = self.kept_file(task.id);
        if let Err(e) = glass_state::write_json(&path, &task.kept(), glass_state::PRIVATE) {
            tracing::warn!(
                task = %task.id,
                error = %e,
                "a task's record couldn't be written: its undo won't outlive a restart of glassd"
            );
        }
        // Dismissed while this was writing: the record goes with the task.
        if task.gone.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Writes `task`'s record within [`KEEP_WITHIN`], with whatever else
    /// changes until then.
    pub(crate) fn keep_soon(self: &Arc<Self>, task: &Arc<Task>) {
        if task.keeping.swap(true, Ordering::SeqCst) {
            return;
        }
        let (rt, task) = (Arc::clone(self), Arc::clone(task));
        tokio::spawn(async move {
            tokio::time::sleep(KEEP_WITHIN).await;
            // Before the record is read off the task: a change from here on
            // asks for a write of its own.
            task.keeping.store(false, Ordering::SeqCst);
            let _ = tokio::task::spawn_blocking(move || rt.keep(&task)).await;
        });
    }

    /// The task is leaving memory for good: its record goes, and nothing
    /// on its way writes it again.
    pub(crate) fn unkeep(&self, task: &Task) {
        task.gone.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(self.kept_file(task.id));
    }

    /// Reads the tasks kept on disk back into memory as ended tasks. Once,
    /// as glassd starts: before it takes calls, and before snapshots are
    /// pruned.
    pub fn restore_tasks(&self) -> Restored {
        let mut found = Restored::default();
        let Ok(folders) = std::fs::read_dir(self.config.state_dir.join("tasks")) else {
            return found;
        };
        let policy = self.policy();
        for folder in folders.flatten() {
            let path = folder.path().join(FILE);
            let kept = match glass_state::read_json::<Kept>(&path) {
                Ok(Some(kept)) => kept,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "a task's record couldn't be read: the task isn't restored");
                    found.unread += 1;
                    continue;
                }
            };
            // A record is its folder's task's: one copied or moved into
            // another folder is nobody's.
            let named = folder.file_name().to_str() == Some(kept.id.to_string().as_str());
            if !kept.readable() || !named {
                tracing::warn!(
                    path = %path.display(),
                    "a task's record isn't one this glassd reads: the task isn't restored"
                );
                found.unread += 1;
                continue;
            }
            if self.tasks().contains_key(&kept.id) {
                continue;
            }
            let rules = policy.for_ended(&kept.user.name, &kept.entry.id, kept.app.is_some());
            let (task, interrupted) = Task::restored(kept, rules);
            let task = Arc::new(task);
            self.tasks().insert(task.id, Arc::clone(&task));
            found.tasks += 1;
            if interrupted {
                // Its end, on the record: the task's records stopped where
                // glassd did.
                let receipt = task.lock().receipt();
                let mut event = self.task_event(
                    &task,
                    EventKind::TaskFinished,
                    format!("Failed: {RESTARTED}"),
                );
                event.detail = serde_json::json!({
                    "actions": receipt.actions,
                    "changes": receipt.changes,
                    "undoable": receipt.undoable,
                    "left_machine": receipt.left_machine,
                    "flagged": receipt.flagged,
                    "interrupted_by_restart": true,
                });
                self.audit(event);
                self.keep(&task);
                found.interrupted += 1;
            }
        }
        found
    }
}
