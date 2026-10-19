//! A screen the shell draws itself (ADR 0016): the console framebuffer, the
//! UI on it (redrawn only where it changed), apps' windows between the
//! wallpaper and the UI when the desktop hosts the compositor, the pointer
//! painted over it all, and the keyboard and pointer turned into the UI's
//! events or the apps' input. The desktop and the sign-in screen each drive
//! one.

use std::path::Path;
use std::time::Instant;

use glass_platform::framebuffer::{Framebuffer, Order};
use glass_shell::render::{Damage, Screen, Updated};
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
    /// Paint the pointer at all (a screen nobody uses, such as the one up
    /// while the computer shuts down, has none).
    pointer_shown: bool,
    /// A button went down over a window: its release goes there too.
    pressed_on_app: bool,
}

/// A batch of input as the UI takes it.
#[derive(Default)]
pub struct Typed {
    /// Where the pointer went, clicks, wheel turns and keys, for the UI.
    pub events: Vec<iced_core::Event>,
    /// Keys the caller claimed as shortcuts, as the messages they stand for.
    pub shortcuts: Vec<Message>,
    /// What goes to the apps' windows instead.
    pub apps: Vec<AppInput>,
}

/// Input for the window under the pointer or with the keyboard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppInput {
    /// The pointer is at (x, y) over a window.
    Moved(f64, f64),
    /// An evdev button code went down or up, on a window or on the bare
    /// desktop (which takes the keyboard from the windows).
    Button(u32, bool),
    /// An evdev key code went down or up.
    Key(u32, bool),
}

/// Where input goes when apps have windows: the shell decides.
pub struct Route<'a> {
    /// Whether a window is at this point.
    pub window_at: &'a dyn Fn(Point) -> bool,
    /// Whether keys that aren't shortcuts go to the focused window.
    pub keys_to_apps: bool,
    /// The shell takes everything (a menu or the lock screen is up).
    pub shell_only: bool,
}

impl Route<'_> {
    /// No apps: everything is the shell's.
    pub const SHELL: Route<'static> = Route {
        window_at: &|_| false,
        keys_to_apps: false,
        shell_only: true,
    };
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
            pointer_shown: true,
            pressed_on_app: false,
        })
    }

    /// No pointer on this screen from now on; one already painted goes.
    pub fn hide_pointer(&mut self) {
        self.pointer_shown = false;
        if let Some(old) = self.painted.take() {
            self.restore_under(old);
        }
    }

    /// Makes this screen's terminal the one showing (`Framebuffer::show`).
    pub fn show(&self) -> std::io::Result<()> {
        self.fb.show()
    }

    /// Closes the screen, leaving its last frame up for the next program to
    /// draw over, so the text console doesn't flash in between.
    pub fn hand_over(self) {
        self.fb.hand_over();
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
        self.input_routed(batch, shortcut, &Route::SHELL)
    }

    /// [`Display::input`] with apps' windows on the screen: the pointer
    /// over a window where the UI draws nothing, and keys while a window
    /// has the keyboard, go to the apps ([`Typed::apps`]), unless `route`
    /// says the shell takes everything. Shortcuts stay the shell's.
    pub fn input_routed(
        &mut self,
        batch: &[Input],
        shortcut: impl Fn(&Key, Modifiers) -> Option<Message>,
        route: &Route<'_>,
    ) -> Typed {
        let (w, h) = self.size();
        let mut typed = Typed::default();
        // Where the pointer is goes to the UI once per batch, and before a
        // click, not for every step on the way.
        let mut moved = false;
        let moved_to =
            |pointer| iced_core::Event::Mouse(mouse::Event::CursorMoved { position: pointer });
        for input in batch {
            match *input {
                Input::MovedTo(x, y) => {
                    self.pointer = Point::new(x, y);
                    moved = true;
                }
                Input::MovedBy(dx, dy) => {
                    self.pointer = Point::new(
                        (self.pointer.x + dx).clamp(0.0, w - 1.0),
                        (self.pointer.y + dy).clamp(0.0, h - 1.0),
                    );
                    moved = true;
                }
                Input::Button(button, down) => {
                    typed.events.push(moved_to(self.pointer));
                    moved = false;
                    // The bare desktop (no UI, no window) takes the keyboard
                    // from the windows; a press on a window keeps its release.
                    let on_window = self.app_has(self.pointer, route);
                    let bare = !route.shell_only && !self.ui_at(self.pointer);
                    let to_app = if down {
                        self.pressed_on_app = on_window;
                        on_window
                    } else {
                        std::mem::take(&mut self.pressed_on_app)
                    };
                    if (to_app || bare)
                        && let Some(code) = button_code(button)
                    {
                        typed.apps.push(AppInput::Moved(
                            f64::from(self.pointer.x),
                            f64::from(self.pointer.y),
                        ));
                        typed.apps.push(AppInput::Button(code, down));
                    }
                    if !to_app {
                        typed.events.push(iced_core::Event::Mouse(if down {
                            mouse::Event::ButtonPressed(button)
                        } else {
                            mouse::Event::ButtonReleased(button)
                        }));
                    }
                }
                Input::Wheel(lines) => {
                    typed
                        .events
                        .push(iced_core::Event::Mouse(mouse::Event::WheelScrolled {
                            delta: mouse::ScrollDelta::Lines { x: 0.0, y: lines },
                        }));
                }
                Input::Key(code, value) => {
                    let mut claimed_any = false;
                    for press in self.keyboard.key(code, value) {
                        let claimed =
                            matches!(press.event, iced_core::keyboard::Event::KeyPressed { .. })
                                .then(|| shortcut(&press.key, press.modifiers))
                                .flatten();
                        match claimed {
                            Some(message) => {
                                claimed_any = true;
                                typed.shortcuts.push(message);
                            }
                            None if route.keys_to_apps && !route.shell_only => {}
                            None => typed.events.push(iced_core::Event::Keyboard(press.event)),
                        }
                    }
                    // Apps repeat keys themselves, so only downs and ups.
                    if route.keys_to_apps && !route.shell_only && !claimed_any && value != 2 {
                        typed.apps.push(AppInput::Key(u32::from(code), value != 0));
                    }
                }
            }
        }
        if moved {
            typed.events.push(moved_to(self.pointer));
            if self.app_has(self.pointer, route) || self.pressed_on_app {
                typed.apps.push(AppInput::Moved(
                    f64::from(self.pointer.x),
                    f64::from(self.pointer.y),
                ));
            }
        }
        self.paint_pointer();
        typed
    }

    /// Whether the pointer at `at` is an app's: over a window, with no UI
    /// drawn there, and the shell not taking everything.
    fn app_has(&self, at: Point, route: &Route<'_>) -> bool {
        !route.shell_only && (route.window_at)(at) && !self.ui_at(at)
    }

    fn ui_at(&self, at: Point) -> bool {
        self.screen
            .ui_at(at.x.max(0.0) as u32, at.y.max(0.0) as u32)
    }

    /// Feeds `events` to `view` with the pointer where it is.
    pub fn update(
        &mut self,
        view: Element<'_, Message>,
        events: &[iced_core::Event],
    ) -> Updated<Message> {
        let mut updated = self.screen.update(view, events, Some(self.pointer));
        // A press or a key can change how the UI looks without a message (a
        // focused input, a held button); the pointer only moving changes it
        // when a widget says so.
        updated.redraw |= events
            .iter()
            .any(|e| !matches!(e, iced_core::Event::Mouse(mouse::Event::CursorMoved { .. })));
        updated
    }

    /// When the screen wants drawing again though nothing changed: a
    /// focused text field's caret blinks.
    pub fn next_redraw(&self) -> Option<Instant> {
        self.screen.next_redraw()
    }

    /// Gives the keyboard focus to the widget with this id.
    pub fn focus(&mut self, view: Element<'_, Message>, id: &'static str) {
        self.screen.focus(view, id);
    }

    /// Draws `view`, copies what changed to the screen and paints the
    /// pointer over it. Returns a line for the log.
    pub fn draw(&mut self, view: Element<'_, Message>) -> String {
        self.draw_over(view, None)
    }

    /// [`Display::draw`] with apps' windows between the wallpaper and the
    /// UI (`glass_compositor::Compositor::pixels`); none hides them.
    pub fn draw_over(&mut self, view: Element<'_, Message>, windows: Option<&[u8]>) -> String {
        let started = Instant::now();
        let damage = self.screen.render_over(view, Some(self.pointer), windows);
        let rendered = started.elapsed();
        let rows = self.copy(&damage);
        format!(
            "drawn in {} ms, {} areas ({rows} rows) copied in {} ms",
            rendered.as_millis(),
            damage.len(),
            (started.elapsed() - rendered).as_millis()
        )
    }

    /// Shows the apps' windows again in `areas` (screen pixels: x, y,
    /// width, height), where only they changed; none hides them.
    pub fn present_windows(&mut self, areas: &[(i32, i32, i32, i32)], windows: Option<&[u8]>) {
        let (width, height) = self.pixels();
        let damage: Vec<Damage> = areas
            .iter()
            .filter_map(|&(x, y, w, h)| {
                let x0 = x.clamp(0, width as i32) as u32;
                let y0 = y.clamp(0, height as i32) as u32;
                let x1 = (x + w).clamp(0, width as i32) as u32;
                let y1 = (y + h).clamp(0, height as i32) as u32;
                (x1 > x0 && y1 > y0).then_some(Damage {
                    x: x0,
                    y: y0,
                    width: x1 - x0,
                    height: y1 - y0,
                })
            })
            .collect();
        let damage = self.screen.compose(&damage, windows);
        self.copy(&damage);
    }

    /// Copies `damage` to the screen and paints the pointer again; returns
    /// how many rows went.
    fn copy(&mut self, damage: &[Damage]) -> u32 {
        let (width, _) = self.pixels();
        let stride = width as usize * 4;
        let mut rows = 0;
        for area in damage {
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
        rows
    }

    /// Restores the rows under the pointer's old place from the screen's
    /// pixels and paints it at its new one.
    fn paint_pointer(&mut self) {
        let (width, height) = self.pixels();
        let stride = width as usize * 4;
        if !self.pointer_shown || self.painted == Some(self.pointer) {
            return;
        }
        if let Some(old) = self.painted {
            self.restore_under(old);
        }
        let at = (self.pointer.x as u32, self.pointer.y as u32);
        let pixels = self.screen.bgra();
        let rows = pointer::rows(at.1, height);
        let mut band = pixels[rows.start as usize * stride..rows.end as usize * stride].to_vec();
        pointer::paint(&mut band, width, rows.start, at.0, at.1);
        self.fb.write_rows(&band, Order::Bgra, width, rows.start);
        self.painted = Some(self.pointer);
    }

    /// Copies the screen's own pixels back over a pointer painted at `at`.
    fn restore_under(&mut self, at: Point) {
        let (width, height) = self.pixels();
        let stride = width as usize * 4;
        let rows = pointer::rows(at.y as u32, height);
        let band = &self.screen.bgra()[rows.start as usize * stride..rows.end as usize * stride];
        self.fb.write_rows(band, Order::Bgra, width, rows.start);
    }
}

/// A mouse button's evdev code, as apps get it.
fn button_code(button: mouse::Button) -> Option<u32> {
    match button {
        mouse::Button::Left => Some(0x110),
        mouse::Button::Right => Some(0x111),
        mouse::Button::Middle => Some(0x112),
        _ => None,
    }
}
