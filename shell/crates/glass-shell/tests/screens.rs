//! Every screen renders headlessly, the same way twice, with its landmarks
//! where the mockups put them; and each still matches its blessed snapshot
//! (`GLASS_SHELL_BLESS=1 cargo test` re-blesses after an intended change;
//! a mismatch writes the new frame next to the target dir for a look).

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use glass_shell::render::{Frame, frame};
use glass_shell::screens::{boot, command_bar, desktop, login};
use glass_shell::{Message, Size, Theme, fixtures};

const SCREEN: (f32, f32) = (1440.0, 900.0);

fn size() -> Size {
    Size::new(SCREEN.0, SCREEN.1)
}

fn render_boot() -> Frame {
    frame(boot::view(&Theme::glass(), 0.42), size(), 1.0)
}

fn render_login() -> Frame {
    let screen = fixtures::sign_in();
    frame(login::view(&Theme::glass(), &screen, SCREEN), size(), 1.0)
}

fn render_desktop() -> Frame {
    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let state = fixtures::desktop(&tasks);
    let windows = vec![desktop::Window {
        title: "Q3 Forecast — Finance".into(),
        x: 48.0,
        y: 52.0,
        width: 968.0,
        height: 744.0,
        content: fixtures::forecast_sheet(),
    }];
    let thumbnail = |task: &glass_api::TaskView| {
        (task.agent.as_str() == "invoice-agent").then(|| fixtures::invoice_entry_thumbnail(&theme))
    };
    frame(
        desktop::view(&theme, &state, windows, thumbnail, None, SCREEN),
        size(),
        1.0,
    )
}

fn render_command_bar() -> Frame {
    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let recent = fixtures::recent_tasks();
    let bar = fixtures::command_bar(&recent);
    let state = desktop::Desktop {
        tiles_visible: false,
        ..fixtures::desktop(&tasks)
    };
    let overlay: glass_shell::Element<'_, Message> = command_bar::overlay(&theme, &bar);
    frame(
        desktop::view(&theme, &state, Vec::new(), |_| None, Some(overlay), SCREEN),
        size(),
        1.0,
    )
}

fn close(actual: [u8; 4], expected: [u8; 3], tolerance: u8) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(a, e)| a.abs_diff(e) <= tolerance)
}

#[test]
fn boot_shows_the_mark_and_progress_on_black() {
    let f = render_boot();
    assert_eq!((f.width, f.height), (1440, 900));
    assert!(close(f.pixel(10, 10), [0, 0, 0], 0), "black background");
    // The mark's left edge and the filled part of the bar.
    assert!(
        close(f.pixel(674, 415), [245, 247, 250], 30),
        "{:?}",
        f.pixel(674, 415)
    );
    assert!(
        close(f.pixel(650, 534), [245, 247, 250], 30),
        "{:?}",
        f.pixel(650, 534)
    );
    assert!(
        close(f.pixel(800, 534), [58, 61, 66], 20),
        "{:?}",
        f.pixel(800, 534)
    );
}

#[test]
fn sign_in_shows_the_clock_over_the_wallpaper() {
    let f = render_login();
    let bright = (590..850)
        .flat_map(|x| (140..230).map(move |y| (x, y)))
        .filter(|&(x, y)| close(f.pixel(x, y), [245, 247, 250], 20))
        .count();
    assert!(
        bright > 2000,
        "the big clock is drawn ({bright} light pixels)"
    );
    assert!(
        f.pixel(5, 450)[2] > f.pixel(5, 450)[0],
        "the wallpaper is blue on the left"
    );
}

#[test]
fn desktop_has_menu_bar_tiles_and_dock_in_place() {
    let f = render_desktop();
    let bar = f.pixel(700, 16);
    assert!(
        bar.iter().take(3).all(|&c| c > 180),
        "light menu bar: {bar:?}"
    );
    // The needs-you tile's ring, at the top of the stack on the right.
    let ring = f.pixel(1065, 120);
    assert!(close(ring, [240, 138, 36], 40), "orange ring: {ring:?}");
    // Dark tile body below it.
    assert!(
        close(f.pixel(1200, 300), [21, 24, 29], 25),
        "{:?}",
        f.pixel(1200, 300)
    );
    // The dock is light and floats above the bottom edge.
    let dock = f.pixel(470, 830);
    assert!(dock.iter().take(3).all(|&c| c > 120), "dock: {dock:?}");
}

#[test]
fn command_bar_sits_over_the_dimmed_desktop() {
    let f = render_command_bar();
    assert!(
        close(f.pixel(500, 500), [23, 26, 31], 20),
        "{:?}",
        f.pixel(500, 500)
    );
    let dock = f.pixel(470, 830);
    assert!(
        dock.iter().take(3).all(|&c| c > 120),
        "the dock stays above the scrim: {dock:?}"
    );
}

#[test]
fn rendering_is_deterministic() {
    assert_eq!(render_desktop().rgba, render_desktop().rgba);
}

/// FNV-1a: enough to notice any change in a frame.
fn digest(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn snapshot(name: &str, f: &Frame) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let path = dir.join(format!("{name}.digest"));
    let actual = digest(&f.rgba);
    if std::env::var_os("GLASS_SHELL_BLESS").is_some() || !path.exists() {
        std::fs::write(&path, format!("{actual}\n")).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    if expected.trim() != actual {
        let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.png"));
        f.write_png(&out).unwrap();
        panic!(
            "{name} changed (digest {actual}, blessed {}). The new frame is {}; \
             if the change is intended, run GLASS_SHELL_BLESS=1 cargo test",
            expected.trim(),
            out.display()
        );
    }
}

#[test]
fn screens_match_their_snapshots() {
    snapshot("boot", &render_boot());
    snapshot("login", &render_login());
    snapshot("desktop", &render_desktop());
    snapshot("command-bar", &render_command_bar());
}
