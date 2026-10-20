//! Every screen renders headlessly, the same way twice, with its landmarks
//! where the mockups put them; and each still matches its blessed snapshot
//! (`GLASS_SHELL_BLESS=1 cargo test` re-blesses after an intended change;
//! a mismatch writes the new frame next to the target dir for a look).

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use glass_api::TaskDetail;
use glass_shell::apps::AppEntry;
use glass_shell::render::{Frame, frame};
use glass_shell::screens::expanded::{self, Expanded, Tab};
use glass_shell::screens::{boot, command_bar, desktop, launcher, login};
use glass_shell::{Message, Size, Theme, fixtures};
use glass_types::Timestamp;
use time::OffsetDateTime;

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

/// First-run setup (UX-2): each step, as Dana answers it.
fn render_setup() -> [Frame; 3] {
    use glass_shell::Message;
    use glass_shell::screens::setup::{ModelChoice, view};
    let shot = |setup: &glass_shell::setup::Setup| {
        frame(view(&Theme::glass(), &setup.screen(), SCREEN), size(), 1.0)
    };
    let mut setup = glass_shell::setup::Setup::new(vec!["root".into()]);
    setup.update(Message::SetupName("Dana Ruiz".into()));
    setup.update(Message::SetupPassword("correct horse".into()));
    let account = shot(&setup);
    setup.update(Message::SetupConfirm("correct horse".into()));
    setup.update(Message::SetupNext);
    setup.update(Message::SetupModels(ModelChoice::ClaudeCode));
    let models = shot(&setup);
    setup.update(Message::SetupNext);
    [account, models, shot(&setup)]
}

/// The agents menu, opened from the menu bar's pill, over a window.
fn render_agents_menu() -> Frame {
    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let state = desktop::Desktop {
        tiles: desktop::Tiles::Menu,
        ..fixtures::desktop(&tasks)
    };
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

/// The system menu, opened from the mark at the left of the menu bar.
fn render_system_menu() -> Frame {
    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let state = desktop::Desktop {
        tiles: desktop::Tiles::Hidden,
        system_menu: Some("Dana Ruiz".into()),
        lockable: true,
        ..fixtures::desktop(&tasks)
    };
    frame(
        desktop::view(&theme, &state, Vec::new(), |_| None, None, SCREEN),
        size(),
        1.0,
    )
}

/// The locked screen, typing the password that unlocks it.
fn render_lock() -> Frame {
    let theme = Theme::glass();
    let mut live = glass_shell::live::Live::new(fixtures::tasks())
        .signed_in_as("Dana Ruiz")
        .lockable();
    live.update(Message::LockScreen);
    live.update(Message::PasswordChanged("hunter2".into()));
    let now = OffsetDateTime::from_unix_timestamp(fixtures::now().unix_seconds()).unwrap();
    frame(live.view(&theme, None, now, SCREEN), size(), 1.0)
}

/// About Glasshouse, from the mark's menu.
fn render_about() -> Frame {
    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let about = glass_shell::screens::system::About {
        version: "0.1.0".into(),
        build: "9cf8edb".into(),
        processor: "Intel(R) Core(TM) Ultra 7 155H".into(),
        memory_gb: 32,
        computer: "dana-laptop".into(),
    };
    let state = desktop::Desktop {
        tiles: desktop::Tiles::Hidden,
        ..fixtures::desktop(&tasks)
    };
    let overlay = glass_shell::screens::system::about_overlay(&theme, &about);
    frame(
        desktop::view(&theme, &state, Vec::new(), |_| None, Some(overlay), SCREEN),
        size(),
        1.0,
    )
}

/// Force Quit with the first running agent chosen.
fn render_force_quit() -> Frame {
    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let running: Vec<_> = tasks.iter().filter(|t| t.state.is_live()).collect();
    let quit = glass_shell::screens::system::ForceQuit {
        chosen: running.first().map(|t| t.id),
        running,
    };
    let state = desktop::Desktop {
        tiles: desktop::Tiles::Hidden,
        ..fixtures::desktop(&tasks)
    };
    let overlay = glass_shell::screens::system::force_quit_overlay(&theme, &quit);
    frame(
        desktop::view(&theme, &state, Vec::new(), |_| None, Some(overlay), SCREEN),
        size(),
        1.0,
    )
}

/// "Restart now?" with agents running.
fn render_restart() -> Frame {
    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let state = desktop::Desktop {
        tiles: desktop::Tiles::Hidden,
        ..fixtures::desktop(&tasks)
    };
    let running = tasks.iter().filter(|t| t.state.is_live()).count();
    let overlay =
        glass_shell::screens::system::power_overlay(&theme, glass_shell::Power::Restart, running);
    frame(
        desktop::view(&theme, &state, Vec::new(), |_| None, Some(overlay), SCREEN),
        size(),
        1.0,
    )
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

/// The launcher over the desktop with `apps`.
fn render_launcher(apps: &[AppEntry]) -> Frame {
    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let state = desktop::Desktop {
        tiles: desktop::Tiles::Hidden,
        ..fixtures::desktop(&tasks)
    };
    let overlay = launcher::overlay(&theme, apps, None);
    frame(
        desktop::view(&theme, &state, Vec::new(), |_| None, Some(overlay), SCREEN),
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
        tiles: desktop::Tiles::Hidden,
        ..fixtures::desktop(&tasks)
    };
    let overlay: glass_shell::Element<'_, Message> = command_bar::overlay(&theme, &bar);
    frame(
        desktop::view(&theme, &state, Vec::new(), |_| None, Some(overlay), SCREEN),
        size(),
        1.0,
    )
}

/// A tile opened into the expanded view, over the desktop.
fn render_expanded_from(detail: &TaskDetail, now: Timestamp) -> Frame {
    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let state = fixtures::expanded_desktop(&tasks, &detail.view.agent_name);
    let view = Expanded {
        task: &detail.view,
        detail: Some(detail),
        tab: Tab::Activity,
        selected: None,
        rollback: None,
        now: OffsetDateTime::from_unix_timestamp(now.unix_seconds()).unwrap(),
    };
    frame(
        desktop::view(
            &theme,
            &state,
            Vec::new(),
            |_| None,
            Some(expanded::overlay(&theme, &view)),
            SCREEN,
        ),
        size(),
        1.0,
    )
}

/// The Expanded artboard.
fn render_expanded() -> Frame {
    render_expanded_from(&fixtures::invoice_detail(), fixtures::expanded_now())
}

/// A needs-you tile's Review: the ask's items and answers.
fn render_expanded_review() -> Frame {
    let tasks = fixtures::tasks();
    render_expanded_from(&fixtures::expense_detail(&tasks[0]), fixtures::now())
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
    let dock = f.pixel(720, 822);
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
    let dock = f.pixel(720, 822);
    assert!(
        dock.iter().take(3).all(|&c| c > 120),
        "the dock stays above the scrim: {dock:?}"
    );
}

#[test]
fn aw10_expanded_view_has_header_panes_and_timeline_in_place() {
    let f = render_expanded();
    let panel = [21, 24, 29];
    assert!(
        close(f.pixel(600, 600), panel, 4),
        "{:?}",
        f.pixel(600, 600)
    );
    // Take over, the header's primary button (AW-11).
    assert!(
        close(f.pixel(1300, 65), [61, 107, 255], 8),
        "{:?}",
        f.pixel(1300, 65)
    );
    // The Now bar (AW-12) and the rule before the right column (AW-13).
    assert!(
        close(f.pixel(900, 690), [30, 34, 40], 4),
        "{:?}",
        f.pixel(900, 690)
    );
    assert!(
        close(f.pixel(1032, 400), [38, 43, 50], 4),
        "{:?}",
        f.pixel(1032, 400)
    );
    // The timeline is lit up to the snapshot Roll back returns to (AW-15).
    assert!(
        close(f.pixel(250, 758), [61, 107, 255], 8),
        "{:?}",
        f.pixel(250, 758)
    );
    assert!(
        close(f.pixel(1000, 758), [74, 83, 97], 8),
        "{:?}",
        f.pixel(1000, 758)
    );
    // The tiles are tucked away; the menu bar and dock stay.
    assert!(
        close(f.pixel(1200, 300), panel, 4),
        "no tile over the right column"
    );
    let dock = f.pixel(720, 822);
    assert!(dock.iter().take(3).all(|&c| c > 120), "dock: {dock:?}");
}

#[test]
fn tl5_the_launcher_badges_a_remix_and_tints_a_tile_for_an_app_without_an_icon() {
    // Dana's apps: My Mail, based on Mail, last, in the second row's fifth
    // column; Dispute Tracker, with no icon, before it.
    let apps = fixtures::apps();
    let f = render_launcher(&apps);
    // The badge's accent in a 24-pixel square at a tile's bottom right.
    let badge = |f: &Frame, (x0, y0): (u32, u32)| {
        (x0..x0 + 24)
            .flat_map(|x| (y0..y0 + 24).map(move |y| (x, y)))
            .filter(|&(x, y)| close(f.pixel(x, y), [61, 107, 255], 12))
            .count()
    };
    let my_mail = (908, 515);
    let mail = (428, 378);
    assert!(
        badge(&f, my_mail) > 150,
        "{} badge pixels",
        badge(&f, my_mail)
    );
    assert_eq!(badge(&f, mail), 0, "Mail itself wears no badge");
    // Mail's glyph in both.
    assert!(close(f.pixel(900, 497), [58, 168, 245], 8));
    assert!(close(f.pixel(420, 360), [58, 168, 245], 8));
    // The same app not based on another is just an app: no badge.
    let mut plain = apps.clone();
    plain.last_mut().unwrap().based_on = None;
    assert_eq!(badge(&render_launcher(&plain), my_mail), 0);
    // Dispute Tracker's initial sits on a tinted tile, not on the dark of
    // every tile.
    let tinted = f.pixel(770, 495);
    assert!(!close(tinted, [21, 24, 29], 6), "{tinted:?}");
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
    let [account, models, ready] = render_setup();
    snapshot("setup-account", &account);
    snapshot("setup-models", &models);
    snapshot("setup-ready", &ready);
    snapshot("desktop", &render_desktop());
    snapshot("agents-menu", &render_agents_menu());
    snapshot("system-menu", &render_system_menu());
    snapshot("lock", &render_lock());
    snapshot("about", &render_about());
    snapshot("force-quit", &render_force_quit());
    snapshot("restart", &render_restart());
    snapshot("launcher", &render_launcher(&fixtures::apps()));
    snapshot("command-bar", &render_command_bar());
    snapshot("expanded", &render_expanded());
    snapshot("expanded-review", &render_expanded_review());
}
