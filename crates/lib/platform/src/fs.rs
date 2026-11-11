//! File access confined to a granted root.
//!
//! Brokers run as root and act on an agent's behalf, while the agent may have
//! planted symlinks or `..` paths to trick them. [`ConfinedDir`] resolves
//! every path inside its root using `cap-std` (openat-style resolution that
//! refuses to escape, the same model Capsicum enforces in the kernel), so a
//! path can never reach outside the grant.

use std::io::{self, Read};
use std::os::unix::fs::{PermissionsExt, fchown};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};

/// Removes `path` and everything under it, however deep it goes. Nothing
/// there is fine; a symlink at `path` is removed, never followed.
///
/// What this removes is often what an agent left behind (a sandbox's root,
/// a computer's folders), so its shape is the agent's to choose. The
/// standard library's `remove_dir_all` is a call deep per folder: a tree of
/// 30,000 nested folders overflows a 2 MiB stack, and a stack overflow ends
/// the whole process, glassd at teardown and again at every start that
/// finds the tree. rm(1) walks without a call per folder (ADR 0007: a
/// base-system command where one does the job).
pub fn remove_tree(path: &Path) -> crate::Result<()> {
    use crate::PlatformError;

    if !path.is_absolute() || path.parent().is_none() {
        return Err(PlatformError::Unsafe(format!(
            "{} is not a folder of ours to remove",
            path.display()
        )));
    }
    match path.symlink_metadata() {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(PlatformError::io(format!("removing {}", path.display()), e));
        }
        Ok(_) => {}
    }
    let Some(text) = path.to_str() else {
        return Err(PlatformError::Unsupported(format!(
            "removing {}: a path that isn't UTF-8",
            path.display()
        )));
    };
    // Longer than other commands get: a tree is as large as it is.
    let limit = std::time::Duration::from_secs(30 * 60);
    crate::cmd::run_within("rm", &["-rf", "--", text], limit).map(drop)
}

/// Who should own files the broker creates for a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// When it last changed, where the file system says.
    pub modified: Option<std::time::SystemTime>,
}

/// What a path names, without following a symlink at its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Dir,
    /// A symlink, socket, device or pipe.
    Other,
}

/// A file's identity and state, enough to tell whether it changed (undo
/// compares what a task left with what is there now). Kept with a task's
/// record (ADR 0020), where it means something only on the file system it
/// was read from: the inode number is part of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stat {
    pub kind: FileKind,
    pub ino: u64,
    pub size: u64,
    pub mode: u32,
    /// Last modification, as seconds and nanoseconds since the epoch.
    pub mtime: (i64, i64),
    /// Last change to the content, owner, mode or links.
    pub ctime: (i64, i64),
}

impl Stat {
    fn of(meta: &Metadata) -> Self {
        let kind = if meta.is_file() {
            FileKind::File
        } else if meta.is_dir() {
            FileKind::Dir
        } else {
            FileKind::Other
        };
        Self {
            kind,
            ino: meta.ino(),
            size: meta.size(),
            mode: meta.mode(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

/// A directory capability: every operation resolves beneath `root`.
#[derive(Debug)]
pub struct ConfinedDir {
    root: PathBuf,
    dir: Dir,
}

impl ConfinedDir {
    pub fn open(root: &Path) -> io::Result<Self> {
        let dir = Dir::open_ambient_dir(root, ambient_authority())?;
        Ok(Self {
            root: root.to_path_buf(),
            dir,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Opens a regular file for reading.
    pub fn open_file(&self, rel: &Path) -> io::Result<std::fs::File> {
        Ok(self.open_regular(relative(rel)?)?.into_std())
    }

    /// Opens `rel` for reading without waiting: a FIFO put where a file was
    /// would otherwise hold the call, and a write's turn, until something
    /// wrote to it (ADR 0052).
    fn open_regular(&self, rel: &Path) -> io::Result<cap_std::fs::File> {
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(libc::O_NONBLOCK);
        let file = self.dir.open_with(rel, &options)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Ok(file)
    }

    /// Reads a whole file, refusing files larger than `max_bytes`.
    pub fn read(&self, rel: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
        let file = self.open_file(rel)?;
        let meta = file.metadata()?;
        if meta.len() > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("{} bytes exceeds the {max_bytes} byte limit", meta.len()),
            ));
        }
        let mut out = Vec::with_capacity(meta.len() as usize);
        file.take(max_bytes + 1).read_to_end(&mut out)?;
        Ok(out)
    }

    /// The JSON file at `rel`, a person's own, which they may have edited:
    /// `None` if there is none.
    ///
    /// One that is there and isn't a `T` (a typo, a file cut short, one
    /// from a newer version) is set aside beside itself as
    /// `<name>.damaged-<unix seconds>`, still theirs, with a warning that
    /// says where, and `None` comes back. The next write then makes a new
    /// file; it used to write over the only copy of what couldn't be
    /// read, and everything else in it was gone without a word. A file
    /// that can't be read at all, or is larger than `max_bytes`, is an
    /// error: starting again over it would be the same loss.
    pub fn read_json_or_set_aside<T: serde::de::DeserializeOwned>(
        &self,
        rel: &Path,
        max_bytes: u64,
    ) -> io::Result<Option<T>> {
        let bytes = match self.read(rel, max_bytes) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let reason = match serde_json::from_slice(&bytes) {
            Ok(value) => return Ok(Some(value)),
            Err(e) => e.to_string(),
        };
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut name = rel.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".damaged-{at}"));
        let aside = rel.with_file_name(name);
        self.rename(rel, &aside)?;
        tracing::warn!(
            file = %self.root.join(rel).display(),
            kept_as = %self.root.join(&aside).display(),
            %reason,
            "a file of a person's choices couldn't be read as what it should be; starting it \
             again and keeping what was there"
        );
        Ok(None)
    }

    /// Writes a file atomically: a temporary file in the same directory,
    /// then a rename over the target. Returns whether the file is new.
    pub fn write(&self, rel: &Path, data: &[u8], owner: Option<Owner>) -> io::Result<bool> {
        self.write_as(rel, data, owner, None)
    }

    /// Like [`write`](Self::write), also setting the permission bits. Owner
    /// and mode are applied to the temporary file before the rename, so the
    /// target never exists with the wrong ones.
    pub fn write_as(
        &self,
        rel: &Path,
        data: &[u8],
        owner: Option<Owner>,
        mode: Option<u32>,
    ) -> io::Result<bool> {
        self.write_labeled(rel, data, owner, mode, None)
    }

    /// Like [`write_as`](Self::write_as), the file labeled at least
    /// `at_least`: what a task writes carries the label of what it read
    /// (DP-11). A higher label already on the file stays.
    pub fn write_labeled(
        &self,
        rel: &Path,
        data: &[u8],
        owner: Option<Owner>,
        mode: Option<u32>,
        at_least: Option<glass_types::Label>,
    ) -> io::Result<bool> {
        self.put(rel, &mut &*data, owner, mode, at_least)
    }

    /// Like [`write_as`](Self::write_as), the content read from `from` as
    /// it is written: a file of any size, never whole in memory. Undo
    /// brings back what was deleted this way, whatever its size.
    pub fn write_from(
        &self,
        rel: &Path,
        from: &mut dyn Read,
        owner: Option<Owner>,
        mode: Option<u32>,
    ) -> io::Result<bool> {
        self.put(rel, from, owner, mode, None)
    }

    /// A file put in place of whatever `rel` named, atomically: what every
    /// write here comes to.
    fn put(
        &self,
        rel: &Path,
        from: &mut dyn Read,
        owner: Option<Owner>,
        mode: Option<u32>,
        at_least: Option<glass_types::Label>,
    ) -> io::Result<bool> {
        let rel = relative(rel)?;
        let meta = self.dir.symlink_metadata(rel).ok();
        let existed = meta.is_some();
        // The rename puts a new file in the old one's place, so what was
        // stored on the old one goes on the new one first: its label
        // (DP-10, ADR 0052), and, unless the caller names one, its mode. A
        // rewrite of a private file (0600) or a script (0755) left it
        // 0644: readable by everyone on the computer, or not runnable.
        let (kept, was) = match &meta {
            Some(meta) if meta.is_file() => (
                crate::xattr::read_label_of(&self.open_regular(rel)?.into_std())?,
                Some(meta.mode() & 0o7777),
            ),
            _ => (None, None),
        };
        let label = kept.max(at_least.filter(|l| *l > glass_types::Label::Public));
        let tmp = self.stage(rel, from, owner, mode.or(was), label)?;
        let result = self.dir.rename(&tmp, &self.dir, rel);
        if result.is_err() {
            let _ = self.dir.remove_file(&tmp);
        }
        result.map(|()| !existed)
    }

    /// Writes a new file atomically, failing with `AlreadyExists` instead of
    /// replacing anything at `rel`, so concurrent writers can't overwrite
    /// each other's files: the temporary file is linked into place, which
    /// fails if the name is taken.
    pub fn write_new(&self, rel: &Path, data: &[u8], owner: Option<Owner>) -> io::Result<()> {
        let rel = relative(rel)?;
        let tmp = self.stage(rel, &mut &*data, owner, None, None)?;
        let result = self.dir.hard_link(&tmp, &self.dir, rel);
        let _ = self.dir.remove_file(&tmp);
        result
    }

    /// A temporary file beside `rel` holding what `from` gives, synced,
    /// with its owner, mode and label set. Its name is unique within this
    /// process.
    fn stage(
        &self,
        rel: &Path,
        from: &mut dyn Read,
        owner: Option<Owner>,
        mode: Option<u32>,
        label: Option<glass_types::Label>,
    ) -> io::Result<PathBuf> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = rel
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path names no file"))?;
        let parent = rel.parent().filter(|p| !p.as_os_str().is_empty());
        let tmp_name = format!(
            ".{}.glass-tmp-{}-{}",
            name.to_string_lossy(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let tmp = match parent {
            Some(p) => p.join(&tmp_name),
            None => PathBuf::from(&tmp_name),
        };
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = self.dir.open_with(&tmp, &options)?;
        let result = (|| {
            io::copy(from, &mut file)?;
            file.sync_all()?;
            let std_file = file.into_std();
            if let Some(owner) = owner {
                fchown(&std_file, Some(owner.uid), Some(owner.gid))?;
            }
            if let Some(mode) = mode {
                std_file.set_permissions(std::fs::Permissions::from_mode(mode & 0o7777))?;
            }
            if let Some(label) = label {
                crate::xattr::write_label_on(&std_file, label)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(tmp),
            Err(e) => {
                let _ = self.dir.remove_file(&tmp);
                Err(e)
            }
        }
    }

    pub fn list(&self, rel: &Path) -> io::Result<Vec<EntryInfo>> {
        let rel = relative_or_root(rel)?;
        let mut entries = Vec::new();
        let iter = if rel.as_os_str().is_empty() {
            self.dir.entries()?
        } else {
            self.dir.read_dir(rel)?
        };
        for entry in iter {
            let entry = entry?;
            let meta = entry.metadata()?;
            entries.push(EntryInfo {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: meta.is_dir(),
                size: meta.len(),
                modified: meta.modified().ok().map(|t| t.into_std()),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    pub fn remove_file(&self, rel: &Path) -> io::Result<()> {
        self.dir.remove_file(relative(rel)?)
    }

    pub fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.dir.rename(relative(from)?, &self.dir, relative(to)?)
    }

    /// Folders made as whoever this process is. For this crate's tests:
    /// a broker is root, and what it makes for a person is theirs
    /// ([`create_dir_all_as`](Self::create_dir_all_as)).
    #[cfg(test)]
    fn create_dir_all(&self, rel: &Path) -> io::Result<()> {
        self.dir.create_dir_all(relative(rel)?)
    }

    /// Like [`create_dir_all`](Self::create_dir_all), but every directory
    /// it creates belongs to `owner`, so a broker running as root never
    /// leaves the user a folder they can't change.
    pub fn create_dir_all_as(&self, rel: &Path, owner: Owner) -> io::Result<()> {
        let mut prefix = PathBuf::new();
        for part in relative(rel)?.components() {
            prefix.push(part);
            if self.dir.symlink_metadata(&prefix).is_ok() {
                continue;
            }
            match self.dir.create_dir(&prefix) {
                Ok(()) => {}
                // Another task made it between the check and here; it is
                // left as it is, like one that was already there.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
            // A plain read-only handle: cap-std's directory handles can be
            // O_PATH on Linux, which fchown refuses.
            let created = self
                .dir
                .open_with(&prefix, OpenOptions::new().read(true))?
                .into_std();
            fchown(&created, Some(owner.uid), Some(owner.gid))?;
        }
        Ok(())
    }

    /// What is at `rel` (the root when empty), without following a symlink
    /// at the end; `None` if nothing is.
    pub fn stat(&self, rel: &Path) -> io::Result<Option<Stat>> {
        let rel = relative_or_root(rel)?;
        let meta = if rel.as_os_str().is_empty() {
            self.dir.dir_metadata()
        } else {
            self.dir.symlink_metadata(rel)
        };
        match meta {
            Ok(meta) => Ok(Some(Stat::of(&meta))),
            Err(e) if is_missing(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Everything under `rel` (the root when empty) that isn't a folder,
    /// relative to the root, without following symlinks. Empty if `rel`
    /// isn't a folder.
    pub fn files(&self, rel: &Path) -> io::Result<Vec<PathBuf>> {
        let entries = match self.list(rel) {
            Ok(entries) => entries,
            Err(e) if is_missing(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in entries {
            let child = rel.join(&entry.name);
            if entry.is_dir {
                out.extend(self.files(&child)?);
            } else {
                out.push(child);
            }
        }
        Ok(out)
    }

    pub fn exists(&self, rel: &Path) -> bool {
        relative(rel).is_ok_and(|rel| self.dir.symlink_metadata(rel).is_ok())
    }

    /// Whether `rel` is a directory itself, not a symlink to one.
    pub fn is_real_dir(&self, rel: &Path) -> bool {
        relative(rel).is_ok_and(|rel| {
            self.dir
                .symlink_metadata(rel)
                .is_ok_and(|m| m.is_dir() && !m.is_symlink())
        })
    }

    /// Removes a directory and everything in it. Symlinks inside are removed,
    /// never followed.
    pub fn remove_dir_all(&self, rel: &Path) -> io::Result<()> {
        self.dir.remove_dir_all(relative(rel)?)
    }

    /// Bytes and files under `rel` (the root when empty), without following
    /// symlinks.
    pub fn disk_usage(&self, rel: &Path) -> io::Result<(u64, u64)> {
        let (mut bytes, mut files) = (0, 0);
        for entry in self.list(rel)? {
            let child = rel.join(&entry.name);
            if entry.is_dir {
                let (b, f) = self.disk_usage(&child)?;
                bytes += b;
                files += f;
            } else {
                bytes += entry.size;
                files += 1;
            }
        }
        Ok((bytes, files))
    }
}

/// Nothing there, or a file where a folder was expected on the way.
fn is_missing(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Rejects absolute paths and empty paths up front with a clear error;
/// `cap-std` independently refuses anything that escapes the root.
fn relative(rel: &Path) -> io::Result<&Path> {
    let rel = relative_or_root(rel)?;
    if rel.as_os_str().is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty path"));
    }
    Ok(rel)
}

fn relative_or_root(rel: &Path) -> io::Result<&Path> {
    if rel.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must be relative to the grant",
        ));
    }
    Ok(rel)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    /// `depth` folders, each inside the last, under `top`: made a handle at
    /// a time, since the whole path is longer than a path may be.
    fn nest(top: &Path, depth: usize) {
        std::fs::create_dir(top).unwrap();
        let mut dir = Dir::open_ambient_dir(top, ambient_authority()).unwrap();
        for _ in 0..depth {
            dir.create_dir("d").unwrap();
            dir = dir.open_dir("d").unwrap();
        }
        dir.write("at-the-bottom", b"x").unwrap();
    }

    // Audit 4.3: a file of choices that didn't parse was read as no
    // choices, and the next save wrote over it.
    #[test]
    fn a_persons_file_that_isnt_what_it_should_be_is_set_aside_not_read_as_empty() {
        #[derive(Debug, PartialEq, serde::Deserialize)]
        struct Mine {
            routines: std::collections::BTreeMap<String, u32>,
        }
        let home = tempfile::tempdir().unwrap();
        let dir = ConfinedDir::open(home.path()).unwrap();
        let rel = Path::new("share/routines.json");
        let read = || dir.read_json_or_set_aside::<Mine>(rel, 4096);
        // None yet.
        assert_eq!(read().unwrap(), None);
        std::fs::create_dir(home.path().join("share")).unwrap();
        let file = home.path().join(rel);
        std::fs::write(&file, br#"{"routines":{"standup":3}}"#).unwrap();
        assert_eq!(read().unwrap().unwrap().routines["standup"], 3);

        // Edited by hand, with a comma too many.
        let typo = br#"{"routines":{"standup":3,}}"#;
        std::fs::write(&file, typo).unwrap();
        assert_eq!(read().unwrap(), None);
        assert!(!file.exists(), "out of the way of the next write");
        let aside: Vec<_> = std::fs::read_dir(home.path().join("share"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(aside.len(), 1);
        let name = aside[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("routines.json.damaged-"), "{name}");
        assert_eq!(std::fs::read(&aside[0]).unwrap(), typo);
        // Asked again, there is none: a new one can be written.
        assert_eq!(read().unwrap(), None);

        // There, and not readable as a file at all, or too large to be
        // one of these: an error, never "none", so nothing writes over it.
        std::fs::create_dir(&file).unwrap();
        assert!(read().is_err());
        std::fs::remove_dir(&file).unwrap();
        std::fs::write(&file, vec![b' '; 5000]).unwrap();
        assert!(read().is_err());
        assert_eq!(std::fs::read(&file).unwrap().len(), 5000);
        // Nor does it follow a link out of the person's home.
        std::fs::remove_file(&file).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("other.json"), b"{").unwrap();
        symlink(outside.path().join("other.json"), &file).unwrap();
        assert!(read().is_err());
        assert_eq!(
            std::fs::read(outside.path().join("other.json")).unwrap(),
            b"{"
        );
    }

    #[test]
    fn pa2_a_tree_as_deep_as_an_agent_can_make_it_is_removed() {
        // What an agent leaves in /work with mkdir and cd in a loop. The
        // standard library removes a folder a call deep: 30,000 of them
        // overflowed this thread's 2 MiB stack, which ends the process.
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("sandbox");
        nest(&root, 30_000);
        remove_tree(&root).unwrap();
        assert!(!root.exists());
        // Nothing there is fine, as at a second teardown.
        remove_tree(&root).unwrap();
    }

    #[test]
    fn pa2_removing_a_tree_never_follows_a_link_out_of_it() {
        let outer = tempfile::tempdir().unwrap();
        let kept = outer.path().join("kept");
        std::fs::create_dir(&kept).unwrap();
        std::fs::write(kept.join("file"), "stays").unwrap();
        let root = outer.path().join("sandbox");
        std::fs::create_dir_all(root.join("work")).unwrap();
        symlink(&kept, root.join("work/out")).unwrap();
        remove_tree(&root).unwrap();
        assert!(!root.exists());
        assert_eq!(std::fs::read_to_string(kept.join("file")).unwrap(), "stays");
        // A link in the tree's own place is removed, not what it names.
        let link = outer.path().join("link");
        symlink(&kept, &link).unwrap();
        remove_tree(&link).unwrap();
        assert!(link.symlink_metadata().is_err());
        assert!(kept.join("file").exists());
        // Only a whole path of ours: never one that depends on where
        // glassd happens to be, and never the root.
        for refused in ["sandbox", "", "/"] {
            assert!(
                matches!(
                    remove_tree(Path::new(refused)),
                    Err(crate::PlatformError::Unsafe(_))
                ),
                "{refused:?}"
            );
        }
    }

    fn setup() -> (tempfile::TempDir, ConfinedDir) {
        let outer = tempfile::tempdir().unwrap();
        std::fs::create_dir(outer.path().join("grant")).unwrap();
        std::fs::write(outer.path().join("secret.txt"), "top secret").unwrap();
        let dir = ConfinedDir::open(&outer.path().join("grant")).unwrap();
        (outer, dir)
    }

    #[test]
    fn dp10_a_rewrite_keeps_the_label_stored_on_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = ConfinedDir::open(tmp.path()).unwrap();
        dir.write(Path::new("q3.csv"), b"revenue\n", None).unwrap();
        let path = tmp.path().join("q3.csv");
        if crate::xattr::write_label(&path, glass_types::Label::Confidential).is_err() {
            return; // no user attributes on this filesystem
        }
        dir.write(Path::new("q3.csv"), b"revenue, revised\n", None)
            .unwrap();
        assert_eq!(
            crate::xattr::read_label(&path).unwrap(),
            Some(glass_types::Label::Confidential),
            "a rewrite dropped the file's label"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"revenue, revised\n");
    }

    #[test]
    fn dp11_a_write_carries_the_label_of_what_its_task_read() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = ConfinedDir::open(tmp.path()).unwrap();
        let path = tmp.path().join("summary.txt");
        let internal = Some(glass_types::Label::Internal);
        dir.write_labeled(Path::new("summary.txt"), b"Q3 up\n", None, None, internal)
            .unwrap();
        let Ok(label) = crate::xattr::read_label(&path) else {
            return; // no user attributes on this filesystem
        };
        assert_eq!(label, internal);
        // A public task's rewrite keeps it; a higher one raises it.
        dir.write_labeled(
            Path::new("summary.txt"),
            b"Q3 up 4%\n",
            None,
            None,
            Some(glass_types::Label::Public),
        )
        .unwrap();
        assert_eq!(crate::xattr::read_label(&path).unwrap(), internal);
        let confidential = Some(glass_types::Label::Confidential);
        dir.write_labeled(
            Path::new("summary.txt"),
            b"Q3 up 4%\n",
            None,
            None,
            confidential,
        )
        .unwrap();
        assert_eq!(crate::xattr::read_label(&path).unwrap(), confidential);
        // Public writes no label.
        dir.write_labeled(
            Path::new("plain.txt"),
            b"hi\n",
            None,
            None,
            Some(glass_types::Label::Public),
        )
        .unwrap();
        assert_eq!(
            crate::xattr::read_label(&tmp.path().join("plain.txt")).unwrap(),
            None
        );
    }

    #[test]
    fn a_fifo_where_a_file_was_is_refused_without_waiting() {
        let (outer, dir) = setup();
        let made = std::process::Command::new("mkfifo")
            .arg(outer.path().join("grant/pipe"))
            .status()
            .unwrap();
        assert!(made.success());
        let err = dir.open_file(Path::new("pipe")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(dir.read(Path::new("pipe"), 1024).is_err());
    }

    #[test]
    fn write_new_never_replaces_and_leaves_no_temporary_files() {
        let (_outer, dir) = setup();
        dir.write_new(Path::new("draft.eml"), b"first", None)
            .unwrap();
        let err = dir
            .write_new(Path::new("draft.eml"), b"second", None)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(dir.read(Path::new("draft.eml"), 100).unwrap(), b"first");
        let names: Vec<_> = dir
            .list(Path::new(""))
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["draft.eml"], "no staging files left");
    }

    #[test]
    fn measures_and_removes_trees_without_following_symlinks() {
        let (outer, dir) = setup();
        dir.create_dir_all(Path::new("cache/a")).unwrap();
        dir.write(Path::new("cache/a/one.bin"), &[0; 100], None)
            .unwrap();
        dir.write(Path::new("cache/two.bin"), &[0; 50], None)
            .unwrap();
        symlink(
            outer.path().join("secret.txt"),
            outer.path().join("grant/cache/link"),
        )
        .unwrap();
        symlink(
            outer.path().join("grant/cache"),
            outer.path().join("grant/alias"),
        )
        .unwrap();
        assert!(dir.is_real_dir(Path::new("cache")));
        assert!(
            !dir.is_real_dir(Path::new("alias")),
            "a symlink is not a real dir"
        );
        let (bytes, files) = dir.disk_usage(Path::new("cache")).unwrap();
        assert_eq!(files, 3);
        assert!(bytes >= 150);
        dir.remove_dir_all(Path::new("cache/a")).unwrap();
        dir.remove_file(Path::new("cache/link")).unwrap();
        assert!(
            outer.path().join("secret.txt").exists(),
            "the link target stays"
        );
        assert!(!dir.exists(Path::new("cache/a")));
    }

    #[test]
    fn directories_created_for_a_user_belong_to_them() {
        use std::os::unix::fs::MetadataExt;
        let (outer, dir) = setup();
        let me = std::fs::metadata(outer.path()).unwrap();
        let owner = Owner {
            uid: me.uid(),
            gid: me.gid(),
        };
        dir.create_dir_all_as(Path::new("a/b/c"), owner).unwrap();
        for d in ["a", "a/b", "a/b/c"] {
            let meta = std::fs::metadata(outer.path().join("grant").join(d)).unwrap();
            assert_eq!((meta.uid(), meta.gid()), (owner.uid, owner.gid), "{d}");
        }
        dir.create_dir_all_as(Path::new("a/b/c"), owner).unwrap();
    }

    #[test]
    fn writes_reads_lists_and_removes() {
        let (_outer, dir) = setup();
        assert!(dir.write(Path::new("a.txt"), b"hello", None).unwrap());
        assert!(!dir.write(Path::new("a.txt"), b"hello again", None).unwrap());
        assert_eq!(dir.read(Path::new("a.txt"), 1024).unwrap(), b"hello again");
        dir.create_dir_all(Path::new("sub/deeper")).unwrap();
        dir.write(Path::new("sub/deeper/b.txt"), b"b", None)
            .unwrap();
        let names: Vec<String> = dir
            .list(Path::new(""))
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["a.txt", "sub"]);
        dir.rename(Path::new("a.txt"), Path::new("sub/a.txt"))
            .unwrap();
        dir.remove_file(Path::new("sub/a.txt")).unwrap();
        assert!(!dir.exists(Path::new("sub/a.txt")));
    }

    #[test]
    fn dot_dot_and_absolute_paths_cannot_escape() {
        let (_outer, dir) = setup();
        assert!(dir.read(Path::new("../secret.txt"), 1024).is_err());
        assert!(dir.read(Path::new("/etc/passwd"), 1024).is_err());
        assert!(dir.write(Path::new("../planted.txt"), b"x", None).is_err());
    }

    #[test]
    fn symlinks_cannot_escape() {
        let (outer, dir) = setup();
        symlink(
            outer.path().join("secret.txt"),
            outer.path().join("grant/link"),
        )
        .unwrap();
        symlink("../secret.txt", outer.path().join("grant/rel-link")).unwrap();
        assert!(dir.read(Path::new("link"), 1024).is_err());
        assert!(dir.read(Path::new("rel-link"), 1024).is_err());
        // Writing through the link must not clobber the outside file.
        let _ = dir.write(Path::new("link"), b"pwned", None);
        assert_eq!(
            std::fs::read_to_string(outer.path().join("secret.txt")).unwrap(),
            "top secret"
        );
    }

    #[test]
    fn write_as_sets_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (outer, dir) = setup();
        dir.write_as(Path::new("m.txt"), b"x", None, Some(0o600))
            .unwrap();
        let mode = std::fs::metadata(outer.path().join("grant/m.txt"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn a_rewrite_keeps_the_files_mode_unless_one_is_named() {
        use std::os::unix::fs::PermissionsExt;
        let (outer, dir) = setup();
        let grant = outer.path().join("grant");
        // A private file and a script, as a person keeps them.
        for (name, mode) in [("private.txt", 0o600), ("run.sh", 0o755)] {
            std::fs::write(grant.join(name), "before").unwrap();
            std::fs::set_permissions(grant.join(name), std::fs::Permissions::from_mode(mode))
                .unwrap();
            // Rewritten as an agent's files.write does: no mode named. It
            // came out 0644, readable by everyone and not runnable.
            assert!(!dir.write(Path::new(name), b"after", None).unwrap());
            assert_eq!(std::fs::read(grant.join(name)).unwrap(), b"after");
            assert_eq!(mode_of(&grant.join(name)), mode, "{name}");
        }
        // A mode that is named wins, as when undo puts a file back.
        dir.write_as(Path::new("private.txt"), b"x", None, Some(0o640))
            .unwrap();
        assert_eq!(mode_of(&grant.join("private.txt")), 0o640);
    }

    #[test]
    fn a_file_is_written_from_a_reader_without_holding_it_whole() {
        let (outer, dir) = setup();
        // Larger than any buffer on the way, and not all the same.
        let content: Vec<u8> = (0..3_000_000u32).map(|n| (n % 251) as u8).collect();
        let created = dir
            .write_from(Path::new("big.bin"), &mut &content[..], None, Some(0o600))
            .unwrap();
        assert!(created);
        let path = outer.path().join("grant/big.bin");
        assert_eq!(std::fs::read(&path).unwrap(), content);
        assert_eq!(mode_of(&path), 0o600);
        // In place of what was there, and nothing left beside it.
        assert!(
            !dir.write_from(Path::new("big.bin"), &mut &b"small"[..], None, None)
                .unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"small");
        assert_eq!(mode_of(&path), 0o600, "its mode stays");
        let names: Vec<String> = dir
            .list(Path::new(""))
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["big.bin"]);
    }

    #[test]
    fn stats_and_lists_files_without_following_symlinks() {
        let (outer, dir) = setup();
        dir.create_dir_all(Path::new("a/b")).unwrap();
        dir.write(Path::new("a/b/one.txt"), b"one", None).unwrap();
        dir.write(Path::new("a/two.txt"), b"two!", None).unwrap();
        symlink(
            outer.path().join("secret.txt"),
            outer.path().join("grant/a/link"),
        )
        .unwrap();

        let one = dir.stat(Path::new("a/b/one.txt")).unwrap().unwrap();
        assert_eq!((one.kind, one.size), (FileKind::File, 3));
        assert_eq!(
            dir.stat(Path::new("a")).unwrap().unwrap().kind,
            FileKind::Dir
        );
        assert_eq!(
            dir.stat(Path::new("")).unwrap().unwrap().kind,
            FileKind::Dir
        );
        let link = dir.stat(Path::new("a/link")).unwrap().unwrap();
        assert_eq!(link.kind, FileKind::Other, "the link, not its target");
        assert_eq!(dir.stat(Path::new("missing")).unwrap(), None);
        assert_eq!(dir.stat(Path::new("a/two.txt/under")).unwrap(), None);
        assert!(dir.stat(Path::new("../secret.txt")).is_err());

        dir.write(Path::new("a/b/one.txt"), b"uno", None).unwrap();
        let again = dir.stat(Path::new("a/b/one.txt")).unwrap().unwrap();
        assert_ne!(again, one, "a rewrite is a new file");

        let mut files = dir.files(Path::new("a")).unwrap();
        files.sort();
        assert_eq!(
            files,
            ["a/b/one.txt", "a/link", "a/two.txt"].map(PathBuf::from)
        );
        assert_eq!(dir.files(Path::new("")).unwrap().len(), 3);
        assert!(dir.files(Path::new("missing")).unwrap().is_empty());
        assert!(dir.files(Path::new("a/two.txt")).unwrap().is_empty());
        assert!(
            dir.open_file(Path::new("a")).is_err(),
            "a folder isn't a file"
        );
    }

    #[test]
    fn size_limit_is_enforced() {
        let (_outer, dir) = setup();
        dir.write(Path::new("big"), &[0u8; 2048], None).unwrap();
        let err = dir.read(Path::new("big"), 1024).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::FileTooLarge);
    }
}
