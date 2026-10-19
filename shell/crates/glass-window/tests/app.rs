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
