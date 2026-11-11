//! Small state files, written whole and read back.
//!
//! Glasshouse keeps much of what it knows in small files: who signed in
//! on a host, a person's routines, what an update had done when the power
//! went. Each store wrote and read its own in its own way, and the ways
//! differed where it matters:
//!
//! - **Writing.** Some synced the file and its folder, some the file, some
//!   nothing, one wrote in place. After a crash the last could hold half a
//!   file, and the unsynced ones an empty one. [`write`] is the one way: a
//!   file beside the target, synced, renamed over it, and the folder
//!   synced, so a crash leaves the old file or the new and never a mix.
//! - **Reading.** Most read a file they couldn't parse as if it weren't
//!   there, then wrote their next change over it. A typo in a file a
//!   person may edit, a file half written, or one from a newer version,
//!   and everything in it was gone without a word. [`read_json`] tells
//!   the three apart: there, not there, or [`ReadError::Damaged`]. What
//!   damaged means is each store's to decide; [`read_json_or_set_aside`]
//!   is for those that start again empty, and keeps what was there.
//!
//! This crate owns no store and knows no schema: it is the two halves
//! every store repeats, and for stores that keep versions of a thing, the
//! rule for which of them are kept ([`kept`]).

pub mod kept;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A file only its owner reads or writes: what state with anything of a
/// person's in it gets.
pub const PRIVATE: u32 = 0o600;
/// A file anyone on the computer may read.
pub const READABLE: u32 = 0o644;

/// Why a state file couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The file is there and couldn't be read (permissions, the disk).
    #[error("reading {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The file is there and isn't what it should be: cut short, edited
    /// into something that doesn't parse, or written by a version whose
    /// fields this one doesn't know.
    #[error("{} is damaged: {reason}", path.display())]
    Damaged { path: PathBuf, reason: String },
}

/// Writes `bytes` as the whole of `path`, readable as `mode` says: beside
/// it, synced, renamed over it, and the folder synced. A crash leaves the
/// old file or the new one. The folder is made if it isn't there.
pub fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} names no file", path.display()),
        )
    })?;
    fs::create_dir_all(dir)?;
    // One name per writer: two writing the same file at once each rename
    // a whole file of their own, and the later one wins.
    let new = dir.join(format!(
        ".{}.new-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        next()
    ));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&new)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&new, path)?;
        File::open(dir)?.sync_all()
    })();
    if written.is_err() {
        let _ = fs::remove_file(&new);
    }
    written
}

fn next() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// [`write`], of `value` as JSON on one line.
pub fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T, mode: u32) -> io::Result<()> {
    write(path, &serde_json::to_vec(value)?, mode)
}

/// [`write`], of `value` as JSON laid out for a person to read (and, where
/// a store says so, to edit).
pub fn write_json_pretty<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    mode: u32,
) -> io::Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    write(path, &text, mode)
}

/// What the JSON file at `path` holds: `None` if there is no such file,
/// and an error, never `None`, if there is one that can't be read or isn't
/// a `T`.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, ReadError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ReadError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| ReadError::Damaged {
            path: path.to_owned(),
            reason: e.to_string(),
        })
}

/// Moves a damaged file out of the way, beside itself, and says where: the
/// store can start again and what was there can still be looked at, or put
/// right and put back.
pub fn set_aside(path: &Path) -> io::Result<PathBuf> {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".damaged-{at}-{}", next()));
    let aside = path.with_file_name(name);
    fs::rename(path, &aside)?;
    Ok(aside)
}

/// [`read_json`] for a store that starts again empty when its file is
/// damaged: the file is [`set_aside`], with a warning that says where,
/// and `None` comes back. The store's next write then makes a new file,
/// instead of writing over the only copy of what couldn't be read. A file
/// that can't be read at all (not one that is damaged) is still an error:
/// starting empty over it would be the same loss.
pub fn read_json_or_set_aside<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, ReadError> {
    match read_json(path) {
        Err(ReadError::Damaged { path, reason }) => {
            match set_aside(&path) {
                Ok(aside) => tracing::warn!(
                    file = %path.display(),
                    kept_as = %aside.display(),
                    %reason,
                    "a state file couldn't be read as what it should be; starting it again and keeping what was there"
                ),
                Err(source) => return Err(ReadError::Io { path, source }),
            }
            Ok(None)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::os::unix::fs::PermissionsExt;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Kept {
        name: String,
        count: u32,
    }

    fn kept() -> Kept {
        Kept {
            name: "dana".into(),
            count: 3,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_file_is_written_whole_with_its_mode_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        // In a folder that isn't there yet.
        let path = dir.path().join("state/people.json");
        assert_eq!(read_json::<Kept>(&path).unwrap(), None);
        write_json(&path, &kept(), PRIVATE).unwrap();
        assert_eq!(read_json::<Kept>(&path).unwrap(), Some(kept()));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600);
        // In place of what was there, with nothing left beside it.
        write_json_pretty(&path, &Kept { count: 4, ..kept() }, READABLE).unwrap();
        assert_eq!(read_json::<Kept>(&path).unwrap().unwrap().count, 4);
        assert!(fs::read_to_string(&path).unwrap().ends_with("}\n"));
        assert_eq!(names(path.parent().unwrap()), ["people.json"]);
        assert!(
            write(dir.path(), b"x", PRIVATE).is_err(),
            "a folder isn't a file"
        );
    }

    #[test]
    fn a_damaged_file_is_never_read_as_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("routines.json");
        // Cut short, a typo, another version's field: each is damage.
        for damaged in [
            r#"{"name": "dana", "cou"#,
            r#"{"name": "dana", "count": 3,}"#,
            r#"{"name": "dana", "count": 3, "newer": true}"#,
            "",
        ] {
            fs::write(&path, damaged).unwrap();
            let err = read_json::<Kept>(&path).unwrap_err();
            assert!(
                matches!(err, ReadError::Damaged { .. }),
                "{damaged:?}: {err}"
            );
        }
        // A file that can't be read is an error too, not "nothing there".
        let unreadable = dir.path().join("a-folder.json");
        fs::create_dir(&unreadable).unwrap();
        assert!(matches!(
            read_json::<Kept>(&unreadable),
            Err(ReadError::Io { .. })
        ));
    }

    #[test]
    fn starting_again_keeps_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("routines.json");
        fs::write(&path, "{ a person's edit, with a typo").unwrap();
        assert_eq!(read_json_or_set_aside::<Kept>(&path).unwrap(), None);
        assert!(!path.exists(), "it's out of the next write's way");
        let left = names(dir.path());
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(left[0].starts_with("routines.json.damaged-"), "{left:?}");
        assert_eq!(
            fs::read_to_string(dir.path().join(&left[0])).unwrap(),
            "{ a person's edit, with a typo"
        );
        // A good file and no file are what they were.
        write_json(&path, &kept(), PRIVATE).unwrap();
        assert_eq!(read_json_or_set_aside::<Kept>(&path).unwrap(), Some(kept()));
        assert_eq!(
            read_json_or_set_aside::<Kept>(&dir.path().join("none.json")).unwrap(),
            None
        );
    }
}
