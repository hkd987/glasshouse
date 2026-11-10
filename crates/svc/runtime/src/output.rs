//! What an agent prints, kept to a size.
//!
//! An agent's standard output and error go to its task's log
//! (`<state>/tasks/<id>/agent.log`), for the person who wants to know what
//! it said and for the line a failed task shows. How much it prints is the
//! agent's to choose, so glassd is in the way of it: the log holds the
//! first [`LOG_MOST`] bytes, and the last few kilobytes stay in memory.
//! Past the limit the log says how much wasn't kept and ends with that
//! tail. It used to be the agent's own file to append to without end, on
//! the disk the audit log is on, and read whole into memory when the agent
//! failed.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::task::JoinHandle;

/// The most of its output a task's log keeps: the first 16 MiB.
pub(crate) const LOG_MOST: u64 = 16 * 1024 * 1024;
/// The end of the output kept in memory, for the line a failure shows and
/// the end of a log that was cut.
const TAIL: usize = 8 * 1024;
/// The longest line a failed task shows as its reason, in characters.
const REASON: usize = 300;
/// How long the last of the output is waited for once the agent is gone.
/// In a jail every process that could write is gone with it; on a
/// development host one it left behind may hold the pipe open.
const DRAIN: Duration = Duration::from_secs(2);

/// Where an agent's output goes.
#[derive(Debug)]
pub(crate) struct Sink {
    log: File,
    /// An agent computer's screen, which its person watches (ADR 0065).
    screen: Option<File>,
    most: u64,
    kept: u64,
    dropped: u64,
    tail: VecDeque<u8>,
}

impl Sink {
    pub(crate) fn new(log: File, screen: Option<File>) -> Self {
        Self::keeping(log, screen, LOG_MOST)
    }

    fn keeping(log: File, screen: Option<File>, most: u64) -> Self {
        Self {
            log,
            screen,
            most,
            kept: 0,
            dropped: 0,
            tail: VecDeque::with_capacity(TAIL),
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if let Some(screen) = &mut self.screen {
            let _ = screen.write_all(bytes);
        }
        let room = usize::try_from(self.most.saturating_sub(self.kept)).unwrap_or(usize::MAX);
        let keep = bytes.len().min(room);
        if keep > 0 && self.log.write_all(&bytes[..keep]).is_ok() {
            self.kept += keep as u64;
        }
        self.dropped += (bytes.len() - keep) as u64;
        let newest = &bytes[bytes.len().saturating_sub(TAIL)..];
        let over = (self.tail.len() + newest.len()).saturating_sub(TAIL);
        self.tail.drain(..over);
        self.tail.extend(newest);
    }

    /// Ends the log: one that was cut says by how much, then ends with the
    /// last of what the agent printed.
    fn finish(&mut self) {
        if self.dropped == 0 {
            return;
        }
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        let _ = write!(
            self.log,
            "\n[{} more bytes of output weren't kept: a task's log holds the first {} bytes. \
             The last of it follows.]\n",
            self.dropped, self.most
        );
        let _ = self.log.write_all(&tail);
    }

    /// The last line with anything on it, at a line's length.
    fn last_line(&self) -> Option<String> {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        String::from_utf8_lossy(&tail)
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(|line| glass_api::clip(line.trim(), REASON).into_owned())
    }
}

/// A running agent's output on its way to its [`Sink`].
#[derive(Debug)]
pub(crate) struct Output {
    sink: Arc<Mutex<Sink>>,
    copying: JoinHandle<()>,
}

impl Output {
    /// Copies what comes `from` the agent into `sink` until its end.
    pub(crate) fn copy(from: impl AsyncRead + Unpin + Send + 'static, sink: Sink) -> Self {
        let sink = Arc::new(Mutex::new(sink));
        let copying = tokio::spawn({
            let sink = Arc::clone(&sink);
            let mut from = from;
            async move {
                let mut buf = vec![0; 8192];
                while let Ok(n @ 1..) = from.read(&mut buf).await {
                    sink.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .write(&buf[..n]);
                }
            }
        });
        Self { sink, copying }
    }

    /// Once the agent is gone: the rest of what it printed is taken, the
    /// log ended, and the last line it printed returned.
    pub(crate) async fn close(mut self) -> Option<String> {
        if tokio::time::timeout(DRAIN, &mut self.copying)
            .await
            .is_err()
        {
            self.copying.abort();
        }
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        sink.finish();
        sink.last_line()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(most: u64) -> (tempfile::TempDir, std::path::PathBuf, Sink) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.log");
        let sink = Sink::keeping(File::create(&path).unwrap(), None, most);
        (dir, path, sink)
    }

    #[test]
    fn a_log_under_its_limit_is_what_the_agent_printed() {
        let (_dir, path, mut sink) = sink(1024);
        sink.write(b"one\ntwo\n");
        sink.write(b"three: the reason\n\n");
        sink.finish();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "one\ntwo\nthree: the reason\n\n"
        );
        assert_eq!(sink.last_line().as_deref(), Some("three: the reason"));
    }

    #[test]
    fn a_log_past_its_limit_keeps_its_start_and_its_end_and_says_what_went() {
        let (_dir, path, mut sink) = sink(100);
        // An agent printing in a loop: megabytes, then why it failed.
        for n in 0..50_000 {
            sink.write(format!("line {n} of noise\n").as_bytes());
        }
        sink.write(b"fatal: the reason it failed\n");
        sink.finish();
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.starts_with("line 0 of noise\n"), "the start is kept");
        assert!(
            log.contains("more bytes of output weren't kept"),
            "{log:.300}"
        );
        assert!(
            log.ends_with("fatal: the reason it failed\n"),
            "and the end"
        );
        assert!(
            log.len() < 100 + 2 * TAIL,
            "it has a size: {} bytes",
            log.len()
        );
        assert_eq!(
            sink.last_line().as_deref(),
            Some("fatal: the reason it failed")
        );
    }

    #[test]
    fn the_reason_is_a_lines_length_however_long_the_line() {
        let (_dir, _path, mut sink) = sink(u64::MAX);
        // One line of megabytes, as a program dumping its state prints.
        sink.write("x".repeat(4 * 1024 * 1024).as_bytes());
        let reason = sink.last_line().unwrap();
        assert!(reason.chars().count() < REASON + 40, "{}", reason.len());
        assert!(reason.contains("more characters"));
        // Nothing printed: no reason from it.
        let (_dir, _path, quiet) = sink_quiet();
        assert_eq!(quiet.last_line(), None);
    }

    fn sink_quiet() -> (tempfile::TempDir, std::path::PathBuf, Sink) {
        sink(10)
    }

    #[tokio::test]
    async fn output_is_copied_until_its_end_and_the_last_line_comes_back() {
        let (_dir, path, sink) = sink(1024);
        let (mut to, from) = tokio::io::duplex(64);
        let output = Output::copy(from, sink);
        tokio::io::AsyncWriteExt::write_all(&mut to, b"working\nerror: no such folder\n")
            .await
            .unwrap();
        drop(to);
        assert_eq!(
            output.close().await.as_deref(),
            Some("error: no such folder")
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "working\nerror: no such folder\n"
        );
    }
}
