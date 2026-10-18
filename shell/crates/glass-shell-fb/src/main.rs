//! `glass-shell-fb`: the desktop on the console framebuffer, without a
//! compositor (ADR 0016, the first step of M2). It draws the caller's live
//! tiles from glassd, and redraws when a task changes or the menu bar clock
//! turns over, copying only the rows that changed to the screen.
//!
//! ```text
//! glass-shell-fb [--tty /dev/ttyv0] [--socket /var/run/glasshouse/control.sock] [--once]
//! ```
//!
//! The terminal shows the desktop until the program ends (Ctrl-C or
//! SIGTERM), then goes back to text. `--once` draws one frame and exits; it
//! stays on screen until the console next draws. The tiles are the caller's,
//! as `glassctl tasks` shows them, and the caller needs read and write
//! access to the terminal.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use glass_api::{Event, TaskView, methods};
use glass_platform::framebuffer::Framebuffer;
use glass_rpc::Client;
use glass_shell::screens::desktop::{self, Desktop};
use glass_shell::{Size, Theme, render};
use glass_types::TaskId;
use time::{OffsetDateTime, UtcOffset};
use tokio::signal::unix::{SignalKind, signal};

/// Changes that arrive this close together make one redraw.
const SETTLE: Duration = Duration::from_millis(50);

struct Args {
    tty: PathBuf,
    socket: PathBuf,
    once: bool,
}

fn args() -> anyhow::Result<Args> {
    let mut parsed = Args {
        tty: PathBuf::from("/dev/ttyv0"),
        socket: PathBuf::from("/var/run/glasshouse/control.sock"),
        once: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--tty" => parsed.tty = value()?.into(),
            "--socket" => parsed.socket = value()?.into(),
            "--once" => parsed.once = true,
            "-h" | "--help" => {
                eprintln!("usage: glass-shell-fb [--tty PATH] [--socket PATH] [--once]");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(parsed)
}

fn main() -> anyhow::Result<()> {
    let args = args()?;
    // Only readable while the process has one thread.
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run(args, offset))
}

async fn run(args: Args, offset: UtcOffset) -> anyhow::Result<()> {
    let client = Client::connect(&args.socket)
        .await
        .with_context(|| format!("connecting to {}", args.socket.display()))?;
    // Subscribe first, so no change falls between the list and the events.
    let _: serde_json::Value = client
        .call(methods::EVENTS_SUBSCRIBE, ())
        .await
        .map_err(|e| anyhow!("events.subscribe: {e}"))?;
    let tasks: Vec<TaskView> = client
        .call(methods::TASK_LIST, ())
        .await
        .map_err(|e| anyhow!("task.list: {e}"))?;
    let mut tiles: BTreeMap<TaskId, TaskView> = tasks.into_iter().map(|t| (t.id, t)).collect();

    let mut screen = Screen::open(args.tty, offset)?;
    screen.draw(&tiles);
    if args.once {
        return Ok(());
    }
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    loop {
        tokio::select! {
            note = client.next_notification() => {
                apply(&mut tiles, note)?;
                let settled = tokio::time::sleep(SETTLE);
                tokio::pin!(settled);
                loop {
                    tokio::select! {
                        () = &mut settled => break,
                        note = client.next_notification() => apply(&mut tiles, note)?,
                    }
                }
            }
            () = tokio::time::sleep(until_next_minute(OffsetDateTime::now_utc())) => {}
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
        screen.draw(&tiles);
    }
    Ok(())
}

/// Folds one notification from glassd into the tiles.
fn apply(
    tiles: &mut BTreeMap<TaskId, TaskView>,
    note: Option<glass_rpc::Request>,
) -> anyhow::Result<()> {
    let note = note.context("glassd closed the connection")?;
    let event = note
        .params
        .filter(|_| note.method == methods::EVENT)
        .and_then(|params| serde_json::from_value::<Event>(params).ok());
    match event {
        Some(Event::TaskUpdated { task }) => {
            tiles.insert(task.id, *task);
        }
        Some(Event::TaskRemoved { task }) => {
            tiles.remove(&task);
        }
        None => {}
    }
    Ok(())
}

/// The framebuffer and the last frame drawn on it.
struct Screen {
    fb: Framebuffer,
    theme: Theme,
    offset: UtcOffset,
    last: Vec<u8>,
}

impl Screen {
    fn open(tty: PathBuf, offset: UtcOffset) -> anyhow::Result<Self> {
        let fb = Framebuffer::open(&tty)?;
        let g = fb.geometry();
        eprintln!(
            "{}: {}x{}, {}-byte rows",
            tty.display(),
            g.width,
            g.height,
            g.stride
        );
        Ok(Self {
            fb,
            theme: Theme::glass(),
            offset,
            last: Vec::new(),
        })
    }

    fn draw(&mut self, tiles: &BTreeMap<TaskId, TaskView>) {
        let started = Instant::now();
        let g = self.fb.geometry();
        let size = (g.width as f32, g.height as f32);
        let tasks: Vec<TaskView> = tiles.values().cloned().collect();
        let state = Desktop::live(&tasks, OffsetDateTime::now_utc().to_offset(self.offset));
        let view = desktop::view(&self.theme, &state, Vec::new(), |_| None, None, size);
        let frame = render::frame(view, Size::new(size.0, size.1), 1.0);
        let rendered = started.elapsed();
        let rows = changed_rows(&self.last, &frame.rgba, frame.width);
        if let Some(rows) = rows.clone() {
            self.fb.write_rgba(&frame.rgba, frame.width, rows);
        }
        eprintln!(
            "{} tiles: drawn in {} ms, {} rows copied in {} ms",
            tasks.len(),
            rendered.as_millis(),
            rows.map_or(0, |r| r.len()),
            (started.elapsed() - rendered).as_millis()
        );
        self.last = frame.rgba;
    }
}

/// The rows that differ between two frames `width` pixels wide, as one
/// range: all of them when the frames' sizes differ, `None` when they're
/// the same.
fn changed_rows(before: &[u8], after: &[u8], width: u32) -> Option<Range<u32>> {
    let stride = width as usize * 4;
    if stride == 0 {
        return None;
    }
    if before.len() != after.len() {
        return Some(0..(after.len() / stride) as u32);
    }
    let differs = |(a, b): (&[u8], &[u8])| a != b;
    let mut rows = before.chunks(stride).zip(after.chunks(stride));
    let first = rows.clone().position(differs)?;
    let last = rows.rposition(differs)?;
    Some(first as u32..last as u32 + 1)
}

/// How long until the clock next shows a new minute.
fn until_next_minute(now: OffsetDateTime) -> Duration {
    let into = Duration::new(u64::from(now.second()), now.nanosecond());
    Duration::from_secs(60).saturating_sub(into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_rows_that_changed_are_copied() {
        let before = vec![0u8; 2 * 4 * 5];
        let mut after = before.clone();
        assert_eq!(changed_rows(&before, &after, 2), None);
        after[8] = 1; // row 1
        after[3 * 8 + 4] = 1; // row 3
        assert_eq!(changed_rows(&before, &after, 2), Some(1..4));
        assert_eq!(changed_rows(&[], &after, 2), Some(0..5));
    }

    #[test]
    fn the_clock_redraws_on_the_minute() {
        let at = OffsetDateTime::from_unix_timestamp(1_792_418_040 + 45).expect("valid");
        assert_eq!(until_next_minute(at), Duration::from_secs(15));
    }
}
