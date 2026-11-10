//! Clicks and keys through the real UI, headless: what the framebuffer
//! shell (and later the compositor's) does with the person's input.

use glass_api::{AnswerApproval, ApprovalAnswer, TaskRef, methods};
use glass_shell::apps::AppEntry;
use glass_shell::live::{Effect, Live};
use glass_shell::render::Screen;
use glass_shell::screens::login;
use glass_shell::session::Password;
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
    let task = (40..180)
        .step_by(6)
        .flat_map(|y| (20..300).step_by(40).map(move |x| (x, y)))
        .find_map(|(x, y)| {
            click(&mut screen, &live, Point::new(x as f32, y as f32))
                .into_iter()
                .find(|m| *m == Message::OpenCommandBar)
        });
    assert!(task.is_some(), "Hand an agent a task, in the system menu");
    let found = (40..320)
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
    for y in (40..320).step_by(6) {
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
        Message::AskPower(glass_shell::Power::Restart),
        Message::AskPower(glass_shell::Power::ShutDown),
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

/// Everything the system menu offers, clicked line by line.
fn system_menu_messages(live: &Live) -> Vec<Message> {
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    let mut seen = Vec::new();
    for y in (40..320).step_by(6) {
        for message in click(&mut screen, live, Point::new(80.0, y as f32)) {
            if !seen.contains(&message) {
                seen.push(message);
            }
        }
    }
    seen
}

/// A key pressed in the focused widget.
fn key(key: Key, text: Option<&str>) -> Event {
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: Physical::Unidentified(NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: text.map(Into::into),
        repeat: false,
    })
}

#[test]
fn lock_screen_is_in_the_system_menu_when_the_sign_in_screen_can_unlock_it() {
    let mut live = Live::new(fixtures::tasks()).signed_in_as("Dana Ruiz");
    live.update(Message::ToggleSystemMenu);
    let seen = system_menu_messages(&live);
    assert!(seen.contains(&Message::SignOut), "{seen:?}");
    assert!(
        !seen.contains(&Message::LockScreen),
        "shown, but nothing could unlock it: {seen:?}"
    );

    let mut live = Live::new(fixtures::tasks())
        .signed_in_as("Dana Ruiz")
        .lockable();
    live.update(Message::ToggleSystemMenu);
    let seen = system_menu_messages(&live);
    assert!(seen.contains(&Message::LockScreen), "{seen:?}");
    live.update(Message::LockScreen);
    assert!(live.is_locked());
}

#[test]
fn locked_the_screen_takes_only_the_password_and_unlocks_on_the_answer() {
    let theme = Theme::glass();
    let mut live = Live::new(fixtures::tasks())
        .signed_in_as("Dana Ruiz")
        .lockable();
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    // Super L as on Windows, Ctrl Super Q as on a Mac.
    for (c, modifiers) in [
        ("l", Modifiers::LOGO),
        ("q", Modifiers::CTRL | Modifiers::LOGO),
    ] {
        assert_eq!(
            live.shortcut(&Key::Character(c.into()), modifiers),
            Some(Message::LockScreen)
        );
    }
    let Some(Effect::Focus(id)) = live.update(Message::LockScreen) else {
        panic!("locking focuses the password field");
    };
    assert!(live.is_locked());
    // Nothing but the password: no shortcut, no other message.
    assert_eq!(
        live.shortcut(&Key::Named(Named::Space), Modifiers::CTRL),
        None
    );
    assert_eq!(live.update(Message::SignOut), None);
    assert_eq!(live.update(Message::OpenCommandBar), None);
    assert!(live.command_bar().is_none());

    screen.focus(live.view(&theme, None, artboard_time(), SIZE), id);
    let typing = [
        key(Key::Character("p".into()), Some("p")),
        key(Key::Character("w".into()), Some("w")),
        key(Key::Named(Named::Enter), None),
    ];
    let mut effects = Vec::new();
    for press in typing {
        let messages = {
            let view = live.view(&theme, None, artboard_time(), SIZE);
            screen.update(view, &[press], None).messages
        };
        effects.extend(messages.into_iter().filter_map(|m| live.update(m)));
    }
    assert_eq!(effects, [Effect::Unlock(Password::new("pw".into()))]);
    // Asked once: pressing Enter again while it's checked asks nothing.
    assert_eq!(live.update(Message::SignIn), None);

    // A wrong password: still locked, the field focused for another try.
    assert_eq!(
        live.unlock_answered(false),
        Some(Effect::Focus(login::PASSWORD))
    );
    assert!(live.is_locked());
    // The right one: the desktop is back, and its shortcuts.
    assert_eq!(live.unlock_answered(true), None);
    assert!(!live.is_locked());
    assert!(
        live.shortcut(&Key::Named(Named::Space), Modifiers::CTRL)
            .is_some()
    );
}

#[test]
fn an_unlocked_screen_cant_be_unlocked_and_a_plain_desktop_cant_lock() {
    let mut live = Live::new(fixtures::tasks());
    assert_eq!(live.update(Message::LockScreen), None);
    assert!(!live.is_locked());
    assert_eq!(live.unlock_answered(true), None);
    assert_eq!(live.update(Message::SignIn), None);
}

#[test]
fn ui1_apps_windows_hide_behind_the_lock_screen_and_menus_take_the_input() {
    let mut live = Live::new(fixtures::tasks())
        .signed_in_as("Dana Ruiz")
        .lockable();
    assert!(live.shows_windows() && !live.takes_all_input());
    // A menu over the windows takes clicks and keys, and they still show.
    live.update(Message::ToggleSystemMenu);
    assert!(live.shows_windows() && live.takes_all_input());
    live.update(Message::ToggleSystemMenu);
    // Locked: nothing of them shows or hears anything.
    live.update(Message::LockScreen);
    assert!(!live.shows_windows() && live.takes_all_input());
    live.unlock_answered(true);
    assert!(live.shows_windows() && !live.takes_all_input());
}

fn window(id: u64, minimized: bool) -> glass_shell::apps::AppWindow {
    glass_shell::apps::AppWindow {
        id,
        app_id: "glass.terminal".into(),
        title: "Terminal".into(),
        minimized,
    }
}

#[test]
fn dk1_the_dock_steps_aside_for_windows_and_comes_back_at_the_bottom_edge() {
    let mut live = Live::new(fixtures::tasks()).with_apps(glass_shell::apps::natives());
    assert!(live.shows_dock(), "with nothing open, the dock is up");
    live.set_windows(vec![window(4, false)]);
    assert!(!live.shows_dock(), "a window has the screen to itself");
    assert!(
        live.pointer_at(899.0, SIZE.1),
        "the bottom edge brings it up"
    );
    assert!(live.shows_dock());
    assert!(!live.pointer_at(850.0, SIZE.1), "on the dock it stays");
    assert!(live.pointer_at(500.0, SIZE.1), "above it, it goes again");
    assert!(!live.shows_dock());
    live.update(Message::ToggleLauncher);
    assert!(live.shows_dock(), "the launcher keeps it up");
    live.update(Message::ToggleLauncher);
    live.set_windows(vec![window(4, true)]);
    assert!(
        live.shows_dock(),
        "minimized windows leave the screen to it"
    );
}

#[test]
fn ad7_the_dock_offers_only_the_apps_in_the_profile() {
    use glass_shell::apps::{AppEntry, Start};
    let web = |id: &str, name: &str| AppEntry {
        id: id.into(),
        name: name.into(),
        start: Start::Web,
        based_on: None,
    };
    let launched = |live: &Live| {
        let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
        let mut ids: Vec<String> = (300..1200)
            .step_by(8)
            .flat_map(|x| click(&mut screen, live, Point::new(x as f32, 860.0)))
            .filter_map(|m| match m {
                Message::Launch(id) => Some(id),
                _ => None,
            })
            .collect();
        ids.dedup();
        ids
    };
    let live = Live::new(fixtures::tasks()).with_apps(vec![web("mail", "Mail")]);
    assert_eq!(launched(&live), ["mail"]);
    let live = Live::new(fixtures::tasks())
        .with_apps(vec![web("calendar", "Calendar"), web("store", "Store")]);
    assert_eq!(launched(&live), ["calendar", "store"]);
}

#[test]
fn dv11_opening_the_launcher_asks_for_the_apps_again() {
    let natives = glass_shell::apps::natives();
    let mut live = Live::new(fixtures::tasks()).with_apps(natives[..1].to_vec());
    // Opening it asks for the person's apps; closing it doesn't.
    assert_eq!(live.update(Message::ToggleLauncher), Some(Effect::ListApps));
    assert_eq!(live.update(Message::ToggleLauncher), None);
    // One installed from the Store since is offered; the same list again
    // changes nothing to draw.
    let added = natives.last().expect("a second native app").clone();
    assert_eq!(live.update(Message::Launch(added.id.clone())), None);
    assert!(live.set_apps(natives.clone()));
    assert!(!live.set_apps(natives));
    live.update(Message::ToggleLauncher);
    assert_eq!(
        live.update(Message::Launch(added.id.clone())),
        Some(Effect::Launch(added))
    );
}

#[test]
fn dk2_the_launcher_offers_the_installed_apps_and_starts_one() {
    let mut live = Live::new(fixtures::tasks()).with_apps(glass_shell::apps::natives());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    // The dock's launcher icon opens it.
    let opened = (480..960)
        .step_by(8)
        .find_map(|x| {
            click(&mut screen, &live, Point::new(x as f32, 860.0))
                .into_iter()
                .find(|m| *m == Message::ToggleLauncher)
        })
        .expect("the launcher icon in the dock");
    live.update(opened);
    assert!(
        live.takes_all_input(),
        "over apps' windows, it takes the input"
    );
    // Its Terminal starts the program.
    let launched = (300..1200)
        .step_by(10)
        .flat_map(|x| (300..600).step_by(10).map(move |y| (x, y)))
        .find_map(|(x, y)| {
            click(&mut screen, &live, Point::new(x as f32, y as f32))
                .into_iter()
                .find(|m| matches!(m, Message::Launch(_)))
        })
        .expect("Terminal in the launcher");
    assert_eq!(
        live.update(launched),
        Some(Effect::Launch(AppEntry::native("terminal").unwrap()))
    );
    assert!(!live.takes_all_input(), "starting an app closes it");
    // Esc closes it too; an id it doesn't offer starts nothing.
    live.update(Message::ToggleLauncher);
    assert_eq!(
        live.shortcut(&Key::Named(Named::Escape), Modifiers::empty()),
        Some(Message::ToggleLauncher)
    );
    assert_eq!(live.update(Message::Launch("glass.nothing".into())), None);
}

#[test]
fn ux2_tab_moves_through_setups_fields_and_shift_tab_back() {
    use glass_shell::screens::setup;
    use glass_shell::setup::Setup;
    let theme = Theme::glass();
    let mut setup = Setup::new(Vec::new());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    screen.focus(setup::view(&theme, &setup.screen(), SIZE), setup::NAME);
    let tab = |shift: bool| {
        let Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            modified_key,
            physical_key,
            location,
            text,
            repeat,
            ..
        }) = key(Key::Named(Named::Tab), Some("\t"))
        else {
            unreachable!()
        };
        Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            modified_key,
            physical_key,
            location,
            modifiers: if shift {
                Modifiers::SHIFT
            } else {
                Modifiers::empty()
            },
            text,
            repeat,
        })
    };
    let mut typed = Vec::new();
    for press in [
        key(Key::Character("a".into()), Some("a")),
        tab(false),
        key(Key::Character("b".into()), Some("b")),
        tab(false),
        key(Key::Character("c".into()), Some("c")),
        tab(true),
        key(Key::Character("d".into()), Some("d")),
    ] {
        let view = setup::view(&theme, &setup.screen(), SIZE);
        for message in screen.update(view, &[press], None).messages {
            typed.push(message.clone());
            setup.update(message);
        }
    }
    assert_eq!(
        typed,
        [
            Message::SetupName("a".into()),
            Message::SetupPassword("b".into()),
            Message::SetupConfirm("c".into()),
            Message::SetupPassword("bd".into()),
        ]
    );
}

/// Apps' windows as the compositor lists them for Alt Tab, the one last
/// used first.
fn windows(titles: &[(u64, &str)]) -> Vec<glass_shell::apps::AppWindow> {
    titles
        .iter()
        .map(|&(id, title)| glass_shell::apps::AppWindow {
            id,
            app_id: "glass.terminal".into(),
            title: title.into(),
            minimized: false,
        })
        .collect()
}

#[test]
fn dk2_alt_tab_raises_the_window_used_before_and_alt_shift_tab_goes_back() {
    let mut live = Live::new(fixtures::tasks());
    let tab = |live: &Live, shift: bool| {
        let mods = if shift {
            Modifiers::ALT | Modifiers::SHIFT
        } else {
            Modifiers::ALT
        };
        live.shortcut(&Key::Named(Named::Tab), mods)
    };
    assert_eq!(tab(&live, false), None, "no windows: Alt Tab is the app's");
    live.set_windows(windows(&[
        (7, "Terminal"),
        (3, "Activity Monitor"),
        (9, "Notes"),
    ]));

    // Quick Alt Tab: the window used before this one.
    let open = tab(&live, false).expect("Alt Tab is a shortcut");
    assert_eq!(open, Message::SwitchWindows { back: false });
    live.update(open);
    assert!(live.is_switching() && live.takes_all_input());
    assert_eq!(live.update(Message::SwitchDone), Some(Effect::Activate(3)));
    assert!(!live.is_switching());

    // Holding Alt, each Tab goes one further, wrapping; Shift Tab comes back.
    live.update(Message::SwitchWindows { back: false });
    live.update(Message::SwitchWindows { back: false });
    live.update(Message::SwitchWindows { back: false });
    live.update(tab(&live, true).unwrap());
    assert_eq!(live.update(Message::SwitchDone), Some(Effect::Activate(9)));

    // Alt Shift Tab from the start: the oldest window; Tab wraps around.
    live.update(Message::SwitchWindows { back: true });
    live.update(Message::SwitchWindows { back: false });
    assert_eq!(live.update(Message::SwitchDone), Some(Effect::Activate(7)));

    // Esc keeps things as they were, and Alt going up then raises nothing.
    live.update(Message::SwitchWindows { back: false });
    let esc = live.shortcut(&Key::Named(Named::Escape), Modifiers::ALT);
    assert_eq!(esc, Some(Message::CancelSwitch));
    live.update(Message::CancelSwitch);
    assert_eq!(live.update(Message::SwitchDone), None);
}

#[test]
fn dk2_the_switcher_follows_windows_closing_and_stays_off_the_lock_screen() {
    let mut live = Live::new(fixtures::tasks()).signed_in_as("Dana Ruiz");
    live.set_windows(windows(&[(7, "Terminal"), (3, "Activity Monitor")]));
    live.update(Message::SwitchWindows { back: false });
    // The chosen window closes while Alt is down: the first one is chosen.
    live.set_windows(windows(&[(7, "Terminal")]));
    assert_eq!(live.update(Message::SwitchDone), Some(Effect::Activate(7)));
    // One window: Alt Tab raises it. None: nothing.
    live.update(Message::SwitchWindows { back: false });
    assert_eq!(live.update(Message::SwitchDone), Some(Effect::Activate(7)));
    live.set_windows(Vec::new());
    assert_eq!(live.update(Message::SwitchWindows { back: false }), None);
    assert!(!live.is_switching());
    // The command bar closes, so the raised window gets the keys.
    live.set_windows(windows(&[(7, "Terminal")]));
    live.update(Message::OpenCommandBar);
    live.update(Message::SwitchWindows { back: false });
    live.update(Message::SwitchDone);
    assert!(!live.takes_all_input());
}

#[test]
fn dk2_the_switcher_shows_each_window_with_the_chosen_one_marked() {
    let theme = Theme::glass();
    let mut live = Live::new(fixtures::tasks());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    live.set_windows(windows(&[(7, "Terminal"), (3, "Activity Monitor")]));
    screen.render(live.view(&theme, None, artboard_time(), SIZE), None);
    let before = screen.frame().rgba;
    live.update(Message::SwitchWindows { back: false });
    screen.render(live.view(&theme, None, artboard_time(), SIZE), None);
    let open = screen.frame().rgba;
    // The panel sits in the middle of the screen.
    let at = |rgba: &[u8], x: usize, y: usize| {
        let i = (y * SIZE.0 as usize + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    let (cx, cy) = (SIZE.0 as usize / 2, SIZE.1 as usize / 2);
    assert_ne!(at(&before, cx, cy), at(&open, cx, cy));
    assert_eq!(
        at(&before, 40, 450),
        at(&open, 40, 450),
        "the rest is left alone"
    );
}

#[test]
fn ux20_the_cheat_sheet_lists_what_the_desktop_does_with_its_keys() {
    let mut live = Live::new(fixtures::tasks());
    live.set_windows(windows(&[(7, "Terminal"), (3, "Activity Monitor")]));
    let ch = |c: &str| Key::Character(c.into());
    let cases = [
        (
            "Ctrl Space",
            Key::Named(Named::Space),
            Modifiers::CTRL,
            Message::OpenCommandBar,
        ),
        (
            "Alt Tab",
            Key::Named(Named::Tab),
            Modifiers::ALT,
            Message::SwitchWindows { back: false },
        ),
        ("Super /", ch("/"), Modifiers::LOGO, Message::OpenShortcuts),
        ("Super N", ch("n"), Modifiers::LOGO, Message::ToggleAgents),
        (
            "Ctrl F2",
            Key::Named(Named::F2),
            Modifiers::CTRL,
            Message::ToggleSystemMenu,
        ),
        ("Super L", ch("l"), Modifiers::LOGO, Message::LockScreen),
        (
            "Ctrl Super Q",
            ch("q"),
            Modifiers::CTRL | Modifiers::LOGO,
            Message::LockScreen,
        ),
        (
            "Shift Super Q",
            ch("Q"),
            Modifiers::SHIFT | Modifiers::LOGO,
            Message::SignOut,
        ),
        (
            "Alt Super Esc",
            Key::Named(Named::Escape),
            Modifiers::ALT | Modifiers::LOGO,
            Message::OpenForceQuit,
        ),
        (
            "Ctrl Shift Esc",
            Key::Named(Named::Escape),
            Modifiers::CTRL | Modifiers::SHIFT,
            Message::OpenForceQuit,
        ),
    ];
    for (keys, key, modifiers, does) in cases {
        assert!(
            glass_shell::shortcuts::listed(keys),
            "{keys} isn't on the sheet"
        );
        assert_eq!(live.shortcut(&key, modifiers), Some(does), "{keys}");
    }
    assert!(glass_shell::shortcuts::listed("Arrows, Enter"));
    // The sheet opens, and Esc closes it.
    live.update(Message::OpenShortcuts);
    assert!(live.takes_all_input());
    let esc = live.shortcut(&Key::Named(Named::Escape), Modifiers::empty());
    assert_eq!(esc, Some(Message::CloseSystemWindow));
}

/// What the shell claims for `key` alone, handed to `update` as the shell
/// does; `None` when it isn't the shell's.
fn press(live: &mut Live, key: Named) -> Option<Option<Effect>> {
    let message = live.shortcut(&Key::Named(key), Modifiers::empty())?;
    Some(live.update(message))
}

#[test]
fn ux20_arrows_move_through_the_system_menu_past_what_cant_be_chosen_and_enter_chooses() {
    use glass_shell::screens::command_bar;
    let mut live = Live::new(fixtures::tasks()).signed_in_as("Dana Ruiz");
    let open = live.shortcut(&Key::Named(Named::F2), Modifiers::CTRL);
    assert_eq!(
        open,
        Some(Message::ToggleSystemMenu),
        "Ctrl F2, as on a Mac"
    );
    live.update(Message::ToggleSystemMenu);
    assert_eq!(press(&mut live, Named::Enter), None, "nothing selected yet");
    // About, Keyboard Shortcuts, then past Check for Updates (not yet).
    for _ in 0..3 {
        assert_eq!(press(&mut live, Named::ArrowDown), Some(None));
    }
    assert_eq!(
        press(&mut live, Named::Enter),
        Some(Some(Effect::Focus(command_bar::INPUT))),
        "Hand an agent a task…"
    );
    assert!(live.command_bar().is_some());
    // The command bar's text field has the arrows and Enter now.
    assert_eq!(press(&mut live, Named::ArrowDown), None);
    assert_eq!(press(&mut live, Named::Enter), None);
    live.update(Message::CloseCommandBar);

    // Up from nothing selects the last line; round the ends as a menu does.
    live.update(Message::ToggleSystemMenu);
    press(&mut live, Named::ArrowUp); // Sign out
    press(&mut live, Named::ArrowDown); // round to About
    assert_eq!(press(&mut live, Named::Enter), Some(None));
    assert_eq!(
        live.shortcut(&Key::Named(Named::Escape), Modifiers::empty()),
        Some(Message::CloseSystemWindow),
        "About Glasshouse is open"
    );
}

#[test]
fn ux20_the_selected_line_is_drawn_and_a_menu_opens_with_none_selected() {
    let theme = Theme::glass();
    let mut live = Live::new(fixtures::tasks()).signed_in_as("Dana Ruiz");
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    let mut frame = |live: &Live| {
        screen.render(live.view(&theme, None, artboard_time(), SIZE), None);
        screen.bgra().to_vec()
    };
    live.update(Message::ToggleSystemMenu);
    let plain = frame(&live);
    press(&mut live, Named::ArrowDown);
    assert_ne!(frame(&live), plain, "About is highlighted");
    live.update(Message::ToggleSystemMenu);
    live.update(Message::ToggleSystemMenu);
    assert_eq!(frame(&live), plain, "opened again, nothing is selected");
}

#[test]
fn ux20_arrows_and_enter_launch_an_app_from_the_launcher() {
    let mut live = Live::new(fixtures::tasks()).with_apps(glass_shell::apps::natives());
    live.update(Message::ToggleLauncher);
    press(&mut live, Named::ArrowRight); // Terminal
    press(&mut live, Named::ArrowRight); // Activity Monitor
    assert_eq!(
        press(&mut live, Named::Enter),
        Some(Some(Effect::Launch(
            AppEntry::native("activity-monitor").unwrap()
        )))
    );
    assert!(!live.takes_all_input(), "the launcher closed");
}

#[test]
fn ux20_super_n_opens_the_agents_menu_and_enter_opens_never_approves() {
    use glass_shell::screens::desktop;
    let tasks = fixtures::tasks();
    let first = desktop::ordered(&tasks)[0].clone();
    assert!(
        first.needs.is_some(),
        "needs-you first: the one Enter could approve"
    );
    let mut live = Live::new(tasks);
    let ch = Key::Character("n".into());
    assert_eq!(
        live.shortcut(&ch, Modifiers::LOGO),
        Some(Message::ToggleAgents)
    );
    live.update(Message::ToggleAgents);
    press(&mut live, Named::ArrowDown);
    assert_eq!(
        press(&mut live, Named::Enter),
        Some(Some(Effect::Get(first.id))),
        "its expanded view, where the ask is answered"
    );
}

#[test]
fn ux20_arrows_and_enter_stay_the_apps_when_no_menu_is_open() {
    let mut live = Live::new(fixtures::tasks());
    live.set_windows(windows(&[(7, "Terminal")]));
    let keys = [
        Named::ArrowUp,
        Named::ArrowDown,
        Named::ArrowLeft,
        Named::ArrowRight,
        Named::Home,
        Named::End,
        Named::Enter,
    ];
    for key in keys {
        assert_eq!(press(&mut live, key), None, "{key:?} on the desktop");
    }
    // With Shift (a selection in Terminal), not even in a menu.
    live.update(Message::ToggleSystemMenu);
    let shifted = live.shortcut(&Key::Named(Named::ArrowDown), Modifiers::SHIFT);
    assert_eq!(shifted, None);
}

#[test]
fn ui5_the_docks_mail_opens_mail_and_once_open_brings_it_forward() {
    use glass_shell::apps::Start;
    let mail = AppEntry {
        id: "mail".into(),
        name: "Mail".into(),
        start: Start::Web,
        based_on: None,
    };
    let mut live = Live::new(fixtures::tasks()).with_apps(vec![mail.clone()]);
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    let clicked = (480..960)
        .step_by(8)
        .find_map(|x| {
            click(&mut screen, &live, Point::new(x as f32, 860.0))
                .into_iter()
                .find(|m| *m == Message::Launch("mail".into()))
        })
        .expect("Mail in the dock");
    assert_eq!(live.update(clicked.clone()), Some(Effect::Launch(mail)));
    // Its window open, the same click raises it instead of opening another.
    live.set_windows(vec![
        glass_shell::apps::AppWindow {
            id: 4,
            app_id: "glass.terminal".into(),
            title: "Terminal".into(),
            minimized: false,
        },
        glass_shell::apps::AppWindow {
            id: 9,
            app_id: "chrome-mail.localhost__-Apps".into(),
            title: "Mail".into(),
            minimized: false,
        },
    ]);
    assert_eq!(live.update(clicked), Some(Effect::Activate(9)));
    // An app the profile doesn't give starts nothing.
    assert_eq!(live.update(Message::Launch("calendar".into())), None);
}

#[test]
fn ux1_the_clock_and_the_lock_follow_the_persons_settings() {
    use glass_api::settings::{SettingFrom, SettingView, SettingsView};
    use std::time::{Duration, Instant};

    let setting = |id: &str, value: serde_json::Value| SettingView {
        id: id.into(),
        value,
        locked: true,
        from: SettingFrom::It,
        editable: false,
    };
    let mut live = Live::new(fixtures::tasks()).lockable();
    // A new policy or the person's own change: ask glassd again.
    assert_eq!(
        live.apply(glass_api::Event::SettingsChanged),
        Some(Effect::LoadSettings)
    );

    let start = Instant::now();
    live.touched(start);
    let minutes = |n: u64| start + Duration::from_secs(n * 60);
    assert!(!live.idle_too_long(minutes(9)), "ten minutes by default");
    assert!(live.idle_too_long(minutes(10)));

    // IT locks it at five minutes and turns the 24-hour clock on.
    assert!(
        live.set_settings(SettingsView {
            settings: vec![
                setting("lock.after_minutes", serde_json::json!(5)),
                setting("clock.hour24", serde_json::json!(true)),
            ],
            utc_offset_secs: None,
        }),
        "the clock changes what's drawn"
    );
    assert!(!live.idle_too_long(minutes(4)));
    assert!(live.idle_too_long(minutes(5)));
    live.touched(minutes(5));
    assert!(
        !live.idle_too_long(minutes(9)),
        "using it starts the count again"
    );

    // A locked screen doesn't lock again.
    live.update(Message::LockScreen);
    assert!(!live.idle_too_long(minutes(30)));
    // Nor does one that can't lock (no sign-in screen behind it).
    let unlockable = Live::new(fixtures::tasks());
    assert!(!unlockable.idle_too_long(Instant::now() + Duration::from_secs(3600)));
}

#[test]
fn adr0031_the_screen_blanks_after_the_persons_minutes_and_the_next_input_only_wakes_it() {
    use glass_api::settings::{SettingFrom, SettingView, SettingsView};
    use std::time::{Duration, Instant};

    let mut live = Live::new(fixtures::tasks()).lockable();
    let start = Instant::now();
    live.touched(start);
    let minutes = |n: u64| start + Duration::from_secs(n * 60);
    assert!(!live.blank_due(minutes(4)), "five minutes by default");
    assert!(live.blank_due(minutes(5)));
    live.blank();
    assert!(live.is_blank() && !live.blank_due(minutes(6)));
    assert!(
        live.takes_all_input(),
        "nothing reaches apps while it's blank"
    );
    assert!(!live.shows_windows());
    assert!(live.wake(), "the first key or move wakes it");
    assert!(!live.wake() && !live.is_blank());

    // IT sets 0: never.
    live.set_settings(SettingsView {
        settings: vec![SettingView {
            id: "display.blank_after_minutes".into(),
            value: serde_json::json!(0),
            locked: true,
            from: SettingFrom::It,
            editable: false,
        }],
        utc_offset_secs: None,
    });
    assert!(!live.blank_due(minutes(600)));
}

#[test]
fn adr0031_do_not_disturb_keeps_banners_from_popping_up_and_the_lock_clock_follows_24_hours() {
    use glass_api::TileState;
    use glass_api::settings::{SettingFrom, SettingView, SettingsView};
    use std::time::Instant;

    let setting = |id: &str, on: bool| SettingView {
        id: id.into(),
        value: serde_json::json!(on),
        locked: false,
        from: SettingFrom::You,
        editable: true,
    };
    let mut live = Live::new(Vec::new());
    live.set_settings(SettingsView {
        settings: vec![setting("notifications.do_not_disturb", true)],
        utc_offset_secs: None,
    });
    let mut waiting = fixtures::tasks().remove(0);
    waiting.state = TileState::NeedsYou;
    live.apply_at(
        glass_api::Event::TaskUpdated {
            task: Box::new(waiting.clone()),
        },
        Instant::now(),
    );
    assert!(live.banners().is_empty(), "it waits in the menu bar only");
    assert_eq!(live.tasks().len(), 1, "and is still there to open");

    // Off again: the next change pops up as before.
    live.set_settings(SettingsView::default());
    let mut done = waiting;
    done.state = TileState::Done;
    live.apply_at(
        glass_api::Event::TaskUpdated {
            task: Box::new(done),
        },
        Instant::now(),
    );
    assert!(!live.banners().is_empty());

    let at = time::macros::datetime!(2026-10-19 14:05 UTC);
    assert_eq!(glass_shell::greeter::clock_lines(at, false).1, "2:05");
    assert_eq!(glass_shell::greeter::clock_lines(at, true).1, "14:05");
}

/// Typed into the open command bar, as its text field sends it.
fn type_query(live: &mut Live, query: &str) {
    live.update(Message::CommandChanged(query.into()));
}

fn fixture_app(id: &str) -> AppEntry {
    fixtures::apps()
        .into_iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("{id} isn't among the fixture's apps"))
}

#[test]
fn dk1_the_command_bar_opens_the_app_typed_with_enter_as_spotlight_does() {
    let mut live = Live::new(fixtures::tasks()).with_apps(fixtures::apps());
    live.update(Message::OpenCommandBar);
    // Nothing typed: no apps, and Enter is the text field's (the agent's).
    assert!(live.command_bar().unwrap().hits.is_empty());
    assert_eq!(press(&mut live, Named::Enter), None);
    type_query(&mut live, "ma");
    let bar = live.command_bar().unwrap();
    let names: Vec<&str> = bar.hits.iter().map(|h| h.app.name.as_str()).collect();
    assert_eq!(names, ["Mail", "My Mail"]);
    assert_eq!(bar.selected, Some(0), "the top hit is lit");
    assert_eq!(
        bar.hits[1].based_on,
        Some("Mail"),
        "the person's own version says whose"
    );
    // Enter opens it, and the bar closes.
    assert_eq!(
        press(&mut live, Named::Enter),
        Some(Some(Effect::Launch(fixture_app("mail"))))
    );
    assert!(live.command_bar().is_none());

    // Down chooses the next, round the end; Enter opens what's lit.
    live.update(Message::OpenCommandBar);
    type_query(&mut live, "ma");
    assert_eq!(press(&mut live, Named::ArrowDown), Some(None));
    assert_eq!(live.command_bar().unwrap().selected, Some(1));
    press(&mut live, Named::ArrowDown);
    assert_eq!(live.command_bar().unwrap().selected, Some(0));
    press(&mut live, Named::ArrowUp);
    assert_eq!(
        press(&mut live, Named::Enter),
        Some(Some(Effect::Launch(fixture_app("my-mail"))))
    );

    // Typing again lights the new top hit; the query keeps Left, Right,
    // Home and End for its cursor.
    live.update(Message::OpenCommandBar);
    type_query(&mut live, "act");
    assert_eq!(live.command_bar().unwrap().selected, Some(0));
    for key in [Named::ArrowLeft, Named::ArrowRight, Named::Home, Named::End] {
        assert_eq!(press(&mut live, key), None, "{key:?} stays the query's");
    }
    // What no app is called leaves the arrows to the query, and Enter to
    // the agent (`ae3_enter_hands_…`).
    type_query(&mut live, "prep me for my 2:00");
    let bar = live.command_bar().unwrap();
    assert!(bar.hits.is_empty() && bar.selected.is_none());
    assert_eq!(press(&mut live, Named::ArrowDown), None);
    assert_eq!(press(&mut live, Named::Escape), Some(None));
    assert!(live.command_bar().is_none());
}

#[test]
fn ae3_enter_hands_what_no_app_is_called_to_the_desktop_agent() {
    let mut live = Live::new(fixtures::tasks()).with_apps(fixtures::apps());
    live.update(Message::OpenCommandBar);
    // Only spaces: nothing to hand off.
    type_query(&mut live, "   ");
    assert_eq!(press(&mut live, Named::Enter), None);
    type_query(&mut live, " What's in my folder? ");
    let Some(Some(Effect::Start(request))) = press(&mut live, Named::Enter) else {
        panic!("Enter starts a task");
    };
    assert_eq!(request.agent.as_str(), "desktop-agent");
    assert_eq!(request.prompt, "What's in my folder?");
    assert!(
        request.scope.read.is_empty() && request.scope.write.is_empty(),
        "no folders unless the person grants them"
    );
    assert!(live.command_bar().is_none(), "the bar closes");
    // A second Enter before glassd answers starts nothing more.
    live.update(Message::OpenCommandBar);
    type_query(&mut live, "again");
    assert_eq!(live.update(Message::AskAgent), None);
    live.update(Message::CloseCommandBar);

    // glassd refuses: the bar comes back with the text and the reason.
    assert_eq!(
        live.start_failed("No AI model is approved for this.".into()),
        Some(Effect::Focus(glass_shell::screens::command_bar::INPUT))
    );
    let bar = live.command_bar().unwrap();
    assert_eq!(bar.query, "What's in my folder?");
    assert_eq!(bar.note, Some("No AI model is approved for this."));
    type_query(&mut live, "What's in my folder?!");
    assert_eq!(live.command_bar().unwrap().note, None, "typing clears it");

    // Asked again and started: the new tile opens.
    let Some(Some(Effect::Start(_))) = press(&mut live, Named::Enter) else {
        panic!("Enter starts a task");
    };
    let mut view = fixtures::tasks()[0].clone();
    view.id = glass_types::TaskId::new();
    let id = view.id;
    assert_eq!(live.started(view), Some(Effect::Get(id)));
    assert_eq!(live.open_task(), Some(id));
    assert!(live.tasks().iter().any(|t| t.id == id));
}

// The desktop dropped what it couldn't send while glassd was away. The
// command bar had already latched "asked", which only an answer cleared:
// Enter did nothing until sign-out, even with glassd back. An Approve took
// its banner away and the answer went nowhere.
#[test]
fn what_glassd_never_heard_is_undone_on_screen_and_the_menu_bar_says_its_away() {
    const WHY: &str = "Lost touch with the system. Try again in a moment.";
    let tasks = fixtures::tasks();
    let ask = tasks
        .iter()
        .find(|t| t.needs.is_some())
        .cloned()
        .expect("a needs-you tile");
    let approval = ask.needs.as_ref().unwrap().id;
    let mut live = Live::new(tasks).with_apps(fixtures::apps());
    assert!(!live.away());
    assert!(live.set_away(true), "that's a change to draw");
    assert!(!live.set_away(true), "and no change the second time");

    // A request for the desktop agent: back in the bar, with why.
    live.update(Message::OpenCommandBar);
    type_query(&mut live, "What's in my folder?");
    let Some(Some(start @ Effect::Start(_))) = press(&mut live, Named::Enter) else {
        panic!("Enter starts a task");
    };
    assert!(live.command_bar().is_none());
    assert_eq!(
        live.not_done(&start, WHY),
        Some(Effect::Focus(glass_shell::screens::command_bar::INPUT))
    );
    let bar = live.command_bar().unwrap();
    assert_eq!(
        (bar.query.as_str(), bar.note),
        ("What's in my folder?", Some(WHY))
    );
    // And Enter asks again: nothing is latched.
    assert!(matches!(
        press(&mut live, Named::Enter),
        Some(Some(Effect::Start(_)))
    ));
    live.update(Message::CloseCommandBar);

    // An ask approved: its banner went with the click, and comes back.
    assert!(live.banners().contains(&ask.id));
    let Some(answer @ Effect::Answer(_)) = live.update(Message::Approve(approval)) else {
        panic!("Approve answers the ask");
    };
    assert!(!live.banners().contains(&ask.id));
    assert_eq!(live.not_done(&answer, WHY), None);
    assert!(
        live.banners().contains(&ask.id),
        "still waiting for an answer"
    );

    // What only changes when glassd says so needs nothing undone.
    assert_eq!(live.not_done(&Effect::Pause(ask.id), WHY), None);
    assert_eq!(live.not_done(&Effect::LoadSettings, WHY), None);

    // The menu bar says so, and stops when glassd is back: the two frames
    // differ in the menu bar, left of the agents, and nowhere else.
    let theme = Theme::glass();
    let drawn = |live: &Live| {
        glass_shell::render::frame(
            live.view(&theme, None, artboard_time(), SIZE),
            Size::new(SIZE.0, SIZE.1),
            1.0,
        )
    };
    let away = drawn(&live);
    assert!(live.set_away(false));
    let back = drawn(&live);
    let bar = glass_shell::screens::desktop::MENU_BAR_HEIGHT as u32;
    let differs: Vec<(u32, u32)> = (0..away.height)
        .flat_map(|y| (0..away.width).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let at = ((y * away.width + x) * 4) as usize;
            away.rgba[at..at + 4] != back.rgba[at..at + 4]
        })
        .collect();
    assert!(!differs.is_empty(), "the menu bar says it's reconnecting");
    assert!(
        differs.iter().all(|&(x, y)| y < bar && x > away.width / 2),
        "and only there: {:?}",
        differs
            .iter()
            .find(|&&(x, y)| y >= bar || x <= away.width / 2)
    );
}

#[test]
fn a_request_whose_answer_was_lost_doesnt_hold_the_command_bar_after_glassd_is_back() {
    let mut live = Live::new(fixtures::tasks()).with_apps(fixtures::apps());
    live.update(Message::OpenCommandBar);
    type_query(&mut live, "Tidy my downloads");
    assert!(matches!(
        press(&mut live, Named::Enter),
        Some(Some(Effect::Start(_)))
    ));
    // No answer ever comes; glassd restarts and the desktop reloads.
    live.reload(fixtures::tasks());
    live.update(Message::OpenCommandBar);
    type_query(&mut live, "Tidy my downloads");
    assert!(
        matches!(press(&mut live, Named::Enter), Some(Some(Effect::Start(_)))),
        "Enter starts a task again"
    );
}

// With the sign-in screen gone (its end of the pipe closed) the lock screen
// said "Checking…" and took no more keys, for good.
#[test]
fn a_lock_screen_whose_password_nobody_checks_takes_it_again() {
    let mut live = Live::new(fixtures::tasks())
        .signed_in_as("Dana Ruiz")
        .lockable();
    assert_eq!(live.unlock_unanswered(), None, "not locked: nothing to do");
    live.update(Message::LockScreen);
    assert_eq!(live.unlock_unanswered(), None, "nothing being checked");
    live.update(Message::PasswordChanged("correct horse".into()));
    assert!(matches!(
        live.update(Message::SignIn),
        Some(Effect::Unlock(_))
    ));
    assert!(live.checking_password());
    // While it's with the sign-in screen, nothing more is taken.
    live.update(Message::PasswordChanged("more".into()));
    assert_eq!(live.update(Message::SignIn), None);

    assert_eq!(
        live.unlock_unanswered(),
        Some(Effect::Focus(glass_shell::screens::login::PASSWORD))
    );
    assert!(!live.checking_password());
    assert!(live.is_locked(), "still locked");
    // And the next try goes to the sign-in screen again.
    live.update(Message::PasswordChanged("correct horse".into()));
    assert!(matches!(
        live.update(Message::SignIn),
        Some(Effect::Unlock(_))
    ));
}

// Every task glassd still had was a row of the command bar's Recent, laid
// out again at each key typed.
#[test]
fn the_command_bars_recent_is_the_newest_few_however_many_tasks_there_are() {
    use glass_shell::screens::command_bar::RECENT_MOST;
    let theme = Theme::glass();
    // Finished, so the menu bar and the banners are the same for any number.
    let mut done = fixtures::tasks().remove(0);
    done.state = glass_api::TileState::Done;
    done.needs = None;
    let many: Vec<_> = (0..200)
        .map(|n| {
            let mut task = done.clone();
            task.id = glass_types::TaskId::new();
            task.title = format!("Task {n}");
            task
        })
        .collect();
    let drawn = |tasks: &[glass_api::TaskView]| {
        let mut live = Live::new(tasks.to_vec()).with_apps(fixtures::apps());
        live.update(Message::OpenCommandBar);
        let bar = live.command_bar();
        glass_shell::render::frame(
            live.view(&theme, bar.as_ref(), artboard_time(), SIZE),
            Size::new(SIZE.0, SIZE.1),
            1.0,
        )
        .rgba
    };
    let all = drawn(&many);
    assert!(
        all == drawn(&many[many.len() - RECENT_MOST..]),
        "two hundred tasks draw as their newest {RECENT_MOST} do"
    );
    assert!(
        all != drawn(&many[..RECENT_MOST]),
        "and not as the oldest do"
    );
}

#[test]
fn dk1_an_app_the_command_bar_found_opens_with_a_click() {
    let theme = Theme::glass();
    let mut live = Live::new(fixtures::tasks()).with_apps(fixtures::apps());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    live.update(Message::OpenCommandBar);
    type_query(&mut live, "dis");
    {
        let bar = live.command_bar();
        let view = live.view(&theme, bar.as_ref(), artboard_time(), SIZE);
        screen.update(view, &[], None);
    }
    // The bar sits at (240, 150); its first app is on the left, under the
    // query and the "Apps" label.
    let clicked = (250..400)
        .step_by(10)
        .flat_map(|y| click(&mut screen, &live, Point::new(500.0, y as f32)))
        .find(|m| matches!(m, Message::Launch(_)));
    assert_eq!(clicked, Some(Message::Launch("dispute-tracker".into())));
    assert_eq!(
        live.update(clicked.unwrap()),
        Some(Effect::Launch(fixture_app("dispute-tracker")))
    );
    assert!(
        live.command_bar().is_none(),
        "the bar closes as the app opens"
    );
}

#[test]
fn dk1_an_app_hidden_from_search_leaves_the_results_and_is_kept_as_the_persons_setting() {
    let theme = Theme::glass();
    let mut live = Live::new(fixtures::tasks()).with_apps(fixtures::apps());
    let mut screen = Screen::new(Size::new(SIZE.0, SIZE.1), 1.0);
    live.update(Message::OpenCommandBar);
    type_query(&mut live, "ma");
    {
        let bar = live.command_bar();
        let view = live.view(&theme, bar.as_ref(), artboard_time(), SIZE);
        screen.update(view, &[], None);
    }
    // The second row's Hide, at the right end of the bar's left column.
    let hide = (250..420)
        .step_by(6)
        .flat_map(|y| (640..840).step_by(12).map(move |x| (x, y)))
        .flat_map(|(x, y)| click(&mut screen, &live, Point::new(x as f32, y as f32)))
        .find(|m| matches!(m, Message::HideFromSearch(id) if id == "my-mail"));
    assert_eq!(hide, Some(Message::HideFromSearch("my-mail".into())));
    let Some(Effect::ChooseSetting(choice)) = live.update(hide.unwrap()) else {
        panic!("hiding is kept by glassd");
    };
    assert_eq!(choice.id, "search.hidden_apps");
    assert_eq!(choice.value, Some(serde_json::json!(["my-mail"])));
    // Gone at once; Mail stays and is lit.
    let bar = live.command_bar().unwrap();
    let names: Vec<&str> = bar.hits.iter().map(|h| h.app.name.as_str()).collect();
    assert_eq!(names, ["Mail"]);
    assert_eq!(bar.selected, Some(0));
    // Hiding it again changes nothing.
    assert_eq!(live.update(Message::HideFromSearch("my-mail".into())), None);
    // Shown again in Settings: glassd's settings come back without it.
    live.set_settings(glass_api::settings::SettingsView::default());
    let bar = live.command_bar().unwrap();
    assert_eq!(bar.hits.len(), 2, "My Mail is back");
}
