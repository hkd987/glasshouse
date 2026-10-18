//! Clicks and keys through the real UI, headless: what the framebuffer
//! shell (and later the compositor's) does with the person's input.

use glass_api::{AnswerApproval, ApprovalAnswer, TaskRef, methods};
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

/// The first message a click somewhere in `xs` by `ys` produces that
/// `want` accepts.
fn click_for(
    screen: &mut Screen,
    live: &Live,
    xs: impl Iterator<Item = u32> + Clone,
    ys: impl Iterator<Item = u32>,
    want: impl Fn(&Message) -> bool,
) -> Option<Message> {
    ys.flat_map(|y| xs.clone().map(move |x| (x, y)))
        .find_map(|(x, y)| {
            click(screen, live, Point::new(x as f32, y as f32))
                .into_iter()
                .find(|m| want(m))
        })
}

#[test]
fn aw10_clicking_a_tile_opens_it_and_its_header_stops_the_task() {
    let tasks = fixtures::tasks();
    let invoices = tasks
        .iter()
        .find(|t| t.agent_name == "Invoice agent")
        .cloned()
        .expect("the invoice agent's tile");
    let id = invoices.id;
    let mut live = Live::new(tasks);
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);

    // Its tile is the last in the stack at the top right.
    let open = click_for(
        &mut screen,
        &live,
        (1080..1400).step_by(40),
        (40..800).step_by(20),
        |m| *m == Message::OpenTask(id),
    )
    .expect("the invoice tile opens");
    assert_eq!(live.update(open), Some(Effect::Get(id)));

    // `task.get` answers.
    let detail = glass_api::TaskDetail {
        view: invoices,
        ..fixtures::invoice_detail()
    };
    live.detail(detail.clone());
    assert_eq!(live.open_task(), Some(id));

    // Stop is in the header, on the right (AW-11).
    let stop = click_for(
        &mut screen,
        &live,
        (1000..1390).step_by(12),
        (64..100).step_by(12),
        |m| *m == Message::Stop(id),
    )
    .expect("a Stop button in the header");
    let effect = live.update(stop).expect("stop asks glassd");
    assert_eq!(effect, Effect::Stop(id));
    assert_eq!(
        effect.task_call(),
        Some((methods::TASK_STOP, TaskRef { task: id }))
    );

    // Roll back, in the timeline's corner, asks what it would change
    // first (AW-15).
    let roll_back = click_for(
        &mut screen,
        &live,
        (1150..1390).step_by(20),
        (740..795).step_by(10),
        |m| *m == Message::RollBack,
    )
    .expect("a Roll back button under the timeline");
    let Some(Effect::Undo(request)) = live.update(roll_back) else {
        panic!("roll back asks task.undo");
    };
    assert!(request.dry_run);
    assert_eq!(request.to, detail.snapshots.last().map(|s| s.id));

    // The collapse button at the top left goes back to the tile (AW-16).
    let collapse = click_for(
        &mut screen,
        &live,
        (50..110).step_by(10),
        (50..115).step_by(10),
        |m| *m == Message::CloseTask,
    )
    .expect("a collapse button");
    live.update(collapse);
    assert_eq!(live.open_task(), None);
}

#[test]
fn da12_review_on_a_tile_shows_the_ask_and_declines_it() {
    let tasks = fixtures::tasks();
    let tile = tasks[0].clone();
    let ask = tile.needs.clone().expect("a needs-you tile");
    let mut live = Live::new(tasks);
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    let review = click_for(
        &mut screen,
        &live,
        (1040..1420).step_by(30),
        (40..420).step_by(14),
        |m| *m == Message::Review(ask.id),
    )
    .expect("a Review button on the needs-you tile");
    assert_eq!(live.update(review), Some(Effect::Get(tile.id)));
    live.detail(fixtures::expense_detail(&tile));
    let decline = click_for(
        &mut screen,
        &live,
        (600..1010).step_by(20),
        (560..650).step_by(10),
        |m| *m == Message::Decline(ask.id),
    )
    .expect("a Decline button under the items");
    assert_eq!(
        live.update(decline),
        Some(Effect::Answer(AnswerApproval {
            approval: ask.id,
            answer: ApprovalAnswer::Deny,
        }))
    );
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
