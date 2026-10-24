//! ZFS snapshots: atomic, instant, and free until data diverges.
//!
//! One snapshot per dataset that holds a root, all taken by a single
//! `zfs snapshot` call so they are atomic together. Old content is read
//! through the hidden `.zfs/snapshot/<name>` directory and changes come from
//! `zfs diff`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use glass_types::SnapshotId;

use super::{ChangeKind, FsChange, SnapshotHandle, SnapshotPart, Snapshotter};
use crate::{PlatformError, Result, cmd};

/// Prefix of every snapshot name the daemon creates.
pub const SNAPSHOT_PREFIX: &str = "glass-";

#[derive(Debug, Default)]
pub struct ZfsSnapshotter {
    /// Which dataset holds each root, so taking a snapshot is one `zfs` call
    /// rather than one per root (every write snapshots first). An entry is
    /// used only while the root is on the same filesystem it was found on.
    datasets: Mutex<HashMap<PathBuf, Dataset>>,
}

#[derive(Debug, Clone)]
struct Dataset {
    name: String,
    mountpoint: PathBuf,
    device: u64,
}

impl ZfsSnapshotter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The dataset holding `path` and that dataset's mountpoint.
    fn dataset_of(&self, path: &Path) -> Result<(String, PathBuf)> {
        let device = std::fs::metadata(path).map(|m| m.dev()).ok();
        let mut known = self.datasets.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(found) = known.get(path)
            && Some(found.device) == device
        {
            return Ok((found.name.clone(), found.mountpoint.clone()));
        }
        let (name, mountpoint) = Self::look_up(path)?;
        if let Some(device) = device {
            known.insert(
                path.to_path_buf(),
                Dataset {
                    name: name.clone(),
                    mountpoint: mountpoint.clone(),
                    device,
                },
            );
        }
        Ok((name, mountpoint))
    }

    fn look_up(path: &Path) -> Result<(String, PathBuf)> {
        let out = cmd::run(
            "zfs",
            &[
                "list",
                "-H",
                "-o",
                "name,mountpoint",
                &path.to_string_lossy(),
            ],
        )
        .map_err(|_| PlatformError::NotSnapshottable {
            path: path.to_path_buf(),
        })?;
        let line = out.lines().next().unwrap_or_default();
        let mut fields = line.split('\t');
        match (fields.next(), fields.next()) {
            (Some(name), Some(mountpoint)) if mountpoint.starts_with('/') => {
                Ok((name.to_owned(), PathBuf::from(mountpoint)))
            }
            _ => Err(PlatformError::NotSnapshottable {
                path: path.to_path_buf(),
            }),
        }
    }

    fn take(&self, snap_name: &str, roots: &[PathBuf]) -> Result<Vec<SnapshotPart>> {
        let mut by_dataset: BTreeMap<String, (PathBuf, Vec<PathBuf>)> = BTreeMap::new();
        for root in roots {
            let (dataset, mountpoint) = self.dataset_of(root)?;
            by_dataset
                .entry(dataset)
                .or_insert_with(|| (mountpoint, Vec::new()))
                .1
                .push(root.clone());
        }
        let parts: Vec<SnapshotPart> = by_dataset
            .into_iter()
            .map(|(dataset, (mountpoint, roots))| SnapshotPart {
                base: mountpoint.join(".zfs/snapshot").join(snap_name),
                name: format!("{dataset}@{snap_name}"),
                mountpoint,
                roots,
            })
            .collect();
        if !parts.is_empty() {
            let mut args = vec!["snapshot"];
            args.extend(parts.iter().map(|p| p.name.as_str()));
            cmd::run("zfs", &args)?;
        }
        Ok(parts)
    }
}

impl Snapshotter for ZfsSnapshotter {
    fn kind(&self) -> &'static str {
        "zfs"
    }

    fn snapshot(&self, id: SnapshotId, roots: &[PathBuf]) -> Result<SnapshotHandle> {
        let snap_name = format!("{SNAPSHOT_PREFIX}{}", id.short());
        let parts = match self.take(&snap_name, roots) {
            Ok(parts) => parts,
            // A dataset renamed under a mounted root keeps its device; look
            // the roots up again once.
            Err(_) => {
                let mut known = self.datasets.lock().unwrap_or_else(|e| e.into_inner());
                for root in roots {
                    known.remove(root);
                }
                drop(known);
                self.take(&snap_name, roots)?
            }
        };
        Ok(SnapshotHandle { id, parts })
    }

    fn changes_since(&self, handle: &SnapshotHandle) -> Result<Vec<FsChange>> {
        let mut changes = Vec::new();
        for part in &handle.parts {
            let out = cmd::run("zfs", &["diff", "-FH", &part.name])?;
            changes.extend(
                parse_diff(&out)
                    .into_iter()
                    .filter(|c| part.covers(&c.path)),
            );
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        changes.dedup();
        Ok(changes)
    }

    fn destroy(&self, handle: &SnapshotHandle) -> Result<()> {
        for part in &handle.parts {
            if !part.name.contains(&format!("@{SNAPSHOT_PREFIX}")) {
                return Err(PlatformError::Unsafe(format!(
                    "{} is not a glasshouse snapshot",
                    part.name
                )));
            }
            cmd::run("zfs", &["destroy", &part.name])?;
        }
        Ok(())
    }

    fn prune(&self, before: SystemTime, keep: &BTreeSet<SnapshotId>) -> Result<usize> {
        let cutoff = before.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let kept: BTreeSet<String> = keep
            .iter()
            .map(|id| format!("{SNAPSHOT_PREFIX}{}", id.short()))
            .collect();
        let out = cmd::run(
            "zfs",
            &["list", "-H", "-p", "-t", "snapshot", "-o", "name,creation"],
        )?;
        let mut gone = 0;
        for (name, created) in ours(&out) {
            let snap = name.split_once('@').map_or("", |(_, s)| s);
            if created < cutoff && !kept.contains(snap) {
                // One that can't go (held, busy) doesn't keep the rest, day
                // after day (ADR 0053).
                match cmd::run("zfs", &["destroy", name]) {
                    Ok(_) => gone += 1,
                    Err(e) => {
                        tracing::warn!(snapshot = name, error = %e, "couldn't remove an old snapshot");
                    }
                }
            }
        }
        Ok(gone)
    }
}

/// Our snapshots in `zfs list -H -p -t snapshot -o name,creation` output:
/// each name and when it was taken (seconds since the epoch).
fn ours(list: &str) -> Vec<(&str, u64)> {
    list.lines()
        .filter_map(|line| {
            let (name, created) = line.split_once('\t')?;
            let snap = name.split_once('@')?.1;
            (snap.starts_with(SNAPSHOT_PREFIX) && !name.contains(char::is_whitespace))
                .then_some(())?;
            Some((name, created.trim().parse().ok()?))
        })
        .collect()
}

/// Parses `zfs diff -FH` output: `<change>\t<type>\t<path>[\t<new path>]`.
/// Directory entries are skipped: callers want files.
fn parse_diff(out: &str) -> Vec<FsChange> {
    let mut changes = Vec::new();
    for line in out.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let (change, kind, path) = match fields.as_slice() {
            [c, k, p, ..] => (*c, *k, unescape(p)),
            _ => continue,
        };
        if kind == "/" {
            continue;
        }
        match change {
            "+" => changes.push(FsChange {
                path: path.into(),
                kind: ChangeKind::Created,
            }),
            "-" => changes.push(FsChange {
                path: path.into(),
                kind: ChangeKind::Deleted,
            }),
            "M" => changes.push(FsChange {
                path: path.into(),
                kind: ChangeKind::Modified,
            }),
            "R" => {
                changes.push(FsChange {
                    path: path.into(),
                    kind: ChangeKind::Deleted,
                });
                if let Some(new) = fields.get(3) {
                    changes.push(FsChange {
                        path: unescape(new).into(),
                        kind: ChangeKind::Created,
                    });
                }
            }
            _ => {}
        }
    }
    changes
}

/// `zfs diff` escapes unprintable bytes and spaces as `\0ooo` (octal).
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = bytes
            .get(i..i + 5)
            .filter(|seq| seq[0] == b'\\' && seq[1] == b'0')
            .and_then(|seq| std::str::from_utf8(&seq[2..]).ok())
            .and_then(|digits| u8::from_str_radix(digits, 8).ok());
        match escaped {
            Some(value) => {
                out.push(value);
                i += 5;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn au20_only_our_snapshots_are_listed_for_pruning() {
        let out = "zroot/home/demo@glass-0ea6688a\t1760900000\n\
                   zroot/home/demo@nightly\t1700000000\n\
                   zroot/home@glass-5d70ab28\t1700000001\n\
                   garbage line\n\
                   zroot/home/demo@glass-x\tnot-a-number\n";
        assert_eq!(
            ours(out),
            [
                ("zroot/home/demo@glass-0ea6688a", 1_760_900_000),
                ("zroot/home@glass-5d70ab28", 1_700_000_001)
            ]
        );
    }

    #[test]
    fn parses_diff_output() {
        let out = "M\t/\t/home/demo/Reports\n\
                   +\tF\t/home/demo/Reports/summary.csv\n\
                   M\tF\t/home/demo/Reports/q3.txt\n\
                   -\tF\t/home/demo/Reports/old.txt\n\
                   R\tF\t/home/demo/Reports/a.txt\t/home/demo/Reports/b.txt\n\
                   +\tF\t/home/demo/Reports/with\\0040space.txt\n";
        let changes = parse_diff(out);
        let pairs: Vec<(String, ChangeKind)> = changes
            .iter()
            .map(|c| (c.path.display().to_string(), c.kind))
            .collect();
        assert_eq!(
            pairs,
            [
                ("/home/demo/Reports/summary.csv".into(), ChangeKind::Created),
                ("/home/demo/Reports/q3.txt".into(), ChangeKind::Modified),
                ("/home/demo/Reports/old.txt".into(), ChangeKind::Deleted),
                ("/home/demo/Reports/a.txt".into(), ChangeKind::Deleted),
                ("/home/demo/Reports/b.txt".into(), ChangeKind::Created),
                (
                    "/home/demo/Reports/with space.txt".into(),
                    ChangeKind::Created
                ),
            ]
        );
    }
}
