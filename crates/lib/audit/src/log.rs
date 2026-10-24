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
use std::sync::Mutex;

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
/// A write that fails (a full disk) may leave part of its line behind. The
/// writer never chains onto it: the next append opens the file again as a
/// start does, cutting the unfinished line off and noting it, so the log
/// takes records again once there's room (ADR 0053, `specs/tla/AuditLog.tla`).
///
/// A task's records are found through an index of where each task's lines
/// start, built by one pass the first time they're asked for and kept up
/// by every append after: reading one task's records no longer reads the
/// whole log, which grows for as long as the computer is used.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    durability: Durability,
    state: Mutex<Option<WriterState>>,
    index: Mutex<Option<TaskIndex>>,
    /// Tests only: writes to let through, then writes to fail as on a full
    /// disk.
    failing: Mutex<(u32, u32)>,
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
    /// record notes how many bytes were dropped.
    pub fn open(path: impl Into<PathBuf>, durability: Durability) -> Result<Self, AuditError> {
        let path = path.into();
        let (state, dropped) = open_writer(&path)?;
        let log = Self {
            path,
            durability,
            state: Mutex::new(Some(state)),
            index: Mutex::new(None),
            failing: Mutex::new((0, 0)),
        };
        if dropped > 0 {
            log.append(recovered(dropped))?;
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
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (skip, writes);
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one event and returns the stored record. After a failed
    /// write, the file is opened again first, as at a start.
    pub fn append(&self, event: AuditEvent) -> Result<AuditRecord, AuditError> {
        let mut guard = self.state.lock().map_err(|_| AuditError::Poisoned)?;
        if guard.is_none() {
            let (state, dropped) = open_writer(&self.path)?;
            *guard = Some(state);
            if dropped > 0 {
                self.write(&mut guard, recovered(dropped))?;
            }
        }
        self.write(&mut guard, event)
    }

    /// One record onto the writer in `guard`, which a failed write leaves
    /// empty: the file may now end in part of a line, and the next append
    /// opens it again rather than chain onto that.
    fn write(
        &self,
        guard: &mut Option<WriterState>,
        event: AuditEvent,
    ) -> Result<AuditRecord, AuditError> {
        let state = guard.as_mut().ok_or(AuditError::Poisoned)?;
        let record = AuditRecord {
            seq: state.next_seq,
            ts: Timestamp::now(),
            prev: state.last_hash.clone(),
            event,
        };
        let body = serde_json::to_string(&record).map_err(|e| AuditError::Io {
            path: self.path.clone(),
            source: io::Error::other(e),
        })?;
        let hash = hash_body(&body);
        let raw = RawValue::from_string(body).map_err(|e| AuditError::Io {
            path: self.path.clone(),
            source: io::Error::other(e),
        })?;
        let mut line = serde_json::to_string(&LineOut {
            hash: &hash,
            body: &raw,
        })
        .map_err(|e| AuditError::Io {
            path: self.path.clone(),
            source: io::Error::other(e),
        })?;
        line.push('\n');

        let failing = {
            let mut failing = self
                .failing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match *failing {
                (0, 0) => false,
                (0, writes) => {
                    failing.1 = writes - 1;
                    true
                }
                (skip, _) => {
                    failing.0 = skip - 1;
                    false
                }
            }
        };
        let written = if failing {
            state
                .file
                .write_all(&line.as_bytes()[..line.len() / 2])
                .and_then(|()| {
                    Err(io::Error::new(
                        io::ErrorKind::StorageFull,
                        "no space left on device (a test's)",
                    ))
                })
        } else {
            state.file.write_all(line.as_bytes())
        }
        .and_then(|()| match self.durability {
            Durability::Sync => state.file.sync_data(),
            Durability::Buffered => Ok(()),
        });
        if let Err(source) = written {
            *guard = None;
            return Err(AuditError::Io {
                path: self.path.clone(),
                source,
            });
        }
        let at = state.len;
        state.len += line.len() as u64;
        state.next_seq += 1;
        state.last_hash = hash;
        // Kept up while the writer is held, so it never misses a record.
        if let Ok(mut index) = self.index.lock()
            && let Some(index) = index.as_mut()
        {
            if let Some(task) = record.event.task {
                index.by_task.entry(task).or_default().push(at);
            }
            index.to = state.len;
        }
        Ok(record)
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
fn open_writer(path: &Path) -> Result<(WriterState, u64), AuditError> {
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
    let (tail, dropped) = read_tail(&mut file).map_err(io_err)?;
    let (next_seq, last_hash) = match tail {
        None => (0, GENESIS.to_owned()),
        Some(line) => {
            let (hash, record) = parse_line(&line).map_err(|reason| AuditError::Corrupt {
                path: path.to_owned(),
                line: 0,
                reason: format!("last record: {reason}"),
            })?;
            (record.seq + 1, hash)
        }
    };
    if dropped > 0 {
        let keep = file.metadata().map_err(io_err)?.len() - dropped;
        file.set_len(keep).map_err(io_err)?;
    }
    let len = file.metadata().map_err(io_err)?.len();
    Ok((
        WriterState {
            file,
            next_seq,
            last_hash,
            len,
        },
        dropped,
    ))
}

/// The note of a line cut short and dropped.
fn recovered(dropped: u64) -> AuditEvent {
    let mut event = AuditEvent::new(
        EventKind::LogRecovered,
        format!("dropped {dropped} bytes of an unacknowledged partial record"),
    );
    event.detail = serde_json::json!({ "dropped_bytes": dropped });
    event
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

/// Finds the last complete line, and how many trailing bytes follow it
/// without a newline (a torn write). Reads backwards in chunks so opening a
/// large log stays cheap.
fn read_tail(file: &mut File) -> io::Result<(Option<String>, u64)> {
    const CHUNK: u64 = 64 * 1024;
    let len = file.metadata()?.len();
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
                    Some(String::from_utf8_lossy(&complete[j + 1..]).into_owned()),
                    dropped,
                ));
            }
            None if start == 0 => {
                return Ok((
                    Some(String::from_utf8_lossy(complete).into_owned()),
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
