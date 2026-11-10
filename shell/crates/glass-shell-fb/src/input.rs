//! The keyboard and pointer, read from the kernel's evdev devices on one
//! thread each and handed to the shell's loop as [`Input`].
//!
//! The devices are looked for again every [`RESCAN`]: a mouse plugged back
//! in, a keyboard that paired after sign-in, the other side of a KVM
//! switch. They were looked for once, when the screen came up, so none of
//! those was ever read, and with the last device gone nothing on the
//! screen could be answered, signing out included.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use glass_platform::input::{InputDevice, RawEvent, codes, devices};
use tokio::sync::mpsc;

/// How often the devices are looked for again. A device that came is read
/// within this.
pub const RESCAN: Duration = Duration::from_secs(2);

/// What the person did, in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// The pointer is at (x, y): a tablet or touch screen.
    MovedTo(f32, f32),
    /// The pointer moved by (dx, dy): a mouse.
    MovedBy(f32, f32),
    Button(iced_core::mouse::Button, bool),
    /// Wheel clicks, up positive.
    Wheel(f32),
    /// An evdev key code and its value: 1 down, 0 up, 2 repeat.
    Key(u16, i32),
}

/// Opens every input device, takes it from the console, and reads it on its
/// own thread; devices that come later are opened as they come. Returns
/// the stream and how many devices it reads to begin with. The looking
/// stops when the stream is dropped.
pub fn start(screen: (u32, u32)) -> (mpsc::UnboundedReceiver<Input>, usize) {
    let (tx, rx) = mpsc::unbounded_channel();
    let reading = Reading::new(tx, screen);
    let opened = reading.take(devices().unwrap_or_default(), open);
    thread::spawn(move || {
        loop {
            thread::sleep(RESCAN);
            if reading.tx.is_closed() {
                return;
            }
            reading.take(devices().unwrap_or_default(), open);
        }
    });
    (rx, opened)
}

/// Opens the device at `path` and takes it from the console.
fn open(path: &Path) -> Result<(InputDevice, Option<Axes>), String> {
    let device = InputDevice::open(path).map_err(|e| e.to_string())?;
    let name = device.name().unwrap_or_default();
    if let Err(e) = device.grab() {
        eprintln!("input: {} ({name}): can't take it: {e}", path.display());
    }
    let axes = device
        .abs_range(codes::ABS_X)
        .and_then(|x| Ok((x, device.abs_range(codes::ABS_Y)?)))
        .ok();
    eprintln!("input: {} ({name})", path.display());
    Ok((device, axes))
}

/// Something events are read from: a device, or a test's stand-in.
trait Source: Send + 'static {
    /// Waits for events. An error, or no events at all, is the device
    /// gone.
    fn read(&mut self) -> io::Result<Vec<RawEvent>>;
}

impl Source for InputDevice {
    fn read(&mut self) -> io::Result<Vec<RawEvent>> {
        InputDevice::read(self)
    }
}

/// The devices being read, by path, and where what they say goes.
#[derive(Clone)]
struct Reading {
    paths: Arc<Mutex<BTreeSet<PathBuf>>>,
    /// Those that couldn't be opened at the last look, so each is said
    /// once and not at every look.
    refused: Arc<Mutex<BTreeSet<PathBuf>>>,
    tx: mpsc::UnboundedSender<Input>,
    screen: (u32, u32),
}

impl Reading {
    fn new(tx: mpsc::UnboundedSender<Input>, screen: (u32, u32)) -> Self {
        Self {
            paths: Arc::default(),
            refused: Arc::default(),
            tx,
            screen,
        }
    }

    fn paths(&self) -> std::sync::MutexGuard<'_, BTreeSet<PathBuf>> {
        self.paths.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Opens those of `found` that aren't being read, each read on a
    /// thread of its own until it goes. Returns how many it opened. One
    /// that can't be opened is tried again at the next look (a device
    /// plugged in during a session is the person's a moment after it
    /// shows, when the sign-in screen has given it to them), and said
    /// once.
    fn take<S: Source>(
        &self,
        found: Vec<PathBuf>,
        open: impl Fn(&Path) -> Result<(S, Option<Axes>), String>,
    ) -> usize {
        let mut opened = 0;
        let mut refused = self.refused.lock().unwrap_or_else(|e| e.into_inner());
        refused.retain(|path| found.contains(path));
        for path in found {
            if self.paths().contains(&path) {
                continue;
            }
            let (device, axes) = match open(&path) {
                Ok(opened) => opened,
                Err(e) => {
                    if refused.insert(path.clone()) {
                        eprintln!("input: {}: can't read it yet: {e}", path.display());
                    }
                    continue;
                }
            };
            refused.remove(&path);
            self.paths().insert(path.clone());
            opened += 1;
            let reading = self.clone();
            thread::spawn(move || {
                let why = read_until_gone(device, Batch::new(axes, reading.screen), &reading.tx);
                // Said, where a reader used to end with nothing in the
                // log, and forgotten, so that the device is opened again
                // when it's back under the same name.
                if let Some(why) = why {
                    eprintln!("input: {} is gone: {why}", path.display());
                }
                reading.paths().remove(&path);
            });
        }
        opened
    }
}

/// Reads `device` until it goes (why, as `Some`) or nobody listens
/// (`None`). Keys and buttons it had down when it went are let go: a
/// keyboard switched away with Ctrl held (a KVM switch's own shortcut)
/// left Ctrl down for good.
fn read_until_gone(
    mut device: impl Source,
    mut batch: Batch,
    tx: &mpsc::UnboundedSender<Input>,
) -> Option<String> {
    let mut down: BTreeSet<Held> = BTreeSet::new();
    let why = loop {
        let events = match device.read() {
            Ok(events) if events.is_empty() => break "it closed".to_owned(),
            Ok(events) => events,
            Err(e) => break e.to_string(),
        };
        for input in events.iter().flat_map(|e| batch.push(*e)) {
            match input {
                Input::Key(code, 0) => drop(down.remove(&Held::Key(code))),
                Input::Key(code, _) => drop(down.insert(Held::Key(code))),
                Input::Button(button, true) => drop(down.insert(Held::Button(code_of(button)))),
                Input::Button(button, false) => drop(down.remove(&Held::Button(code_of(button)))),
                _ => {}
            }
            if tx.send(input).is_err() {
                return None;
            }
        }
    };
    for held in down {
        let up = match held {
            Held::Key(code) => Input::Key(code, 0),
            Held::Button(code) => Input::Button(button_of(code), false),
        };
        if tx.send(up).is_err() {
            return None;
        }
    }
    Some(why)
}

/// A key or a button a device has down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Held {
    Key(u16),
    Button(u16),
}

fn code_of(button: iced_core::mouse::Button) -> u16 {
    match button {
        iced_core::mouse::Button::Right => codes::BTN_RIGHT,
        iced_core::mouse::Button::Middle => codes::BTN_MIDDLE,
        _ => codes::BTN_LEFT,
    }
}

fn button_of(code: u16) -> iced_core::mouse::Button {
    match code {
        codes::BTN_RIGHT => iced_core::mouse::Button::Right,
        codes::BTN_MIDDLE => iced_core::mouse::Button::Middle,
        _ => iced_core::mouse::Button::Left,
    }
}

type Axes = (std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>);

/// One device's events between two `SYN_REPORT`s. A tablet reports only
/// the axes that changed, so its last X and Y are kept; a mouse's motion
/// adds up. Buttons and the wheel wait for the report's end, so a click
/// that came with a move acts where the move put the pointer.
struct Batch {
    axes: Option<Axes>,
    screen: (u32, u32),
    /// The tablet's position, as it last reported each axis.
    at: (Option<i32>, Option<i32>),
    /// The tablet moved in this report.
    moved: bool,
    by: (i32, i32),
    /// Buttons and wheel turns in this report, in order.
    waiting: Vec<Input>,
}

impl Batch {
    fn new(axes: Option<Axes>, screen: (u32, u32)) -> Self {
        Self {
            axes,
            screen,
            at: (None, None),
            moved: false,
            by: (0, 0),
            waiting: Vec::new(),
        }
    }

    /// What the event makes happen: nothing until the report ends, except
    /// keys, which stand alone.
    fn push(&mut self, event: RawEvent) -> Vec<Input> {
        match (event.kind, event.code) {
            (codes::EV_ABS, codes::ABS_X) => (self.at.0, self.moved) = (Some(event.value), true),
            (codes::EV_ABS, codes::ABS_Y) => (self.at.1, self.moved) = (Some(event.value), true),
            (codes::EV_REL, codes::REL_X) => self.by.0 += event.value,
            (codes::EV_REL, codes::REL_Y) => self.by.1 += event.value,
            (codes::EV_REL, codes::REL_WHEEL) => {
                self.waiting.push(Input::Wheel(event.value as f32));
            }
            (codes::EV_KEY, code) => {
                let button = match code {
                    codes::BTN_LEFT => iced_core::mouse::Button::Left,
                    codes::BTN_RIGHT => iced_core::mouse::Button::Right,
                    codes::BTN_MIDDLE => iced_core::mouse::Button::Middle,
                    _ => return vec![Input::Key(code, event.value)],
                };
                if event.value != 2 {
                    self.waiting.push(Input::Button(button, event.value == 1));
                }
            }
            (codes::EV_SYN, codes::SYN_REPORT) => return self.flush(),
            _ => {}
        }
        Vec::new()
    }

    /// The report's move first, then its buttons.
    fn flush(&mut self) -> Vec<Input> {
        let by = std::mem::take(&mut self.by);
        let moved = std::mem::take(&mut self.moved);
        let mut inputs = Vec::with_capacity(1 + self.waiting.len());
        if let (true, Some((xs, ys)), (Some(x), Some(y))) = (moved, &self.axes, self.at) {
            inputs.push(Input::MovedTo(
                scale(x, xs, self.screen.0),
                scale(y, ys, self.screen.1),
            ));
        } else if by != (0, 0) {
            inputs.push(Input::MovedBy(by.0 as f32, by.1 as f32));
        }
        inputs.append(&mut self.waiting);
        inputs
    }
}

/// An absolute axis value as a screen pixel.
fn scale(value: i32, range: &std::ops::RangeInclusive<i32>, pixels: u32) -> f32 {
    let span = (range.end() - range.start()).max(1) as f32;
    ((value - range.start()) as f32 / span * (pixels.saturating_sub(1)) as f32)
        .clamp(0.0, pixels as f32 - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: u16, code: u16, value: i32) -> RawEvent {
        RawEvent { kind, code, value }
    }

    #[test]
    fn a_tablets_x_and_y_make_one_move_in_screen_pixels() {
        let mut batch = Batch::new(Some((0..=32767, 0..=32767)), (1281, 801));
        assert!(
            batch
                .push(ev(codes::EV_ABS, codes::ABS_X, 16_384))
                .is_empty()
        );
        assert!(
            batch
                .push(ev(codes::EV_ABS, codes::ABS_Y, 32_767))
                .is_empty()
        );
        let moved = batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0));
        assert!(
            matches!(moved[..], [Input::MovedTo(x, y)] if (x - 640.0).abs() < 0.1 && y == 800.0)
        );
        // A button alone moves nothing.
        assert!(batch.push(ev(codes::EV_KEY, codes::BTN_LEFT, 1)).is_empty());
        assert_eq!(
            batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0)),
            [Input::Button(iced_core::mouse::Button::Left, true)]
        );
    }

    #[test]
    fn a_tablet_moving_along_one_axis_still_moves() {
        let mut batch = Batch::new(Some((0..=32767, 0..=32767)), (1281, 801));
        batch.push(ev(codes::EV_ABS, codes::ABS_X, 0));
        batch.push(ev(codes::EV_ABS, codes::ABS_Y, 0));
        batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0));
        // Only X changed: the report has no Y.
        batch.push(ev(codes::EV_ABS, codes::ABS_X, 32_767));
        assert_eq!(
            batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0)),
            [Input::MovedTo(1280.0, 0.0)]
        );
    }

    #[test]
    fn a_click_that_comes_with_a_move_lands_where_the_move_went() {
        let mut batch = Batch::new(Some((0..=32767, 0..=32767)), (1281, 801));
        batch.push(ev(codes::EV_ABS, codes::ABS_X, 0));
        batch.push(ev(codes::EV_ABS, codes::ABS_Y, 0));
        batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0));
        // The button can come before the axes in one report.
        batch.push(ev(codes::EV_KEY, codes::BTN_LEFT, 1));
        batch.push(ev(codes::EV_ABS, codes::ABS_X, 16_384));
        batch.push(ev(codes::EV_ABS, codes::ABS_Y, 16_384));
        let report = batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0));
        assert!(matches!(
            report[..],
            [
                Input::MovedTo(x, y),
                Input::Button(iced_core::mouse::Button::Left, true)
            ] if (x - 640.0).abs() < 0.1 && (y - 400.0).abs() < 0.1
        ));
    }

    /// A stand-in device: what it says, read by read, then gone.
    struct Script(std::collections::VecDeque<io::Result<Vec<RawEvent>>>);

    impl Source for Script {
        fn read(&mut self) -> io::Result<Vec<RawEvent>> {
            self.0
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::other("Device not configured")))
        }
    }

    fn script(reads: Vec<Vec<RawEvent>>) -> Script {
        Script(reads.into_iter().map(Ok).collect())
    }

    fn all(rx: &mut mpsc::UnboundedReceiver<Input>) -> Vec<Input> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn a_device_that_goes_lets_go_of_what_it_had_down() {
        // A keyboard switched away with Ctrl held, a mouse unplugged
        // mid-drag: the key and the button stayed down for good.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let keyboard = script(vec![
            vec![ev(codes::EV_KEY, 29, 1)],
            vec![ev(codes::EV_KEY, 30, 1), ev(codes::EV_KEY, 30, 0)],
            vec![ev(codes::EV_KEY, 29, 2)],
        ]);
        let why = read_until_gone(keyboard, Batch::new(None, (1280, 800)), &tx);
        assert_eq!(why.as_deref(), Some("Device not configured"));
        assert_eq!(
            all(&mut rx),
            [
                Input::Key(29, 1),
                Input::Key(30, 1),
                Input::Key(30, 0),
                Input::Key(29, 2),
                // Gone: Ctrl comes up, and the key already let go doesn't again.
                Input::Key(29, 0),
            ]
        );
        let mouse = script(vec![vec![
            ev(codes::EV_KEY, codes::BTN_LEFT, 1),
            ev(codes::EV_SYN, codes::SYN_REPORT, 0),
        ]]);
        read_until_gone(mouse, Batch::new(None, (1280, 800)), &tx);
        assert_eq!(
            all(&mut rx),
            [
                Input::Button(iced_core::mouse::Button::Left, true),
                Input::Button(iced_core::mouse::Button::Left, false),
            ]
        );
        // A read of nothing is the device closed, not something to ask
        // again at once, for good.
        let closed = script(vec![vec![]]);
        let why = read_until_gone(closed, Batch::new(None, (1280, 800)), &tx);
        assert_eq!(why.as_deref(), Some("it closed"));
        // Nobody listening ends the reader without a word.
        drop(rx);
        let unheard = script(vec![vec![ev(codes::EV_KEY, 30, 1)]]);
        assert_eq!(
            read_until_gone(unheard, Batch::new(None, (1280, 800)), &tx),
            None
        );
    }

    #[test]
    fn a_device_that_comes_later_or_comes_back_is_read() {
        // Looked for once, a keyboard paired after sign-in or a mouse
        // plugged back in was never read.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let reading = Reading::new(tx, (1280, 800));
        let (first, second) = (
            PathBuf::from("/dev/input/event0"),
            PathBuf::from("/dev/input/event1"),
        );
        // What each device says when opened: one key, named by how many
        // times it has been opened, and then it goes.
        let opens = Arc::new(Mutex::new(0u16));
        let open = |_: &Path| {
            let mut n = opens.lock().unwrap();
            *n += 1;
            Ok((script(vec![vec![ev(codes::EV_KEY, 100 + *n, 1)]]), None))
        };
        let wait = |rx: &mut mpsc::UnboundedReceiver<Input>, n: usize| {
            let mut got = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while got.len() < n {
                assert!(std::time::Instant::now() < deadline, "{got:?}");
                got.extend(all(rx));
                thread::sleep(Duration::from_millis(5));
            }
            got
        };
        let gone = |reading: &Reading| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !reading.paths().is_empty() {
                assert!(std::time::Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
        };

        // One device at first: read, then gone (its key let go).
        assert_eq!(reading.take(vec![first.clone()], open), 1);
        assert_eq!(wait(&mut rx, 2), [Input::Key(101, 1), Input::Key(101, 0)]);
        gone(&reading);
        // The next look finds it back, and another: both are read.
        assert_eq!(reading.take(vec![first.clone(), second.clone()], open), 2);
        let keys = wait(&mut rx, 4);
        assert!(
            keys.contains(&Input::Key(102, 1)) && keys.contains(&Input::Key(103, 1)),
            "{keys:?}"
        );
        gone(&reading);

        // One still being read isn't opened a second time.
        let (hold_tx, hold_rx) = std::sync::mpsc::channel::<()>();
        struct Held(std::sync::mpsc::Receiver<()>);
        impl Source for Held {
            fn read(&mut self) -> io::Result<Vec<RawEvent>> {
                let _ = self.0.recv();
                Err(io::Error::other("unplugged"))
            }
        }
        let held = Mutex::new(Some(hold_rx));
        let open_held = |_: &Path| {
            let rx = held.lock().unwrap().take();
            rx.map(|rx| (Held(rx), None))
                .ok_or("Permission denied".to_owned())
        };
        assert_eq!(reading.take(vec![first.clone()], open_held), 1);
        assert_eq!(reading.take(vec![first.clone()], open_held), 0);
        assert_eq!(reading.paths().len(), 1);
        drop(hold_tx);
        gone(&reading);
        // One that can't be opened is tried again at the next look, and
        // noted once however many looks it takes.
        assert_eq!(reading.take(vec![second.clone()], open_held), 0);
        assert_eq!(reading.take(vec![second.clone()], open_held), 0);
        assert!(reading.paths().is_empty());
        assert_eq!(reading.refused.lock().unwrap().len(), 1);
        // Opened in the end, or gone, it isn't one that refused any more.
        assert_eq!(reading.take(vec![second.clone()], open), 1);
        assert!(reading.refused.lock().unwrap().is_empty());
        wait(&mut rx, 2);
        gone(&reading);
        assert_eq!(reading.take(vec![second.clone()], open_held), 0);
        assert_eq!(reading.take(vec![], open_held), 0);
        assert!(reading.refused.lock().unwrap().is_empty());
    }

    #[test]
    fn a_mouses_motion_adds_up_and_keys_pass_through() {
        let mut batch = Batch::new(None, (1280, 800));
        batch.push(ev(codes::EV_REL, codes::REL_X, 3));
        batch.push(ev(codes::EV_REL, codes::REL_X, 2));
        assert_eq!(
            batch.push(ev(codes::EV_SYN, codes::SYN_REPORT, 0)),
            [Input::MovedBy(5.0, 0.0)]
        );
        assert_eq!(batch.push(ev(codes::EV_KEY, 30, 1)), [Input::Key(30, 1)]);
    }
}
