//! The compositor with real Wayland clients on its socket (PRD UI-1): a
//! window opens centered in the work area showing its own pixels, clicks and
//! keys reach the window they should, a click raises a window, a frame where
//! nothing changed composes nothing, and each window's title bar (DK-1)
//! stacks with it, moves it and closes it.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use glass_compositor::client::{self, Event, Running, Spec};
use glass_compositor::{BUTTON_LEFT, CONTROLS, Compositor, Rect, TITLE_BAR, WorkArea};

const SIZE: (u32, u32) = (800, 600);
const WORK_AREA: WorkArea = WorkArea {
    top: 32,
    bottom: 80,
};
/// evdev's `KEY_A`.
const KEY_A: u32 = 30;

fn compositor() -> (Compositor, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("wayland-test");
    let compositor = Compositor::new(socket.to_str().unwrap(), SIZE, WORK_AREA).unwrap();
    (compositor, socket, dir)
}

fn window(socket: &std::path::Path, title: &str, size: (u32, u32), color: u32) -> Running {
    client::spawn(
        socket.to_path_buf(),
        Spec {
            title: title.into(),
            app_id: format!("test.{}", title.to_lowercase()),
            size,
            color,
        },
    )
}

/// Runs the compositor until `done` holds, or fails after five seconds.
fn until(compositor: &mut Compositor, what: &str, mut done: impl FnMut(&mut Compositor) -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !done(compositor) {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        compositor.dispatch().unwrap();
        compositor.render().unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Runs the compositor until `client` reports `want`.
fn until_told(compositor: &mut Compositor, client: &Running, want: &Event) {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        compositor.dispatch().unwrap();
        compositor.render().unwrap();
        while let Ok(event) = client.events.try_recv() {
            if &event == want {
                return;
            }
        }
        assert!(Instant::now() < end, "timed out waiting for {want:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Stops `client`, running the compositor while it says goodbye.
fn finish(compositor: &mut Compositor, client: Running) {
    client.stop();
    until(compositor, "the client to go", |_| client.is_done());
    client.join().unwrap();
}

fn pixel(compositor: &Compositor, x: i32, y: i32) -> [u8; 4] {
    let at = (y as usize * SIZE.0 as usize + x as usize) * 4;
    compositor.pixels()[at..at + 4].try_into().unwrap()
}

/// BGRA bytes of `0xAARRGGBB`.
fn bgra(color: u32) -> [u8; 4] {
    color.to_le_bytes()
}

#[test]
fn ui1_a_window_opens_centered_in_the_work_area_showing_its_pixels() {
    let (mut compositor, socket, _dir) = compositor();
    let blue = 0xFF2F_7BEA;
    let hello = window(&socket, "Hello", (200, 100), blue);
    until(&mut compositor, "the window", |c| {
        c.windows().first().is_some_and(|w| w.area.width > 0)
    });
    // It may take another frame for its pixels to land.
    until(&mut compositor, "its pixels", |c| {
        pixel(c, 400, 290) == bgra(blue)
    });

    let windows = compositor.windows();
    assert_eq!(windows.len(), 1);
    let w = &windows[0];
    assert_eq!(
        (w.title.as_str(), w.app_id.as_str()),
        ("Hello", "test.hello")
    );
    // Centered across, and with its title bar down the space between menu
    // bar and dock.
    let down =
        WORK_AREA.top + TITLE_BAR + (600 - WORK_AREA.top - WORK_AREA.bottom - TITLE_BAR - 100) / 2;
    assert_eq!(
        w.area,
        Rect {
            x: 300,
            y: down,
            width: 200,
            height: 100
        }
    );
    assert!(w.focused, "a new window has the keyboard");
    // Its corners are its own; outside it the desktop shows through.
    assert_eq!(pixel(&compositor, 300, down), bgra(blue));
    assert_eq!(pixel(&compositor, 499, down + 99), bgra(blue));
    assert_eq!(pixel(&compositor, 299, down), [0, 0, 0, 0]);
    assert_eq!(pixel(&compositor, 10, 10), [0, 0, 0, 0]);
    finish(&mut compositor, hello);
}

#[test]
fn ui1_the_pointer_and_keys_reach_the_window_under_and_in_focus() {
    let (mut compositor, socket, _dir) = compositor();
    let hello = window(&socket, "Hello", (200, 100), 0xFF33_3333);
    until_told(&mut compositor, &hello, &Event::Focused);
    let area = compositor.windows()[0].area;

    compositor.pointer_moved((f64::from(area.x + 20), f64::from(area.y + 10)));
    until_told(
        &mut compositor,
        &hello,
        &Event::PointerEntered { x: 20, y: 10 },
    );
    compositor.pointer_button(BUTTON_LEFT, true);
    until_told(
        &mut compositor,
        &hello,
        &Event::Button {
            button: BUTTON_LEFT,
            pressed: true,
        },
    );
    compositor.pointer_button(BUTTON_LEFT, false);
    compositor.key(KEY_A, true);
    until_told(
        &mut compositor,
        &hello,
        &Event::Key {
            code: KEY_A,
            pressed: true,
        },
    );
    compositor.key(KEY_A, false);
    finish(&mut compositor, hello);
}

#[test]
fn ui1_a_click_raises_the_window_under_it_and_gives_it_the_keyboard() {
    let (mut compositor, socket, _dir) = compositor();
    let (red, green) = (0xFFC5_3030, 0xFF2E_9E5B);
    let big = window(&socket, "Big", (400, 300), red);
    until_told(&mut compositor, &big, &Event::Focused);
    let small = window(&socket, "Small", (200, 100), green);
    until_told(&mut compositor, &small, &Event::Focused);
    until(&mut compositor, "the small window on top", |c| {
        pixel(c, 400, c.windows()[1].area.y + 50) == bgra(green)
    });
    let big_area = compositor.windows()[0].area;
    assert!(!compositor.windows()[0].focused);

    // The big window shows around the small one: click its left part.
    compositor.pointer_moved((f64::from(big_area.x + 10), f64::from(big_area.y + 150)));
    compositor.pointer_button(BUTTON_LEFT, true);
    compositor.pointer_button(BUTTON_LEFT, false);
    until_told(&mut compositor, &big, &Event::Focused);
    until(&mut compositor, "the big window raised", |c| {
        pixel(c, 400, big_area.y + 150) == bgra(red)
    });
    let windows = compositor.windows();
    assert_eq!(windows.last().map(|w| w.title.as_str()), Some("Big"));
    assert!(windows.last().unwrap().focused);
    finish(&mut compositor, small);
    finish(&mut compositor, big);
}

#[test]
fn ui22_a_frame_where_nothing_changed_composes_nothing() {
    let (mut compositor, socket, _dir) = compositor();
    let hello = window(&socket, "Hello", (200, 100), 0xFF33_3333);
    until_told(&mut compositor, &hello, &Event::Focused);
    // Let the first frames settle.
    for _ in 0..5 {
        compositor.dispatch().unwrap();
        compositor.render().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    compositor.dispatch().unwrap();
    assert_eq!(compositor.render().unwrap(), Vec::new());
    finish(&mut compositor, hello);
    // The window going redraws just where it was.
    until(&mut compositor, "the window gone", |c| {
        c.windows().is_empty()
    });
    let mut redrawn = Vec::new();
    until(&mut compositor, "its place cleared", |c| {
        redrawn.extend(c.render().unwrap());
        pixel(c, 400, 300) == [0, 0, 0, 0]
    });
    assert!(
        redrawn.iter().all(|r| r.width <= 200 && r.height <= 100),
        "{redrawn:?}"
    );
}

/// A title bar of one color, as the desktop would draw one.
fn bar(color: u32, width: i32) -> Vec<u8> {
    bgra(color).repeat((width * TITLE_BAR) as usize)
}

#[test]
fn dk1_each_window_has_a_title_bar_that_moves_it() {
    let (mut compositor, socket, _dir) = compositor();
    let (blue, grey) = (0xFF2F_7BEA, 0xFFE9_ECEF);
    let hello = window(&socket, "Hello", (200, 100), blue);
    until_told(&mut compositor, &hello, &Event::Focused);
    let w = compositor.windows()[0].clone();
    compositor.set_title_bar(w.id, &bar(grey, w.area.width), w.area.width);
    let above = w.area.y - TITLE_BAR / 2;
    until(&mut compositor, "the title bar", |c| {
        pixel(c, w.area.x + 100, above) == bgra(grey)
    });

    // Drag it by the bar: the window follows, and the app hears no click.
    compositor.pointer_moved((f64::from(w.area.x + 100), f64::from(above)));
    compositor.pointer_button(BUTTON_LEFT, true);
    compositor.pointer_moved((f64::from(w.area.x + 150), f64::from(above + 40)));
    compositor.pointer_button(BUTTON_LEFT, false);
    until(&mut compositor, "the window moved", |c| {
        c.windows()[0].area.x == w.area.x + 50
    });
    let moved = compositor.windows()[0].area;
    assert_eq!((moved.x, moved.y), (w.area.x + 50, w.area.y + 40));
    until(&mut compositor, "its pixels moved", |c| {
        pixel(c, moved.x + 1, moved.y + 1) == bgra(blue)
            && pixel(c, moved.x + 100, moved.y - TITLE_BAR / 2) == bgra(grey)
    });
    assert_eq!(pixel(&compositor, w.area.x + 1, w.area.y + 1), [0, 0, 0, 0]);
    while let Ok(event) = hello.events.try_recv() {
        assert!(!matches!(event, Event::Button { .. }), "{event:?}");
    }

    // The title bar can't go up under the menu bar.
    compositor.pointer_moved((f64::from(moved.x + 100), f64::from(moved.y - 5)));
    compositor.pointer_button(BUTTON_LEFT, true);
    compositor.pointer_moved((f64::from(moved.x + 100), 0.0));
    compositor.pointer_button(BUTTON_LEFT, false);
    compositor.dispatch().unwrap();
    assert_eq!(compositor.windows()[0].area.y, WORK_AREA.top + TITLE_BAR);
    finish(&mut compositor, hello);
}

#[test]
fn dk1_the_close_control_asks_the_app_to_close() {
    let (mut compositor, socket, _dir) = compositor();
    let hello = window(&socket, "Hello", (200, 100), 0xFF33_3333);
    until_told(&mut compositor, &hello, &Event::Focused);
    let w = compositor.windows()[0].area;
    compositor.pointer_moved((f64::from(w.x + CONTROLS[0]), f64::from(w.y - TITLE_BAR / 2)));
    compositor.pointer_button(BUTTON_LEFT, true);
    compositor.pointer_button(BUTTON_LEFT, false);
    until_told(&mut compositor, &hello, &Event::CloseAsked);
    until(&mut compositor, "the window gone", |c| {
        c.windows().is_empty()
    });
    hello.join().unwrap();
}

#[test]
fn dk1_closing_the_front_window_gives_the_next_one_the_keyboard() {
    let (mut compositor, socket, _dir) = compositor();
    let back = window(&socket, "Back", (400, 300), 0xFFC5_3030);
    until_told(&mut compositor, &back, &Event::Focused);
    let front = window(&socket, "Front", (200, 100), 0xFF2E_9E5B);
    until_told(&mut compositor, &front, &Event::Focused);
    finish(&mut compositor, front);
    until_told(&mut compositor, &back, &Event::Focused);
    assert!(compositor.windows()[0].focused);
    finish(&mut compositor, back);
}

#[test]
fn dk1_a_title_bar_stacks_with_its_window() {
    let (mut compositor, socket, _dir) = compositor();
    let (red, green, grey) = (0xFFC5_3030, 0xFF2E_9E5B, 0xFFE9_ECEF);
    let big = window(&socket, "Big", (400, 300), red);
    until_told(&mut compositor, &big, &Event::Focused);
    let small = window(&socket, "Small", (200, 100), green);
    until_told(&mut compositor, &small, &Event::Focused);
    for w in compositor.windows() {
        compositor.set_title_bar(w.id, &bar(grey, w.area.width), w.area.width);
    }
    let s = compositor.windows()[1].area;
    // The small window's bar lies over the big window's content.
    until(&mut compositor, "the small window's bar", |c| {
        pixel(c, s.x + 100, s.y - TITLE_BAR / 2) == bgra(grey)
    });
    // Raise the big one: its content covers the small window and its bar.
    let b = compositor.windows()[0].area;
    compositor.pointer_moved((f64::from(b.x + 10), f64::from(b.y + 150)));
    compositor.pointer_button(BUTTON_LEFT, true);
    compositor.pointer_button(BUTTON_LEFT, false);
    until(&mut compositor, "the big window over both", |c| {
        pixel(c, s.x + 100, s.y - TITLE_BAR / 2) == bgra(red)
            && pixel(c, s.x + 100, s.y + 50) == bgra(red)
    });
    finish(&mut compositor, small);
    finish(&mut compositor, big);
}
