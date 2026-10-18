//! Rollback: "Undo all" (AU-21), "Roll back to [time]" (AW-15) and the
//! Changes view's per-change Undo.
//!
//! For every root the task's snapshots cover, take the earliest snapshot at
//! or after the target point; it predates any change the agent made to that
//! root, because a root is always snapshotted before the first change to it.
//! Files that differ from it are put back (or removed, if they were created
//! since). Changes that left the machine are listed, not undone. Undoing
//! single changes does the same for just the files those changes wrote.
//!
//! Limits, stated plainly in every report: rollback covers the task's
//! snapshotted roots, and restores them as they were at the snapshot, so a
//! change the user made in the same folders during the task is rolled back
//! too. `dry_run` shows exactly what would change first.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glass_api::{ActivityKind, TileState, UndoReport, UndoRequest};
use glass_audit::EventKind;
use glass_platform::fs::{ConfinedDir, Owner};
use glass_platform::snapshot::{ChangeKind, SnapshotHandle, Snapshotter};
use glass_types::Timestamp;

use crate::error::Result;
use crate::runtime::display_path;
use crate::task::Task;
use crate::{Caller, Runtime, RuntimeError};

impl Runtime {
    pub async fn undo(
        self: &Arc<Self>,
        caller: &Caller,
        request: UndoRequest,
    ) -> Result<UndoReport> {
        let task = self
            .task(request.task)
            .filter(|t| caller.is_admin() || t.user.uid == caller.uid)
            .ok_or_else(|| RuntimeError::NotFound(format!("no task {}", request.task.short())))?;

        let (since, plan) = rollback_plan(&task, request.to)?;
        let selection = select(&task, &request.changes, since)?;
        if !request.dry_run && matches!(task.lock().tile, TileState::Working | TileState::NeedsYou)
        {
            // Nothing may change underneath a rollback. The task stays held
            // afterwards; the agent restarts only if the user resumes (AW-22).
            self.hold(&task, TileState::Paused, "Paused for rollback");
            self.publish(&task);
        }

        let snapshots = Arc::clone(&self.snapshots);
        let owner = task.ctx.owner;
        let home = task.user.home.clone();
        let dry_run = request.dry_run;
        let only = selection.paths.clone();
        let mut report = tokio::task::spawn_blocking(move || {
            apply(
                snapshots.as_ref(),
                &plan,
                only.as_ref(),
                owner,
                &home,
                dry_run,
            )
        })
        .await
        .map_err(|e| RuntimeError::Internal(e.to_string()))??;

        report.not_undone = task
            .lock()
            .changes
            .iter()
            .filter(|c| !c.reversible && selection.ids.contains(&c.id))
            .map(|c| format!("{} — this left the machine and can't be recalled", c.target))
            .collect();

        if !dry_run {
            {
                let mut state = task.lock();
                for change in state.changes.iter_mut() {
                    if change.reversible && selection.ids.contains(&change.id) {
                        change.undone = true;
                    }
                }
            }
            let what = if request.changes.is_empty() {
                format!("Rolled back to {}", since.clock())
            } else {
                let n = request.changes.len();
                format!("Undid {n} change{}", if n == 1 { "" } else { "s" })
            };
            let summary = format!(
                "{what}: restored {}, removed {}, {} not undoable",
                report.restored.len(),
                report.removed.len(),
                report.not_undone.len()
            );
            let mut event = self.task_event(&task, EventKind::Rollback, summary.clone());
            event.targets = report
                .restored
                .iter()
                .chain(&report.removed)
                .cloned()
                .collect();
            event.snapshot = request.to;
            event.detail = serde_json::json!({
                "changes": request.changes,
                "restored": report.restored,
                "removed": report.removed,
                "not_undone": report.not_undone,
            });
            self.audit(event);
            {
                let mut state = task.lock();
                state.note(ActivityKind::State, summary.clone());
                state.caption = summary;
            }
            self.publish(&task);
        }
        Ok(report)
    }
}

/// The changes a rollback covers, and the files to limit it to.
struct Selection {
    ids: BTreeSet<u32>,
    /// `~/…` paths to restore; `None` restores everything since the point.
    paths: Option<BTreeSet<String>>,
}

fn select(task: &Task, wanted: &[u32], since: Timestamp) -> Result<Selection> {
    let state = task.lock();
    if wanted.is_empty() {
        return Ok(Selection {
            ids: state
                .changes
                .iter()
                .filter(|c| c.at >= since)
                .map(|c| c.id)
                .collect(),
            paths: None,
        });
    }
    let mut selection = Selection {
        ids: BTreeSet::new(),
        paths: Some(BTreeSet::new()),
    };
    for id in wanted {
        let change = state
            .changes
            .iter()
            .find(|c| c.id == *id)
            .ok_or_else(|| RuntimeError::NotFound(format!("this task has no change {id}")))?;
        selection.ids.insert(change.id);
        if change.reversible {
            selection
                .paths
                .get_or_insert_default()
                .extend(change.paths.iter().cloned());
        }
    }
    Ok(selection)
}

/// Which snapshot restores each root, and the point in time rolled back to.
fn rollback_plan(
    task: &Task,
    to: Option<glass_types::SnapshotId>,
) -> Result<(Timestamp, Vec<(PathBuf, SnapshotHandle)>)> {
    let state = task.lock();
    let since = match to {
        Some(id) => state
            .snapshots
            .iter()
            .find(|s| s.marker.id == id)
            .map(|s| s.marker.at)
            .ok_or_else(|| {
                RuntimeError::NotFound("no such snapshot on this task's timeline".into())
            })?,
        None => match state.snapshots.first() {
            Some(first) => first.marker.at,
            None => task.started_at,
        },
    };
    let mut by_root: BTreeMap<PathBuf, SnapshotHandle> = BTreeMap::new();
    for taken in state.snapshots.iter().filter(|s| s.marker.at >= since) {
        for part in &taken.handle.parts {
            for root in &part.roots {
                by_root
                    .entry(root.clone())
                    .or_insert_with(|| taken.handle.clone());
            }
        }
    }
    Ok((since, by_root.into_iter().collect()))
}

fn apply(
    snapshots: &dyn Snapshotter,
    plan: &[(PathBuf, SnapshotHandle)],
    only: Option<&BTreeSet<String>>,
    owner: Owner,
    home: &Path,
    dry_run: bool,
) -> Result<UndoReport> {
    let mut report = UndoReport {
        dry_run,
        ..UndoReport::default()
    };
    let mut diffs: BTreeMap<glass_types::SnapshotId, Vec<glass_platform::snapshot::FsChange>> =
        BTreeMap::new();
    for (root, handle) in plan {
        let changes = match diffs.entry(handle.id) {
            Entry::Occupied(known) => known.into_mut(),
            Entry::Vacant(slot) => slot.insert(
                snapshots
                    .changes_since(handle)
                    .map_err(|e| RuntimeError::Internal(format!("reading changes: {e}")))?,
            ),
        };
        let dir = ConfinedDir::open(root)
            .map_err(|e| RuntimeError::Internal(format!("{}: {e}", root.display())))?;
        for change in changes.iter().filter(|c| c.path.starts_with(root)) {
            let Ok(rel) = change.path.strip_prefix(root) else {
                continue;
            };
            let shown = display_path(home, &change.path);
            // A change's path can be a folder (a cache cleanup): it covers
            // everything under it.
            if only.is_some_and(|paths| {
                !paths
                    .iter()
                    .any(|p| shown == *p || shown.starts_with(&format!("{p}/")))
            }) {
                continue;
            }
            match change.kind {
                ChangeKind::Created => {
                    if !dry_run {
                        dir.remove_file(rel).map_err(|e| {
                            RuntimeError::Internal(format!("removing {shown}: {e}"))
                        })?;
                    }
                    report.removed.push(shown);
                }
                ChangeKind::Modified | ChangeKind::Deleted => {
                    let Some(old) = snapshots.snapshot_path(handle, &change.path) else {
                        continue;
                    };
                    if !dry_run {
                        restore(&dir, rel, &old, owner).map_err(|e| {
                            RuntimeError::Internal(format!("restoring {shown}: {e}"))
                        })?;
                    }
                    report.restored.push(shown);
                }
            }
        }
    }
    Ok(report)
}

fn restore(dir: &ConfinedDir, rel: &Path, old: &Path, owner: Owner) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let data = std::fs::read(old)?;
    let mode = std::fs::metadata(old)?.permissions().mode();
    if let Some(parent) = rel.parent().filter(|p| !p.as_os_str().is_empty()) {
        // Folders the rollback brings back belong to the user, not root.
        dir.create_dir_all_as(parent, owner)?;
    }
    dir.write_as(rel, &data, Some(owner), Some(mode))
        .map(|_| ())
}
