//! What a redraw costs on this machine, for comparing changes to the
//! renderer: `cargo test -p glass-shell --test redraw_cost -- --ignored
//! --nocapture`. (In the VM everything is about 15x slower.)

use std::time::Instant;

use glass_shell::live::Live;
use glass_shell::render::Screen;
use glass_shell::{Size, Theme, fixtures, wallpaper};
use time::{Duration, OffsetDateTime};

#[test]
#[ignore]
fn a_clock_tick_redraws_in_a_few_milliseconds() {
    let theme = Theme::glass();
    let live = Live::new(fixtures::tasks()).without_wallpaper();
    let size = (1280.0, 800.0);
    let at = OffsetDateTime::from_unix_timestamp(fixtures::now().unix_seconds()).expect("valid");
    let mut screen = Screen::new(Size::new(size.0, size.1), 1.0);
    let started = Instant::now();
    screen.set_backdrop(wallpaper::backdrop(&theme.wallpaper, 1280, 800));
    eprintln!("backdrop {:?}", started.elapsed());
    let started = Instant::now();
    screen.render(live.view(&theme, None, at, size), None);
    eprintln!("first frame {:?}", started.elapsed());
    for minutes in 1..4 {
        let started = Instant::now();
        let damage = screen.render(
            live.view(&theme, None, at + Duration::minutes(minutes), size),
            None,
        );
        eprintln!("clock tick {:?}: {} areas", started.elapsed(), damage.len());
    }
    let started = Instant::now();
    let damage = screen.render(
        live.view(&theme, None, at + Duration::minutes(3), size),
        None,
    );
    eprintln!(
        "nothing changed {:?}: {} areas",
        started.elapsed(),
        damage.len()
    );
}

#[test]
#[ignore]
fn a_keystroke_in_the_command_bar_redraws_in_a_few_milliseconds() {
    use glass_shell::Message;

    let theme = Theme::glass();
    let mut live = Live::new(fixtures::tasks()).without_wallpaper();
    let size = (1280.0, 800.0);
    let at = OffsetDateTime::from_unix_timestamp(fixtures::now().unix_seconds()).expect("valid");
    let mut screen = Screen::new(Size::new(size.0, size.1), 1.0);
    screen.set_backdrop(wallpaper::backdrop(&theme.wallpaper, 1280, 800));
    live.update(Message::OpenCommandBar);
    let draw = |screen: &mut Screen, live: &Live| {
        let bar = live.command_bar();
        screen.render(live.view(&theme, bar.as_ref(), at, size), None)
    };
    draw(&mut screen, &live);
    for text in ["P", "Pr", "Pre"] {
        live.update(Message::CommandChanged(text.into()));
        let started = Instant::now();
        let damage = draw(&mut screen, &live);
        eprintln!("keystroke {:?}: {damage:?}", started.elapsed());
    }
}
