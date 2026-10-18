//! Rollback: "Undo all" (AU-21), "Roll back to [time]" (AW-15) and the
//! Changes view's per-change Undo (ADR 0017).
//!
//! Undo puts back the files the task's own changes touched, and nothing else.
//! Every change records its files (`written`), and so do the edits the user
//! made while driving, which hand back turns into changes. For each file, the
//! latest snapshot taken before the first change being undone has it as it
//! was: undo restores that copy, or removes the file if it didn't exist then.
//! A folder a change covered whole (a cache cleanup) is put back file by file.
//!
//! Undo leaves a file as it is, and says so in the report, when it isn't as
//! the task left it (someone changed it since, and their version stays), or
//! when a change this undo keeps touched it after that snapshot. Changes that
//! left the machine are listed, not undone. `dry_run` shows exactly what
//! would change first.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glass_api::{ActivityKind, ChangeEntry, ChangeKind, TileState, UndoReport, UndoRequest};
use glass_audit::EventKind;
use glass_platform::fs::{ConfinedDir, FileKind, Owner, Stat};
use glass_platform::snapshot::{self, SnapshotHandle, Snapshotter};
use glass_types::{ActionName, Timestamp};

use crate::error::Result;
use crate::runtime::display_path;
use crate::task::Task;
use crate::written::{self, Left, Seen, Touched, Written};
use crate::{Caller, Runtime, RuntimeError};

/// The action the user's own edits while driving are recorded under.
const BY_YOU: &str = "you.edit";

/// Why a file was left as it is.
const CHANGED_SINCE: &str = "changed since this task, so it was left as it is";
const KEPT: &str = "also changed by a step this undo keeps, so it was left as it is";
const NOT_A_FILE: &str = "not a regular file, so it was left as it is";
const NO_RECORD: &str = "no record of how this task left it, so it was left as it is";

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

        let Plan {
            since,
            ids,
            items,
            written,
        } = plan(&task, &request)?;
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
        let (mut report, done) = tokio::task::spawn_blocking(move || {
            apply(snapshots.as_ref(), &items, &written, owner, &home, dry_run)
        })
        .await
        .map_err(|e| RuntimeError::Internal(e.to_string()))?;

        let mut not_undone: Vec<String> = task
            .lock()
            .changes
            .iter()
            .filter(|c| !c.reversible && ids.contains(&c.id))
            .map(|c| format!("{} — this left the machine and can't be recalled", c.target))
            .collect();
        not_undone.append(&mut report.not_undone);
        report.not_undone = not_undone;

        if !dry_run {
            {
                let mut state = task.lock();
                // A change stays undoable while any of its files was left
                // as it is.
                let blocked: BTreeSet<u32> = ids
                    .iter()
                    .copied()
                    .filter(|id| {
                        state.written.change(*id).is_some_and(|t| {
                            t.paths
                                .iter()
                                .any(|p| done.left_alone.iter().any(|f| f.starts_with(p)))
                        })
                    })
                    .collect();
                for change in state.changes.iter_mut() {
                    if change.reversible
                        && ids.contains(&change.id)
                        && !blocked.contains(&change.id)
                    {
                        change.undone = true;
                    }
                }
                state.written.note(done.seen);
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

    /// Hand back (AW-21): the files the user changed while driving become
    /// changes of the task, attributed to them and undoable like the agent's.
    /// Returns their `~/…` paths.
    pub(crate) async fn record_edits(
        &self,
        task: &Task,
        takeover: SnapshotHandle,
    ) -> Result<Vec<String>> {
        let snapshots = Arc::clone(&self.snapshots);
        let written = task.lock().written.clone();
        let id = takeover.id;
        let (edits, seen) = tokio::task::spawn_blocking(move || {
            // The user's edits don't go through the broker, so this is the
            // one place undo asks the file system what changed: in the
            // task's writable folders, since take over (ADR 0017).
            let changes = snapshots.changes_since(&takeover).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "can't tell what changed while the user drove");
                Vec::new()
            });
            let roots: Vec<PathBuf> = takeover
                .parts
                .iter()
                .flat_map(|p| p.roots.iter().cloned())
                .collect();
            let mut edits = Vec::new();
            let mut seen = Seen::default();
            for change in changes {
                let saw = written::look(&roots, [change.path.as_path()]);
                // An action already under way at take over is the task's.
                if !written.explains(&saw) {
                    edits.push(change);
                    seen.extend(saw);
                }
            }
            (edits, seen)
        })
        .await
        .map_err(|e| RuntimeError::Internal(e.to_string()))?;

        let action = ActionName::try_from(BY_YOU.to_owned())
            .map_err(|e| RuntimeError::Internal(e.to_string()))?;
        let at = Timestamp::now();
        let mut state = task.lock();
        // Undone, they go back to how they were at take over.
        let before = state
            .snapshots
            .iter()
            .position(|s| s.marker.id == id)
            .map_or(state.snapshots.len(), |i| i + 1);
        let mut shown = Vec::new();
        for edit in edits {
            let path = display_path(&task.user.home, &edit.path);
            let id = state.changes.len() as u32;
            state.changes.push(ChangeEntry {
                id,
                at,
                kind: match edit.kind {
                    snapshot::ChangeKind::Created => ChangeKind::Created,
                    snapshot::ChangeKind::Modified => ChangeKind::Modified,
                    snapshot::ChangeKind::Deleted => ChangeKind::Deleted,
                },
                target: format!("{path} (by you)"),
                action: action.clone(),
                reversible: true,
                paths: vec![path.clone()],
                undone: false,
            });
            let paths = vec![edit.path];
            state.written.touched(id, Touched { paths, before });
            shown.push(path);
        }
        state.written.note(seen);
        Ok(shown)
    }
}

/// What an undo covers.
struct Plan {
    /// The point rolled back to.
    since: Timestamp,
    /// The changes it undoes.
    ids: BTreeSet<u32>,
    items: Vec<Item>,
    written: Written,
}

/// A file or folder a change being undone touched.
struct Item {
    path: PathBuf,
    /// The snapshot that has it as it was, and the root of it holding it.
    from: Option<(SnapshotHandle, PathBuf)>,
    /// A change this undo keeps touched it after that snapshot.
    kept: bool,
}

fn plan(task: &Task, request: &UndoRequest) -> Result<Plan> {
    let state = task.lock();
    let since = match request.to {
        Some(id) => state
            .snapshots
            .iter()
            .find(|s| s.marker.id == id)
            .map(|s| s.marker.at)
            .ok_or_else(|| {
                RuntimeError::NotFound("no such snapshot on this task's timeline".into())
            })?,
        None => state
            .snapshots
            .first()
            .map_or(task.started_at, |s| s.marker.at),
    };
    let ids: BTreeSet<u32> = if request.changes.is_empty() {
        state
            .changes
            .iter()
            .filter(|c| c.at >= since && !c.undone)
            .map(|c| c.id)
            .collect()
    } else {
        if let Some(id) = request
            .changes
            .iter()
            .find(|id| !state.changes.iter().any(|c| c.id == **id))
        {
            return Err(RuntimeError::NotFound(format!(
                "this task has no change {id}"
            )));
        }
        request.changes.iter().copied().collect()
    };
    let touched = |c: &ChangeEntry| state.written.change(c.id).filter(|_| c.reversible);

    // Each path, with the snapshots that existed when the first change
    // being undone touched it.
    let mut first: BTreeMap<&Path, usize> = BTreeMap::new();
    for t in state
        .changes
        .iter()
        .filter(|c| ids.contains(&c.id))
        .filter_map(touched)
    {
        for path in &t.paths {
            first.entry(path).or_insert(t.before);
        }
    }
    let items = first
        .into_iter()
        .map(|(path, before)| {
            let found = state.snapshots[..before.min(state.snapshots.len())]
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, s)| covering_root(&s.handle, path).map(|root| (i, s, root)));
            let kept = found.as_ref().is_some_and(|(i, ..)| {
                state
                    .changes
                    .iter()
                    .filter(|c| !ids.contains(&c.id) && !c.undone)
                    .filter_map(touched)
                    .any(|t| *i < t.before && t.paths.iter().any(|p| overlap(p, path)))
            });
            Item {
                path: path.to_path_buf(),
                from: found.map(|(_, s, root)| (s.handle.clone(), root)),
                kept,
            }
        })
        .collect();
    Ok(Plan {
        since,
        ids,
        items,
        written: state.written.clone(),
    })
}

fn covering_root(handle: &SnapshotHandle, path: &Path) -> Option<PathBuf> {
    let roots: Vec<PathBuf> = handle
        .parts
        .iter()
        .flat_map(|p| p.roots.iter().cloned())
        .collect();
    written::confine(&roots, path).map(|(root, _)| root.to_path_buf())
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// What a real undo changed in the record.
#[derive(Default)]
struct Done {
    seen: Seen,
    left_alone: BTreeSet<PathBuf>,
}

enum Step {
    Nothing,
    Remove,
    Restore { mode: u32 },
    Leave(&'static str),
}

fn apply(
    snapshots: &dyn Snapshotter,
    items: &[Item],
    written: &Written,
    owner: Owner,
    home: &Path,
    dry_run: bool,
) -> (UndoReport, Done) {
    let mut report = UndoReport {
        dry_run,
        ..UndoReport::default()
    };
    let mut done = Done::default();
    let mut leave = |report: &mut UndoReport, file: &Path, why: &str| {
        report
            .not_undone
            .push(format!("{} — {why}", display_path(home, file)));
        done.left_alone.insert(file.to_path_buf());
    };
    let mut handled = BTreeSet::new();
    let mut seen = Seen::default();
    for item in items {
        let opened = match &item.from {
            Some((handle, root)) => open(snapshots, handle, root).map(|dirs| (dirs, root)),
            None => Err("no snapshot has it from before the change".to_owned()),
        };
        let ((live, copy), root) = match opened {
            Ok(opened) => opened,
            Err(why) => {
                leave(&mut report, &item.path, &why);
                continue;
            }
        };
        let Ok(rel) = item.path.strip_prefix(root) else {
            continue;
        };
        let files = match files_under(&live, &copy, rel) {
            Ok(files) => files,
            Err(e) => {
                leave(&mut report, &item.path, &format!("couldn't be undone: {e}"));
                continue;
            }
        };
        for rel in files {
            let file = root.join(&rel);
            if !handled.insert(file.clone()) {
                continue;
            }
            let outcome =
                step(&live, &copy, &rel, written.left(&file), item.kept).and_then(|planned| {
                    match planned {
                        _ if dry_run => Ok(planned),
                        Step::Remove => {
                            live.remove_file(&rel)?;
                            seen.removed(file.clone());
                            Ok(planned)
                        }
                        Step::Restore { mode } => {
                            restore(&live, &copy, &rel, mode, owner, &file, &mut seen)?;
                            Ok(planned)
                        }
                        Step::Nothing | Step::Leave(_) => Ok(planned),
                    }
                });
            let shown = display_path(home, &file);
            match outcome {
                Ok(Step::Nothing) => {}
                Ok(Step::Remove) => report.removed.push(shown),
                Ok(Step::Restore { .. }) => report.restored.push(shown),
                Ok(Step::Leave(why)) => leave(&mut report, &file, why),
                Err(e) => leave(&mut report, &file, &format!("couldn't be undone: {e}")),
            }
        }
    }
    done.seen = seen;
    (report, done)
}

/// The live root, and the snapshot's copy of it.
fn open(
    snapshots: &dyn Snapshotter,
    handle: &SnapshotHandle,
    root: &Path,
) -> std::result::Result<(ConfinedDir, ConfinedDir), String> {
    let copy = snapshots
        .snapshot_path(handle, root)
        .ok_or_else(|| "its folder isn't in the snapshot".to_owned())?;
    let open =
        |path: &Path| ConfinedDir::open(path).map_err(|e| format!("couldn't be undone: {e}"));
    Ok((open(root)?, open(&copy)?))
}

/// The files `rel` stands for: itself, or if it's a folder on either side,
/// everything in it on both.
fn files_under(live: &ConfinedDir, copy: &ConfinedDir, rel: &Path) -> io::Result<Vec<PathBuf>> {
    let is_dir = |dir: &ConfinedDir| {
        dir.stat(rel)
            .map(|s| s.is_some_and(|s| s.kind == FileKind::Dir))
    };
    if !is_dir(copy)? && !is_dir(live)? {
        return Ok(vec![rel.to_path_buf()]);
    }
    let mut files: BTreeSet<PathBuf> = copy.files(rel)?.into_iter().collect();
    files.extend(live.files(rel)?);
    Ok(files.into_iter().collect())
}

/// What undoing `rel` takes, comparing the snapshot's copy (`then`), the
/// file now, and how the task left it.
fn step(
    live: &ConfinedDir,
    copy: &ConfinedDir,
    rel: &Path,
    left: Option<Option<&Left>>,
    kept: bool,
) -> io::Result<Step> {
    let (then, now) = (copy.stat(rel)?, live.stat(rel)?);
    if same(live, copy, rel, then, now)? {
        return Ok(Step::Nothing);
    }
    let as_left = match left {
        None => return Ok(Step::Leave(NO_RECORD)),
        Some(Some(left)) => left.matches(live, rel, now)?,
        Some(None) => now.is_none(),
    };
    if !as_left {
        // New where the task left nothing, and not in the snapshot: it's
        // someone else's and none of undo's business.
        let theirs = then.is_none() && left == Some(None);
        return Ok(if theirs {
            Step::Nothing
        } else {
            Step::Leave(CHANGED_SINCE)
        });
    }
    if kept {
        return Ok(Step::Leave(KEPT));
    }
    let is_file = |s: &Stat| s.kind == FileKind::File;
    Ok(match (then, now) {
        (None, Some(now)) if is_file(&now) => Step::Remove,
        (Some(then), now) if is_file(&then) && now.as_ref().is_none_or(is_file) => {
            Step::Restore { mode: then.mode }
        }
        _ => Step::Leave(NOT_A_FILE),
    })
}

/// Whether the file is the same in the snapshot and now. A file untouched
/// since a ZFS snapshot is the same object there, with the same times, so
/// metadata usually settles it; otherwise the bytes do. (Times alone don't:
/// two writes can land within one clock tick.)
fn same(
    live: &ConfinedDir,
    copy: &ConfinedDir,
    rel: &Path,
    then: Option<Stat>,
    now: Option<Stat>,
) -> io::Result<bool> {
    match (then, now) {
        (None, None) => Ok(true),
        (Some(then), Some(now)) if then.kind == now.kind && then.size == now.size => {
            if (then.ino, then.mtime, then.ctime) == (now.ino, now.mtime, now.ctime) {
                Ok(true)
            } else if then.kind == FileKind::File {
                written::same_content(copy.open_file(rel)?, live.open_file(rel)?)
            } else {
                Ok(false)
            }
        }
        _ => Ok(false),
    }
}

fn restore(
    live: &ConfinedDir,
    copy: &ConfinedDir,
    rel: &Path,
    mode: u32,
    owner: Owner,
    file: &Path,
    seen: &mut Seen,
) -> io::Result<()> {
    let data = copy.read(rel, u64::MAX - 1)?;
    if let Some(parent) = rel.parent().filter(|p| !p.as_os_str().is_empty()) {
        // Folders the rollback brings back belong to the user, not root.
        live.create_dir_all_as(parent, owner)?;
    }
    live.write_as(rel, &data, Some(owner), Some(mode))?;
    if let Some(stat) = live.stat(rel)? {
        seen.restored(file.to_path_buf(), stat, &data);
    }
    Ok(())
}
