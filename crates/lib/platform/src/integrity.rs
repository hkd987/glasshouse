//! The system's files as they are (ADR 0064, EP-1, ADR 0026's
//! `integrity`): their digests, to compare with the list an image's
//! release signed, and the system immutable flag that keeps them
//! read-only.
//!
//! The system is the image's programs, libraries and kernel: what
//! [`SYSTEM_DIRS`] hold, debug symbols and the kernel's module index
//! aside. Configuration (`/etc`, `/usr/local/etc`), state (`/var`) and
//! people's files are not the system.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use glass_types::FileDigest;
use sha2::Digest as _;

use crate::{PlatformError, Result};

/// Where the system's files are, from the root.
pub const SYSTEM_DIRS: &[&str] = &[
    "bin",
    "sbin",
    "lib",
    "libexec",
    "usr/bin",
    "usr/sbin",
    "usr/lib",
    "usr/lib32",
    "usr/libexec",
    "usr/local/bin",
    "usr/local/sbin",
    "usr/local/lib",
    "usr/local/libexec",
    "boot/kernel",
];

/// Under [`SYSTEM_DIRS`] but not the system: debug symbols, which run
/// nowhere.
const SKIPPED_DIRS: &[&str] = &["usr/lib/debug"];

/// The kernel's module index, which `kldxref` writes again whenever a
/// module is added: it names modules, it runs nothing.
const SKIPPED_NAMES: &[&str] = &["linker.hints"];

/// `SF_IMMUTABLE` in `<sys/stat.h>`: not even root changes, renames or
/// removes the file until the flag is taken off.
#[cfg(target_os = "freebsd")]
const SF_IMMUTABLE: u32 = 0x0002_0000;

/// Every system file under `root`, by its path from `root`
/// (`usr/bin/ls`): regular files by digest, symbolic links by where they
/// point. Files are hashed on as many threads as the computer has. One
/// that can't be read gets a digest that says so, which matches nothing.
pub fn system_files(root: &Path) -> Result<BTreeMap<String, FileDigest>> {
    let mut found = BTreeMap::new();
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for dir in SYSTEM_DIRS {
        walk(root, &root.join(dir), &mut found, &mut files)?;
    }
    for (path, digest) in hash_all(&files) {
        found.insert(path, digest);
    }
    Ok(found)
}

fn walk(
    root: &Path,
    dir: &Path,
    found: &mut BTreeMap<String, FileDigest>,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(PlatformError::io(format!("reading {}", dir.display()), e)),
    };
    let mut entries: Vec<_> = entries
        .collect::<std::io::Result<_>>()
        .map_err(|e| PlatformError::io(format!("reading {}", dir.display()), e))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let relative = relative.to_string_lossy().into_owned();
        let name = entry.file_name();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| PlatformError::io(format!("reading {}", path.display()), e))?;
        if meta.is_dir() {
            if !SKIPPED_DIRS.contains(&relative.as_str()) {
                walk(root, &path, found, files)?;
            }
        } else if meta.file_type().is_symlink() {
            let to = std::fs::read_link(&path)
                .map_err(|e| PlatformError::io(format!("reading {}", path.display()), e))?;
            found.insert(
                relative,
                FileDigest::link(to.to_string_lossy().into_owned()),
            );
        } else if meta.is_file() && !SKIPPED_NAMES.contains(&name.to_string_lossy().as_ref()) {
            files.push((relative, path));
        }
    }
    Ok(())
}

/// `files` hashed in parallel, each by one thread.
fn hash_all(files: &[(String, PathBuf)]) -> Vec<(String, FileDigest)> {
    let next = AtomicUsize::new(0);
    let done = Mutex::new(Vec::with_capacity(files.len()));
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(8));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut buf = vec![0u8; 1 << 16];
                let mut mine = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((relative, path)) = files.get(i) else {
                        break;
                    };
                    let digest = hash(path, &mut buf)
                        .unwrap_or_else(|e| FileDigest::file(format!("unreadable: {e}"), 0));
                    mine.push((relative.clone(), digest));
                }
                done.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend(mine);
            });
        }
    });
    done.into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn hash(path: &Path, buf: &mut [u8]) -> std::io::Result<FileDigest> {
    let mut file = std::fs::File::open(path)?;
    let mut sha = sha2::Sha256::new();
    let mut bytes = 0u64;
    loop {
        let n = file.read(buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        bytes += n as u64;
    }
    Ok(FileDigest::file(hex::encode(sha.finalize()), bytes))
}

/// Whether `path` carries the system immutable flag (`schg`). `None` where
/// files have no such flag (the development host).
pub fn immutable(path: &Path) -> Option<bool> {
    #[cfg(target_os = "freebsd")]
    {
        use std::os::freebsd::fs::MetadataExt as _;
        std::fs::symlink_metadata(path)
            .ok()
            .map(|m| m.st_flags() & SF_IMMUTABLE != 0)
    }
    #[cfg(not(target_os = "freebsd"))]
    {
        let _ = path;
        None
    }
}

/// Sets the system immutable flag on `paths`, or takes it off
/// (`chflags`), a few hundred at a time.
pub fn set_immutable(paths: &[PathBuf], on: bool) -> Result<()> {
    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported(
            "files have no immutable flag here".into(),
        ));
    }
    let flag = if on { "schg" } else { "noschg" };
    for chunk in paths.chunks(200) {
        let mut args: Vec<&str> = vec![flag];
        args.extend(chunk.iter().filter_map(|p| p.to_str()));
        crate::cmd::run("/bin/chflags", &args)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The system's files, by digest and link, and nothing outside the
    /// system's folders, nor debug symbols or the module index.
    #[test]
    fn ep1_the_system_is_its_programs_libraries_and_kernel() {
        let root = tempfile::tempdir().unwrap();
        let at = |p: &str| root.path().join(p);
        for dir in ["usr/bin", "usr/lib/debug/usr/bin", "boot/kernel", "etc"] {
            std::fs::create_dir_all(at(dir)).unwrap();
        }
        std::fs::write(at("usr/bin/yes"), b"y\n").unwrap();
        std::fs::write(at("usr/lib/debug/usr/bin/yes.debug"), b"symbols").unwrap();
        std::fs::write(at("boot/kernel/kernel"), b"the kernel").unwrap();
        std::fs::write(at("boot/kernel/linker.hints"), b"index").unwrap();
        std::fs::write(at("etc/rc.conf"), b"hostname=x\n").unwrap();
        std::os::unix::fs::symlink("yes", at("usr/bin/no")).unwrap();

        let found = system_files(root.path()).unwrap();
        let names: Vec<&str> = found.keys().map(String::as_str).collect();
        assert_eq!(names, ["boot/kernel/kernel", "usr/bin/no", "usr/bin/yes"]);
        assert_eq!(found["usr/bin/yes"].bytes, 2);
        assert_eq!(
            found["usr/bin/yes"].sha256,
            "3bb2abb69ebb27fbfe63c7639624c6ec5e331b841a5bc8c3ebc10b9285e90877"
        );
        assert_eq!(found["usr/bin/no"].link.as_deref(), Some("yes"));

        // The same files, many: every one hashed once, whatever the threads.
        for n in 0..300 {
            std::fs::write(at(&format!("usr/bin/p{n}")), n.to_string()).unwrap();
        }
        let found = system_files(root.path()).unwrap();
        assert_eq!(found.len(), 303);
        assert_eq!(found["usr/bin/p7"].bytes, 1);
    }
}
