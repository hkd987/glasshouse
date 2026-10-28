//! A computer's folder tree, read for a save and written back by a restore.
//! Every path is resolved beneath the tree's own folder (cap-std, as
//! `openat` does), so nothing outside it is read or written, whatever a
//! symlink in it says.

use std::fs::File;
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;

use cap_std::ambient_authority;
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, Metadata, MetadataExt, OpenOptions, OpenOptionsExt,
};

use super::manifest::{Entry, Kind, Manifest, Stamp};

/// A tree as read.
pub(super) struct Tree {
    /// Its own folder's.
    pub root: Stamp,
    /// A folder before what's in it, names in order.
    pub entries: Vec<Entry>,
    /// What no store holds: sockets, pipes and devices.
    pub skipped: Vec<String>,
}

/// Stores a regular file's content and says its size and chunks.
pub(super) type Chunker<'a> =
    &'a mut dyn FnMut(&mut File, &str) -> Result<(u64, Vec<Option<String>>), String>;

/// Writes a file's chunks into it, opened and empty.
pub(super) type Filler<'a> = &'a dyn Fn(&File, u64, &[Option<String>], &str) -> Result<(), String>;

/// Reads the tree at `dir`, handing each regular file to `chunk`.
pub(super) fn read(dir: &Path, chunk: Chunker<'_>) -> Result<Tree, String> {
    let meta =
        std::fs::symlink_metadata(dir).map_err(|e| format!("can't read {}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} isn't a folder", dir.display()));
    }
    let root = Dir::open_ambient_dir(dir, ambient_authority())
        .map_err(|e| format!("can't read {}: {e}", dir.display()))?;
    let own = root
        .dir_metadata()
        .map_err(|e| format!("can't read {}: {e}", dir.display()))?;
    let mut tree = Tree {
        root: stamp(&own),
        entries: Vec::new(),
        skipped: Vec::new(),
    };
    walk(&root, "", &mut tree, chunk)?;
    Ok(tree)
}

fn walk(dir: &Dir, at: &str, tree: &mut Tree, chunk: Chunker<'_>) -> Result<(), String> {
    let failed = |path: &str, e: io::Error| format!("can't save {}: {e}", shown(path));
    let mut names = Vec::new();
    for entry in dir.entries().map_err(|e| failed(at, e))? {
        let entry = entry.map_err(|e| failed(at, e))?;
        let name = entry.file_name().into_string().map_err(|name| {
            format!(
                "can't save {}: its name isn't UTF-8",
                shown(&join(at, &name.to_string_lossy()))
            )
        })?;
        names.push(name);
    }
    names.sort();
    for name in names {
        let path = join(at, &name);
        let meta = dir.symlink_metadata(&name).map_err(|e| failed(&path, e))?;
        let kind = meta.file_type();
        if kind.is_dir() {
            let child = dir.open_dir(&name).map_err(|e| failed(&path, e))?;
            same(
                &child.dir_metadata().map_err(|e| failed(&path, e))?,
                &meta,
                &path,
            )?;
            tree.entries.push(Entry {
                path: path.clone(),
                stamp: stamp(&meta),
                kind: Kind::Directory,
            });
            walk(&child, &path, tree, &mut *chunk)?;
        } else if kind.is_file() {
            let file = dir.open(&name).map_err(|e| failed(&path, e))?;
            same(
                &file.metadata().map_err(|e| failed(&path, e))?,
                &meta,
                &path,
            )?;
            let (size, chunks) = chunk(&mut file.into_std(), &path)?;
            tree.entries.push(Entry {
                path,
                stamp: stamp(&meta),
                kind: Kind::File { size, chunks },
            });
        } else if kind.is_symlink() {
            let target = dir
                .read_link_contents(&name)
                .map_err(|e| failed(&path, e))?
                .into_os_string()
                .into_string()
                .map_err(|_| format!("can't save {}: its target isn't UTF-8", shown(&path)))?;
            tree.entries.push(Entry {
                path,
                stamp: stamp(&meta),
                kind: Kind::Symlink { target },
            });
        } else {
            tree.skipped.push(path);
        }
    }
    Ok(())
}

/// That what was opened is what was listed, not something swapped in
/// since.
fn same(opened: &Metadata, listed: &Metadata, path: &str) -> Result<(), String> {
    if opened.dev() != listed.dev() || opened.ino() != listed.ino() {
        return Err(format!(
            "can't save {}: it changed while it was being saved",
            shown(path)
        ));
    }
    Ok(())
}

fn stamp(meta: &Metadata) -> Stamp {
    Stamp::new(meta.mode(), meta.mtime(), meta.mtime_nsec())
}

fn join(at: &str, name: &str) -> String {
    if at.is_empty() {
        name.to_owned()
    } else {
        format!("{at}/{name}")
    }
}

fn shown(path: &str) -> &str {
    if path.is_empty() {
        "the tree's folder"
    } else {
        path
    }
}

/// Writes `manifest`'s tree into `dir`, which must be empty or absent,
/// `fill` writing each file's chunks. An absent `dir` is written beside it
/// and renamed into place when whole; an empty one is written into, and
/// emptied again if anything fails.
pub(super) fn restore(manifest: &Manifest, dir: &Path, fill: Filler<'_>) -> Result<(), String> {
    let place = dir.display();
    let root_stamp = manifest
        .root
        .ok_or_else(|| format!("version {} is not a tree", manifest.version))?;
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {
            let root = Dir::open_ambient_dir(dir, ambient_authority())
                .map_err(|e| format!("can't restore into {place}: {e}"))?;
            let mut entries = root
                .entries()
                .map_err(|e| format!("can't restore into {place}: {e}"))?;
            if entries.next().is_some() {
                return Err(format!("can't restore into {place}: it isn't empty"));
            }
            let written = write(&root, manifest, root_stamp, fill);
            if written.is_err() {
                empty(&root);
            }
            written
        }
        Ok(_) => Err(format!("can't restore into {place}: it isn't a folder")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let parent = dir
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let name = dir
                .file_name()
                .ok_or_else(|| format!("can't restore into {place}: it names no folder"))?
                .to_string_lossy();
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("can't restore into {place}: {e}"))?;
            let temp = parent.join(format!(".{name}.restoring-{}", super::unique()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&temp)
                .map_err(|e| format!("can't restore into {place}: {e}"))?;
            let written = Dir::open_ambient_dir(&temp, ambient_authority())
                .map_err(|e| format!("can't restore into {place}: {e}"))
                .and_then(|root| write(&root, manifest, root_stamp, fill))
                .and_then(|()| {
                    std::fs::rename(&temp, dir)
                        .map_err(|e| format!("can't restore into {place}: {e}"))
                });
            if written.is_err() {
                let _ = std::fs::remove_dir_all(&temp);
            }
            written
        }
        Err(e) => Err(format!("can't restore into {place}: {e}")),
    }
}

/// Folders and files first, owner-only until they're whole; then symlinks,
/// so none exists to be followed while anything is written; then folders'
/// times and modes, deepest first, since writing in a folder changes its
/// time and its mode may forbid writing.
fn write(
    root: &Dir,
    manifest: &Manifest,
    root_stamp: Stamp,
    fill: Filler<'_>,
) -> Result<(), String> {
    let failed = |path: &str, e: io::Error| format!("can't restore {}: {e}", shown(path));
    let mut private = DirBuilder::new();
    private.mode(0o700);
    for entry in &manifest.entries {
        let path = entry.path.as_str();
        match &entry.kind {
            Kind::Directory => root
                .create_dir_with(path, &private)
                .map_err(|e| failed(path, e))?,
            Kind::File { size, chunks } => {
                let file = root
                    .open_with(
                        path,
                        OpenOptions::new().write(true).create_new(true).mode(0o600),
                    )
                    .map_err(|e| failed(path, e))?
                    .into_std();
                fill(&file, *size, chunks, path)?;
                file.set_len(*size)
                    .and_then(|()| entry.stamp.apply(&file))
                    .map_err(|e| failed(path, e))?;
            }
            Kind::Symlink { .. } => {}
        }
    }
    for entry in &manifest.entries {
        if let Kind::Symlink { target } = &entry.kind {
            root.symlink_contents(target, &entry.path)
                .map_err(|e| failed(&entry.path, e))?;
        }
    }
    for entry in manifest.entries.iter().rev() {
        if let Kind::Directory = entry.kind {
            stamp_folder(root, &entry.path, entry.stamp).map_err(|e| failed(&entry.path, e))?;
        }
    }
    stamp_folder(root, ".", root_stamp).map_err(|e| failed("", e))
}

/// Sets a folder's time and mode through a descriptor opened for reading:
/// cap-std's own for folders (`O_PATH`) can't change either.
fn stamp_folder(root: &Dir, path: &str, stamp: Stamp) -> io::Result<()> {
    stamp.apply(&root.open(path)?.into_std())
}

/// Removes everything in `root`, as far as it can.
fn empty(root: &Dir) {
    let Ok(entries) = root.entries() else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let _ = if entry.file_type().is_ok_and(|t| t.is_dir()) {
            root.remove_dir_all(&name)
        } else {
            root.remove_file(&name)
        };
    }
}
