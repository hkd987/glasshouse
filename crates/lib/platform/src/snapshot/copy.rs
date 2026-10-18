//! Snapshots by copying, for development hosts without ZFS. Each root is
//! copied whole, so keep development data small.

use std::fs;
use std::path::{Path, PathBuf};

use glass_types::SnapshotId;

use super::{ChangeKind, FsChange, SnapshotHandle, SnapshotPart, Snapshotter};
use crate::{PlatformError, Result};

#[derive(Debug)]
pub struct CopySnapshotter {
    store: PathBuf,
}

impl CopySnapshotter {
    pub fn new(store: impl Into<PathBuf>) -> Self {
        Self {
            store: store.into(),
        }
    }
}

impl Snapshotter for CopySnapshotter {
    fn kind(&self) -> &'static str {
        "copy"
    }

    fn snapshot(&self, id: SnapshotId, roots: &[PathBuf]) -> Result<SnapshotHandle> {
        let mut parts = Vec::new();
        for (index, root) in roots.iter().enumerate() {
            let base = self.store.join(id.to_string()).join(index.to_string());
            fs::create_dir_all(&base)
                .map_err(|e| PlatformError::io("creating snapshot store", e))?;
            if root.exists() {
                copy_tree(root, &base)?;
            }
            parts.push(SnapshotPart {
                roots: vec![root.clone()],
                mountpoint: root.clone(),
                base,
                name: format!("copy:{id}/{index}"),
            });
        }
        Ok(SnapshotHandle { id, parts })
    }

    fn changes_since(&self, handle: &SnapshotHandle) -> Result<Vec<FsChange>> {
        let mut changes = Vec::new();
        for part in &handle.parts {
            let root = &part.mountpoint;
            let now = list_files(root)?;
            let then = list_files(&part.base)?;
            for rel in &now {
                let current = root.join(rel);
                if then.contains(rel) {
                    let old = part.base.join(rel);
                    if fs::read(&current).ok() != fs::read(&old).ok() {
                        changes.push(FsChange {
                            path: current,
                            kind: ChangeKind::Modified,
                        });
                    }
                } else {
                    changes.push(FsChange {
                        path: current,
                        kind: ChangeKind::Created,
                    });
                }
            }
            for rel in then.iter().filter(|rel| !now.contains(rel)) {
                changes.push(FsChange {
                    path: root.join(rel),
                    kind: ChangeKind::Deleted,
                });
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(changes)
    }

    fn destroy(&self, handle: &SnapshotHandle) -> Result<()> {
        let dir = self.store.join(handle.id.to_string());
        match fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(PlatformError::io(format!("removing {}", dir.display()), e))
            }
            _ => Ok(()),
        }
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for rel in list_files(from)? {
        let target = to.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| PlatformError::io("creating snapshot dir", e))?;
        }
        fs::copy(from.join(&rel), &target)
            .map_err(|e| PlatformError::io("copying into snapshot", e))?;
    }
    Ok(())
}

/// Regular files under `root`, relative to it. Symlinks are not followed.
fn list_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = root.join(&rel);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(PlatformError::io(format!("listing {}", dir.display()), e)),
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let child = rel.join(entry.file_name());
            if kind.is_dir() {
                stack.push(child);
            } else if kind.is_file() {
                out.push(child);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_changes_and_serves_old_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Reports");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("keep.txt"), "same").unwrap();
        fs::write(root.join("edit.txt"), "before").unwrap();
        fs::write(root.join("sub/gone.txt"), "bye").unwrap();

        let snaps = CopySnapshotter::new(dir.path().join("store"));
        let handle = snaps
            .snapshot(SnapshotId::new(), std::slice::from_ref(&root))
            .unwrap();

        fs::write(root.join("edit.txt"), "after").unwrap();
        fs::remove_file(root.join("sub/gone.txt")).unwrap();
        fs::write(root.join("new.txt"), "hi").unwrap();

        let changes = snaps.changes_since(&handle).unwrap();
        let pairs: Vec<(String, ChangeKind)> = changes
            .iter()
            .map(|c| {
                (
                    c.path.strip_prefix(&root).unwrap().display().to_string(),
                    c.kind,
                )
            })
            .collect();
        assert_eq!(
            pairs,
            [
                ("edit.txt".into(), ChangeKind::Modified),
                ("new.txt".into(), ChangeKind::Created),
                ("sub/gone.txt".into(), ChangeKind::Deleted),
            ]
        );

        let old = snaps
            .snapshot_path(&handle, &root.join("edit.txt"))
            .unwrap();
        assert_eq!(fs::read_to_string(old).unwrap(), "before");
        assert!(
            snaps
                .snapshot_path(&handle, &root.join("new.txt"))
                .is_none()
        );
        assert!(
            snaps
                .snapshot_path(&handle, &dir.path().join("elsewhere.txt"))
                .is_none()
        );

        snaps.destroy(&handle).unwrap();
        assert!(
            !dir.path()
                .join("store")
                .join(handle.id.to_string())
                .exists()
        );
    }
}
