//! Point-in-time copies of a task's data, for rollback (PRD "Undo").
//!
//! A snapshot covers a set of roots (the folders a task may change). Undo
//! restores files one by one from where their old content can be read
//! ([`Snapshotter::snapshot_path`]), rather than rolling back whole datasets,
//! and only the files the task changed (ADR 0017). What changed since a
//! snapshot ([`Snapshotter::changes_since`]) is for what the broker didn't
//! see: the user's own edits while driving a task.

mod copy;
mod zfs;

use std::fmt;
use std::path::{Path, PathBuf};

use glass_types::SnapshotId;
use serde::{Deserialize, Serialize};

pub use copy::CopySnapshotter;
pub use zfs::ZfsSnapshotter;

use crate::Result;

/// A taken snapshot. Serializable so the runtime can persist it with the
/// task record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotHandle {
    pub id: SnapshotId,
    pub parts: Vec<SnapshotPart>,
}

/// The piece of a snapshot that covers some roots on one dataset (ZFS) or
/// one copy (development).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPart {
    /// Roots this part covers.
    pub roots: Vec<PathBuf>,
    /// Where `base` is mounted in the live filesystem.
    pub mountpoint: PathBuf,
    /// Directory holding the snapshot's view of `mountpoint`.
    pub base: PathBuf,
    /// Backend-specific name, such as `zroot/home/demo@glass-…`.
    pub name: String,
}

impl SnapshotPart {
    fn covers(&self, path: &Path) -> bool {
        self.roots.iter().any(|r| path.starts_with(r))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
}

/// One file that differs between a snapshot and now. Renames appear as a
/// deletion of the old path and a creation of the new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

pub trait Snapshotter: Send + Sync + fmt::Debug {
    fn kind(&self) -> &'static str;

    /// Snapshots every root, atomically where the backend allows.
    fn snapshot(&self, id: SnapshotId, roots: &[PathBuf]) -> Result<SnapshotHandle>;

    /// Where `path`'s content as of the snapshot can be read, or `None` if it
    /// did not exist then (or is outside the snapshot).
    fn snapshot_path(&self, handle: &SnapshotHandle, path: &Path) -> Option<PathBuf> {
        let part = handle.parts.iter().find(|p| p.covers(path))?;
        let rel = path.strip_prefix(&part.mountpoint).ok()?;
        let candidate = part.base.join(rel);
        candidate.symlink_metadata().is_ok().then_some(candidate)
    }

    /// Files under the snapshot's roots that differ from the snapshot now.
    /// With ZFS this walks every change in the dataset since the snapshot,
    /// so keep it off paths that run often.
    fn changes_since(&self, handle: &SnapshotHandle) -> Result<Vec<FsChange>>;

    fn destroy(&self, handle: &SnapshotHandle) -> Result<()>;
}
