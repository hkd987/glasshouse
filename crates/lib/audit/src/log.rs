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

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

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
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    durability: Durability,
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
}

/// Where each task's records start in the file, up to `to`.
#[derive(Debug, Default)]
struct TaskIndex {
    to: u64,
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
        let path = path.into();
        let (state, recovery) = open_writer(&path, None)?;
        let log = Self {
            path,
            durability,
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
        Ok(stored)
    }

    /// `task`'s records, oldest first, each checked against its own hash.
    /// The chain as a whole is [`verify`]'s to check.
    pub fn task_records(&self, task: TaskId) -> Result<Vec<AuditRecord>, AuditError> {
        let offsets = self.offsets(task)?;
        let io_err = |source| AuditError::Io {
            path: self.path.clone(),
            source,
        };
        let mut reader = BufReader::new(File::open(&self.path).map_err(io_err)?);
        let mut out = Vec::with_capacity(offsets.len());
        let mut line = String::new();
        for at in offsets {
            reader.seek(SeekFrom::Start(at)).map_err(io_err)?;
            line.clear();
            reader.read_line(&mut line).map_err(io_err)?;
            let (_, record) =
                parse_line(line.trim_end_matches('\n')).map_err(|reason| AuditError::Corrupt {
                    path: self.path.clone(),
                    line: 0,
                    reason: format!("at byte {at}: {reason}"),
                })?;
            out.push(record);
        }
        Ok(out)
    }

    /// Where `task`'s records start, building the index the first time:
    /// the long pass without holding the writer, then what was appended
    /// meanwhile with it held, so the index is whole once it's in place.
    fn offsets(&self, task: TaskId) -> Result<Vec<u64>, AuditError> {
        if let Some(index) = self
            .index
            .lock()
            .map_err(|_| AuditError::Poisoned)?
            .as_ref()
        {
            return Ok(index.by_task.get(&task).cloned().unwrap_or_default());
        }
        let mut index = TaskIndex::default();
        let len = self.current_len()?;
        self.scan(&mut index, len)?;
        let guard = self.state.lock().map_err(|_| AuditError::Poisoned)?;
        let len = guard.as_ref().ok_or(AuditError::Poisoned)?.len;
        self.scan(&mut index, len)?;
        let mut slot = self.index.lock().map_err(|_| AuditError::Poisoned)?;
        let found = index.by_task.get(&task).cloned().unwrap_or_default();
        *slot = Some(index);
        drop(slot);
        drop(guard);
        Ok(found)
    }

    fn current_len(&self) -> Result<u64, AuditError> {
        let guard = self.state.lock().map_err(|_| AuditError::Poisoned)?;
        Ok(guard.as_ref().ok_or(AuditError::Poisoned)?.len)
    }

    /// Adds to `index` the records from where it got to up to `to`.
    fn scan(&self, index: &mut TaskIndex, to: u64) -> Result<(), AuditError> {
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
            path: self.path.clone(),
            source,
        };
        let mut reader = BufReader::new(File::open(&self.path).map_err(io_err)?);
        reader.seek(SeekFrom::Start(index.to)).map_err(io_err)?;
        let mut at = index.to;
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
                index.by_task.entry(task).or_default().push(at);
            }
            at += read as u64;
        }
        index.to = at;
        Ok(())
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
        None => (0, GENESIS.to_owned()),
        Some((line, start)) => match parse_line(&line) {
            Ok((hash, record)) => (record.seq + 1, hash),
            // Damage at the very end. The record before it says where the
            // chain stands; if that one is damaged too, this isn't a
            // damaged end, and it is for a person to look at.
            Err(reason) => {
                let (before, _) = read_tail(&mut file, start).map_err(io_err)?;
                let chain = match before {
                    None => (0, GENESIS.to_owned()),
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
    Ok((
        WriterState {
            file,
            next_seq,
            last_hash,
            len,
        },
        recovery,
    ))
}

/// Copies the log's bytes from `start` to `end` (a damaged last record,
/// and anything after it) into a new file beside it, for root alone, and
/// returns its name: `<log>.damaged-<unix seconds>`.
fn set_aside(path: &Path, file: &mut File, start: u64, end: u64) -> Result<String, AuditError> {
    use std::os::unix::fs::OpenOptionsExt;
    let io_err = |source| AuditError::Io {
        path: path.to_owned(),
        source,
    };
    let log_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audit.log".to_owned());
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
}

/// Reads every record in order, checking each hash and chain link.
pub fn verify(path: &Path) -> Result<VerifyReport, AuditError> {
    let mut report = VerifyReport {
        records: 0,
        last_hash: GENESIS.to_owned(),
        unfinished: 0,
    };
    report.unfinished = for_each_record(path, |_, hash, _| {
        report.records += 1;
        report.last_hash = hash.to_owned();
    })?;
    Ok(report)
}

/// The records written at or after `since` (Unix seconds) that `filter`
/// keeps, oldest first, read from the end of the file back: a report of the
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
    const CHUNK: u64 = 64 * 1024;
    let io_err = |source| AuditError::Io {
        path: path.into(),
        source,
    };
    let mut file = File::open(path).map_err(io_err)?;
    let mut end = file.metadata().map_err(io_err)?.len();
    // Bytes of a line whose start is in a chunk not read yet.
    let mut carry: Vec<u8> = Vec::new();
    let mut out = Vec::new();
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
                None => {
                    out.reverse();
                    return Ok(out);
                }
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
                out.reverse();
                return Ok(out);
            }
            if filter(&record) {
                out.push(record);
            }
        }
        if start == 0 {
            out.reverse();
            return Ok(out);
        }
        carry = buf[..first].to_vec();
        end = start;
    }
}

/// Reads every record in order, verifying the chain as it goes, and returns
/// those `filter` keeps. Stops at the first broken link.
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

/// Visits each record in order, checking the chain. Stops before a last
/// line without its newline (a write under way, or one cut short), which
/// isn't a record yet: its length in bytes.
fn for_each_record(
    path: &Path,
    mut visit: impl FnMut(u64, &str, AuditRecord),
) -> Result<u64, AuditError> {
    let file = File::open(path).map_err(|source| AuditError::Io {
        path: path.into(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let mut expected_prev = GENESIS.to_owned();
    let mut bytes = Vec::new();
    let mut expected_seq = 0;
    loop {
        let line_no = expected_seq + 1;
        let corrupt = |reason: String| AuditError::Corrupt {
            path: path.into(),
            line: line_no,
            reason,
        };
        bytes.clear();
        let read = reader
            .read_until(b'\n', &mut bytes)
            .map_err(|source| AuditError::Io {
                path: path.into(),
                source,
            })?;
        if read == 0 {
            return Ok(0);
        }
        if bytes.last() != Some(&b'\n') {
            return Ok(read as u64);
        }
        let line = std::str::from_utf8(&bytes[..read - 1])
            .map_err(|e| corrupt(format!("not text: {e}")))?;
        let (hash, record) = parse_line(line).map_err(corrupt)?;
        if record.prev != expected_prev {
            return Err(corrupt(format!(
                "chain broken: prev is {} but the record before hashes to {}",
                record.prev, expected_prev
            )));
        }
        if record.seq != expected_seq {
            return Err(corrupt(format!(
                "expected seq {expected_seq}, found {}",
                record.seq
            )));
        }
        expected_prev = hash.clone();
        visit(line_no, &hash, record);
        expected_seq += 1;
    }
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
}
