//! The on-disk log: one JSON object per line, each carrying the hash of its
//! body, and each body carrying the hash of the record before it.
//!
//! ```text
//! {"hash":"<sha256 of body bytes>","body":{"seq":7,"ts":"…","prev":"<hash of record 6>","event":{…}}}
//! ```
//!
//! The hash covers the body exactly as written, so verification never depends
//! on re-serializing JSON the same way. Editing, dropping or reordering any
//! record breaks the chain from that point on.
//!
//! The log is written to one file and kept as several (audit 4.7): once the
//! file is a month old or 32 MiB ([`Segments`]) it is closed as a
//! **segment**, renamed beside the log as `<log>.seg-<its first record's
//! seq>`, and a new file begun whose first record chains onto the
//! segment's last. Nothing in a segment changes again. Reading the log
//! ([`verify`], [`read`], [`read_since`], a task's records) reads the
//! segments and the file as one chain.
//!
//! Segments are what a retention rule removes ([`AuditLog::trim`]): whole
//! ones, oldest first, each only once its newest record is older than the
//! rule. A `log_trimmed` record is written first, saying through which
//! record the log was removed and that record's hash. The oldest record
//! still kept chains onto exactly that, so the log still verifies, and
//! removing records any other way still shows.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use glass_types::{TaskId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::{AuditEvent, EventKind};

/// `prev` of the first record in a log.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("audit log {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("audit log {path} is corrupt at line {line}: {reason}")]
    Corrupt {
        path: PathBuf,
        line: u64,
        reason: String,
    },
    #[error(
        "audit log writer is unusable after an earlier failure; the next record opens it again"
    )]
    Poisoned,
}

/// A record as stored: sequence number, time, chain link and event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub seq: u64,
    pub ts: Timestamp,
    pub prev: String,
    pub event: AuditEvent,
}

#[derive(Serialize)]
struct LineOut<'a> {
    hash: &'a str,
    body: &'a RawValue,
}

#[derive(Deserialize)]
struct LineIn<'a> {
    hash: String,
    #[serde(borrow)]
    body: &'a RawValue,
}

/// When the file the log is written to is closed as a segment and a new
/// one begun: at the first record written after either is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segments {
    /// It holds this many bytes.
    pub bytes: u64,
    /// Its first record is this old.
    pub age: Duration,
}

impl Default for Segments {
    /// A month, or 32 MiB: retention then removes about a month at a
    /// time, and no one file takes long to read.
    fn default() -> Self {
        Self {
            bytes: 32 * 1024 * 1024,
            age: Duration::from_secs(30 * 24 * 3600),
        }
    }
}

/// Why the file wasn't closed as a segment.
enum Unsealed {
    /// It is as it was, and goes on taking records.
    Kept(String),
    /// It was renamed, and no new file could be made in its place.
    Gone(AuditError),
}

/// What [`AuditLog::trim`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trimmed {
    /// How many records.
    pub records: u64,
    /// The last record removed, and its hash.
    pub through_seq: u64,
    pub last_hash: String,
    /// When the newest record removed was written.
    pub newest: Timestamp,
    /// The segments' file names.
    pub segments: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// fsync after every record. The default for production.
    Sync,
    /// Leave flushing to the OS. For tests and development only.
    Buffered,
}

/// An append-only, hash-chained audit log file.
///
/// Appends are serialized through a mutex; each record is written with one
/// `write` call and, by default, fsynced before `append` returns. A record is
/// acknowledged only once `append` returns `Ok`.
///
/// A write that fails (a full disk) may leave part of its line behind, and
/// one whose fsync fails leaves all of it. The writer never chains onto
/// either: the next append opens the file again as a start does, cutting
/// off everything past the last record it acknowledged and noting it, so
/// the log takes records again once there's room and holds no record whose
/// append failed (ADR 0053, `specs/tla/AuditLog.tla`).
///
/// A last record that doesn't match its own hash (damage: the writer never
/// leaves one) is set aside in a file beside the log and noted, and the
/// chain goes on from the record before it, where glassd used to refuse to
/// start for good. Nothing is lost that the chain protected: the end of a
/// hash chain can be cut off without a trace by whoever can write the
/// file, and what was there is kept. Two damaged records at the end are
/// more than a damaged end, and still refused.
///
/// A task's records are found through an index of where each task's lines
/// start, built by one pass the first time they're asked for and kept up
/// by every append after: reading one task's records no longer reads the
/// whole log, which grows for as long as the computer is used.
///
/// Records can share a disk sync: [`AuditLog::append_all`] writes a run of
/// them as one run of lines with one sync. A run is one write: it is all in
/// the log, or, after a failure, none of it is (cut back as one record is).
/// The runtime writes whatever is waiting as a run (`Runtime::audit`), so
/// records asked for while a sync is under way share the next one; each
/// used to have a sync of its own, one after another whoever asked.
///
/// The file is closed as a segment and a new one begun as [`Segments`]
/// says, under the writer's lock, between two runs: a segment ends at a
/// record that was acknowledged.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    durability: Durability,
    segments: Segments,
    /// How many times the file was closed as a segment since this log was
    /// opened: an index being built notices one that happened meanwhile.
    seals: AtomicU64,
    /// Why the file couldn't be closed as a segment when it was last due,
    /// if it couldn't. It goes on taking records meanwhile.
    unsealed: Mutex<Option<String>>,
    /// How many runs the log has written, each with one sync where it
    /// syncs at all (for tests and measurements).
    runs: AtomicU64,
    /// Tests only: milliseconds each run takes at least to reach the disk,
    /// as a busy disk's does.
    slow: AtomicU64,
    /// Tests only: runs wait at the disk while this is set, so a test can
    /// have records arrive during one.
    held: Mutex<bool>,
    let_go: Condvar,
    state: Mutex<Option<WriterState>>,
    /// After a failed write: where the last acknowledged record ends.
    /// Whatever the file holds past it was never acknowledged, and the
    /// next opening cuts it off.
    cut_to: Mutex<Option<u64>>,
    /// The file a damaged last record was set aside in when this log was
    /// opened, for whoever opened it to say so.
    set_aside: Option<String>,
    index: Mutex<Option<TaskIndex>>,
    /// Tests only: writes to fail, and how.
    failing: Mutex<Failing>,
}

/// Tests only: after `skip` more writes, the next `writes` fail.
#[derive(Debug, Default, Clone, Copy)]
struct Failing {
    skip: u32,
    writes: u32,
    /// As a failed fsync does, with the whole line written; else as a full
    /// disk does, with half of it.
    whole: bool,
}

/// What opening the log had to put right.
#[derive(Debug, Default)]
struct Recovered {
    /// Bytes past the last good record, cut off: a write cut short, or one
    /// never acknowledged.
    dropped: u64,
    /// The file a damaged last record was kept in, beside the log.
    set_aside: Option<String>,
}

impl Recovered {
    fn anything(&self) -> bool {
        self.dropped > 0 || self.set_aside.is_some()
    }
}

#[derive(Debug)]
struct WriterState {
    file: File,
    next_seq: u64,
    last_hash: String,
    /// The file's length: where the next record starts.
    len: u64,
    /// The file's first record: its seq and when it was written. `None`
    /// while the file is empty.
    first: Option<(u64, Timestamp)>,
}

/// Where each task's records start: in the file, up to `to`, and in each
/// segment.
#[derive(Debug, Default)]
struct TaskIndex {
    to: u64,
    by_task: HashMap<TaskId, Vec<u64>>,
    /// The segments' own, oldest first.
    sealed: Vec<SealedTasks>,
}

/// Where each task's records start in one segment.
#[derive(Debug, Clone)]
struct SealedTasks {
    path: PathBuf,
    by_task: HashMap<TaskId, Vec<u64>>,
}

impl AuditLog {
    /// Opens (or creates) the log at `path` and positions after its last
    /// record.
    ///
    /// If the file ends in a partial line, that write was never acknowledged
    /// (it was cut short by a crash). It is truncated and a `log_recovered`
    /// record notes how many bytes were dropped. A complete last record
    /// that doesn't match its hash is set aside beside the log and noted
    /// the same way.
    pub fn open(path: impl Into<PathBuf>, durability: Durability) -> Result<Self, AuditError> {
        Self::open_with(path, durability, Segments::default())
    }

    /// [`AuditLog::open`], with when the file is closed as a segment.
    pub fn open_with(
        path: impl Into<PathBuf>,
        durability: Durability,
        segments: Segments,
    ) -> Result<Self, AuditError> {
        let path = path.into();
        let (state, recovery) = open_writer(&path, None)?;
        let log = Self {
            path,
            durability,
            segments,
            seals: AtomicU64::new(0),
            unsealed: Mutex::new(None),
            runs: AtomicU64::new(0),
            slow: AtomicU64::new(0),
            held: Mutex::new(false),
            let_go: Condvar::new(),
            state: Mutex::new(Some(state)),
            cut_to: Mutex::new(None),
            set_aside: recovery.set_aside.clone(),
            index: Mutex::new(None),
            failing: Mutex::new(Failing::default()),
        };
        if recovery.anything() {
            log.append(recovered(&recovery))?;
        }
        Ok(log)
    }

    /// Tests only: after `skip` more writes, the next `writes` fail as on a
    /// full disk, each after half its line has landed.
    #[doc(hidden)]
    pub fn fail_writes_for_tests(&self, skip: u32, writes: u32) {
        *self
            .failing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Failing {
            skip,
            writes,
            whole: false,
        };
    }

    /// Tests only: after `skip` more writes, the next `writes` fail as a
    /// failed fsync does: the whole line is in the file, and the append is
    /// an error.
    #[doc(hidden)]
    pub fn fail_syncs_for_tests(&self, skip: u32, writes: u32) {
        *self
            .failing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Failing {
            skip,
            writes,
            whole: true,
        };
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Why the log's file couldn't be closed as a segment when it was last
    /// due, if it couldn't (something is where the segment goes; the
    /// folder won't take a rename). The file goes on taking records, and
    /// is closed once it can be: nothing is lost, and the log isn't
    /// stopped for it.
    pub fn unsealed(&self) -> Option<String> {
        self.unsealed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The name of the file, beside the log, that a damaged last record
    /// was set aside in when this log was opened; `None` if it wasn't
    /// damaged. The log's own `log_recovered` record says the same.
    pub fn set_aside_at_open(&self) -> Option<&str> {
        self.set_aside.as_deref()
    }

    /// How many runs the log has written since it was opened: one write and
    /// (where it syncs) one sync each.
    #[doc(hidden)]
    pub fn runs_for_tests(&self) -> u64 {
        self.runs.load(Ordering::SeqCst)
    }

    /// Tests only: every run from now on takes at least `millis` to reach
    /// the disk, as on a disk busy with something else.
    #[doc(hidden)]
    pub fn slow_syncs_for_tests(&self, millis: u64) {
        self.slow.store(millis, Ordering::SeqCst);
    }

    /// Tests only: runs wait at the disk from now until this is called
    /// with `false`.
    #[doc(hidden)]
    pub fn hold_syncs_for_tests(&self, hold: bool) {
        *self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = hold;
        self.let_go.notify_all();
    }

    /// Appends one event and returns the stored record, once it is synced.
    /// After a failed write, the file is opened again first, as at a start.
    pub fn append(&self, event: AuditEvent) -> Result<AuditRecord, AuditError> {
        let mut records = self.append_all(vec![event])?;
        records.pop().ok_or(AuditError::Poisoned)
    }

    /// Appends `events` in order as one run: one write and one sync for all
    /// of them. All are in the log when this returns `Ok`, each with its
    /// record; after an `Err` none is, and the caller writes them again.
    /// After a failed write, the file is opened again first, as at a start.
    ///
    /// Waits for the disk, and for a run being written: call it where
    /// waiting is allowed (the runtime's `blocking`).
    pub fn append_all(&self, events: Vec<AuditEvent>) -> Result<Vec<AuditRecord>, AuditError> {
        let mut guard = self.state.lock().map_err(|_| AuditError::Poisoned)?;
        if guard.is_none() {
            let cut_to = *self
                .cut_to
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (state, recovery) = open_writer(&self.path, cut_to)?;
            *guard = Some(state);
            if recovery.anything() {
                self.write(&mut guard, vec![recovered(&recovery)])?;
            }
        }
        self.write(&mut guard, events)
    }

    /// A run of records onto the writer in `guard`, as one write and one
    /// sync: all of them are in the log, or none is. A failed write leaves
    /// `guard` empty: the file may now end in part of the run, and the
    /// next append opens it again rather than chain onto that.
    fn write(
        &self,
        guard: &mut Option<WriterState>,
        events: Vec<AuditEvent>,
    ) -> Result<Vec<AuditRecord>, AuditError> {
        // Closed as a segment first, if it's due: between two runs, so a
        // segment ends at a record that was acknowledged.
        if let Some(state) = guard.as_mut()
            && self.due(state)
        {
            let sealed = self.seal(state);
            let mut unsealed = self
                .unsealed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match sealed {
                Ok(()) => *unsealed = None,
                // The file is as it was: it takes this run too, and is
                // closed when it can be.
                Err(Unsealed::Kept(why)) => *unsealed = Some(why),
                // Renamed, and no new file could be made: there is nothing
                // to write to, and nothing to cut back to when one is.
                Err(Unsealed::Gone(e)) => {
                    drop(unsealed);
                    *self
                        .cut_to
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    *guard = None;
                    return Err(e);
                }
            }
        }
        let state = guard.as_mut().ok_or(AuditError::Poisoned)?;
        let unwritable = |e: serde_json::Error| AuditError::Io {
            path: self.path.clone(),
            source: io::Error::other(e),
        };
        let mut seq = state.next_seq;
        let mut prev = state.last_hash.clone();
        let mut lines = String::new();
        // Each record with how long its line is.
        let mut records: Vec<(AuditRecord, usize)> = Vec::with_capacity(events.len());
        // Tests only: how much of the run lands before the write fails.
        let mut landed: Option<usize> = None;
        for event in events {
            let record = AuditRecord {
                seq,
                ts: Timestamp::now(),
                prev,
                event,
            };
            let body = serde_json::to_string(&record).map_err(unwritable)?;
            let hash = hash_body(&body);
            let raw = RawValue::from_string(body).map_err(unwritable)?;
            let mut line = serde_json::to_string(&LineOut {
                hash: &hash,
                body: &raw,
            })
            .map_err(unwritable)?;
            line.push('\n');
            if landed.is_none() {
                let mut failing = self
                    .failing
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match *failing {
                    Failing { writes: 0, .. } => {}
                    Failing {
                        skip: 0,
                        writes,
                        whole,
                    } => {
                        failing.writes = writes - 1;
                        let of_this = if whole { line.len() } else { line.len() / 2 };
                        landed = Some(lines.len() + of_this);
                    }
                    Failing { skip, .. } => failing.skip = skip - 1,
                }
            }
            lines.push_str(&line);
            records.push((record, line.len()));
            seq += 1;
            prev = hash;
        }
        let written = match landed {
            Some(landed) => state
                .file
                .write_all(&lines.as_bytes()[..landed])
                .and_then(|()| {
                    Err(io::Error::new(
                        io::ErrorKind::StorageFull,
                        "no space left on device (a test's)",
                    ))
                }),
            None => state.file.write_all(lines.as_bytes()),
        }
        .and_then(|()| {
            self.runs.fetch_add(1, Ordering::SeqCst);
            // Tests only: a disk that is slow, or that waits to be let go.
            let slow = self.slow.load(Ordering::SeqCst);
            if slow > 0 {
                std::thread::sleep(std::time::Duration::from_millis(slow));
            }
            let mut held = self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while *held {
                held = self
                    .let_go
                    .wait(held)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            drop(held);
            match self.durability {
                Durability::Sync => state.file.sync_data(),
                Durability::Buffered => Ok(()),
            }
        });
        if let Err(source) = written {
            // Part of the run may be in the file, or all of it (the fsync
            // failed): none of it was acknowledged, and each caller is told
            // it failed and writes its record again. Where the last
            // acknowledged record ends is kept, and the next opening cuts
            // back to it; a whole line left there was chained onto, and
            // stood in the log beside the second writing of the same
            // record.
            *self
                .cut_to
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(state.len);
            *guard = None;
            return Err(AuditError::Io {
                path: self.path.clone(),
                source,
            });
        }
        // Kept up while the writer is held, so it never misses a record.
        let mut index = self.index.lock().ok();
        let mut index = index.as_mut().and_then(|index| index.as_mut());
        let mut stored = Vec::with_capacity(records.len());
        for (record, len) in records {
            let at = state.len;
            state.len += len as u64;
            if let Some(index) = index.as_mut() {
                if let Some(task) = record.event.task {
                    index.by_task.entry(task).or_default().push(at);
                }
                index.to = state.len;
            }
            stored.push(record);
        }
        state.next_seq = seq;
        state.last_hash = prev;
        if state.first.is_none() {
            state.first = stored.first().map(|record| (record.seq, record.ts));
        }
        Ok(stored)
    }

    /// Whether the file is due to be closed as a segment.
    fn due(&self, state: &WriterState) -> bool {
        let Some((_, since)) = state.first else {
            return false;
        };
        state.len >= self.segments.bytes
            || Timestamp::now().seconds_since(since) >= self.segments.age.as_secs()
    }

    /// Closes the file as a segment and begins a new one: the file, whole
    /// and on disk, is renamed to its segment's name, and an empty file
    /// takes its place. The chain goes on in the new file from the
    /// segment's last record.
    ///
    /// Stopped between the two, there is no file: the next opening makes
    /// one, and reads where the chain stands from the newest segment
    /// (`chain_before`).
    fn seal(&self, state: &mut WriterState) -> Result<(), Unsealed> {
        let io_err = |source| AuditError::Io {
            path: self.path.clone(),
            source,
        };
        let Some((first, _)) = state.first else {
            return Ok(());
        };
        let kept = |what: &str, e: io::Error| Unsealed::Kept(format!("{what}: {e}"));
        state
            .file
            .sync_all()
            .map_err(|e| kept("it couldn't be made sure of on disk first", e))?;
        let permissions = state
            .file
            .metadata()
            .map_err(|e| kept("it couldn't be looked at", e))?
            .permissions();
        let segment = segment_path(&self.path, first);
        // A rename replaces what's there: a file by this name is not one
        // this log is about to make, and is never written over.
        if segment.exists() {
            return Err(Unsealed::Kept(format!(
                "{} is there already, and isn't written over",
                segment.display()
            )));
        }
        fs::rename(&self.path, &segment)
            .map_err(|e| kept(&format!("renaming it to {}", segment.display()), e))?;
        self.seals.fetch_add(1, Ordering::SeqCst);
        // The index's records of the file are the segment's now.
        if let Ok(mut index) = self.index.lock()
            && let Some(index) = index.as_mut()
        {
            let by_task = std::mem::take(&mut index.by_task);
            index.to = 0;
            write_tasks(&segment, state.len, &by_task);
            index.sealed.push(SealedTasks {
                path: segment,
                by_task,
            });
        }
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|file| file.set_permissions(permissions).map(|()| file))
            .map_err(|e| Unsealed::Gone(io_err(e)))?;
        sync_dir(&self.path);
        state.file = file;
        state.len = 0;
        state.first = None;
        Ok(())
    }

    /// Removes the segments whose every record was written before
    /// `before`, oldest first: a retention rule, which `rule` names for
    /// the record ("400 days"). The file being written is never removed.
    ///
    /// A `log_trimmed` record is written, and on disk, before anything
    /// goes: through which record the log is removed, and that record's
    /// hash, which is what the oldest record kept chains onto. The
    /// segments then go one at a time, oldest first. Stopped anywhere
    /// after the record, the log verifies as it is (what's still here is
    /// covered by the record), and the next call removes the rest, with a
    /// record of its own.
    pub fn trim(&self, before: Timestamp, rule: &str) -> Result<Option<Trimmed>, AuditError> {
        let mut going: Vec<(u64, PathBuf)> = Vec::new();
        let mut last: Option<(u64, String, Timestamp)> = None;
        for (first, segment) in sealed(&self.path)? {
            // A segment's last record is its newest.
            let (hash, record) = last_record(&segment)?;
            if record.ts.unix_millis() >= before.unix_millis() {
                break;
            }
            going.push((first, segment));
            last = Some((record.seq, hash, record.ts));
        }
        let (Some((through_seq, last_hash, newest)), Some((first, _))) = (last, going.first())
        else {
            return Ok(None);
        };
        let trimmed = Trimmed {
            records: through_seq + 1 - first,
            through_seq,
            last_hash,
            newest,
            segments: going.iter().map(|(_, segment)| log_name(segment)).collect(),
        };
        let mut event = AuditEvent::new(
            EventKind::LogTrimmed,
            format!(
                "removed {} records older than {rule}: all through record {through_seq}",
                trimmed.records
            ),
        );
        event.detail = serde_json::json!({
            "through_seq": trimmed.through_seq,
            "last_hash": trimmed.last_hash,
            "records": trimmed.records,
            "newest": trimmed.newest,
            "segments": trimmed.segments,
            "rule": rule,
        });
        self.append(event)?;
        for (_, segment) in &going {
            match fs::remove_file(segment) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(AuditError::Io {
                        path: segment.clone(),
                        source,
                    });
                }
            }
            let _ = fs::remove_file(tasks_path(segment));
        }
        sync_dir(&self.path);
        if let Ok(mut index) = self.index.lock()
            && let Some(index) = index.as_mut()
        {
            index
                .sealed
                .retain(|sealed| !going.iter().any(|(_, gone)| gone == &sealed.path));
        }
        Ok(Some(trimmed))
    }

    /// `task`'s records, oldest first, each checked against its own hash.
    /// The chain as a whole is [`verify`]'s to check.
    pub fn task_records(&self, task: TaskId) -> Result<Vec<AuditRecord>, AuditError> {
        let mut out = Vec::new();
        let mut line = String::new();
        for (path, offsets) in self.places(task)? {
            let io_err = |source| AuditError::Io {
                path: path.clone(),
                source,
            };
            let file = match File::open(&path) {
                Ok(file) => file,
                // A segment the retention rule removed meanwhile.
                Err(e) if e.kind() == io::ErrorKind::NotFound && path != self.path => continue,
                Err(e) => return Err(io_err(e)),
            };
            let mut reader = BufReader::new(file);
            for at in offsets {
                reader.seek(SeekFrom::Start(at)).map_err(io_err)?;
                line.clear();
                reader.read_line(&mut line).map_err(io_err)?;
                let (_, record) = parse_line(line.trim_end_matches('\n')).map_err(|reason| {
                    AuditError::Corrupt {
                        path: path.clone(),
                        line: 0,
                        reason: format!("at byte {at}: {reason}"),
                    }
                })?;
                out.push(record);
            }
        }
        Ok(out)
    }

    /// Where `task`'s records are: each file that has some, oldest first,
    /// and where each record starts in it.
    ///
    /// The index is built the first time: the long part without holding
    /// the writer (each segment's tasks, kept beside it once worked out,
    /// and the file so far), then what was appended meanwhile with it
    /// held, so the index is whole once it's in place. The file closed as
    /// a segment in the middle starts it again.
    fn places(&self, task: TaskId) -> Result<Vec<(PathBuf, Vec<u64>)>, AuditError> {
        loop {
            if let Some(index) = self
                .index
                .lock()
                .map_err(|_| AuditError::Poisoned)?
                .as_ref()
            {
                return Ok(index.places(&self.path, task));
            }
            let seals = self.seals.load(Ordering::SeqCst);
            let mut index = TaskIndex::default();
            for (_, segment) in sealed(&self.path)? {
                match tasks_of(&segment) {
                    Ok(by_task) => index.sealed.push(SealedTasks {
                        path: segment,
                        by_task,
                    }),
                    // Removed by the retention rule since it was listed.
                    Err(AuditError::Io { source, .. })
                        if source.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            let len = self.current_len()?;
            self.scan(&mut index, len)?;
            let guard = self.state.lock().map_err(|_| AuditError::Poisoned)?;
            if self.seals.load(Ordering::SeqCst) != seals {
                continue;
            }
            let len = guard.as_ref().ok_or(AuditError::Poisoned)?.len;
            self.scan(&mut index, len)?;
            let mut slot = self.index.lock().map_err(|_| AuditError::Poisoned)?;
            let found = index.places(&self.path, task);
            *slot = Some(index);
            drop(slot);
            drop(guard);
            return Ok(found);
        }
    }

    fn current_len(&self) -> Result<u64, AuditError> {
        let guard = self.state.lock().map_err(|_| AuditError::Poisoned)?;
        Ok(guard.as_ref().ok_or(AuditError::Poisoned)?.len)
    }

    /// Adds to `index` the file's records from where it got to up to `to`.
    fn scan(&self, index: &mut TaskIndex, to: u64) -> Result<(), AuditError> {
        index.to = scan_tasks(&self.path, index.to, to, &mut index.by_task)?;
        Ok(())
    }
}

impl TaskIndex {
    /// Where `task`'s records are, oldest file first; `file` is the one
    /// being written.
    fn places(&self, file: &Path, task: TaskId) -> Vec<(PathBuf, Vec<u64>)> {
        let mut places: Vec<(PathBuf, Vec<u64>)> = self
            .sealed
            .iter()
            .filter_map(|sealed| Some((sealed.path.clone(), sealed.by_task.get(&task)?.clone())))
            .collect();
        if let Some(offsets) = self.by_task.get(&task) {
            places.push((file.to_owned(), offsets.clone()));
        }
        places
    }
}

/// Adds to `by_task` where each task's records start in `path`, from byte
/// `from` up to `to`, and returns how far it got.
fn scan_tasks(
    path: &Path,
    from: u64,
    to: u64,
    by_task: &mut HashMap<TaskId, Vec<u64>>,
) -> Result<u64, AuditError> {
    #[derive(Deserialize)]
    struct Line<'a> {
        #[serde(borrow)]
        body: &'a RawValue,
    }
    #[derive(Deserialize)]
    struct Body {
        event: Event,
    }
    #[derive(Deserialize)]
    struct Event {
        #[serde(default)]
        task: Option<TaskId>,
    }
    let io_err = |source| AuditError::Io {
        path: path.to_owned(),
        source,
    };
    let mut reader = BufReader::new(File::open(path).map_err(io_err)?);
    reader.seek(SeekFrom::Start(from)).map_err(io_err)?;
    let mut at = from;
    let mut line = String::new();
    while at < to {
        line.clear();
        let read = reader.read_line(&mut line).map_err(io_err)?;
        if read == 0 {
            break;
        }
        let task = serde_json::from_str::<Line<'_>>(&line)
            .ok()
            .and_then(|l| serde_json::from_str::<Body>(l.body.get()).ok())
            .and_then(|b| b.event.task);
        if let Some(task) = task {
            by_task.entry(task).or_default().push(at);
        }
        at += read as u64;
    }
    Ok(at)
}

/// A file's name, as text.
fn log_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audit.log".to_owned())
}

/// The file the segment that starts at record `first` is kept in, beside
/// the log: `<log>.seg-<first, twelve digits>`. Names sort as their
/// records do.
fn segment_path(path: &Path, first: u64) -> PathBuf {
    path.with_file_name(format!("{}.seg-{first:012}", log_name(path)))
}

/// The log's segments, oldest first, each with its first record's seq.
fn sealed(path: &Path) -> Result<Vec<(u64, PathBuf)>, AuditError> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AuditError::Io {
                path: dir.to_owned(),
                source,
            });
        }
    };
    let prefix = format!("{}.seg-", log_name(path));
    let mut segments: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let first: u64 = name.to_str()?.strip_prefix(&prefix)?.parse().ok()?;
            let segment = segment_path(path, first);
            // Its name exactly: not its tasks' file, not a copy someone made.
            (segment.file_name() == Some(name.as_os_str())).then_some((first, segment))
        })
        .collect();
    segments.sort();
    Ok(segments)
}

/// A file's last record and its hash: a segment's, which ends whole.
fn last_record(path: &Path) -> Result<(String, AuditRecord), AuditError> {
    let io_err = |source| AuditError::Io {
        path: path.to_owned(),
        source,
    };
    let corrupt = |reason: String| AuditError::Corrupt {
        path: path.to_owned(),
        line: 0,
        reason,
    };
    let mut file = File::open(path).map_err(io_err)?;
    let len = file.metadata().map_err(io_err)?.len();
    let (last, torn) = read_tail(&mut file, len).map_err(io_err)?;
    if torn > 0 {
        return Err(corrupt(
            "a segment that ends in part of a record".to_owned(),
        ));
    }
    let (line, _) = last.ok_or_else(|| corrupt("a segment with no record in it".to_owned()))?;
    parse_line(&line).map_err(|reason| corrupt(format!("last record: {reason}")))
}

/// Where the chain stands before the file's first record: after the
/// newest segment's last record, or at the very start.
fn chain_before(path: &Path) -> Result<(u64, String), AuditError> {
    match sealed(path)?.pop() {
        Some((_, segment)) => {
            let (hash, record) = last_record(&segment)?;
            Ok((record.seq + 1, hash))
        }
        None => Ok((0, GENESIS.to_owned())),
    }
}

/// Where a segment's index of its tasks is kept: beside it.
fn tasks_path(segment: &Path) -> PathBuf {
    segment.with_file_name(format!("{}.tasks", log_name(segment)))
}

/// A segment's tasks as kept beside it: worked out once, since a segment
/// never changes. Only a help to reading: every record read through it is
/// still checked against its own hash, and `len` ties it to the segment
/// it was made from.
#[derive(Serialize, Deserialize)]
struct KeptTasks {
    len: u64,
    tasks: Vec<(TaskId, Vec<u64>)>,
}

/// Keeps a segment's tasks beside it, if it can.
fn write_tasks(segment: &Path, len: u64, by_task: &HashMap<TaskId, Vec<u64>>) {
    let kept = KeptTasks {
        len,
        tasks: by_task
            .iter()
            .map(|(task, at)| (*task, at.clone()))
            .collect(),
    };
    let Ok(bytes) = serde_json::to_vec(&kept) else {
        return;
    };
    let path = tasks_path(segment);
    let making = path.with_extension("tasks.new");
    let written = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&making)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| fs::rename(&making, &path));
    if written.is_err() {
        let _ = fs::remove_file(&making);
    }
}

/// Where each task's records start in a segment: from what's kept beside
/// it, or read from the segment (and kept).
fn tasks_of(segment: &Path) -> Result<HashMap<TaskId, Vec<u64>>, AuditError> {
    let len = fs::metadata(segment)
        .map_err(|source| AuditError::Io {
            path: segment.to_owned(),
            source,
        })?
        .len();
    let kept = fs::read(tasks_path(segment))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<KeptTasks>(&bytes).ok())
        .filter(|kept| kept.len == len);
    if let Some(kept) = kept {
        return Ok(kept.tasks.into_iter().collect());
    }
    let mut by_task = HashMap::new();
    scan_tasks(segment, 0, len, &mut by_task)?;
    write_tasks(segment, len, &by_task);
    Ok(by_task)
}

/// Makes a rename or a removal beside the log last through a power cut,
/// if it can.
fn sync_dir(path: &Path) {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
}

/// Opens the log's file for appending, after its last complete line: a
/// line that ends without its newline was cut short (a crash, or a full
/// disk) and never acknowledged, so it's cut off. How many bytes were.
fn open_writer(path: &Path, cut_to: Option<u64>) -> Result<(WriterState, Recovered), AuditError> {
    let io_err = |source| AuditError::Io {
        path: path.to_owned(),
        source,
    };
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)
        .map_err(io_err)?;
    let mut recovery = Recovered::default();
    let mut len = file.metadata().map_err(io_err)?.len();
    // Past the last record this writer acknowledged: a write that failed.
    if let Some(at) = cut_to.filter(|&at| at < len) {
        file.set_len(at).map_err(io_err)?;
        recovery.dropped += len - at;
        len = at;
    }
    let (last, torn) = read_tail(&mut file, len).map_err(io_err)?;
    let (next_seq, last_hash) = match last {
        // Nothing in the file: the chain goes on from the newest segment,
        // if the log has any (the file was just closed as one).
        None => chain_before(path)?,
        Some((line, start)) => match parse_line(&line) {
            Ok((hash, record)) => (record.seq + 1, hash),
            // Damage at the very end. The record before it says where the
            // chain stands; if that one is damaged too, this isn't a
            // damaged end, and it is for a person to look at.
            Err(reason) => {
                let (before, _) = read_tail(&mut file, start).map_err(io_err)?;
                let chain = match before {
                    None => chain_before(path)?,
                    Some((line, _)) => {
                        let (hash, record) =
                            parse_line(&line).map_err(|second| AuditError::Corrupt {
                                path: path.to_owned(),
                                line: 0,
                                reason: format!(
                                    "last record: {reason}; and the one before it: {second}"
                                ),
                            })?;
                        (record.seq + 1, hash)
                    }
                };
                // Kept before it is cut: never the one without the other.
                recovery.set_aside = Some(set_aside(path, &mut file, start, len)?);
                file.set_len(start).map_err(io_err)?;
                recovery.dropped += len - start;
                len = start;
                chain
            }
        },
    };
    // A line without its newline: a write cut short, never acknowledged.
    if torn > 0 && recovery.set_aside.is_none() {
        file.set_len(len - torn).map_err(io_err)?;
        recovery.dropped += torn;
    }
    let len = file.metadata().map_err(io_err)?.len();
    // The file's first record says when it is due to be closed as a
    // segment, and what the segment is called. One that can't be read
    // leaves the file as it is, for `verify` to say why.
    let first = match len {
        0 => None,
        _ => {
            file.seek(SeekFrom::Start(0)).map_err(io_err)?;
            let mut line = String::new();
            BufReader::new(&file).read_line(&mut line).map_err(io_err)?;
            parse_line(line.trim_end_matches('\n'))
                .ok()
                .map(|(_, record)| (record.seq, record.ts))
        }
    };
    Ok((
        WriterState {
            file,
            next_seq,
            last_hash,
            len,
            first,
        },
        recovery,
    ))
}

/// Copies the log's bytes from `start` to `end` (a damaged last record,
/// and anything after it) into a new file beside it, for root alone, and
/// returns its name: `<log>.damaged-<unix seconds>`.
fn set_aside(path: &Path, file: &mut File, start: u64, end: u64) -> Result<String, AuditError> {
    let io_err = |source| AuditError::Io {
        path: path.to_owned(),
        source,
    };
    let log_name = log_name(path);
    let stamp = Timestamp::now().unix_seconds();
    let mut tries = 0u32;
    let (name, mut kept) = loop {
        let name = if tries == 0 {
            format!("{log_name}.damaged-{stamp}")
        } else {
            format!("{log_name}.damaged-{stamp}-{tries}")
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path.with_file_name(&name))
        {
            Ok(kept) => break (name, kept),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < 100 => tries += 1,
            Err(e) => return Err(io_err(e)),
        }
    };
    file.seek(SeekFrom::Start(start)).map_err(io_err)?;
    io::copy(&mut Read::by_ref(file).take(end - start), &mut kept).map_err(io_err)?;
    kept.sync_all().map_err(io_err)?;
    Ok(name)
}

/// The note of what opening the log put right.
fn recovered(recovery: &Recovered) -> AuditEvent {
    let dropped = recovery.dropped;
    match &recovery.set_aside {
        None => {
            let mut event = AuditEvent::new(
                EventKind::LogRecovered,
                format!("dropped {dropped} bytes of an unacknowledged partial record"),
            );
            event.detail = serde_json::json!({ "dropped_bytes": dropped });
            event
        }
        Some(name) => {
            let mut event = AuditEvent::new(
                EventKind::LogRecovered,
                format!(
                    "set aside {dropped} bytes: the last record didn't match its hash (kept in {name})"
                ),
            );
            event.detail = serde_json::json!({
                "dropped_bytes": dropped,
                "damaged": true,
                "kept_in": name,
            });
            event
        }
    }
}

/// Result of checking a whole log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub records: u64,
    pub last_hash: String,
    /// Bytes after the last complete record: a write under way, or one a
    /// crash or a full disk cut short (dropped when the log is next opened
    /// for writing). Not a record, and not corruption.
    pub unfinished: u64,
    /// The seq of the oldest record kept: 0 unless the retention rule has
    /// removed records, which a `log_trimmed` record accounts for.
    pub first_seq: u64,
    /// How many files the records are in: the segments and the file being
    /// written.
    pub files: u64,
}

/// Reads every record in order, checking each hash and chain link, through
/// the segments and the file being written.
///
/// Beside a writer, a break found while the log's files changed (the file
/// was closed as a segment, or old segments were removed, in the middle of
/// the pass) is looked at again: only one that is still there with the
/// files at rest is reported.
pub fn verify(path: &Path) -> Result<VerifyReport, AuditError> {
    let mut tries = 0u32;
    loop {
        let before = shape(path);
        match verify_once(path) {
            Err(AuditError::Corrupt { .. }) if tries < 20 && shape(path) != before => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(u64::from(tries) * 5));
            }
            other => return other,
        }
    }
}

fn verify_once(path: &Path) -> Result<VerifyReport, AuditError> {
    let mut report = VerifyReport {
        records: 0,
        last_hash: GENESIS.to_owned(),
        unfinished: 0,
        first_seq: 0,
        files: 0,
    };
    let walked = for_each_record(path, |_, hash, _| {
        report.records += 1;
        report.last_hash = hash.to_owned();
    })?;
    report.unfinished = walked.unfinished;
    report.first_seq = walked.first_seq;
    report.files = walked.files;
    Ok(report)
}

/// The log's files, oldest first, each open: the segments, then the file
/// being written. Held open, a file that is renamed (closed as a segment)
/// or removed (by the retention rule) meanwhile still reads to its end.
///
/// The file being written is opened, the segments are listed and opened,
/// and the file being written is opened again:
/// - A file closed as a segment in between is open twice, under two
///   names. It is read once: files are told apart by what they are, not
///   by what they're called, and put in the order of their first records.
/// - A segment removed after it was listed takes those before it with it:
///   the retention rule removes the oldest first.
///
/// That is one whole chain unless the log was closed into segments, or
/// trimmed, more than once while this ran; [`verify`] looks again when it
/// finds a break and the files have changed under it.
fn open_files(path: &Path) -> Result<Vec<(PathBuf, File)>, AuditError> {
    use std::os::unix::fs::MetadataExt;
    let io_err = |at: &Path, source| AuditError::Io {
        path: at.to_owned(),
        source,
    };
    let writing = |files: &mut Vec<(u64, PathBuf, File)>| -> Result<(), AuditError> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            // Between the file being renamed as a segment and the new one
            // being made, or a log stopped there: it holds nothing yet.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_err(path, e)),
        };
        let it = file.metadata().map_err(|e| io_err(path, e))?;
        let open_already = files.iter().any(|(_, _, other)| {
            other
                .metadata()
                .is_ok_and(|m| m.dev() == it.dev() && m.ino() == it.ino())
        });
        if !open_already {
            // An empty file, or one whose first record can't be read (the
            // pass over it says why), goes last.
            let mut line = String::new();
            BufReader::new(&file)
                .read_line(&mut line)
                .map_err(|e| io_err(path, e))?;
            file.seek(SeekFrom::Start(0)).map_err(|e| io_err(path, e))?;
            let first = line
                .strip_suffix('\n')
                .and_then(|line| parse_line(line).ok())
                .map_or(u64::MAX, |(_, record)| record.seq);
            files.push((first, path.to_owned(), file));
        }
        Ok(())
    };
    let mut files: Vec<(u64, PathBuf, File)> = Vec::new();
    writing(&mut files)?;
    let mut segments: Vec<(u64, PathBuf, File)> = Vec::new();
    for (first, segment) in sealed(path)? {
        match File::open(&segment) {
            Ok(file) => segments.push((first, segment, file)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => segments.clear(),
            Err(e) => return Err(io_err(&segment, e)),
        }
    }
    // The file opened first, if it's a segment by now, is read as that.
    if let Some((_, _, first)) = files.first()
        && let Ok(it) = first.metadata()
        && segments.iter().any(|(_, _, segment)| {
            segment
                .metadata()
                .is_ok_and(|m| m.dev() == it.dev() && m.ino() == it.ino())
        })
    {
        files.clear();
    }
    files.append(&mut segments);
    writing(&mut files)?;
    if files.is_empty() {
        return Err(io_err(path, io::Error::from(io::ErrorKind::NotFound)));
    }
    files.sort_by_key(|(first, ..)| *first);
    Ok(files
        .into_iter()
        .map(|(_, path, file)| (path, file))
        .collect())
}

/// What the log's files are: its segments, and which file is being
/// written (told by what file it is, not by its name). A change between
/// two looks means the log was closed into a segment or trimmed in
/// between.
#[derive(PartialEq, Eq)]
struct Shape {
    segments: Vec<(u64, PathBuf)>,
    writing: Option<(u64, u64)>,
}

fn shape(path: &Path) -> Shape {
    use std::os::unix::fs::MetadataExt;
    Shape {
        segments: sealed(path).unwrap_or_default(),
        writing: fs::metadata(path).ok().map(|m| (m.dev(), m.ino())),
    }
}

/// The records written at or after `since` (Unix seconds) that `filter`
/// keeps, oldest first, read from the end of the log back: a report of the
/// last day costs that day's records, not the whole log. Each line is
/// checked against its own hash; the chain is [`verify`]'s. Records are
/// appended in time order, so the first older one ends the read (a clock
/// stepped back can hide a record behind it). A last line without its
/// newline is a write under way, or one cut short: not a record yet.
pub fn read_since(
    path: &Path,
    since: i64,
    mut filter: impl FnMut(&AuditRecord) -> bool,
) -> Result<Vec<AuditRecord>, AuditError> {
    let mut out = Vec::new();
    // The file being written, then the segments, newest first.
    for (at, mut file) in open_files(path)?.into_iter().rev() {
        if since_in(&mut file, &at, since, &mut filter, &mut out)? {
            break;
        }
    }
    out.reverse();
    Ok(out)
}

/// Adds to `out`, newest first, the records of one file written at or
/// after `since` that `filter` keeps. Whether it met an older one: the
/// files before this one are older still.
fn since_in(
    file: &mut File,
    path: &Path,
    since: i64,
    filter: &mut impl FnMut(&AuditRecord) -> bool,
    out: &mut Vec<AuditRecord>,
) -> Result<bool, AuditError> {
    const CHUNK: u64 = 64 * 1024;
    let io_err = |source| AuditError::Io {
        path: path.into(),
        source,
    };
    let mut end = file.metadata().map_err(io_err)?.len();
    // Bytes of a line whose start is in a chunk not read yet.
    let mut carry: Vec<u8> = Vec::new();
    // Whether the end of the file has been looked at for an unfinished line.
    let mut tail = false;
    loop {
        let start = end.saturating_sub(CHUNK);
        let mut buf = vec![0u8; (end - start) as usize];
        file.seek(SeekFrom::Start(start)).map_err(io_err)?;
        file.read_exact(&mut buf).map_err(io_err)?;
        buf.extend_from_slice(&carry);
        if !tail {
            match buf.iter().rposition(|&b| b == b'\n') {
                Some(i) => {
                    tail = true;
                    buf.truncate(i + 1);
                }
                // All of it unfinished so far: look further back.
                None if start > 0 => {
                    end = start;
                    continue;
                }
                None => return Ok(false),
            }
        }
        // Whole lines are those after a newline in this chunk, or all of it
        // at the file's start.
        let first = match start {
            0 => 0,
            _ => match buf.iter().position(|&b| b == b'\n') {
                Some(i) => i + 1,
                None => {
                    carry = buf;
                    end = start;
                    continue;
                }
            },
        };
        for line in buf[first..]
            .rsplit(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
        {
            let line = std::str::from_utf8(line).map_err(|e| AuditError::Corrupt {
                path: path.into(),
                line: 0,
                reason: format!("not text: {e}"),
            })?;
            let (_, record) = parse_line(line).map_err(|reason| AuditError::Corrupt {
                path: path.into(),
                line: 0,
                reason,
            })?;
            if record.ts.unix_seconds() < since {
                return Ok(true);
            }
            if filter(&record) {
                out.push(record);
            }
        }
        if start == 0 {
            return Ok(false);
        }
        carry = buf[..first].to_vec();
        end = start;
    }
}

/// Reads every record in order, verifying the chain as it goes, and returns
/// those `filter` keeps. Stops at the first broken link. (One pass: beside
/// a writer that closes the file as a segment in the middle of it, use
/// [`verify`] to tell a break from that.)
pub fn read(
    path: &Path,
    mut filter: impl FnMut(&AuditRecord) -> bool,
) -> Result<Vec<AuditRecord>, AuditError> {
    let mut out = Vec::new();
    for_each_record(path, |_, _, record| {
        if filter(&record) {
            out.push(record);
        }
    })?;
    Ok(out)
}

/// What a pass over the whole log found beside its records.
struct Walked {
    /// The length of a last line without its newline.
    unfinished: u64,
    first_seq: u64,
    files: u64,
}

/// Visits each record in order, through the segments and the file being
/// written, checking the chain: each record's own hash, its link to the
/// one before it (across files too) and its seq.
///
/// The oldest record kept is the log's first (seq 0, chained onto
/// [`GENESIS`]), or one a `log_trimmed` record in the log covers: no
/// later than the record after the last one that record says the
/// retention rule removed. Where it is exactly that record, it must chain
/// onto the hash the `log_trimmed` record gives. (Segments go one at a
/// time, oldest first: stopped part way, some of what the record covers
/// is still here, and the chain through it is checked like any other.)
/// Anything else is records gone that nothing accounts for.
///
/// Stops before a last line without its newline (a write under way, or one
/// cut short), which isn't a record yet.
fn for_each_record(
    path: &Path,
    mut visit: impl FnMut(u64, &str, AuditRecord),
) -> Result<Walked, AuditError> {
    let files = open_files(path)?;
    let mut walked = Walked {
        unfinished: 0,
        first_seq: 0,
        files: files.len() as u64,
    };
    // The seq and `prev` the next record must have; none before the first.
    let mut expected: Option<(u64, String)> = None;
    // What the oldest record kept chains onto, when it isn't the log's
    // first: the seq and hash of a record that's gone.
    let mut onto: Option<(u64, String)> = None;
    // What each `log_trimmed` record says was removed: through which
    // record, and its hash.
    let mut trims: Vec<(u64, String)> = Vec::new();
    let mut visited = 0u64;
    let last = files.len().saturating_sub(1);
    for (nth, (at, file)) in files.into_iter().enumerate() {
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        let mut line_no = 0u64;
        loop {
            line_no += 1;
            let corrupt = |reason: String| AuditError::Corrupt {
                path: at.clone(),
                line: line_no,
                reason,
            };
            bytes.clear();
            let read = reader
                .read_until(b'\n', &mut bytes)
                .map_err(|source| AuditError::Io {
                    path: at.clone(),
                    source,
                })?;
            if read == 0 {
                break;
            }
            if bytes.last() != Some(&b'\n') {
                // Only the file being written ends in a write under way.
                if nth != last {
                    return Err(corrupt("a segment ends in part of a record".to_owned()));
                }
                walked.unfinished = read as u64;
                break;
            }
            let line = std::str::from_utf8(&bytes[..read - 1])
                .map_err(|e| corrupt(format!("not text: {e}")))?;
            let (hash, record) = parse_line(line).map_err(corrupt)?;
            match &expected {
                None if record.seq == 0 => {
                    if record.prev != GENESIS {
                        return Err(corrupt(format!(
                            "chain broken: the first record's prev is {}",
                            record.prev
                        )));
                    }
                }
                None => {
                    walked.first_seq = record.seq;
                    onto = Some((record.seq - 1, record.prev.clone()));
                }
                Some((seq, prev)) => {
                    if &record.prev != prev {
                        return Err(corrupt(format!(
                            "chain broken: prev is {} but the record before hashes to {prev}",
                            record.prev
                        )));
                    }
                    if record.seq != *seq {
                        return Err(corrupt(format!("expected seq {seq}, found {}", record.seq)));
                    }
                }
            }
            if record.event.kind == EventKind::LogTrimmed
                && let (Some(through), Some(hash)) = (
                    record.event.detail["through_seq"].as_u64(),
                    record.event.detail["last_hash"].as_str(),
                )
            {
                trims.push((through, hash.to_owned()));
            }
            expected = Some((record.seq + 1, hash.clone()));
            visited += 1;
            visit(visited, &hash, record);
        }
    }
    if let Some(onto) = onto
        && !trims
            .iter()
            .any(|(through, hash)| *through > onto.0 || (*through == onto.0 && *hash == onto.1))
    {
        return Err(AuditError::Corrupt {
            path: path.to_owned(),
            line: 1,
            reason: format!(
                "the log starts at record {}, and no record of the retention rule removing \
                 those before it says so: records are gone that nothing accounts for",
                onto.0 + 1
            ),
        });
    }
    Ok(walked)
}

/// Parses one line and checks its hash. Returns the hash and the record.
fn parse_line(line: &str) -> Result<(String, AuditRecord), String> {
    let parsed: LineIn<'_> =
        serde_json::from_str(line).map_err(|e| format!("not a record: {e}"))?;
    let actual = hash_body(parsed.body.get());
    if actual != parsed.hash {
        return Err(format!(
            "hash mismatch: stored {} but body hashes to {actual}",
            parsed.hash
        ));
    }
    let record: AuditRecord =
        serde_json::from_str(parsed.body.get()).map_err(|e| format!("bad body: {e}"))?;
    Ok((parsed.hash, record))
}

fn hash_body(body: &str) -> String {
    hex::encode(Sha256::digest(body.as_bytes()))
}

/// Finds the last complete line in the file's first `len` bytes, with
/// where it starts, and how many bytes follow it without a newline (a torn
/// write). Reads backwards in chunks so opening a large log stays cheap.
fn read_tail(file: &mut File, len: u64) -> io::Result<(Option<(String, u64)>, u64)> {
    const CHUNK: u64 = 64 * 1024;
    let mut end = len;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let start = end.saturating_sub(CHUNK);
        let mut chunk = vec![0u8; (end - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
        end = start;

        // Bytes after the final newline belong to a torn write.
        let last_nl = buf.iter().rposition(|&b| b == b'\n');
        let complete = match last_nl {
            Some(i) => &buf[..i],
            None if start == 0 => return Ok((None, buf.len() as u64)),
            None => continue,
        };
        let dropped = (buf.len() - complete.len() - 1) as u64;
        match complete.iter().rposition(|&b| b == b'\n') {
            Some(j) => {
                return Ok((
                    Some((
                        String::from_utf8_lossy(&complete[j + 1..]).into_owned(),
                        start + j as u64 + 1,
                    )),
                    dropped,
                ));
            }
            None if start == 0 => {
                return Ok((
                    Some((String::from_utf8_lossy(complete).into_owned(), 0)),
                    dropped,
                ));
            }
            None => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(summary: &str, task: Option<TaskId>) -> AuditEvent {
        let mut event = AuditEvent::new(EventKind::Action, summary);
        event.task = task;
        event
    }

    /// What a write a full disk cut short leaves: part of its line, and
    /// the writer gone.
    fn cut_short(log: &AuditLog) {
        let mut file = OpenOptions::new().append(true).open(&log.path).unwrap();
        file.write_all(br#"{"hash":"0f1e","body":{"seq":"#).unwrap();
        *log.state.lock().unwrap() = None;
    }

    // AuditLog.tla, AU1_TheLogRecovers: the writer was unusable until glassd
    // restarted, so a disk that filled once refused every record after.
    #[test]
    fn au1_a_writer_cut_short_by_a_full_disk_takes_records_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        let task = TaskId::new();
        log.append(action("one", Some(task))).unwrap();
        // The index is built, and kept up after.
        assert_eq!(log.task_records(task).unwrap().len(), 1);
        cut_short(&log);
        log.append(action("two", Some(task))).unwrap();

        let report = verify(&path).unwrap();
        assert_eq!((report.records, report.unfinished), (3, 0));
        let kinds: Vec<EventKind> = read(&path, |_| true)
            .unwrap()
            .into_iter()
            .map(|r| r.event.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                EventKind::Action,
                EventKind::LogRecovered,
                EventKind::Action
            ]
        );
        let summaries: Vec<String> = log
            .task_records(task)
            .unwrap()
            .into_iter()
            .map(|r| r.event.summary)
            .collect();
        assert_eq!(summaries, ["one", "two"]);
    }

    // The fsync failed after the whole line was written: the caller was
    // told the record failed and wrote it again, and the first writing
    // stayed in the file, chained onto: the same record twice, one of them
    // never acknowledged.
    #[test]
    fn au1_a_record_whose_fsync_failed_isnt_left_in_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        let task = TaskId::new();
        log.append(action("one", Some(task))).unwrap();
        log.fail_syncs_for_tests(0, 1);
        assert!(log.append(action("two", Some(task))).is_err());
        // It's in the file, whole, until the writer opens it again.
        assert_eq!(verify(&path).unwrap().records, 2);
        // The runtime writes what failed again (Runtime::audit).
        log.append(action("two", Some(task))).unwrap();

        let all = read(&path, |_| true).unwrap();
        let said: Vec<(EventKind, &str)> = all
            .iter()
            .map(|r| (r.event.kind, r.event.summary.as_str()))
            .collect();
        assert_eq!(said[0], (EventKind::Action, "one"));
        assert_eq!(said[1].0, EventKind::LogRecovered);
        assert_eq!(said[2], (EventKind::Action, "two"));
        assert_eq!(said.len(), 3, "two is there once");
        assert!(all[1].event.detail["dropped_bytes"].as_u64().unwrap() > 0);
        assert_eq!(all[1].event.detail.get("damaged"), None);
        let report = verify(&path).unwrap();
        assert_eq!((report.records, report.unfinished), (3, 0));
        let summaries: Vec<String> = log
            .task_records(task)
            .unwrap()
            .into_iter()
            .map(|r| r.event.summary)
            .collect();
        assert_eq!(summaries, ["one", "two"]);
    }

    #[test]
    fn au1_a_run_of_records_is_one_write_chained_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open(&path, Durability::Sync).unwrap();
        let task = TaskId::new();
        log.append(action("before", Some(task))).unwrap();
        let before = log.runs_for_tests();
        let run = log
            .append_all(vec![
                action("one", Some(task)),
                action("two", None),
                action("three", Some(task)),
            ])
            .unwrap();
        // One write and one sync for the three.
        assert_eq!(log.runs_for_tests(), before + 1);
        assert_eq!(run.iter().map(|r| r.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(
            run.iter()
                .map(|r| r.event.summary.as_str())
                .collect::<Vec<_>>(),
            ["one", "two", "three"]
        );
        // What was handed back is what the log holds, its chain whole.
        let all = read(&path, |_| true).unwrap();
        assert_eq!(&all[1..], &run[..]);
        let report = verify(&path).unwrap();
        assert_eq!((report.records, report.unfinished), (4, 0));
        // The task's records are found, in order, through the index.
        log.append(action("after", Some(task))).unwrap();
        let theirs: Vec<String> = log
            .task_records(task)
            .unwrap()
            .into_iter()
            .map(|r| r.event.summary)
            .collect();
        assert_eq!(theirs, ["before", "one", "three", "after"]);
        // An empty run writes nothing.
        assert!(log.append_all(Vec::new()).unwrap().is_empty());
        assert_eq!(verify(&path).unwrap().records, 5);
    }

    #[test]
    fn au1_a_run_of_records_that_fails_fails_as_one_and_none_of_it_stays() {
        // The run's second record fails, as a failed fsync does (its lines
        // landed) and as a full disk does (half a line): either way the
        // whole run is an error, none of it is left in the log, and each
        // record is there once when the run is written again.
        for whole in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("audit.log");
            let log = AuditLog::open(&path, Durability::Buffered).unwrap();
            let task = TaskId::new();
            log.append(action("before", Some(task))).unwrap();
            let run = || {
                vec![
                    action("one", Some(task)),
                    action("two", Some(task)),
                    action("three", Some(task)),
                ]
            };
            if whole {
                log.fail_syncs_for_tests(1, 1);
            } else {
                log.fail_writes_for_tests(1, 1);
            }
            assert!(log.append_all(run()).is_err(), "whole: {whole}");
            // The caller writes the run again (Runtime::audit).
            log.append_all(run()).unwrap();

            let said: Vec<String> = read(&path, |_| true)
                .unwrap()
                .into_iter()
                .filter(|r| r.event.kind == EventKind::Action)
                .map(|r| r.event.summary)
                .collect();
            assert_eq!(said, ["before", "one", "two", "three"], "whole: {whole}");
            let report = verify(&path).unwrap();
            // The four, and the note that the log was cut back.
            assert_eq!((report.records, report.unfinished), (5, 0));
            let theirs: Vec<String> = log
                .task_records(task)
                .unwrap()
                .into_iter()
                .map(|r| r.event.summary)
                .collect();
            assert_eq!(theirs, ["before", "one", "two", "three"]);
        }
    }

    /// Changes one byte of the log's last line, inside its body.
    fn damage_last_record(path: &Path) {
        let mut bytes = std::fs::read(path).unwrap();
        let end = bytes.len() - 1;
        let start = bytes[..end]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let at = start + (end - start) / 2;
        bytes[at] = if bytes[at] == b'x' { b'y' } else { b'x' };
        std::fs::write(path, bytes).unwrap();
    }

    fn set_aside_files(dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".damaged-"))
            .collect();
        files.sort();
        files
    }

    // A last record that didn't match its hash made `open` an error, so
    // glassd refused to start at every restart, with nothing to do about
    // it but edit the file by hand.
    #[test]
    fn au2_a_damaged_last_record_is_set_aside_and_the_chain_goes_on() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let task = TaskId::new();
        {
            let log = AuditLog::open(&path, Durability::Buffered).unwrap();
            for summary in ["one", "two", "three"] {
                log.append(action(summary, Some(task))).unwrap();
            }
        }
        let whole = std::fs::read(&path).unwrap();
        damage_last_record(&path);
        assert!(verify(&path).is_err(), "it is damaged");
        let damaged = std::fs::read(&path).unwrap();

        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        log.append(action("four", Some(task))).unwrap();
        // One and two, the note, four: a chain that checks out.
        let report = verify(&path).unwrap();
        assert_eq!((report.records, report.unfinished), (4, 0));
        let all = read(&path, |_| true).unwrap();
        let seqs: Vec<u64> = all.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, [0, 1, 2, 3]);
        let note = &all[2];
        assert_eq!(note.event.kind, EventKind::LogRecovered);
        assert_eq!(note.event.detail["damaged"], true);
        // The damaged record is kept, whole, for root alone, where the note says.
        let kept = set_aside_files(dir.path());
        assert_eq!(kept.len(), 1);
        assert_eq!(
            note.event.detail["kept_in"],
            kept[0].file_name().unwrap().to_string_lossy().as_ref()
        );
        let third_line = &damaged[damaged.len() - (whole.len() - whole_len_before_last(&whole))..];
        assert_eq!(std::fs::read(&kept[0]).unwrap(), third_line);
        assert_eq!(
            std::fs::metadata(&kept[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(note.event.detail["dropped_bytes"], third_line.len() as u64);
        let summaries: Vec<String> = log
            .task_records(task)
            .unwrap()
            .into_iter()
            .map(|r| r.event.summary)
            .collect();
        assert_eq!(summaries, ["one", "two", "four"]);
    }

    /// Where the last line of a log's bytes starts.
    fn whole_len_before_last(bytes: &[u8]) -> usize {
        bytes[..bytes.len() - 1]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1)
    }

    #[test]
    fn au2_a_log_whose_only_record_is_damaged_starts_again_and_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        {
            let log = AuditLog::open(&path, Durability::Buffered).unwrap();
            log.append(action("one", None)).unwrap();
        }
        damage_last_record(&path);
        // With a write cut short after it, too: all of it is set aside.
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"hash":"0f1e","body":{"seq":"#).unwrap();
        drop(file);
        let before = std::fs::read(&path).unwrap();

        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        log.append(action("two", None)).unwrap();
        let all = read(&path, |_| true).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            (all[0].seq, all[0].event.kind),
            (0, EventKind::LogRecovered)
        );
        assert_eq!(all[0].prev, GENESIS);
        assert_eq!(all[0].event.detail["dropped_bytes"], before.len() as u64);
        let kept = set_aside_files(dir.path());
        assert_eq!(std::fs::read(&kept[0]).unwrap(), before);
        assert_eq!(verify(&path).unwrap().records, 2);
    }

    // More than a damaged end: that is for a person to look at, and the
    // file is left exactly as it was.
    #[test]
    fn au2_two_damaged_records_at_the_end_are_still_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        {
            let log = AuditLog::open(&path, Durability::Buffered).unwrap();
            for summary in ["one", "two", "three"] {
                log.append(action(summary, None)).unwrap();
            }
        }
        // The last two lines, each changed.
        let mut bytes = std::fs::read(&path).unwrap();
        let last = whole_len_before_last(&bytes);
        let second = whole_len_before_last(&bytes[..last]);
        for at in [second + 20, last + 20] {
            bytes[at] = if bytes[at] == b'x' { b'y' } else { b'x' };
        }
        std::fs::write(&path, &bytes).unwrap();

        let refused = AuditLog::open(&path, Durability::Buffered).unwrap_err();
        assert!(
            matches!(&refused, AuditError::Corrupt { reason, .. } if reason.contains("and the one before it")),
            "{refused}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "untouched");
        assert!(set_aside_files(dir.path()).is_empty());
    }

    // The test seam the runtime's tests use: a full disk for a number of
    // writes, each leaving half its line; the opening again after them
    // fails too while the disk is full.
    #[test]
    fn au1_a_full_disk_refuses_records_until_there_is_room() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        log.append(action("one", None)).unwrap();
        log.fail_writes_for_tests(0, 2);
        assert!(log.append(action("two", None)).is_err());
        assert_eq!(verify(&path).unwrap().records, 1);
        assert!(verify(&path).unwrap().unfinished > 0);
        // Opening again cuts the half line off, but the note can't be written.
        assert!(log.append(action("two", None)).is_err());
        log.append(action("two", None)).unwrap();
        let report = verify(&path).unwrap();
        assert_eq!((report.records, report.unfinished), (3, 0));
    }

    // Segments and the retention rule (audit 4.7).

    /// A log whose file is closed as a segment before every run after its
    /// first: each run a segment of its own.
    fn each_run_a_segment() -> Segments {
        Segments {
            bytes: u64::MAX,
            age: Duration::ZERO,
        }
    }

    fn files_beside(path: &Path, infix: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(infix))
            .collect();
        names.sort();
        names
    }

    fn summaries(records: &[AuditRecord]) -> Vec<String> {
        records.iter().map(|r| r.event.summary.clone()).collect()
    }

    #[test]
    fn au1_the_file_is_closed_as_a_segment_and_the_chain_goes_on_in_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        // A segment once the file holds 600 bytes: every few records.
        let rule = Segments {
            bytes: 600,
            age: Duration::from_secs(3600),
        };
        let log = AuditLog::open_with(&path, Durability::Buffered, rule).unwrap();
        let (a, b) = (TaskId::new(), TaskId::new());
        // The index is built before most of the log is: it is kept up
        // through every segment closed from here on.
        log.append(action("a 0", Some(a))).unwrap();
        assert_eq!(log.task_records(a).unwrap().len(), 1);
        for n in 1..30 {
            let task = if n % 3 == 0 { b } else { a };
            log.append(action(
                &format!("{} {n}", if task == a { "a" } else { "b" }),
                Some(task),
            ))
            .unwrap();
        }
        let segments = files_beside(&path, ".seg-");
        let sealed_now: Vec<String> = segments
            .iter()
            .filter(|n| !n.ends_with(".tasks"))
            .cloned()
            .collect();
        assert!(sealed_now.len() >= 4, "{segments:?}");
        assert_eq!(sealed_now[0], "audit.log.seg-000000000000");
        // Each segment's tasks are kept beside it.
        for segment in &sealed_now {
            assert!(segments.contains(&format!("{segment}.tasks")), "{segment}");
        }

        // One chain through all of them: every record, in order, verified.
        let report = verify(&path).unwrap();
        assert_eq!(
            (report.records, report.first_seq, report.unfinished),
            (30, 0, 0)
        );
        assert_eq!(report.files, sealed_now.len() as u64 + 1);
        let all = read(&path, |_| true).unwrap();
        assert_eq!(
            all.iter().map(|r| r.seq).collect::<Vec<_>>(),
            (0..30).collect::<Vec<_>>()
        );
        assert_eq!(all[0].prev, GENESIS);

        // A task's records through the index are those a whole read finds.
        for task in [a, b] {
            let whole: Vec<AuditRecord> = all
                .iter()
                .filter(|r| r.event.task == Some(task))
                .cloned()
                .collect();
            assert_eq!(log.task_records(task).unwrap(), whole);
        }
        // And for a log opened afresh, which reads each segment's tasks
        // from beside it, or works them out where that's gone or stale.
        drop(log);
        fs::remove_file(dir.path().join(format!("{}.tasks", sealed_now[1]))).unwrap();
        fs::write(dir.path().join(format!("{}.tasks", sealed_now[2])), b"{").unwrap();
        let log = AuditLog::open_with(&path, Durability::Buffered, rule).unwrap();
        let whole: Vec<AuditRecord> = all
            .iter()
            .filter(|r| r.event.task == Some(b))
            .cloned()
            .collect();
        assert_eq!(log.task_records(b).unwrap(), whole);
        assert!(dir.path().join(format!("{}.tasks", sealed_now[1])).exists());

        // The chain goes on where it stood.
        let next = log.append(action("a 30", Some(a))).unwrap();
        assert_eq!(next.seq, 30);
        assert_eq!(verify(&path).unwrap().records, 31);
        assert_eq!(log.task_records(a).unwrap().last().unwrap().seq, 30);

        // The last day's records are read from the end back, through as
        // many files as it takes.
        let recent = read_since(&path, 0, |r| r.event.task == Some(b)).unwrap();
        assert_eq!(summaries(&recent), summaries(&whole));
        let ahead = Timestamp::now().unix_seconds() + 3600;
        assert!(read_since(&path, ahead, |_| true).unwrap().is_empty());
    }

    #[test]
    fn au1_a_file_is_closed_once_its_first_record_is_old_enough() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open_with(&path, Durability::Buffered, each_run_a_segment()).unwrap();
        // A run is never split: a segment ends where a run did.
        log.append_all(vec![action("one", None), action("two", None)])
            .unwrap();
        log.append(action("three", None)).unwrap();
        log.append(action("four", None)).unwrap();
        assert_eq!(
            files_beside(&path, ".seg-"),
            ["audit.log.seg-000000000000", "audit.log.seg-000000000002"]
        );
        // With the default rule nothing is closed for a month, or 32 MiB.
        let other = dir.path().join("other.log");
        let log = AuditLog::open(&other, Durability::Buffered).unwrap();
        for n in 0..50 {
            log.append(action(&format!("{n}"), None)).unwrap();
        }
        assert!(files_beside(&other, "other.log.seg-").is_empty());
    }

    #[test]
    fn au1_a_log_stopped_between_closing_a_segment_and_beginning_the_next_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        for n in 0..3 {
            log.append(action(&format!("{n}"), None)).unwrap();
        }
        drop(log);
        // glassd stopped right after the rename: the file is a segment,
        // and there is no file.
        fs::rename(&path, dir.path().join("audit.log.seg-000000000000")).unwrap();
        // The log still verifies as it is.
        let report = verify(&path).unwrap();
        assert_eq!((report.records, report.files), (3, 1));
        assert_eq!(read_since(&path, 0, |_| true).unwrap().len(), 3);

        // Opened, it goes on from the segment's last record, not from the
        // start.
        let log = AuditLog::open(&path, Durability::Buffered).unwrap();
        let next = log.append(action("3", None)).unwrap();
        assert_eq!(next.seq, 3);
        let all = read(&path, |_| true).unwrap();
        assert_eq!(all[3].prev, {
            let (hash, _) = last_record(&dir.path().join("audit.log.seg-000000000000")).unwrap();
            hash
        });
        assert_eq!(verify(&path).unwrap().records, 4);
    }

    #[test]
    fn au1_a_segment_is_never_written_over_and_the_log_goes_on_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open_with(&path, Durability::Buffered, each_run_a_segment()).unwrap();
        log.append(action("one", None)).unwrap();
        assert_eq!(log.unsealed(), None);
        // Something is where its segment would go. It isn't written over,
        // and the log isn't stopped for it: the file takes the record, and
        // says why it wasn't closed.
        let taken = dir.path().join("audit.log.seg-000000000000");
        fs::write(&taken, b"not this log's").unwrap();
        log.append(action("two", None)).unwrap();
        assert_eq!(fs::read(&taken).unwrap(), b"not this log's");
        let why = log.unsealed().unwrap();
        assert!(why.contains("is there already"), "{why}");
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
        // Out of the way, the file is closed at the next record, and
        // nothing of the log was lost.
        fs::remove_file(&taken).unwrap();
        log.append(action("three", None)).unwrap();
        assert_eq!(log.unsealed(), None);
        assert_eq!(files_beside(&path, ".seg-"), ["audit.log.seg-000000000000"]);
        assert_eq!(
            summaries(&read(&path, |_| true).unwrap()),
            ["one", "two", "three"]
        );
        assert_eq!(verify(&path).unwrap().records, 3);
    }

    #[test]
    fn au3_retention_removes_whole_old_segments_and_the_log_still_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open_with(&path, Durability::Buffered, each_run_a_segment()).unwrap();
        let (old, both) = (TaskId::new(), TaskId::new());
        log.append(action("old 0", Some(old))).unwrap();
        log.append(action("both 1", Some(both))).unwrap();
        log.append(action("old 2", Some(old))).unwrap();
        // The index knows the segments before any goes.
        assert_eq!(log.task_records(old).unwrap().len(), 2);
        std::thread::sleep(Duration::from_millis(20));
        let kept_from = Timestamp::now();
        std::thread::sleep(Duration::from_millis(20));
        log.append(action("both 3", Some(both))).unwrap();
        log.append(action("both 4", Some(both))).unwrap();

        // Nothing is older than the rule yet.
        let long_ago = Timestamp::from_unix_seconds(1_000_000_000).unwrap();
        assert_eq!(log.trim(long_ago, "400 days").unwrap(), None);
        assert_eq!(verify(&path).unwrap().records, 5);

        // The three records written before `kept_from` are each a segment
        // of their own; the fourth is a segment too, and newer; the fifth
        // is in the file being written.
        let before = read(&path, |_| true).unwrap();
        let trimmed = log.trim(kept_from, "400 days").unwrap().unwrap();
        assert_eq!((trimmed.records, trimmed.through_seq), (3, 2));
        assert_eq!(trimmed.newest, before[2].ts);
        assert_eq!(trimmed.last_hash, before[3].prev);
        assert_eq!(
            trimmed.segments,
            [
                "audit.log.seg-000000000000",
                "audit.log.seg-000000000001",
                "audit.log.seg-000000000002"
            ]
        );
        assert_eq!(
            files_beside(&path, ".seg-0"),
            // What's left: the fourth record's segment, and the fifth's,
            // closed as the record of the removal was written.
            [
                "audit.log.seg-000000000003",
                "audit.log.seg-000000000003.tasks",
                "audit.log.seg-000000000004",
                "audit.log.seg-000000000004.tasks"
            ]
        );

        // The log verifies from the oldest record kept: the record of the
        // removal says what it chains onto.
        let report = verify(&path).unwrap();
        assert_eq!((report.first_seq, report.records), (3, 3));
        let after = read(&path, |_| true).unwrap();
        assert_eq!(after[0], before[3]);
        let note = after.last().unwrap();
        assert_eq!(note.event.kind, EventKind::LogTrimmed);
        assert_eq!(
            note.event.summary,
            "removed 3 records older than 400 days: all through record 2"
        );
        assert_eq!(note.event.detail["through_seq"], 2);
        assert_eq!(note.event.detail["last_hash"], before[3].prev.as_str());
        assert_eq!(note.event.detail["rule"], "400 days");

        // A task's records are those still kept, for this log and for one
        // opened afresh.
        assert!(log.task_records(old).unwrap().is_empty());
        assert_eq!(
            summaries(&log.task_records(both).unwrap()),
            ["both 3", "both 4"]
        );
        drop(log);
        let log = AuditLog::open_with(&path, Durability::Buffered, each_run_a_segment()).unwrap();
        assert!(log.task_records(old).unwrap().is_empty());
        assert_eq!(log.task_records(both).unwrap().len(), 2);
        assert_eq!(read_since(&path, 0, |_| true).unwrap().len(), 3);

        // The record of a removal is itself removed in time, and the next
        // one's record carries the log.
        std::thread::sleep(Duration::from_millis(20));
        log.append(action("later", None)).unwrap();
        let again = log.trim(Timestamp::now(), "400 days").unwrap().unwrap();
        assert_eq!((again.records, again.through_seq), (3, 5));
        let report = verify(&path).unwrap();
        assert_eq!(report.first_seq, 6);
        assert_eq!(
            summaries(&read(&path, |_| true).unwrap()),
            [
                "later",
                "removed 3 records older than 400 days: all through record 5"
            ]
        );
    }

    #[test]
    fn au2_records_gone_that_no_record_accounts_for_show() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let write = || {
            for name in files_beside(&path, "audit.log") {
                fs::remove_file(dir.path().join(name)).unwrap();
            }
            let log =
                AuditLog::open_with(&path, Durability::Buffered, each_run_a_segment()).unwrap();
            for n in 0..5 {
                log.append(action(&format!("{n}"), None)).unwrap();
                // Each a few milliseconds after the last: the rule goes by
                // when records were written.
                std::thread::sleep(Duration::from_millis(3));
            }
            log
        };
        // The oldest segment deleted by hand: the log starts at a record
        // nothing says it may start at.
        drop(write());
        fs::remove_file(dir.path().join("audit.log.seg-000000000000")).unwrap();
        let err = verify(&path).unwrap_err().to_string();
        assert!(err.contains("the log starts at record 1"), "{err}");
        assert!(err.contains("nothing accounts for"), "{err}");
        assert!(read(&path, |_| true).is_err());

        // One from the middle: the chain is broken there.
        drop(write());
        fs::remove_file(dir.path().join("audit.log.seg-000000000002")).unwrap();
        let err = verify(&path).unwrap_err().to_string();
        assert!(err.contains("chain broken"), "{err}");

        // The rule removed the first two; someone removes one more and
        // points at the rule. Its record names another record.
        let log = write();
        std::thread::sleep(Duration::from_millis(20));
        let records = read(&path, |_| true).unwrap();
        log.trim(records[2].ts, "400 days").unwrap().unwrap();
        assert_eq!(verify(&path).unwrap().first_seq, 2);
        fs::remove_file(dir.path().join("audit.log.seg-000000000002")).unwrap();
        let err = verify(&path).unwrap_err().to_string();
        assert!(err.contains("the log starts at record 3"), "{err}");

        // A record of a removal can't be put in by hand: it is a record
        // like any other, and the chain breaks where it was added.
        drop(write());
        fs::remove_file(dir.path().join("audit.log.seg-000000000000")).unwrap();
        let first_kept = dir.path().join("audit.log.seg-000000000001");
        let genuine = fs::read_to_string(&first_kept).unwrap();
        let forged = genuine.replace("\"action\"", "\"log_trimmed\"");
        assert_ne!(forged, genuine);
        fs::write(&first_kept, forged).unwrap();
        let err = verify(&path).unwrap_err().to_string();
        assert!(err.contains("hash mismatch"), "{err}");
    }

    #[test]
    fn au3_a_removal_stopped_after_its_record_is_finished_by_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open_with(&path, Durability::Buffered, each_run_a_segment()).unwrap();
        for n in 0..3 {
            log.append(action(&format!("{n}"), None)).unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
        let cut = Timestamp::now();
        // The removal's record is written and glassd stops before any
        // segment goes: the segments are as they were.
        let kept = dir.path().join("kept");
        fs::create_dir(&kept).unwrap();
        let old: Vec<String> = files_beside(&path, ".seg-0")
            .into_iter()
            .filter(|n| !n.ends_with(".tasks"))
            .collect();
        for name in &old {
            fs::copy(dir.path().join(name), kept.join(name)).unwrap();
        }
        log.trim(cut, "400 days").unwrap().unwrap();
        for name in &old {
            fs::copy(kept.join(name), dir.path().join(name)).unwrap();
        }
        // More than the record says was removed is still here: the log
        // verifies from its first record.
        let report = verify(&path).unwrap();
        assert_eq!((report.first_seq, report.records), (0, 4));
        // Stopped after the oldest segment went: what's left of the
        // removal is covered by its record, and verifies too.
        fs::remove_file(dir.path().join(&old[0])).unwrap();
        let report = verify(&path).unwrap();
        assert_eq!((report.first_seq, report.records), (1, 3));
        assert_eq!(read_since(&path, 0, |_| true).unwrap().len(), 3);
        // The next pass removes them, with a record of its own; and with
        // them the third record, whose file was closed as the first
        // removal's record was written.
        let again = log.trim(cut, "400 days").unwrap().unwrap();
        assert_eq!((again.records, again.through_seq), (2, 2));
        let report = verify(&path).unwrap();
        assert_eq!((report.first_seq, report.records), (3, 2));
        assert!(
            read(&path, |_| true)
                .unwrap()
                .iter()
                .all(|r| r.event.kind == EventKind::LogTrimmed)
        );
    }

    #[test]
    fn au1_the_log_is_read_whole_while_it_is_closed_into_segments_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let log = AuditLog::open_with(&path, Durability::Buffered, each_run_a_segment()).unwrap();
        log.append(action("first", None)).unwrap();
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for n in 0..400 {
                    log.append(action(&format!("{n}"), None)).unwrap();
                    if n % 50 == 49 {
                        log.trim(Timestamp::now(), "a moment").unwrap();
                    }
                }
                done.store(true, Ordering::SeqCst);
            });
            // Beside the writer: every pass sees one whole chain, whatever
            // was renamed or removed while it read.
            let mut passes = 0;
            while !done.load(Ordering::SeqCst) {
                let report = verify(&path).unwrap();
                assert!(report.records >= 1);
                read_since(&path, 0, |_| true).unwrap();
                passes += 1;
            }
            assert!(passes > 0);
        });
        verify(&path).unwrap();
    }
}
