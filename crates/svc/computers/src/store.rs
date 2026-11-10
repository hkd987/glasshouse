//! Where a computer lives while it sleeps (ADR 0065 §11): in S3, as
//! content-addressed chunks, or in a folder standing in for it. A host's
//! disk is only a cache.
//!
//! A version of a computer is a manifest
//! (`computers/<computer>/versions/<version>.json`) of a folder tree or of
//! one large file (a disk image, a memory snapshot): every path with its
//! mode, its time, its kind (file, folder, or symlink and its target) and
//! its chunks. A chunk is up to [`CHUNK_SIZE`] of a file, named by its
//! SHA-256 (`chunks/<sha256>`) and uploaded only if the store doesn't have
//! it, so versions and computers share what's the same. A chunk of zeros is
//! recorded as zeros and never stored: a sparse image costs what's written
//! in it. Version ids are times, so they sort by when they were saved.
//!
//! A restore checks the manifest first and every chunk's SHA-256 as it
//! reads it, and writes only beneath the folder it restores into: paths are
//! relative, never `..`, each in a folder listed before it, and symlinks
//! are made last and never followed. A tree comes back exactly: contents,
//! modes, times, symlinks and empty folders.
//!
//! [`Store::gc`] deletes the chunks no manifest refers to, but never one
//! written in the last hour ([`GRACE`]): a save writes its manifest last,
//! and gives up rather than write one more than an hour after it began, so
//! nothing it uploaded is collected under it.
//!
//! Not kept: owners (a computer's files are all its own), sockets, pipes
//! and devices (named in [`Saved::skipped`]), hard links (each path is a
//! file of its own, their chunks shared) and symlinks' own times.

mod blobs;
mod manifest;
mod tree;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};

pub use blobs::{Blobs, FolderBlobs, Listed, S3Blobs};
use manifest::{Entry, FORMAT, Kind, Manifest, Of, Stamp};

/// The most of a file one chunk holds.
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;
/// How long GC leaves a chunk no manifest refers to: a save in progress
/// has this long to write its manifest.
pub const GRACE: Duration = Duration::from_secs(3600);

/// Computers' versions, kept in [`Blobs`].
#[derive(Debug, Clone)]
pub struct Store {
    blobs: Arc<dyn Blobs>,
    grace: Duration,
}

/// What a save stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The new version's id: when the save began, so versions sort by it.
    pub version: String,
    /// Regular files in it.
    pub files: u64,
    /// Their bytes, all told, zeros too.
    pub bytes: u64,
    /// The bytes uploaded: chunks the store didn't have.
    pub uploaded: u64,
    /// Paths left out: sockets, pipes and devices, which no store holds.
    pub skipped: Vec<String>,
}

/// What [`Store::gc`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Collected {
    /// Chunks kept: a manifest refers to them, or they're too new to tell.
    pub kept: u64,
    pub deleted: u64,
    /// The bytes the deleted chunks held.
    pub freed: u64,
}

/// Chunks of one save on their way to the store at once, at most. A host
/// being replaced has minutes for all its computers (audit 7.1), and a
/// chunk is a question and an upload to S3, one after the other: a few at
/// a time is a few times as fast, without holding more than this many
/// chunks in memory twice over.
pub const CHUNKS_AT_ONCE: usize = 4;

/// One save's uploads: a chunk it meets twice is looked for once, and one
/// its computer's last version has isn't looked for at all.
struct Upload {
    stored: HashSet<String>,
    sending: Arc<Sending>,
    buf: Vec<u8>,
    files: u64,
    bytes: u64,
}

/// Chunks going to the store, [`CHUNKS_AT_ONCE`] at a time.
#[derive(Debug)]
struct Sending {
    blobs: Arc<dyn Blobs>,
    state: Mutex<SendState>,
    /// Told when a chunk has gone, or failed to.
    turn: Condvar,
}

#[derive(Debug, Default)]
struct SendState {
    /// Chunks being stored now.
    flying: usize,
    /// The first failure: nothing more is sent, and the save fails.
    failed: Option<String>,
    /// Bytes stored: chunks the store didn't have.
    uploaded: u64,
}

impl Sending {
    fn state(&self) -> std::sync::MutexGuard<'_, SendState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Stores the chunk `name` unless the store has it, beside the others
    /// on their way; waits for room among them first. An error is an
    /// earlier chunk's: the save has failed.
    fn send(self: &Arc<Self>, name: String, bytes: Vec<u8>) -> Result<(), String> {
        let mut state = self.state();
        loop {
            if let Some(failed) = &state.failed {
                return Err(failed.clone());
            }
            if state.flying < CHUNKS_AT_ONCE {
                break;
            }
            state = self
                .turn
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.flying += 1;
        drop(state);
        let sending = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("glass-chunk".to_owned())
            .spawn(move || {
                // Counted as landed whatever happens in here, or the save
                // would wait on it for good.
                let landing = Landing {
                    sending: &sending,
                    result: Some(Err("storing a chunk stopped unexpectedly".to_owned())),
                };
                let key = chunk_key(&name);
                let stored = sending.blobs.exists(&key).and_then(|there| {
                    if there {
                        Ok(0)
                    } else {
                        sending
                            .blobs
                            .put_hashed(&key, &bytes, &name)
                            .map(|()| bytes.len() as u64)
                    }
                });
                landing.with(stored);
            });
        if let Err(e) = spawned {
            let mut state = self.state();
            state.flying -= 1;
            let failed = format!("can't start storing a chunk: {e}");
            state.failed.get_or_insert_with(|| failed.clone());
            self.turn.notify_all();
            return Err(failed);
        }
        Ok(())
    }

    /// Waits for every chunk sent: the bytes stored, or the first failure.
    fn finish(&self) -> Result<u64, String> {
        let mut state = self.state();
        while state.flying > 0 {
            state = self
                .turn
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        match &state.failed {
            Some(failed) => Err(failed.clone()),
            None => Ok(state.uploaded),
        }
    }
}

/// One chunk being stored, counted as done when this goes.
struct Landing<'a> {
    sending: &'a Sending,
    result: Option<Result<u64, String>>,
}

impl Landing<'_> {
    fn with(mut self, result: Result<u64, String>) {
        self.result = Some(result);
    }
}

impl Drop for Landing<'_> {
    fn drop(&mut self) {
        let mut state = self.sending.state();
        match self.result.take() {
            Some(Ok(bytes)) => state.uploaded += bytes,
            Some(Err(e)) => {
                state.failed.get_or_insert(e);
            }
            None => {}
        }
        state.flying -= 1;
        self.sending.turn.notify_all();
    }
}

impl Upload {
    /// Splits `file` into chunks, storing those the store lacks: its size
    /// and its chunks.
    fn file(&mut self, file: &mut File, path: &str) -> Result<(u64, Vec<Option<String>>), String> {
        let mut buf = std::mem::take(&mut self.buf);
        buf.resize(CHUNK_SIZE, 0);
        let mut size = 0u64;
        let mut chunks = Vec::new();
        let result = loop {
            let n = match fill(file, &mut buf) {
                Ok(n) => n,
                Err(e) => break Err(format!("can't read {path}: {e}")),
            };
            if n == 0 {
                break Ok(());
            }
            match self.chunk(&buf[..n]) {
                Ok(chunk) => chunks.push(chunk),
                Err(e) => break Err(e),
            }
            size += n as u64;
            if n < CHUNK_SIZE {
                break Ok(());
            }
        };
        self.buf = buf;
        result?;
        self.files += 1;
        self.bytes += size;
        Ok((size, chunks))
    }

    /// Zeros are `None` and stored nowhere; anything else is stored under
    /// its SHA-256 unless it is already, on its way beside the chunks
    /// before it.
    fn chunk(&mut self, bytes: &[u8]) -> Result<Option<String>, String> {
        if bytes.iter().all(|&b| b == 0) {
            return Ok(None);
        }
        let name = hex::encode(Sha256::digest(bytes));
        if self.stored.insert(name.clone()) {
            self.sending.send(name.clone(), bytes.to_vec())?;
        }
        Ok(Some(name))
    }
}

/// Reads until `buf` is full or the file ends: how much it read.
fn fill(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

impl Store {
    pub fn new(blobs: Arc<dyn Blobs>) -> Self {
        Self {
            blobs,
            grace: GRACE,
        }
    }

    /// Another time for [`GRACE`]: GC leaves chunks younger than `grace`,
    /// and a save must write its manifest within it.
    #[must_use]
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Saves the folder tree at `dir` as a new version of `computer`.
    pub fn save_tree(&self, computer: &str, dir: &Path) -> Result<Saved, String> {
        check_computer(computer)?;
        let started = Instant::now();
        let version = self.next_version(computer)?;
        let mut upload = self.upload(computer);
        let tree = tree::read(dir, &mut |file, path| upload.file(file, path));
        // Every chunk is in the store before the manifest that names it,
        // and none is still on its way when this returns, failed or not.
        let uploaded = upload.sending.finish();
        let (tree, uploaded) = (tree?, uploaded?);
        let manifest = Manifest {
            format: FORMAT,
            computer: computer.to_owned(),
            version: version.clone(),
            chunk_size: CHUNK_SIZE as u64,
            of: Of::Tree,
            root: Some(tree.root),
            entries: tree.entries,
        };
        self.commit(&manifest, started)?;
        Ok(Saved {
            version,
            files: upload.files,
            bytes: upload.bytes,
            uploaded,
            skipped: tree.skipped,
        })
    }

    /// Saves one large file (a disk image, a memory snapshot) as a new
    /// version of `computer`.
    pub fn save_file(&self, computer: &str, file: &Path) -> Result<Saved, String> {
        check_computer(computer)?;
        let started = Instant::now();
        let shown = file.display();
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("can't save {shown}: its name isn't UTF-8"))?;
        let meta = fs::symlink_metadata(file).map_err(|e| format!("can't save {shown}: {e}"))?;
        if !meta.is_file() {
            return Err(format!("can't save {shown}: it isn't a regular file"));
        }
        let mut opened = File::open(file).map_err(|e| format!("can't save {shown}: {e}"))?;
        let now = opened
            .metadata()
            .map_err(|e| format!("can't save {shown}: {e}"))?;
        if (now.dev(), now.ino()) != (meta.dev(), meta.ino()) {
            return Err(format!(
                "can't save {shown}: it changed while it was being saved"
            ));
        }
        let version = self.next_version(computer)?;
        let mut upload = self.upload(computer);
        let read = upload.file(&mut opened, &shown.to_string());
        let uploaded = upload.sending.finish();
        let ((size, chunks), uploaded) = (read?, uploaded?);
        let manifest = Manifest {
            format: FORMAT,
            computer: computer.to_owned(),
            version: version.clone(),
            chunk_size: CHUNK_SIZE as u64,
            of: Of::File,
            root: None,
            entries: vec![Entry {
                path: name.to_owned(),
                stamp: Stamp::new(meta.mode(), meta.mtime(), meta.mtime_nsec()),
                kind: Kind::File { size, chunks },
            }],
        };
        self.commit(&manifest, started)?;
        Ok(Saved {
            version,
            files: upload.files,
            bytes: upload.bytes,
            uploaded,
            skipped: Vec::new(),
        })
    }

    /// Puts `version` of `computer`, a tree, back into `dir`, which must be
    /// empty or absent.
    pub fn restore_tree(&self, computer: &str, version: &str, dir: &Path) -> Result<(), String> {
        let manifest = self.manifest(computer, version)?;
        if manifest.of != Of::Tree {
            return Err(format!(
                "version {version} of {computer} is one file, not a folder tree"
            ));
        }
        tree::restore(&manifest, dir, &|file, size, chunks, path| {
            self.write_chunks(file, size, manifest.chunk_size, chunks, path)
        })
    }

    /// Puts `version` of `computer`, one file, back at `file`, replacing
    /// what's there once it's whole. Its zeros are left as holes.
    pub fn restore_file(&self, computer: &str, version: &str, file: &Path) -> Result<(), String> {
        let manifest = self.manifest(computer, version)?;
        let (Of::File, [entry]) = (manifest.of, manifest.entries.as_slice()) else {
            return Err(format!(
                "version {version} of {computer} is a folder tree, not one file"
            ));
        };
        let Kind::File { size, chunks } = &entry.kind else {
            return Err(format!("version {version} of {computer} holds no file"));
        };
        let shown = file.display().to_string();
        let name = file
            .file_name()
            .ok_or_else(|| format!("can't restore {shown}: it names no file"))?
            .to_string_lossy();
        let parent = file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| format!("can't restore {shown}: {e}"))?;
        let temp = parent.join(format!(".{name}.restoring-{}", unique()));
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| format!("can't restore {shown}: {e}"))
            .and_then(|out| {
                self.write_chunks(&out, *size, manifest.chunk_size, chunks, &shown)?;
                out.set_len(*size)
                    .and_then(|()| entry.stamp.apply(&out))
                    .and_then(|()| fs::rename(&temp, file))
                    .map_err(|e| format!("can't restore {shown}: {e}"))
            });
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written
    }

    /// `computer`'s versions, oldest first.
    pub fn versions(&self, computer: &str) -> Result<Vec<String>, String> {
        check_computer(computer)?;
        let prefix = format!("computers/{computer}/versions/");
        let mut versions: Vec<String> = self
            .blobs
            .list(&prefix)?
            .into_iter()
            .filter_map(|listed| {
                let version = listed.key.strip_prefix(&prefix)?.strip_suffix(".json")?;
                valid_version(version).then(|| version.to_owned())
            })
            .collect();
        versions.sort();
        Ok(versions)
    }

    /// `computer`'s newest version, if it has one.
    pub fn latest(&self, computer: &str) -> Result<Option<String>, String> {
        Ok(self.versions(computer)?.pop())
    }

    /// Forgets `version` of `computer`. Its chunks go at the next GC, unless
    /// another version refers to them.
    pub fn remove_version(&self, computer: &str, version: &str) -> Result<(), String> {
        check_computer(computer)?;
        if !valid_version(version) {
            return Err(format!("{version:?} isn't a version"));
        }
        self.blobs.delete(&manifest_key(computer, version))
    }

    /// Keeps `computer`'s record beside its versions: what a host that
    /// never had it needs to list it and wake it.
    pub fn put_record(&self, computer: &str, record: &[u8]) -> Result<(), String> {
        check_computer(computer)?;
        self.blobs.put(&record_key(computer), record)
    }

    /// The records of the computers whose ids start with `prefix`: each
    /// one's id and record.
    pub fn records(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
        check_computer(prefix)?;
        let mut out = Vec::new();
        for listed in self.blobs.list(&format!("computers/{prefix}"))? {
            let Some(computer) = listed
                .key
                .strip_prefix("computers/")
                .and_then(|rest| rest.strip_suffix("/record.json"))
                .filter(|c| !c.contains('/') && check_computer(c).is_ok())
            else {
                continue;
            };
            // Removed since it was listed.
            if let Some(record) = self.blobs.get(&listed.key)? {
                out.push((computer.to_owned(), record));
            }
        }
        Ok(out)
    }

    /// Forgets `computer` and every version of it. Its chunks go at the
    /// next GC, unless another computer's versions refer to them.
    pub fn remove(&self, computer: &str) -> Result<(), String> {
        check_computer(computer)?;
        for listed in self.blobs.list(&format!("computers/{computer}/"))? {
            self.blobs.delete(&listed.key)?;
        }
        Ok(())
    }

    /// Deletes every chunk no manifest refers to and older than the grace
    /// period. A manifest it can't read stops it before it deletes anything:
    /// it may refer to chunks.
    pub fn gc(&self) -> Result<Collected, String> {
        let mut referenced: HashSet<String> = HashSet::new();
        for listed in self.blobs.list("computers/")? {
            if !is_manifest_key(&listed.key) {
                continue;
            }
            // Removed since it was listed.
            let Some(bytes) = self.blobs.get(&listed.key)? else {
                continue;
            };
            let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| {
                format!(
                    "GC stopped, having deleted nothing: {} can't be read ({e}), and it may \
                     refer to chunks",
                    listed.key
                )
            })?;
            referenced.extend(manifest.chunk_names().map(str::to_owned));
        }
        let now = SystemTime::now();
        let mut collected = Collected::default();
        for chunk in self.blobs.list("chunks/")? {
            let name = chunk.key.strip_prefix("chunks/").unwrap_or_default();
            let young = match now.duration_since(chunk.modified) {
                Ok(age) => age < self.grace,
                // Written after now, by this clock.
                Err(_) => true,
            };
            if referenced.contains(name) || young {
                collected.kept += 1;
                continue;
            }
            self.blobs.delete(&chunk.key)?;
            collected.deleted += 1;
            collected.freed += chunk.size;
        }
        Ok(collected)
    }

    /// A save of `computer` about to start. The chunks its latest version
    /// has are in the store, so they aren't asked about again: most of a
    /// computer is as it last slept, and each chunk asked about was a
    /// request to S3 that said "yes". (That version is still there, so GC
    /// keeps what it names. If it can't be read, every chunk is asked
    /// about, as before.)
    fn upload(&self, computer: &str) -> Upload {
        let stored = self
            .latest(computer)
            .ok()
            .flatten()
            .and_then(|version| self.manifest(computer, &version).ok())
            .map(|manifest| manifest.chunk_names().map(str::to_owned).collect())
            .unwrap_or_default();
        Upload {
            stored,
            sending: Arc::new(Sending {
                blobs: Arc::clone(&self.blobs),
                state: Mutex::new(SendState::default()),
                turn: Condvar::new(),
            }),
            buf: Vec::new(),
            files: 0,
            bytes: 0,
        }
    }

    /// A new version id for `computer`: now, unless its latest version is
    /// as late (a clock set back), and never the same twice.
    fn next_version(&self, computer: &str) -> Result<String, String> {
        static LAST: AtomicU64 = AtomicU64::new(0);
        let floor = self
            .latest(computer)?
            .as_deref()
            .and_then(version_nanos)
            .map_or(0, |n| n.saturating_add(1));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        let mut next = 0;
        let _ = LAST.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |last| {
            next = now.max(last.saturating_add(1)).max(floor);
            Some(next)
        });
        Ok(version_id(next))
    }

    /// Writes the manifest, last, if the save was quick enough that GC
    /// can't have collected what it uploaded.
    fn commit(&self, manifest: &Manifest, started: Instant) -> Result<(), String> {
        if started.elapsed() >= self.grace {
            return Err(format!(
                "saving {} took longer than GC leaves new chunks alone ({} minutes), so \
                 nothing was saved; saving again reuses what was uploaded",
                manifest.computer,
                self.grace.as_secs() / 60
            ));
        }
        let json = serde_json::to_vec(manifest)
            .map_err(|e| format!("can't write the manifest of {}: {e}", manifest.computer))?;
        self.blobs
            .put(&manifest_key(&manifest.computer, &manifest.version), &json)
    }

    /// `version` of `computer`'s manifest, checked.
    fn manifest(&self, computer: &str, version: &str) -> Result<Manifest, String> {
        check_computer(computer)?;
        if !valid_version(version) {
            return Err(format!("{version:?} isn't a version"));
        }
        let bytes = self
            .blobs
            .get(&manifest_key(computer, version))?
            .ok_or_else(|| format!("{computer} has no version {version}"))?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| format!("version {version} of {computer} can't be read: {e}"))?;
        manifest.check(computer, version)?;
        Ok(manifest)
    }

    /// Writes a file's chunks into `out` where they go, leaving zeros as
    /// holes.
    fn write_chunks(
        &self,
        out: &File,
        size: u64,
        chunk_size: u64,
        chunks: &[Option<String>],
        path: &str,
    ) -> Result<(), String> {
        for (i, chunk) in chunks.iter().enumerate() {
            let Some(name) = chunk else { continue };
            let at = i as u64 * chunk_size;
            let bytes = self.fetch(name, chunk_size.min(size - at), path)?;
            out.write_all_at(&bytes, at)
                .map_err(|e| format!("can't restore {path}: {e}"))?;
        }
        Ok(())
    }

    /// A chunk, checked against its name as it's read.
    fn fetch(&self, name: &str, len: u64, path: &str) -> Result<Vec<u8>, String> {
        let bytes = self
            .blobs
            .get(&chunk_key(name))?
            .ok_or_else(|| format!("can't restore {path}: its chunk {name} isn't in the store"))?;
        if bytes.len() as u64 != len || hex::encode(Sha256::digest(&bytes)) != name {
            return Err(format!(
                "can't restore {path}: its chunk {name} doesn't match its SHA-256, so the \
                 store's copy is damaged or was changed"
            ));
        }
        Ok(bytes)
    }
}

/// Letters, digits and `-`, as a computer's id is.
fn check_computer(computer: &str) -> Result<(), String> {
    let fits = (1..=64).contains(&computer.len())
        && !computer.starts_with('-')
        && computer
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if fits {
        Ok(())
    } else {
        Err(format!(
            "{computer:?} isn't a computer's id: letters, digits and hyphens"
        ))
    }
}

fn chunk_key(name: &str) -> String {
    format!("chunks/{name}")
}

fn record_key(computer: &str) -> String {
    format!("computers/{computer}/record.json")
}

fn manifest_key(computer: &str, version: &str) -> String {
    format!("computers/{computer}/versions/{version}.json")
}

fn is_manifest_key(key: &str) -> bool {
    matches!(
        key.split('/').collect::<Vec<_>>().as_slice(),
        ["computers", _, "versions", name] if name.ends_with(".json")
    )
}

/// `20261027T153012.123456789Z`: a time to the nanosecond, in UTC, that
/// sorts as text as it does as a time.
fn version_id(nanos: u64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
        .ok()
        .and_then(|t| {
            t.format(format_description!(
                "[year][month][day]T[hour][minute][second].[subsecond digits:9]Z"
            ))
            .ok()
        })
        .unwrap_or_default()
}

fn version_nanos(version: &str) -> Option<u64> {
    let time = PrimitiveDateTime::parse(
        version,
        format_description!("[year][month][day]T[hour][minute][second].[subsecond digits:9]Z"),
    )
    .ok()?
    .assume_utc();
    u64::try_from(time.unix_timestamp_nanos()).ok()
}

fn valid_version(version: &str) -> bool {
    version_nanos(version).is_some_and(|n| version_id(n) == version)
}

/// A name no other write in this process, or any other, is using.
fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}
