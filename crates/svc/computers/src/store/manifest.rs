//! A version's manifest: every path it holds, with its mode, its time, its
//! kind and its chunks; and the checks a restore makes of one before it
//! writes anything.

use std::collections::HashSet;
use std::fs::{File, Permissions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The manifest format this reads and writes.
pub(super) const FORMAT: u32 = 1;
/// The largest chunk a manifest may say it has.
const MAX_CHUNK: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub format: u32,
    pub computer: String,
    pub version: String,
    /// Every chunk of a file but its last is this long.
    pub chunk_size: u64,
    pub of: Of,
    /// A tree's own folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Stamp>,
    /// A folder comes before what's in it.
    pub entries: Vec<Entry>,
}

/// What a version holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Of {
    /// A folder tree: a computer's root, or its folder of files.
    Tree,
    /// One large file: a disk image or a memory snapshot.
    File,
}

/// A path's mode (permissions with setuid, setgid and sticky) and when its
/// content last changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Stamp {
    pub mode: u32,
    pub mtime: i64,
    pub mtime_nsec: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Entry {
    /// Relative to the tree's folder, `/` between names. A file version's
    /// one entry has the file's own name.
    pub path: String,
    #[serde(flatten)]
    pub stamp: Stamp,
    #[serde(flatten)]
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum Kind {
    Directory,
    /// `chunks` are SHA-256s, in order; `null` is a chunk of zeros, never
    /// stored.
    File {
        size: u64,
        chunks: Vec<Option<String>>,
    },
    Symlink {
        target: String,
    },
}

impl Stamp {
    pub fn new(mode: u32, mtime: i64, mtime_nsec: i64) -> Self {
        Self {
            mode: mode & 0o7777,
            mtime,
            mtime_nsec: u32::try_from(mtime_nsec)
                .ok()
                .filter(|n| *n < 1_000_000_000)
                .unwrap_or(0),
        }
    }

    fn time(&self) -> SystemTime {
        let nanos = Duration::from_nanos(u64::from(self.mtime_nsec));
        match u64::try_from(self.mtime) {
            Ok(secs) => UNIX_EPOCH + Duration::from_secs(secs) + nanos,
            Err(_) => UNIX_EPOCH - Duration::from_secs(self.mtime.unsigned_abs()) + nanos,
        }
    }

    /// Sets an open file's or folder's time, then its mode: a mode can take
    /// away the right to change either.
    pub fn apply(&self, file: &File) -> io::Result<()> {
        file.set_modified(self.time())?;
        file.set_permissions(Permissions::from_mode(self.mode & 0o7777))
    }
}

impl Manifest {
    /// Every chunk it refers to.
    pub fn chunk_names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().flat_map(|entry| match &entry.kind {
            Kind::File { chunks, .. } => chunks.iter().flatten().map(String::as_str).collect(),
            _ => Vec::new(),
        })
    }

    /// Checks all a restore relies on, before it writes anything: that it's
    /// this computer's version, that every path is relative and stays
    /// beneath the folder it's restored into, that every path's folder is
    /// one listed before it (never a symlink), that no path is there twice,
    /// and that every file's chunks add up to its size.
    pub fn check(&self, computer: &str, version: &str) -> Result<(), String> {
        let bad = |why: String| Err(format!("version {version} of {computer} is refused: {why}"));
        if self.format != FORMAT {
            return bad(format!(
                "its format is {}, and this reads {FORMAT}",
                self.format
            ));
        }
        if self.computer != computer || self.version != version {
            return bad("it says it's another computer's, or another version".into());
        }
        if !(1..=MAX_CHUNK).contains(&self.chunk_size) {
            return bad(format!("its chunk size, {}, can't be", self.chunk_size));
        }
        match self.of {
            Of::Tree if self.root.is_none() => {
                return bad("it's a tree without its own folder's mode".into());
            }
            Of::File
                if !matches!(
                    self.entries.as_slice(),
                    [Entry {
                        kind: Kind::File { .. },
                        ..
                    }]
                ) =>
            {
                return bad("a file version holds one file".into());
            }
            _ => {}
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut folders: HashSet<&str> = HashSet::new();
        for entry in &self.entries {
            let path = entry.path.as_str();
            safe_path(path).or_else(bad)?;
            if !seen.insert(path) {
                return bad(format!("{path:?} is in it twice"));
            }
            if let Some((folder, _)) = path.rsplit_once('/')
                && !folders.contains(folder)
            {
                return bad(format!("{path:?} isn't in a folder listed before it"));
            }
            match &entry.kind {
                Kind::Directory => {
                    folders.insert(path);
                }
                Kind::File { size, chunks } => {
                    if chunks.len() as u64 != size.div_ceil(self.chunk_size) {
                        return bad(format!(
                            "{path:?} has {} chunks for {size} bytes",
                            chunks.len()
                        ));
                    }
                    if let Some(name) = chunks.iter().flatten().find(|n| !is_chunk_name(n)) {
                        return bad(format!("{name:?} isn't a chunk's name"));
                    }
                }
                Kind::Symlink { target } => {
                    if target.is_empty() || target.contains('\0') {
                        return bad(format!("{path:?} is a symlink to nothing"));
                    }
                }
            }
        }
        Ok(())
    }
}

/// A path that stays beneath the folder it's taken from: relative, and no
/// part of it empty, `.` or `..`.
pub(super) fn safe_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.starts_with('/') || path.contains('\0') {
        return Err(format!("{path:?} isn't a relative path"));
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!(
            "{path:?} could reach outside the folder it's restored into"
        ));
    }
    Ok(())
}

/// A SHA-256 in lowercase hex.
pub(super) fn is_chunk_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
