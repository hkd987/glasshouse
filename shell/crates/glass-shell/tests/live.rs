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
