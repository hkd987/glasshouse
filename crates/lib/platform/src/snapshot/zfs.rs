//! ZFS snapshots: atomic, instant, and free until data diverges.
//!
//! One snapshot per dataset that holds a root, all taken by a single
//! `zfs snapshot` call so they are atomic together. Old content is read
//! through the hidden `.zfs/snapshot/<name>` directory and changes come from
//! `zfs diff`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glass_types::SnapshotId;

use super::{ChangeKind, FsChange, SnapshotHandle, SnapshotPart, Snapshotter};
use crate::{PlatformError, Result, cmd};

/// Prefix of every snapshot name the daemon creates.
pub const SNAPSHOT_PREFIX: &str = "glass-";

#[derive(Debug, Default)]
pub struct ZfsSnapshotter;

impl ZfsSnapshotter {
    /// The dataset holding `path` and that dataset's mountpoint.
    fn dataset_of(path: &Path) -> Result<(String, PathBuf)> {
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
}

impl Snapshotter for ZfsSnapshotter {
    fn kind(&self) -> &'static str {
        "zfs"
    }

    fn snapshot(&self, id: SnapshotId, roots: &[PathBuf]) -> Result<SnapshotHandle> {
        let snap_name = format!("{SNAPSHOT_PREFIX}{}", id.short());
        let mut by_dataset: BTreeMap<String, (PathBuf, Vec<PathBuf>)> = BTreeMap::new();
        for root in roots {
            let (dataset, mountpoint) = Self::dataset_of(root)?;
            by_dataset
                .entry(dataset)
                .or_insert_with(|| (mountpoint, Vec::new()))
                .1
                .push(root.clone());
        }
        let parts: Vec<SnapshotPart> = by_dataset
            .into_iter()
            .map(|(dataset, (mountpoint, roots))| SnapshotPart {
                base: mountpoint.join(".zfs/snapshot").join(&snap_name),
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
}

/// Parses `zfs diff -FH` output: `<change>\t<type>\t<path>[\t<new path>]`.
/// Directory entries are skipped; undo works on files.
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
