//! A screen the shell draws itself, before the compositor (ADR 0016): the
//! console framebuffer, the UI on it (redrawn only where it changed), the
//! pointer painted over it, and the keyboard and pointer turned into the
//! UI's events. The desktop and the sign-in screen each drive one.

use std::path::Path;
use std::time::Instant;

use glass_platform::framebuffer::{Framebuffer, Order};
use glass_shell::render::Screen;
use glass_shell::{Element, Message, Size};
use iced_core::keyboard::{Key, Modifiers};
use iced_core::{Point, mouse};

use crate::input::Input;
use crate::{keys, pointer};

pub struct Display {
    fb: Framebuffer,
    screen: Screen,
    keyboard: keys::Keyboard,
    pointer: Point,
    /// Where the pointer was painted, if it was.
    painted: Option<Point>,
}

/// A batch of input as the UI takes it.
#[derive(Default)]
pub struct Typed {
    /// Clicks, wheel turns and keys for the UI.
    pub events: Vec<iced_core::Event>,
    /// Keys the caller claimed as shortcuts, as the messages they stand for.
    pub shortcuts: Vec<Message>,
}

impl Display {
    /// Opens the framebuffer behind `tty`, with `backdrop` (B, G, R, A at
    /// the screen's size) kept behind the UI.
    pub fn open(tty: &Path, backdrop: impl FnOnce(u32, u32) -> Vec<u8>) -> anyhow::Result<Self> {
        let fb = Framebuffer::open(tty)?;
        let g = fb.geometry();
        eprintln!(
            "{}: {}x{}, {}-byte rows",
            tty.display(),
            g.width,
            g.height,
            g.stride
        );
        let size = Size::new(g.width as f32, g.height as f32);
        let mut screen = Screen::new(size, 1.0);
        screen.set_backdrop(backdrop(g.width, g.height));
        Ok(Self {
            fb,
            screen,
            keyboard: keys::Keyboard::default(),
            pointer: Point::new(size.width / 2.0, size.height / 2.0),
            painted: None,
        })
    }

    /// The screen's size in pixels.
    pub fn pixels(&self) -> (u32, u32) {
        let g = self.fb.geometry();
        (g.width, g.height)
    }

    /// The screen's size, as views lay out on it.
    pub fn size(&self) -> (f32, f32) {
        let (w, h) = self.pixels();
        (w as f32, h as f32)
    }

    /// Moves the pointer (painting it at once) and turns clicks and keys into
    /// the UI's events. A key press `shortcut` claims becomes its message
    /// instead.
    pub fn input(
        &mut self,
        batch: &[Input],
        shortcut: impl Fn(&Key, Modifiers) -> Option<Message>,
    ) -> Typed {
        let (w, h) = self.size();
        let mut typed = Typed::default();
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
                    typed
                        .events
                        .push(iced_core::Event::Mouse(mouse::Event::CursorMoved {
                            position: self.pointer,
                        }));
                    typed.events.push(iced_core::Event::Mouse(if down {
                        mouse::Event::ButtonPressed(button)
                    } else {
                        mouse::Event::ButtonReleased(button)
                    }));
                }
                Input::Wheel(lines) => {
                    typed
                        .events
                        .push(iced_core::Event::Mouse(mouse::Event::WheelScrolled {
                            delta: mouse::ScrollDelta::Lines { x: 0.0, y: lines },
                        }));
                }
                Input::Key(code, value) => {
                    for press in self.keyboard.key(code, value) {
                        let claimed =
                            matches!(press.event, iced_core::keyboard::Event::KeyPressed { .. })
                                .then(|| shortcut(&press.key, press.modifiers))
                                .flatten();
                        match claimed {
                            Some(message) => typed.shortcuts.push(message),
                            None => typed.events.push(iced_core::Event::Keyboard(press.event)),
                        }
                    }
                }
            }
        }
        self.paint_pointer();
        typed
    }

    /// Feeds `events` to `view` with the pointer where it is, and returns
    /// the messages it produced.
    pub fn update(
        &mut self,
        view: Element<'_, Message>,
        events: &[iced_core::Event],
    ) -> Vec<Message> {
        self.screen.update(view, events, Some(self.pointer))
    }

    /// Gives the keyboard focus to the widget with this id.
    pub fn focus(&mut self, view: Element<'_, Message>, id: &'static str) {
        self.screen.focus(view, id);
    }

    /// Draws `view`, copies what changed to the screen and paints the
    /// pointer over it. Returns a line for the log.
    pub fn draw(&mut self, view: Element<'_, Message>) -> String {
        let started = Instant::now();
        let damage = self.screen.render(view, Some(self.pointer));
        let rendered = started.elapsed();
        let (width, _) = self.pixels();
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
        format!(
            "drawn in {} ms, {} areas ({rows} rows) copied in {} ms",
            rendered.as_millis(),
            damage.len(),
            (started.elapsed() - rendered).as_millis()
        )
    }

    /// Restores the rows under the pointer's old place from the screen's
    /// pixels and paints it at its new one.
    fn paint_pointer(&mut self) {
        let (width, height) = self.pixels();
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
