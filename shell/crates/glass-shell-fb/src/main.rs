//! `glass-shell-fb`: the desktop on the console framebuffer, without a
//! compositor (ADR 0016, the first step of M2). It draws the caller's live
//! tiles from glassd and takes the keyboard and pointer from the kernel's
//! evdev devices: Approve on a tile answers its ask, Ctrl Space opens the
//! command bar. It redraws when a task changes, the person does something,
//! or the menu bar clock turns over: only the areas whose content changed
//! are drawn again and copied to the screen, and moving the pointer
//! repaints only the rows it crosses.
//!
//! ```text
//! glass-shell-fb [--tty /dev/ttyv0] [--socket /var/run/glasshouse/control.sock] [--once] [--no-input]
//! ```
//!
//! The terminal shows the desktop until the program ends (Ctrl-C or
//! SIGTERM), then goes back to text. `--once` draws one frame and exits; it
//! stays on screen until the console next draws. The tiles are the caller's,
//! as `glassctl tasks` shows them, and the caller needs read and write
//! access to the terminal and read access to `/dev/input/event*`.

mod input;
mod keys;
mod pointer;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use glass_api::{Event, TaskView, methods};
use glass_platform::framebuffer::{Framebuffer, Order};
use glass_rpc::Client;
use glass_shell::live::{Effect, Live};
use glass_shell::render::Screen;
use glass_shell::{Size, Theme, wallpaper};
use iced_core::{Point, mouse};
use time::{OffsetDateTime, UtcOffset};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::input::Input;

/// Changes that arrive this close together make one redraw.
const SETTLE: Duration = Duration::from_millis(50);

struct Args {
    tty: PathBuf,
    socket: PathBuf,
    once: bool,
    input: bool,
}

fn args() -> anyhow::Result<Args> {
    let mut parsed = Args {
        tty: PathBuf::from("/dev/ttyv0"),
        socket: PathBuf::from("/var/run/glasshouse/control.sock"),
        once: false,
        input: true,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--tty" => parsed.tty = value()?.into(),
            "--socket" => parsed.socket = value()?.into(),
            "--once" => parsed.once = true,
            "--no-input" => parsed.input = false,
            "-h" | "--help" => {
                eprintln!(
                    "usage: glass-shell-fb [--tty PATH] [--socket PATH] [--once] [--no-input]"
                );
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

    let mut shell = Shell::open(args.tty, offset, Live::new(tasks).without_wallpaper())?;
    shell.draw();
    if args.once {
        return Ok(());
    }
    let (mut inputs, devices) = if args.input {
        input::start(shell.screen_size())
    } else {
        (mpsc::unbounded_channel().1, 0)
    };
    eprintln!("reading {devices} input devices");
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    loop {
        let redraw;
        tokio::select! {
            note = client.next_notification() => {
                shell.apply(note)?;
                let settled = tokio::time::sleep(SETTLE);
                tokio::pin!(settled);
                loop {
                    tokio::select! {
                        () = &mut settled => break,
                        note = client.next_notification() => shell.apply(note)?,
                    }
                }
                redraw = true;
            }
            Some(first) = inputs.recv() => {
                // Everything already waiting, then one redraw for all of it.
                let mut batch = vec![first];
                while let Ok(more) = inputs.try_recv() {
                    batch.push(more);
                }
                for effect in shell.input(&batch) {
                    shell.carry_out(&client, effect).await;
                }
                redraw = shell.changed();
            }
            () = tokio::time::sleep(until_next_minute(OffsetDateTime::now_utc())) => redraw = true,
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
        if redraw {
            shell.draw();
        }
    }
    Ok(())
}

/// The desktop on the framebuffer: its state, its UI (which keeps the
/// pixels drawn) and where the pointer is.
struct Shell {
    fb: Framebuffer,
    theme: Theme,
    offset: UtcOffset,
    live: Live,
    screen: Screen,
    keyboard: keys::Keyboard,
    pointer: Point,
    /// Where the pointer was painted, if it was.
    painted: Option<Point>,
    /// Something changed that the next frame should show.
    dirty: bool,
}

impl Shell {
    fn open(tty: PathBuf, offset: UtcOffset, live: Live) -> anyhow::Result<Self> {
        let fb = Framebuffer::open(&tty)?;
        let g = fb.geometry();
        eprintln!(
            "{}: {}x{}, {}-byte rows",
            tty.display(),
            g.width,
            g.height,
            g.stride
        );
        let size = Size::new(g.width as f32, g.height as f32);
        let theme = Theme::glass();
        // The wallpaper stays drawn behind the UI, so a change redraws only
        // what's over it.
        let mut screen = Screen::new(size, 1.0);
        screen.set_backdrop(wallpaper::backdrop(&theme.wallpaper, g.width, g.height));
        Ok(Self {
            fb,
            theme,
            offset,
            live,
            screen,
            keyboard: keys::Keyboard::default(),
            pointer: Point::new(size.width / 2.0, size.height / 2.0),
            painted: None,
            dirty: false,
        })
    }

    fn screen_size(&self) -> (u32, u32) {
        let g = self.fb.geometry();
        (g.width, g.height)
    }

    fn size(&self) -> (f32, f32) {
        let (w, h) = self.screen_size();
        (w as f32, h as f32)
    }

    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc().to_offset(self.offset)
    }

    /// Folds in one notification from glassd.
    fn apply(&mut self, note: Option<glass_rpc::Request>) -> anyhow::Result<()> {
        let note = note.context("glassd closed the connection")?;
        let event = note
            .params
            .filter(|_| note.method == methods::EVENT)
            .and_then(|params| serde_json::from_value::<Event>(params).ok());
        if let Some(event) = event {
            self.live.apply(event);
        }
        Ok(())
    }

    /// Handles a batch of input: the pointer moves at once; clicks and keys
    /// go through the UI. Returns what they ask for.
    fn input(&mut self, batch: &[Input]) -> Vec<Effect> {
        let (w, h) = self.size();
        let mut events = Vec::new();
        let mut shortcuts = Vec::new();
        for input in batch {
            match *input {
                Input::MovedTo(x, y) => self.pointer = Point::new(x, y),
                Input::MovedBy(dx, dy) => {
                    self.pointer = Point::new(
                        (self.pointer.x + dx).clamp(0.0, w - 1.0),
                        (self.pointer.y + dy).clamp(0.0, h - 1.0),
                    );
                }
                Input::Button(button, down) => {
                    events.push(iced_core::Event::Mouse(mouse::Event::CursorMoved {
                        position: self.pointer,
                    }));
                    events.push(iced_core::Event::Mouse(if down {
                        mouse::Event::ButtonPressed(button)
                    } else {
                        mouse::Event::ButtonReleased(button)
                    }));
                }
                Input::Wheel(lines) => {
                    events.push(iced_core::Event::Mouse(mouse::Event::WheelScrolled {
                        delta: mouse::ScrollDelta::Lines { x: 0.0, y: lines },
                    }));
                }
                Input::Key(code, value) => {
                    for press in self.keyboard.key(code, value) {
                        let shortcut =
                            matches!(press.event, iced_core::keyboard::Event::KeyPressed { .. })
                                .then(|| self.live.shortcut(&press.key, press.modifiers))
                                .flatten();
                        match shortcut {
                            Some(message) => shortcuts.push(message),
                            None => events.push(iced_core::Event::Keyboard(press.event)),
                        }
                    }
                }
            }
        }
        self.paint_pointer();

        let mut messages = shortcuts;
        if !events.is_empty() {
            let bar = self.live.command_bar();
            let view = self
                .live
                .view(&self.theme, bar.as_ref(), self.now(), self.size());
            messages.extend(self.screen.update(view, &events, Some(self.pointer)));
            // A press or a key can change how the UI looks without a message
            // (a focused input, a held button).
            self.dirty = true;
        }
        let mut effects = Vec::new();
        for message in messages {
            self.dirty = true;
            match self.live.update(message) {
                Some(Effect::Focus(id)) => {
                    let bar = self.live.command_bar();
                    let view = self
                        .live
                        .view(&self.theme, bar.as_ref(), self.now(), self.size());
                    self.screen.focus(view, id);
                }
                Some(effect) => effects.push(effect),
                None => {}
            }
        }
        effects
    }

    /// Whether the last input changed what the screen should show.
    fn changed(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Makes the call an effect asks for. A refusal shows up in the tiles,
    /// so it's only logged here.
    async fn carry_out(&mut self, client: &Client, effect: Effect) {
        if let Effect::Answer(answer) = effect {
            let result: Result<serde_json::Value, _> =
                client.call(methods::APPROVAL_ANSWER, &answer).await;
            match result {
                Ok(_) => eprintln!("answered {}", answer.approval),
                Err(e) => eprintln!("approval.answer: {e}"),
            }
        }
    }

    fn draw(&mut self) {
        let started = Instant::now();
        let (w, h) = self.size();
        let bar = self.live.command_bar();
        let view = self
            .live
            .view(&self.theme, bar.as_ref(), self.now(), (w, h));
        let damage = self.screen.render(view, Some(self.pointer));
        drop(bar);
        let rendered = started.elapsed();
        let (width, _) = self.screen_size();
        let stride = width as usize * 4;
        let mut rows = 0;
        for area in &damage {
            let band = area.y as usize * stride..(area.y + area.height) as usize * stride;
            self.fb
                .write_rows(&self.screen.bgra()[band], Order::Bgra, width, area.y);
            rows += area.height;
        }
        if !damage.is_empty() {
            // A redrawn area may have covered the pointer; paint it afresh.
            self.painted = None;
            self.paint_pointer();
        }
        eprintln!(
            "{} tiles: drawn in {} ms, {} areas ({rows} rows) copied in {} ms",
            self.live.tasks().len(),
            rendered.as_millis(),
            damage.len(),
            (started.elapsed() - rendered).as_millis()
        );
    }

    /// Restores the rows under the pointer's old place from the screen's
    /// pixels and paints it at its new one.
    fn paint_pointer(&mut self) {
        let (width, height) = self.screen_size();
        let stride = width as usize * 4;
        if self.painted == Some(self.pointer) {
            return;
        }
        let at = (self.pointer.x as u32, self.pointer.y as u32);
        let pixels = self.screen.bgra();
        if let Some(old) = self.painted {
            let rows = pointer::rows(old.y as u32, height);
            let band = &pixels[rows.start as usize * stride..rows.end as usize * stride];
            self.fb.write_rows(band, Order::Bgra, width, rows.start);
        }
        let rows = pointer::rows(at.1, height);
        let mut band = pixels[rows.start as usize * stride..rows.end as usize * stride].to_vec();
        pointer::paint(&mut band, width, rows.start, at.0, at.1);
        self.fb.write_rows(&band, Order::Bgra, width, rows.start);
        self.painted = Some(self.pointer);
    }
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
    fn the_clock_redraws_on_the_minute() {
        let at = OffsetDateTime::from_unix_timestamp(1_792_418_040 + 45).expect("valid");
        assert_eq!(until_next_minute(at), Duration::from_secs(15));
    }
}
