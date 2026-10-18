//! Clicks and keys through the real UI, headless: what the framebuffer
//! shell (and later the compositor's) does with the person's input.

use glass_shell::live::{Effect, Live};
use glass_shell::render::Screen;
use glass_shell::{Message, Size, Theme, fixtures};
use iced_core::keyboard::key::{Named, NativeCode, Physical};
use iced_core::keyboard::{self, Key, Location, Modifiers};
use iced_core::{Event, Point, mouse};
use time::OffsetDateTime;

const SIZE: (f32, f32) = (1440.0, 900.0);

fn artboard_time() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(fixtures::now().unix_seconds()).expect("valid")
}

fn click(screen: &mut Screen, live: &Live, at: Point) -> Vec<Message> {
    let theme = Theme::glass();
    let bar = live.command_bar();
    let view = live.view(&theme, bar.as_ref(), artboard_time(), SIZE);
    let events = [
        Event::Mouse(mouse::Event::CursorMoved { position: at }),
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ];
    screen.update(view, &events, Some(at))
}

#[test]
fn da12_clicking_approve_on_the_needs_you_tile_approves_its_ask() {
    let tasks = fixtures::tasks();
    let ask = tasks
        .iter()
        .find_map(|t| t.needs.clone())
        .expect("a needs-you tile");
    let mut live = Live::new(tasks);
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    // The tile stack is at the top right; look for the button there.
    let found = (40..420)
        .step_by(14)
        .flat_map(|y| (1040..1420).step_by(30).map(move |x| (x, y)))
        .find_map(|(x, y)| {
            click(&mut screen, &live, Point::new(x as f32, y as f32))
                .into_iter()
                .find(|m| *m == Message::Approve(ask.id))
        })
        .expect("an Approve button in the tile stack");
    assert!(matches!(
        live.update(found),
        Some(Effect::Answer(answer)) if answer.approval == ask.id
    ));
}

#[test]
fn da1_typing_into_the_command_bar_after_ctrl_space() {
    let theme = Theme::glass();
    let mut live = Live::new(fixtures::tasks());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    let open = live
        .shortcut(&Key::Named(Named::Space), Modifiers::CTRL)
        .expect("Ctrl Space is a shortcut");
    let Some(Effect::Focus(id)) = live.update(open) else {
        panic!("opening the bar focuses its input");
    };
    {
        let bar = live.command_bar();
        screen.focus(live.view(&theme, bar.as_ref(), artboard_time(), SIZE), id);
    }
    for c in ["h", "i"] {
        let key = Key::Character(c.into());
        let press = Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: Some(c.into()),
            repeat: false,
        });
        let messages = {
            let bar = live.command_bar();
            let view = live.view(&theme, bar.as_ref(), artboard_time(), SIZE);
            screen.update(view, &[press], None)
        };
        for message in messages {
            live.update(message);
        }
    }
    assert_eq!(live.command_bar().map(|b| b.query), Some("hi".into()));
}

#[test]
fn redrawing_only_what_changed_matches_drawing_everything() {
    redraws_match(false);
}

#[test]
fn redrawing_over_a_backdrop_matches_drawing_everything() {
    redraws_match(true);
}

/// A tile changes: the screen redraws a small part, and the result is what
/// drawing everything again gives. With `backdrop`, the screen keeps the
/// wallpaper and the view leaves it out.
fn redraws_match(backdrop: bool) {
    use glass_api::{Event as TaskEvent, TileState};

    let theme = Theme::glass();
    let tasks = fixtures::tasks();
    let mut live = Live::new(tasks.clone());
    if backdrop {
        live = live.without_wallpaper();
    }
    let screen = || {
        let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
        if backdrop {
            screen.set_backdrop(glass_shell::wallpaper::backdrop(
                &theme.wallpaper,
                SIZE.0 as u32,
                SIZE.1 as u32,
            ));
        }
        screen
    };
    let render = |screen: &mut Screen, live: &Live| {
        screen.render(live.view(&theme, None, artboard_time(), SIZE), None)
    };
    let mut kept = screen();
    let first = render(&mut kept, &live);
    assert_eq!(first.len(), 1, "the first frame draws everything");

    // One tile finishes.
    let mut done = tasks[1].clone();
    done.state = TileState::Done;
    done.needs = None;
    live.apply(TaskEvent::TaskUpdated {
        task: Box::new(done),
    });
    let changed = render(&mut kept, &live);
    let area: u32 = changed.iter().map(|d| d.width * d.height).sum();
    assert!(!changed.is_empty());
    assert!(
        area < (SIZE.0 * SIZE.1 / 3.0) as u32,
        "a tile change redraws a small part: {changed:?}"
    );

    let mut fresh = screen();
    render(&mut fresh, &live);
    // Shadows blurred under a smaller clip can round a step differently;
    // nothing a person could see.
    let worst = kept
        .frame()
        .rgba
        .iter()
        .zip(&fresh.frame().rgba)
        .map(|(a, b)| a.abs_diff(*b))
        .max();
    assert!(
        worst.is_some_and(|d| d <= 2),
        "redrawing the damage gives the pixels drawing it all does (off by {worst:?})"
    );
    let again = render(&mut kept, &live);
    assert!(again.is_empty(), "nothing changed: {again:?}");
}
