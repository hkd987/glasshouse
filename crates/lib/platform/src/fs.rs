//! File access confined to a granted root.
//!
//! Brokers run as root and act on an agent's behalf, while the agent may have
//! planted symlinks or `..` paths to trick them. [`ConfinedDir`] resolves
//! every path inside its root using `cap-std` (openat-style resolution that
//! refuses to escape, the same model Capsicum enforces in the kernel), so a
//! path can never reach outside the grant.

use std::io::{self, Read, Write};
use std::os::unix::fs::{PermissionsExt, fchown};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};

/// Who should own files the broker creates for a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
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

    /// Reads a whole file, refusing files larger than `max_bytes`.
    pub fn read(&self, rel: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
        let rel = relative(rel)?;
        let file = self.dir.open(rel)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
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
        let rel = relative(rel)?;
        let existed = self.dir.symlink_metadata(rel).is_ok();
        let tmp = self.stage(rel, data, owner, mode)?;
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
        let tmp = self.stage(rel, data, owner, None)?;
        let result = self.dir.hard_link(&tmp, &self.dir, rel);
        let _ = self.dir.remove_file(&tmp);
        result
    }

    /// A temporary file beside `rel` holding `data`, synced, with its owner
    /// and mode set. Its name is unique within this process.
    fn stage(
        &self,
        rel: &Path,
        data: &[u8],
        owner: Option<Owner>,
        mode: Option<u32>,
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
            file.write_all(data)?;
            file.sync_all()?;
            let std_file = file.into_std();
            if let Some(owner) = owner {
                fchown(&std_file, Some(owner.uid), Some(owner.gid))?;
            }
            if let Some(mode) = mode {
                std_file.set_permissions(std::fs::Permissions::from_mode(mode & 0o7777))?;
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

    pub fn create_dir_all(&self, rel: &Path) -> io::Result<()> {
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
            self.dir.create_dir(&prefix)?;
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

    fn setup() -> (tempfile::TempDir, ConfinedDir) {
        let outer = tempfile::tempdir().unwrap();
        std::fs::create_dir(outer.path().join("grant")).unwrap();
        std::fs::write(outer.path().join("secret.txt"), "top secret").unwrap();
        let dir = ConfinedDir::open(&outer.path().join("grant")).unwrap();
        (outer, dir)
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

    #[test]
    fn size_limit_is_enforced() {
        let (_outer, dir) = setup();
        dir.write(Path::new("big"), &[0u8; 2048], None).unwrap();
        let err = dir.read(Path::new("big"), 1024).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::FileTooLarge);
    }
}
