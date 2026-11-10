//! What a task wrote, for undo (ADR 0017).
//!
//! Each change keeps the files it touched (as host paths; `ChangeEntry::paths`
//! shows them to the user) and how many of the task's snapshots existed when
//! it ran: the latest of those covering a file has the file as it was before
//! the change. After every change, and after every undo, the runtime also
//! notes how the task left each file. A file still as the task left it is the
//! task's to undo; one that isn't was changed since by someone else (the
//! user, another app, another task), and undo leaves it alone.
//!
//! Each change also keeps how it left its files, apart from anything later
//! (ADR 0051): once undo put a file back to someone else's version, the
//! task's earlier change is judged by how that change left the file, not by
//! how the undo did, or the task would take the other's version back too.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use glass_platform::fs::{ConfinedDir, FileKind, Stat};
use sha2::{Digest, Sha256};

/// Files up to this size are fingerprinted by content too, so a file
/// rewritten with the same bytes still counts as the task left it. That
/// covers what agents write (`files.write` takes its content inline); larger
/// files, and files under a folder a change covered (a moved folder can be
/// big), are compared by metadata alone, which only errs towards leaving a
/// file alone.
pub(crate) const DIGEST_LIMIT: u64 = 1 << 20;

/// Undo's record of one task's writes.
#[derive(Debug, Default, Clone)]
pub(crate) struct Written {
    changes: BTreeMap<u32, Touched>,
    files: BTreeMap<PathBuf, Option<Left>>,
    /// Paths the task left with nothing under them but what `files` names:
    /// folders a change covered whole (a cache cleanup) and files it removed.
    cleared: BTreeSet<PathBuf>,
    /// How each change left its files, read right after it.
    after: BTreeMap<u32, After>,
}

/// How one change left its files: `files` and `cleared` as in [`Written`].
#[derive(Debug, Default, Clone)]
struct After {
    files: BTreeMap<PathBuf, Option<Left>>,
    cleared: BTreeSet<PathBuf>,
}

/// The files one change touched.
#[derive(Debug, Clone)]
pub(crate) struct Touched {
    pub paths: Vec<PathBuf>,
    /// How many of the task's snapshots existed when the change ran.
    pub before: usize,
}

/// How the task left a file that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Left {
    stat: Stat,
    digest: Option<[u8; 32]>,
}

/// Files as a change or an undo left them, read off the disk.
#[derive(Debug, Default, Clone)]
pub(crate) struct Seen(Vec<Saw>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Saw {
    File(PathBuf, Left),
    /// A folder and every file in it, or (with no files) nothing at all.
    Tree(PathBuf, Vec<(PathBuf, Left)>),
}

impl Written {
    pub fn touched(&mut self, id: u32, touched: Touched) {
        self.changes.insert(id, touched);
    }

    pub fn change(&self, id: u32) -> Option<&Touched> {
        self.changes.get(&id)
    }

    /// How the task left `file`: `Some(None)` if it left nothing there,
    /// `None` if it never touched it.
    pub fn left(&self, file: &Path) -> Option<Option<&Left>> {
        lookup(&self.files, &self.cleared, file)
    }

    /// How change `id` left `file`, as [`Self::left`]; for a change with no
    /// record of its own, how the task left it.
    pub fn left_after(&self, id: u32, file: &Path) -> Option<Option<&Left>> {
        match self.after.get(&id) {
            Some(after) => lookup(&after.files, &after.cleared, file),
            None => self.left(file),
        }
    }

    /// Notes `seen` as how changes `ids` left their files, and the task.
    pub fn note_changes(&mut self, ids: &[u32], seen: Seen) {
        let mut after = After::default();
        apply(&mut after.files, &mut after.cleared, seen.clone());
        for id in ids {
            self.after.insert(*id, after.clone());
        }
        self.note(seen);
    }

    /// Whether `seen` is exactly how the task left those files.
    pub fn explains(&self, seen: &Seen) -> bool {
        seen.0.iter().all(|saw| match saw {
            Saw::File(path, left) => self.left(path) == Some(Some(left)),
            Saw::Tree(path, files) => files.is_empty() && matches!(self.left(path), Some(None)),
        })
    }

    pub fn note(&mut self, seen: Seen) {
        apply(&mut self.files, &mut self.cleared, seen);
    }
}

fn lookup<'a>(
    files: &'a BTreeMap<PathBuf, Option<Left>>,
    cleared: &BTreeSet<PathBuf>,
    file: &Path,
) -> Option<Option<&'a Left>> {
    match files.get(file) {
        Some(left) => Some(left.as_ref()),
        None => cleared.iter().any(|p| file.starts_with(p)).then_some(None),
    }
}

fn apply(files: &mut BTreeMap<PathBuf, Option<Left>>, cleared: &mut BTreeSet<PathBuf>, seen: Seen) {
    for saw in seen.0 {
        match saw {
            Saw::File(path, left) => {
                files.insert(path, Some(left));
            }
            Saw::Tree(path, under) => {
                files.retain(|f, _| !f.starts_with(&path));
                files.extend(under.into_iter().map(|(f, left)| (f, Some(left))));
                cleared.insert(path);
            }
        }
    }
}

impl Seen {
    pub fn extend(&mut self, other: Seen) {
        self.0.extend(other.0);
    }

    /// `file` was put back with `data`.
    pub fn restored(&mut self, file: PathBuf, stat: Stat, data: &[u8]) {
        let digest = (data.len() as u64 <= DIGEST_LIMIT).then(|| Sha256::digest(data).into());
        self.0.push(Saw::File(file, Left { stat, digest }));
    }

    /// `file` was brought back as `stat`, too large for its content to be
    /// kept track of (over [`DIGEST_LIMIT`]): its size and times say
    /// whether someone changed it since.
    pub fn restored_large(&mut self, file: PathBuf, stat: Stat) {
        self.0.push(Saw::File(file, Left { stat, digest: None }));
    }

    /// `file` was removed.
    pub fn removed(&mut self, file: PathBuf) {
        self.0.push(Saw::Tree(file, Vec::new()));
    }
}

impl Left {
    /// Whether `now`, at `rel` in `dir`, is still this.
    pub fn matches(&self, dir: &ConfinedDir, rel: &Path, now: Option<Stat>) -> io::Result<bool> {
        let Some(now) = now else { return Ok(false) };
        if now == self.stat {
            return Ok(true);
        }
        match self.digest {
            Some(digest) if now.kind == FileKind::File && now.size == self.stat.size => {
                Ok(digest_of(dir.open_file(rel)?)? == digest)
            }
            _ => Ok(false),
        }
    }
}

/// Reads how `paths` are now, each through the deepest of `roots` holding it.
/// Paths outside every root, or that can't be read, are skipped.
pub(crate) fn look<'a>(roots: &[PathBuf], paths: impl IntoIterator<Item = &'a Path>) -> Seen {
    let mut dirs: BTreeMap<&Path, ConfinedDir> = BTreeMap::new();
    let mut seen = Seen::default();
    for path in paths {
        let Some((root, rel)) = confine(roots, path) else {
            continue;
        };
        let dir = match dirs.entry(root) {
            std::collections::btree_map::Entry::Occupied(dir) => dir.into_mut(),
            std::collections::btree_map::Entry::Vacant(slot) => match ConfinedDir::open(root) {
                Ok(dir) => slot.insert(dir),
                Err(e) => {
                    tracing::debug!(root = %root.display(), error = %e, "can't note what a change left");
                    continue;
                }
            },
        };
        match saw(dir, rel) {
            Ok(saw) => seen.0.push(saw),
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "can't note what a change left");
            }
        }
    }
    seen
}

/// The deepest of `roots` holding `path`, and `path` relative to it.
pub(crate) fn confine<'a, 'p>(
    roots: &'a [PathBuf],
    path: &'p Path,
) -> Option<(&'a Path, &'p Path)> {
    let root = roots
        .iter()
        .filter(|r| path.starts_with(r))
        .max_by_key(|r| r.components().count())?;
    Some((root, path.strip_prefix(root).ok()?))
}

fn saw(dir: &ConfinedDir, rel: &Path) -> io::Result<Saw> {
    let at = |rel: &Path| {
        if rel.as_os_str().is_empty() {
            dir.root().to_path_buf()
        } else {
            dir.root().join(rel)
        }
    };
    match dir.stat(rel)? {
        Some(stat) if stat.kind == FileKind::Dir => {
            let mut files = Vec::new();
            for file in dir.files(rel)? {
                if let Some(stat) = dir.stat(&file)? {
                    files.push((at(&file), Left { stat, digest: None }));
                }
            }
            Ok(Saw::Tree(at(rel), files))
        }
        Some(stat) => Ok(Saw::File(at(rel), left_of(dir, rel, stat)?)),
        None => Ok(Saw::Tree(at(rel), Vec::new())),
    }
}

fn left_of(dir: &ConfinedDir, rel: &Path, stat: Stat) -> io::Result<Left> {
    let digest = if stat.kind == FileKind::File && stat.size <= DIGEST_LIMIT {
        Some(digest_of(dir.open_file(rel)?)?)
    } else {
        None
    };
    Ok(Left { stat, digest })
}

fn digest_of(mut file: File) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        match file.read(&mut buf)? {
            0 => return Ok(hasher.finalize().into()),
            n => hasher.update(&buf[..n]),
        }
    }
}

/// Whether two files hold the same bytes.
pub(crate) fn same_content(mut a: File, mut b: File) -> io::Result<bool> {
    let (mut x, mut y) = (vec![0; 64 * 1024], vec![0; 64 * 1024]);
    loop {
        let n = fill(&mut a, &mut x)?;
        if n != fill(&mut b, &mut y)? || x[..n] != y[..n] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buf` is full or the file ends.
fn fill(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn notes_files_folders_and_removals() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("Reports");
        std::fs::create_dir_all(root.join("old")).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("old/b.txt"), "b").unwrap();
        let roots = [root.clone()];
        let mut written = Written::default();

        let seen = look(
            &roots,
            [root.join("a.txt").as_path(), root.join("old").as_path()],
        );
        assert!(!written.explains(&seen));
        written.note(seen);
        assert!(matches!(written.left(&root.join("a.txt")), Some(Some(_))));
        assert!(matches!(
            written.left(&root.join("old/b.txt")),
            Some(Some(_))
        ));
        assert_eq!(
            written.left(&root.join("old/new.txt")),
            Some(None),
            "nothing else was there"
        );
        assert_eq!(written.left(&root.join("other.txt")), None, "never touched");
        assert!(written.explains(&look(&roots, [root.join("a.txt").as_path()])));

        std::fs::write(root.join("a.txt"), "A").unwrap();
        assert!(!written.explains(&look(&roots, [root.join("a.txt").as_path()])));
        std::fs::remove_dir_all(root.join("old")).unwrap();
        written.note(look(&roots, [root.join("old").as_path()]));
        assert_eq!(written.left(&root.join("old/b.txt")), Some(None));
        assert!(
            look(
                &[home.path().join("Elsewhere")],
                [root.join("a.txt").as_path()]
            )
            .0
            .is_empty()
        );
    }

    #[test]
    fn the_same_bytes_rewritten_still_match() {
        let home = tempfile::tempdir().unwrap();
        let dir = ConfinedDir::open(home.path()).unwrap();
        dir.write(Path::new("f.txt"), b"same", None).unwrap();
        let rel = Path::new("f.txt");
        let left = left_of(&dir, rel, dir.stat(rel).unwrap().unwrap()).unwrap();
        dir.write(rel, b"same", None).unwrap();
        let now = dir.stat(rel).unwrap();
        assert_ne!(now, Some(left.stat), "a new file");
        assert!(left.matches(&dir, rel, now).unwrap());
        dir.write(rel, b"diff", None).unwrap();
        assert!(!left.matches(&dir, rel, dir.stat(rel).unwrap()).unwrap());
        assert!(!left.matches(&dir, rel, None).unwrap());

        dir.write(Path::new("g.txt"), b"same", None).unwrap();
        let open = |name: &str| dir.open_file(Path::new(name)).unwrap();
        assert!(same_content(open("f.txt"), open("f.txt")).unwrap());
        assert!(!same_content(open("f.txt"), open("g.txt")).unwrap());
    }
}
