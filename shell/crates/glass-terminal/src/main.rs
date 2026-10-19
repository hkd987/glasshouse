//! `glass-terminal`: Terminal, the person's shell in a window. A native
//! first-party app (UI-8, ADR 0025) on
//! `glass-window`: alacritty's terminal state and pseudo-terminal, drawn as
//! rows of text in Geist Mono in the desktop's dark style.
//!
//! ```text
//! glass-terminal [--title TEXT] [-- PROGRAM ARGS...]
//! ```
//!
//! Runs the person's login shell (`$SHELL -l`, `/bin/sh` without one), or
//! PROGRAM; the window closes when it exits.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{self, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as TermColor, CursorShape, NamedColor, Rgb};
use anyhow::Context;
use glass_shell::{Element, fonts};
use glass_window::{App, KeyPress, Wake, Waker};
use iced_core::keyboard::Modifiers;
use iced_core::text::LineHeight;
use iced_core::{Color, Length, Padding};
use iced_widget::{Column, Row, container, mouse_area, text};
use xkb::Keysym;

use glass_window::xkb;

const COLUMNS: usize = 80;
const LINES: usize = 24;
const FONT_SIZE: f32 = 13.0;
/// Geist Mono's advance is 0.6 em.
const CELL_WIDTH: f32 = FONT_SIZE * 0.6;
const CELL_HEIGHT: f32 = 17.0;
const PADDING: f32 = 10.0;
/// The window's background and the terminal's default colors.
const BACKGROUND: Rgb = Rgb {
    r: 0x15,
    g: 0x18,
    b: 0x1D,
};
const FOREGROUND: Rgb = Rgb {
    r: 0xE6,
    g: 0xE8,
    b: 0xEB,
};
/// The 16 ANSI colors, readable on the dark background.
const ANSI: [Rgb; 16] = [
    rgb(0x1B1E24),
    rgb(0xE5534B),
    rgb(0x57AB5A),
    rgb(0xC69026),
    rgb(0x539BF5),
    rgb(0xB083F0),
    rgb(0x39C5CF),
    rgb(0xADBAC7),
    rgb(0x636E7B),
    rgb(0xFF938A),
    rgb(0x6BC46D),
    rgb(0xDAAA3F),
    rgb(0x6CB6FF),
    rgb(0xDCBDFB),
    rgb(0x56D4DD),
    rgb(0xF0F6FC),
];

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

fn main() -> anyhow::Result<()> {
    let mut title = "Terminal".to_owned();
    let mut program = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--title" => title = args.next().context("--title needs a value")?,
            "--" => {
                let rest: Vec<String> = args.by_ref().collect();
                program = rest.split_first().map(|(p, a)| (p.clone(), a.to_vec()));
            }
            "-h" | "--help" => {
                eprintln!("usage: glass-terminal [--title TEXT] [-- PROGRAM ARGS...]");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let wake = Wake::new()?;
    let terminal = Terminal::start(title, program, wake.waker()?)?;
    glass_window::run(terminal, wake)
}

/// Events from the terminal's reader thread, kept for the window's loop.
#[derive(Clone)]
struct Proxy {
    waker: Arc<Waker>,
    events: Arc<Mutex<Vec<TermEvent>>>,
}

impl EventListener for Proxy {
    fn send_event(&self, event: TermEvent) {
        if !matches!(event, TermEvent::Wakeup)
            && let Ok(mut events) = self.events.lock()
        {
            events.push(event);
        }
        self.waker.wake();
    }
}

struct Size;

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        LINES
    }
    fn screen_lines(&self) -> usize {
        LINES
    }
    fn columns(&self) -> usize {
        COLUMNS
    }
}

struct Terminal {
    term: Arc<FairMutex<Term<Proxy>>>,
    notifier: Notifier,
    events: Arc<Mutex<Vec<TermEvent>>>,
    default_title: String,
    title: String,
    exited: bool,
}

#[derive(Debug, Clone)]
enum Message {
    /// The wheel turned: lines, up positive.
    Scroll(f32),
}

impl Terminal {
    fn start(
        title: String,
        program: Option<(String, Vec<String>)>,
        waker: Waker,
    ) -> anyhow::Result<Self> {
        let shell = program.unwrap_or_else(|| {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            (shell, vec!["-l".into()])
        });
        let options = tty::Options {
            shell: Some(tty::Shell::new(shell.0, shell.1)),
            working_directory: std::env::var_os("HOME").map(Into::into),
            drain_on_exit: false,
            env: HashMap::from([
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ]),
        };
        let size = WindowSize {
            num_lines: LINES as u16,
            num_cols: COLUMNS as u16,
            cell_width: CELL_WIDTH as u16,
            cell_height: CELL_HEIGHT as u16,
        };
        let pty = tty::new(&options, size, 0).context("opening a pseudo-terminal")?;
        let events = Arc::new(Mutex::new(Vec::new()));
        let proxy = Proxy {
            waker: Arc::new(waker),
            events: events.clone(),
        };
        let term = Arc::new(FairMutex::new(Term::new(
            term::Config::default(),
            &Size,
            proxy.clone(),
        )));
        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)
            .context("starting the terminal")?;
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();
        Ok(Self {
            term,
            notifier,
            events,
            title: title.clone(),
            default_title: title,
            exited: false,
        })
    }
}

impl App for Terminal {
    type Message = Message;

    fn title(&self) -> String {
        self.title.clone()
    }

    fn app_id(&self) -> String {
        "glass.terminal".into()
    }

    fn size(&self) -> (u32, u32) {
        (
            (COLUMNS as f32 * CELL_WIDTH + PADDING * 2.0).ceil() as u32,
            (LINES as f32 * CELL_HEIGHT + PADDING * 2.0).ceil() as u32,
        )
    }

    fn background(&self) -> [u8; 4] {
        [BACKGROUND.b, BACKGROUND.g, BACKGROUND.r, 0xFF]
    }

    fn view(&self) -> Element<'_, Message> {
        let rows = self.rows();
        let mut lines = Column::new();
        for row in rows {
            let mut runs = Row::new();
            for run in row {
                let mut piece = container(
                    text(run.text)
                        .font(fonts::MONO)
                        .size(FONT_SIZE)
                        .line_height(LineHeight::Absolute(CELL_HEIGHT.into()))
                        .color(color(run.fg)),
                );
                if run.bg != BACKGROUND {
                    let bg = color(run.bg);
                    piece = piece.style(move |_| container::Style {
                        background: Some(bg.into()),
                        ..container::Style::default()
                    });
                }
                runs = runs.push(piece);
            }
            lines = lines.push(runs.height(Length::Fixed(CELL_HEIGHT)));
        }
        mouse_area(
            container(lines)
                .padding(Padding::new(PADDING))
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_scroll(|delta| {
            let lines = match delta {
                iced_core::mouse::ScrollDelta::Lines { y, .. } => y,
                iced_core::mouse::ScrollDelta::Pixels { y, .. } => y / CELL_HEIGHT,
            };
            Message::Scroll(lines)
        })
        .into()
    }

    fn update(&mut self, message: Message) {
        match message {
            Message::Scroll(lines) => {
                let lines = (lines * 3.0).round() as i32;
                self.term.lock().scroll_display(Scroll::Delta(lines));
            }
        }
    }

    fn key(&mut self, press: &KeyPress) -> bool {
        let app_cursor = self.term.lock().mode().contains(TermMode::APP_CURSOR);
        if let Some(bytes) = key_bytes(press, app_cursor) {
            // Typing shows the bottom again, as in any terminal.
            self.term.lock().scroll_display(Scroll::Bottom);
            self.notifier.notify(bytes);
        }
        true
    }

    fn woken(&mut self) -> bool {
        let events = std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()));
        for event in events {
            match event {
                TermEvent::Title(title) => self.title = title,
                TermEvent::ResetTitle => self.title = self.default_title.clone(),
                TermEvent::PtyWrite(text) => self.notifier.notify(text.into_bytes()),
                TermEvent::ChildExit(_) | TermEvent::Exit => self.exited = true,
                _ => {}
            }
        }
        true
    }

    fn done(&self) -> bool {
        self.exited
    }
}

/// A stretch of a row in one look.
struct Run {
    text: String,
    fg: Rgb,
    bg: Rgb,
}

impl Terminal {
    /// The screen as rows of runs, the cursor shown as one cell in reverse.
    fn rows(&self) -> Vec<Vec<Run>> {
        let term = self.term.lock();
        let content = term.renderable_content();
        let colors = content.colors;
        let resolve = |c: TermColor| -> Rgb {
            match c {
                TermColor::Spec(rgb) => rgb,
                TermColor::Named(NamedColor::Foreground) => FOREGROUND,
                TermColor::Named(NamedColor::Background) => BACKGROUND,
                TermColor::Named(named) => {
                    let index = named as usize;
                    colors[index].unwrap_or_else(|| indexed(index as u8))
                }
                TermColor::Indexed(index) => {
                    colors[index as usize].unwrap_or_else(|| indexed(index))
                }
            }
        };
        let cursor = content.cursor;
        let offset = content.display_offset as i32;
        let mut rows: Vec<Vec<Run>> = (0..LINES).map(|_| Vec::new()).collect();
        for indexed_cell in content.display_iter {
            let line = indexed_cell.point.line.0 + offset;
            let Some(row) = usize::try_from(line).ok().and_then(|l| rows.get_mut(l)) else {
                continue;
            };
            let cell = &indexed_cell.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let (mut fg, mut bg) = (resolve(cell.fg), resolve(cell.bg));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let at_cursor = cursor.shape != CursorShape::Hidden
                && indexed_cell.point == cursor.point
                && content.display_offset == 0;
            if at_cursor {
                (fg, bg) = (BACKGROUND, FOREGROUND);
            }
            let c = if cell.c == '\0' { ' ' } else { cell.c };
            match row.last_mut() {
                Some(run) if run.fg == fg && run.bg == bg && !at_cursor => run.text.push(c),
                _ => row.push(Run {
                    text: c.to_string(),
                    fg,
                    bg,
                }),
            }
            // The cursor's cell stands alone, so the next cell starts afresh.
            if at_cursor {
                row.push(Run {
                    text: String::new(),
                    fg: BACKGROUND,
                    bg: BACKGROUND,
                });
            }
        }
        for row in &mut rows {
            row.retain(|run| !run.text.is_empty());
        }
        rows
    }
}

fn color(rgb: Rgb) -> Color {
    Color::from_rgb8(rgb.r, rgb.g, rgb.b)
}

/// xterm's 256 colors: the 16 ANSI ones, a 6×6×6 cube, then greys.
fn indexed(index: u8) -> Rgb {
    match index {
        0..16 => ANSI[index as usize],
        16..232 => {
            let i = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb {
                r: level(i / 36),
                g: level(i / 6 % 6),
                b: level(i % 6),
            }
        }
        _ => {
            let v = 8 + (index - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
    }
}

/// What a key sends to the shell, as xterm sends it. Arrows say `ESC O`
/// instead of `ESC [` in application cursor mode (as `vi` asks for).
fn key_bytes(press: &KeyPress, app_cursor: bool) -> Option<Cow<'static, [u8]>> {
    let arrow = |c: u8| -> Cow<'static, [u8]> {
        if app_cursor {
            vec![0x1b, b'O', c].into()
        } else {
            vec![0x1b, b'[', c].into()
        }
    };
    let fixed = |s: &'static [u8]| Some(Cow::Borrowed(s));
    match press.keysym {
        Keysym::Return | Keysym::KP_Enter => return fixed(b"\r"),
        Keysym::BackSpace => return fixed(b"\x7f"),
        Keysym::Tab => return fixed(b"\t"),
        Keysym::ISO_Left_Tab => return fixed(b"\x1b[Z"),
        Keysym::Escape => return fixed(b"\x1b"),
        Keysym::Up => return Some(arrow(b'A')),
        Keysym::Down => return Some(arrow(b'B')),
        Keysym::Right => return Some(arrow(b'C')),
        Keysym::Left => return Some(arrow(b'D')),
        Keysym::Home => return Some(arrow(b'H')),
        Keysym::End => return Some(arrow(b'F')),
        Keysym::Insert => return fixed(b"\x1b[2~"),
        Keysym::Delete => return fixed(b"\x1b[3~"),
        Keysym::Page_Up => return fixed(b"\x1b[5~"),
        Keysym::Page_Down => return fixed(b"\x1b[6~"),
        Keysym::F1 => return fixed(b"\x1bOP"),
        Keysym::F2 => return fixed(b"\x1bOQ"),
        Keysym::F3 => return fixed(b"\x1bOR"),
        Keysym::F4 => return fixed(b"\x1bOS"),
        _ => {}
    }
    let mut bytes = press.text.clone().into_bytes();
    if bytes.is_empty() {
        return None;
    }
    // Ctrl with a letter is that letter's control code (xkb gives it when
    // the keymap does; this covers the rest).
    if press.modifiers.contains(Modifiers::CTRL)
        && bytes.len() == 1
        && bytes[0].is_ascii_alphabetic()
    {
        bytes[0] &= 0x1f;
    }
    // Alt sends ESC first.
    if press.modifiers.contains(Modifiers::ALT) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(keysym: Keysym, text: &str, modifiers: Modifiers) -> KeyPress {
        KeyPress {
            keysym,
            text: text.into(),
            modifiers,
            repeat: false,
        }
    }

    fn sent(p: &KeyPress, app_cursor: bool) -> Vec<u8> {
        key_bytes(p, app_cursor)
            .map(|b| b.into_owned())
            .unwrap_or_default()
    }

    #[test]
    fn keys_send_what_a_shell_expects() {
        let none = Modifiers::empty();
        assert_eq!(sent(&press(Keysym::a, "a", none), false), b"a");
        assert_eq!(sent(&press(Keysym::Return, "\r", none), false), b"\r");
        assert_eq!(
            sent(&press(Keysym::BackSpace, "\x08", none), false),
            b"\x7f"
        );
        assert_eq!(sent(&press(Keysym::Up, "", none), false), b"\x1b[A");
        assert_eq!(sent(&press(Keysym::Up, "", none), true), b"\x1bOA");
        // Ctrl C interrupts; Alt b goes back a word.
        assert_eq!(
            sent(&press(Keysym::c, "c", Modifiers::CTRL), false),
            b"\x03"
        );
        assert_eq!(
            sent(&press(Keysym::b, "b", Modifiers::ALT), false),
            b"\x1bb"
        );
        assert_eq!(sent(&press(Keysym::Shift_L, "", none), false), b"");
    }

    #[test]
    fn colors_cover_the_256_palette() {
        assert_eq!(indexed(1), ANSI[1]);
        assert_eq!(indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            indexed(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(
            indexed(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }
}
