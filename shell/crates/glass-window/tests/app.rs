//! A native app's window against the real compositor (UI-6): it shows the
//! app's view, a click reaches its buttons, keys reach the app with the
//! text the keymap gives them, and the close control ends it.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glass_compositor::{BUTTON_LEFT, CONTROLS, Compositor, TITLE_BAR, WorkArea};
use glass_shell::Element;
use glass_window::{App, KeyPress, Wake};
use iced_core::{Color, Length};
use iced_widget::{button, container, text};

/// What the app saw.
type Seen = Arc<Mutex<Vec<String>>>;

struct Counter {
    seen: Seen,
    clicks: u32,
}

#[derive(Debug, Clone)]
struct Clicked;

impl App for Counter {
    type Message = Clicked;

    fn title(&self) -> String {
        format!("Counter {}", self.clicks)
    }
    fn app_id(&self) -> String {
        "test.counter".into()
    }
    fn size(&self) -> (u32, u32) {
        (200, 100)
    }
    fn background(&self) -> [u8; 4] {
        [0x30, 0x20, 0x10, 0xFF]
    }
    fn view(&self) -> Element<'_, Clicked> {
        // The button fills the window's left half.
        container(
            button(text(self.clicks.to_string()))
                .on_press(Clicked)
                .width(Length::Fixed(100.0))
                .height(Length::Fill)
                .style(|_, _| button::Style {
                    background: Some(Color::WHITE.into()),
                    ..button::Style::default()
                }),
        )
        .into()
    }
    fn update(&mut self, Clicked: Clicked) {
        self.clicks += 1;
        self.seen
            .lock()
            .unwrap()
            .push(format!("clicked {}", self.clicks));
    }
    fn key(&mut self, press: &KeyPress) -> bool {
        self.seen
            .lock()
            .unwrap()
            .push(format!("key {:?}", press.text));
        true
    }
    fn close(&mut self) -> bool {
        self.seen.lock().unwrap().push("close".into());
        true
    }
}

fn until(compositor: &mut Compositor, what: &str, mut done: impl FnMut(&mut Compositor) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done(compositor) {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        compositor.dispatch().unwrap();
        compositor.render().unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn pixel(compositor: &Compositor, x: i32, y: i32) -> [u8; 4] {
    let at = (y as usize * 800 + x as usize) * 4;
    compositor.pixels()[at..at + 4].try_into().unwrap()
}

#[test]
fn ui6_a_native_app_shows_its_view_and_takes_clicks_keys_and_close() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("wayland-test");
    let mut compositor = Compositor::new(
        socket.to_str().unwrap(),
        (800, 600),
        WorkArea {
            top: 32,
            bottom: 80,
        },
    )
    .unwrap();
    let seen = Seen::default();
    let app = Counter {
        seen: seen.clone(),
        clicks: 0,
    };
    let path = socket.clone();
    let running =
        std::thread::spawn(move || glass_window::run_on(&path, app, Wake::new().unwrap()));

    until(&mut compositor, "the window", |c| {
        c.windows().first().is_some_and(|w| w.area.width == 200)
    });
    let w = compositor.windows()[0].area;
    assert_eq!(compositor.windows()[0].title, "Counter 0");
    // Its view: the white button on the left, the background on the right.
    until(&mut compositor, "its view", |c| {
        pixel(c, w.x + 150, w.y + 50) == [0x30, 0x20, 0x10, 0xFF]
            && pixel(c, w.x + 20, w.y + 50) == [0xFF, 0xFF, 0xFF, 0xFF]
    });

    // A click on the button; the title follows the count.
    compositor.pointer_moved((f64::from(w.x + 50), f64::from(w.y + 50)));
    compositor.pointer_button(BUTTON_LEFT, true);
    compositor.pointer_button(BUTTON_LEFT, false);
    until(&mut compositor, "the click", |c| {
        c.windows()[0].title == "Counter 1"
    });

    // evdev KEY_A, and with Shift (KEY_LEFTSHIFT).
    compositor.key(30, true);
    compositor.key(30, false);
    compositor.key(42, true);
    compositor.key(30, true);
    compositor.key(30, false);
    compositor.key(42, false);
    until(&mut compositor, "the keys", |_| {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("key"))
            .count()
            >= 2
    });

    // The close control ends it.
    compositor.pointer_moved((f64::from(w.x + CONTROLS[0]), f64::from(w.y - TITLE_BAR / 2)));
    compositor.pointer_button(BUTTON_LEFT, true);
    compositor.pointer_button(BUTTON_LEFT, false);
    until(&mut compositor, "the app to end", |_| running.is_finished());
    running.join().unwrap().unwrap();

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen, ["clicked 1", "key \"a\"", "key \"A\"", "close"]);
}

/// An app that copies its name on C and pastes on V (DK-2).
struct Clipper {
    name: &'static str,
    size: (u32, u32),
    seen: Seen,
    copy: bool,
    paste: bool,
}

impl App for Clipper {
    type Message = Clicked;

    fn title(&self) -> String {
        self.name.into()
    }
    fn app_id(&self) -> String {
        format!("test.{}", self.name.to_lowercase())
    }
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn background(&self) -> [u8; 4] {
        [0x30, 0x20, 0x10, 0xFF]
    }
    fn view(&self) -> Element<'_, Clicked> {
        container(text(self.name)).into()
    }
    fn update(&mut self, Clicked: Clicked) {}
    fn key(&mut self, press: &KeyPress) -> bool {
        match press.text.as_str() {
            "c" => self.copy = true,
            "v" => self.paste = true,
            _ => {}
        }
        true
    }
    fn copied(&mut self) -> Option<String> {
        let copied = std::mem::take(&mut self.copy).then(|| format!("from {}", self.name))?;
        self.seen
            .lock()
            .unwrap()
            .push(format!("{} copied", self.name));
        Some(copied)
    }
    fn wants_paste(&mut self) -> bool {
        std::mem::take(&mut self.paste)
    }
    fn paste(&mut self, text: String) {
        self.seen
            .lock()
            .unwrap()
            .push(format!("{} pasted {text}", self.name));
    }
}

#[test]
fn dk2_copy_in_one_app_and_paste_in_another() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("wayland-test");
    let mut compositor = Compositor::new(
        socket.to_str().unwrap(),
        (800, 600),
        WorkArea {
            top: 32,
            bottom: 80,
        },
    )
    .unwrap();
    let seen = Seen::default();
    let start = |name, size| {
        let app = Clipper {
            name,
            size,
            seen: seen.clone(),
            copy: false,
            paste: false,
        };
        let path = socket.clone();
        std::thread::spawn(move || glass_window::run_on(&path, app, Wake::new().unwrap()))
    };
    let _a = start("A", (300, 150));
    until(&mut compositor, "A's window", |c| c.windows().len() == 1);
    let _b = start("B", (200, 100));
    until(&mut compositor, "B's window", |c| {
        c.windows().len() == 2 && c.windows()[1].focused
    });
    let a = compositor.windows()[0].area;
    let click = |compositor: &mut Compositor, x: i32, y: i32| {
        compositor.pointer_moved((f64::from(x), f64::from(y)));
        compositor.pointer_button(BUTTON_LEFT, true);
        compositor.pointer_button(BUTTON_LEFT, false);
    };
    // evdev KEY_C and KEY_V.
    let press = |compositor: &mut Compositor, code: u32| {
        compositor.key(code, true);
        compositor.key(code, false);
    };
    let saw = |what: &'static str| {
        let seen = seen.clone();
        move |_: &mut Compositor| seen.lock().unwrap().iter().any(|s| s == what)
    };
    // The app sends its copy right after it records it; the compositor
    // takes it from the app with the keyboard only, so it's let in before
    // the focus moves on.
    let settle = |compositor: &mut Compositor| {
        let end = Instant::now() + Duration::from_millis(100);
        until(compositor, "a moment", |_| Instant::now() >= end);
    };

    // B copies; A, clicked (its left edge, beside B), pastes it.
    press(&mut compositor, 46);
    until(&mut compositor, "B's copy", saw("B copied"));
    settle(&mut compositor);
    click(&mut compositor, a.x + 20, a.y + 20);
    until(&mut compositor, "A in front", |c| {
        c.windows()[1].title == "A"
    });
    press(&mut compositor, 47);
    until(&mut compositor, "A's paste", saw("A pasted from B"));
    // A copies and pastes its own (no round trip, so no waiting on
    // itself); B, brought back (Alt Tab: A covers it now), pastes A's.
    press(&mut compositor, 46);
    until(&mut compositor, "A's copy", saw("A copied"));
    settle(&mut compositor);
    press(&mut compositor, 47);
    until(&mut compositor, "A's own paste", saw("A pasted from A"));
    let b_id = compositor
        .windows()
        .iter()
        .find(|w| w.title == "B")
        .unwrap()
        .id;
    compositor.activate(b_id);
    until(&mut compositor, "B in front", |c| {
        c.windows()[1].title == "B"
    });
    press(&mut compositor, 47);
    until(&mut compositor, "B's paste", saw("B pasted from A"));
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "B copied",
            "A pasted from B",
            "A copied",
            "A pasted from A",
            "B pasted from A"
        ]
    );
}

/// An app that takes whatever size it's asked for (DK-1's zoom).
struct Sizer {
    size: (u32, u32),
    seen: Seen,
}

impl App for Sizer {
    type Message = Clicked;

    fn title(&self) -> String {
        "Sizer".into()
    }
    fn app_id(&self) -> String {
        "test.sizer".into()
    }
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn background(&self) -> [u8; 4] {
        [0x30, 0x20, 0x10, 0xFF]
    }
    fn view(&self) -> Element<'_, Clicked> {
        container(text("sized")).into()
    }
    fn update(&mut self, Clicked: Clicked) {}
    fn resize(&mut self, size: (u32, u32)) -> bool {
        self.size = size;
        self.seen
            .lock()
            .unwrap()
            .push(format!("{}x{}", size.0, size.1));
        true
    }
}

#[test]
fn dk1_zoom_fills_the_work_area_and_again_puts_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("wayland-test");
    let work = WorkArea {
        top: 32,
        bottom: 80,
    };
    let mut compositor = Compositor::new(socket.to_str().unwrap(), (800, 600), work).unwrap();
    let seen = Seen::default();
    let app = Sizer {
        size: (200, 100),
        seen: seen.clone(),
    };
    let path = socket.clone();
    std::thread::spawn(move || glass_window::run_on(&path, app, Wake::new().unwrap()));
    until(&mut compositor, "the window", |c| {
        c.windows().first().is_some_and(|w| w.area.width == 200)
    });
    let before = compositor.windows()[0].area;
    let zoom = |compositor: &mut Compositor, area: glass_compositor::Rect| {
        compositor.pointer_moved((
            f64::from(area.x + CONTROLS[2]),
            f64::from(area.y - TITLE_BAR / 2),
        ));
        compositor.pointer_button(BUTTON_LEFT, true);
        compositor.pointer_button(BUTTON_LEFT, false);
    };
    zoom(&mut compositor, before);
    let height = 600 - work.top - TITLE_BAR - work.bottom;
    until(&mut compositor, "the window zoomed", |c| {
        c.windows()[0].area
            == glass_compositor::Rect {
                x: 0,
                y: work.top + TITLE_BAR,
                width: 800,
                height,
            }
    });
    let zoomed = compositor.windows()[0].area;
    zoom(&mut compositor, zoomed);
    until(&mut compositor, "the window back", |c| {
        c.windows()[0].area == before
    });
    assert_eq!(
        *seen.lock().unwrap(),
        [format!("800x{height}"), "200x100".into()]
    );
}

#[test]
fn dk1_dragging_a_corner_resizes_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("wayland-test");
    let work = WorkArea {
        top: 32,
        bottom: 80,
    };
    let mut compositor = Compositor::new(socket.to_str().unwrap(), (800, 600), work).unwrap();
    let seen = Seen::default();
    let app = Sizer {
        size: (200, 100),
        seen: seen.clone(),
    };
    let path = socket.clone();
    std::thread::spawn(move || glass_window::run_on(&path, app, Wake::new().unwrap()));
    until(&mut compositor, "the window", |c| {
        c.windows().first().is_some_and(|w| w.area.width == 200)
    });
    let w = compositor.windows()[0].area;
    // Just past the bottom right corner, then 100 right and 50 down.
    let corner = (f64::from(w.x + w.width + 2), f64::from(w.y + w.height + 2));
    compositor.pointer_moved(corner);
    compositor.pointer_button(BUTTON_LEFT, true);
    compositor.pointer_moved((corner.0 + 100.0, corner.1 + 50.0));
    compositor.pointer_button(BUTTON_LEFT, false);
    until(&mut compositor, "the window resized", |c| {
        let a = c.windows()[0].area;
        (a.x, a.y, a.width, a.height) == (w.x, w.y, 300, 150)
    });
    assert_eq!(*seen.lock().unwrap(), ["300x150"]);
}
