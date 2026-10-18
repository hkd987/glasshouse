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
    screen.update(view, &events, Some(at)).messages
}

/// The pointer moving to `at`, nothing pressed.
fn point(screen: &mut Screen, live: &Live, at: Point) -> glass_shell::render::Updated<Message> {
    let theme = Theme::glass();
    let bar = live.command_bar();
    let view = live.view(&theme, bar.as_ref(), artboard_time(), SIZE);
    let moved = [Event::Mouse(mouse::Event::CursorMoved { position: at })];
    screen.update(view, &moved, Some(at))
}

#[test]
fn ux_the_pointer_on_a_banner_keeps_it_and_leaving_lets_it_go() {
    let mut live = Live::new(fixtures::tasks());
    assert!(
        !live.banners().is_empty(),
        "what's waiting shows as banners"
    );
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    let empty = Point::new(400.0, 450.0);
    let moved = point(&mut screen, &live, empty);
    assert!(moved.messages.is_empty());
    assert!(
        !moved.redraw,
        "the pointer crossing the wallpaper draws nothing"
    );
    // Onto the banners, at the top right.
    let noticed = (60..300).step_by(20).any(|y| {
        point(&mut screen, &live, Point::new(1200.0, y as f32)).messages == [Message::HoldBanners]
    });
    assert!(noticed, "the banners notice the pointer");
    live.update(Message::HoldBanners);
    assert_eq!(
        live.next_change(),
        None,
        "held while the pointer is on them"
    );
    assert_eq!(
        point(&mut screen, &live, empty).messages,
        [Message::LetBannersGo]
    );
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
fn aw10_from_the_agents_menu_a_tile_opens_and_its_header_stops_the_task() {
    let tasks = fixtures::tasks();
    let invoices = tasks
        .iter()
        .find(|t| t.agent_name == "Invoice agent")
        .cloned()
        .expect("the invoice agent's tile");
    let id = invoices.id;
    let mut live = Live::new(tasks);
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);

    // It isn't a banner (it doesn't need the person), so open the agents
    // menu from the menu bar's pill first.
    let toggle = click_for(
        &mut screen,
        &live,
        (900..1440).step_by(20),
        [14].into_iter(),
        |m| *m == Message::ToggleAgents,
    )
    .expect("the agents pill opens the menu");
    assert_eq!(live.update(toggle), None);

    // Its tile is the last in the menu at the top right.
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
            screen.update(view, &[press], None).messages
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

#[test]
fn ux_a_banner_coming_and_going_redraws_only_where_it_is() {
    use glass_api::{Event as TaskEvent, TileState};
    use glass_shell::live::BANNER;
    use std::time::Instant;

    let theme = Theme::glass();
    let working = fixtures::tasks()
        .into_iter()
        .find(|t| t.state == TileState::Working)
        .expect("a working tile");
    let mut live = Live::new(Vec::new());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    let mut render =
        |live: &Live| screen.render(live.view(&theme, None, artboard_time(), SIZE), None);
    render(&live);
    let at = Instant::now();
    live.apply_at(
        TaskEvent::TaskUpdated {
            task: Box::new(working),
        },
        at,
    );
    // The banner, the pill's count and the dock's agent: not the screen,
    // and nothing drawn twice.
    let came = render(&live);
    let area: u32 = came.iter().map(|d| d.width * d.height).sum();
    assert!(area < (SIZE.0 * SIZE.1 / 8.0) as u32, "{came:?}");
    assert!(
        came.iter()
            .all(|d| d.y + d.height < 300 || d.y > SIZE.1 as u32 - 100),
        "the top right and the dock: {came:?}"
    );
    assert!(live.tick(at + BANNER));
    let went = render(&live);
    assert!(!went.is_empty());
    assert!(
        went.iter().all(|d| d.y + d.height < 300),
        "only the top right, where the banner was: {went:?}"
    );
}

#[test]
fn the_mark_opens_the_system_menu_and_sign_out_signs_out() {
    let mut live = Live::new(fixtures::tasks()).signed_in_as("Dana Ruiz");
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    assert_eq!(
        click(&mut screen, &live, Point::new(24.0, 16.0)),
        [Message::ToggleSystemMenu]
    );
    live.update(Message::ToggleSystemMenu);
    let task = (40..120)
        .step_by(6)
        .flat_map(|y| (20..300).step_by(40).map(move |x| (x, y)))
        .find_map(|(x, y)| {
            click(&mut screen, &live, Point::new(x as f32, y as f32))
                .into_iter()
                .find(|m| *m == Message::OpenCommandBar)
        });
    assert!(task.is_some(), "Hand an agent a task, in the system menu");
    let found = (40..220)
        .step_by(6)
        .flat_map(|y| (20..300).step_by(40).map(move |x| (x, y)))
        .find_map(|(x, y)| {
            click(&mut screen, &live, Point::new(x as f32, y as f32))
                .into_iter()
                .find(|m| *m == Message::SignOut)
        })
        .expect("Sign out in the system menu");
    assert_eq!(live.update(found), Some(Effect::SignOut));
}

#[test]
fn the_pointer_going_onto_a_button_redraws_it_and_off_again() {
    let live = Live::new(fixtures::tasks());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    let theme = Theme::glass();
    screen.render(
        live.view(&theme, None, artboard_time(), SIZE),
        Some(Point::new(400.0, 450.0)),
    );
    // The agents pill, in the menu bar at the top right.
    let pill = (1100..1300)
        .step_by(10)
        .map(|x| Point::new(x as f32, 16.0))
        .find(|&at| point(&mut screen, &live, at).redraw)
        .expect("the pill notices the pointer");
    assert!(!point(&mut screen, &live, pill).redraw, "still on it");
    assert!(point(&mut screen, &live, Point::new(400.0, 450.0)).redraw);
}

#[test]
fn a_focused_text_field_asks_to_be_drawn_again_for_its_caret() {
    let theme = Theme::glass();
    let mut live = Live::new(fixtures::tasks());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    let draw = |screen: &mut Screen, live: &Live| {
        let bar = live.command_bar();
        screen.render(live.view(&theme, bar.as_ref(), artboard_time(), SIZE), None);
    };
    draw(&mut screen, &live);
    assert_eq!(screen.next_redraw(), None, "nothing blinks on the desktop");
    live.update(Message::OpenCommandBar);
    let bar = live.command_bar();
    screen.focus(
        live.view(&theme, bar.as_ref(), artboard_time(), SIZE),
        glass_shell::screens::command_bar::INPUT,
    );
    draw(&mut screen, &live);
    let wake = screen.next_redraw().expect("the caret blinks");
    assert!(wake <= std::time::Instant::now() + std::time::Duration::from_secs(1));
}

#[test]
fn aw10_the_agents_icon_in_the_dock_opens_the_agents_menu() {
    let live = Live::new(fixtures::tasks());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    // The dock is centered at the bottom; the agents icon is its last.
    let found = (700..1100)
        .step_by(12)
        .map(|x| Point::new(x as f32, SIZE.1 - 40.0))
        .any(|at| click(&mut screen, &live, at) == [Message::ToggleAgents]);
    assert!(found, "the dock's agents icon opens the menu");
}

#[test]
fn the_system_menu_has_about_and_force_quit() {
    let mut live = Live::new(fixtures::tasks());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    live.update(Message::ToggleSystemMenu);
    let mut seen = Vec::new();
    for y in (40..200).step_by(6) {
        for message in click(&mut screen, &live, Point::new(80.0, y as f32)) {
            if !seen.contains(&message) {
                seen.push(message);
            }
        }
    }
    for want in [
        Message::OpenAbout,
        Message::OpenCommandBar,
        Message::OpenForceQuit,
        Message::SignOut,
    ] {
        assert!(seen.contains(&want), "{want:?} in {seen:?}");
    }
}

#[test]
fn force_quit_lists_the_running_agents_to_choose_from() {
    let tasks = fixtures::tasks();
    let mut live = Live::new(tasks.clone());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    live.update(Message::OpenForceQuit);
    let chosen = (300..700).step_by(8).find_map(|y| {
        click(&mut screen, &live, Point::new(720.0, y as f32))
            .into_iter()
            .find_map(|m| match m {
                Message::ChooseToQuit(task) => Some(task),
                _ => None,
            })
    });
    let chosen = chosen.expect("a running agent in the list");
    assert!(tasks.iter().any(|t| t.id == chosen && t.state.is_live()));
    // A click on the window itself, away from its buttons, keeps it open.
    let window = click(&mut screen, &live, Point::new(720.0, 330.0));
    assert!(!window.contains(&Message::CloseSystemWindow), "{window:?}");
    assert_eq!(
        click(&mut screen, &live, Point::new(40.0, 600.0)),
        [Message::CloseSystemWindow],
        "a click outside closes it"
    );
}
