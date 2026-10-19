//! Apps' windows sit between the wallpaper and the shell's UI (UI-1): the
//! UI covers them (menu bar, menus, the lock screen), they cover the
//! wallpaper, and hiding them shows the wallpaper again.

#![allow(clippy::unwrap_used)]

use glass_shell::render::{Damage, Screen};
use glass_shell::{Element, Message, Size};
use iced_core::{Color, Length};
use iced_widget::{container, row, space};

const W: u32 = 200;
const H: u32 = 100;
/// B, G, R, A.
const WALLPAPER: [u8; 4] = [0x10, 0x20, 0x30, 0xFF];
const RED: [u8; 4] = [0, 0, 0xFF, 0xFF];
const GREEN: [u8; 4] = [0, 0xFF, 0, 0xFF];
const WHITE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

/// A white box at x 50..150, y 0..50: the shell's UI.
fn ui<'a>() -> Element<'a, Message> {
    row![
        space().width(Length::Fixed(50.0)),
        container(space())
            .width(Length::Fixed(100.0))
            .height(Length::Fixed(50.0))
            .style(|_| container::Style {
                background: Some(Color::WHITE.into()),
                ..container::Style::default()
            }),
    ]
    .into()
}

/// A window filling x 0..100, the full height, in `color`.
fn windows(color: [u8; 4]) -> Vec<u8> {
    let mut pixels = vec![0; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..100 {
            let at = ((y * W + x) * 4) as usize;
            pixels[at..at + 4].copy_from_slice(&color);
        }
    }
    pixels
}

fn pixel(screen: &Screen, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * W + x) * 4) as usize;
    screen.bgra()[at..at + 4].try_into().unwrap()
}

#[test]
fn ui1_windows_sit_between_the_wallpaper_and_the_ui() {
    let mut screen = Screen::new(Size::new(W as f32, H as f32), 1.0);
    screen.set_backdrop(WALLPAPER.repeat((W * H) as usize));
    screen.render_over(ui(), None, Some(&windows(RED)));

    assert_eq!(pixel(&screen, 25, 75), RED, "the window over the wallpaper");
    assert_eq!(pixel(&screen, 75, 25), WHITE, "the UI over the window");
    assert_eq!(pixel(&screen, 125, 25), WHITE, "the UI over the wallpaper");
    assert_eq!(
        pixel(&screen, 175, 75),
        WALLPAPER,
        "nothing over the wallpaper"
    );
    assert!(screen.ui_at(75, 25) && !screen.ui_at(25, 75));

    // Only the window changes: composed again where it is, the UI on top.
    let window = [Damage {
        x: 0,
        y: 0,
        width: 100,
        height: 100,
    }];
    screen.compose(&window, Some(&windows(GREEN)));
    assert_eq!(pixel(&screen, 25, 75), GREEN);
    assert_eq!(pixel(&screen, 75, 25), WHITE);

    // Hidden: the wallpaper where it was.
    screen.compose(&window, None);
    assert_eq!(pixel(&screen, 25, 75), WALLPAPER);
    assert_eq!(pixel(&screen, 75, 25), WHITE);
}

#[test]
fn ui1_hiding_windows_leaves_none_of_their_pixels_even_where_the_ui_is_unchanged() {
    let mut screen = Screen::new(Size::new(W as f32, H as f32), 1.0);
    screen.set_backdrop(WALLPAPER.repeat((W * H) as usize));
    screen.render_over(ui(), None, Some(&windows(RED)));
    assert_eq!(pixel(&screen, 25, 75), RED);
    // The same UI (the lock screen changes only part of it), windows hidden:
    // nothing of the window anywhere, though no UI changed over it.
    let damage = screen.render_over(ui(), None, None);
    assert_eq!(damage.len(), 1, "all of the screen: {damage:?}");
    for (x, y) in [(25, 75), (5, 5), (99, 99)] {
        assert_eq!(pixel(&screen, x, y), WALLPAPER, "at {x},{y}");
    }
    // And back.
    screen.render_over(ui(), None, Some(&windows(RED)));
    assert_eq!(pixel(&screen, 25, 75), RED);
}

#[test]
fn dk1_a_title_bar_has_the_controls_on_the_left_and_round_top_corners() {
    use glass_shell::screens::window::{Controls, title_bar};
    let controls = Controls {
        first: 16.0,
        step: 20.0,
        radius: 6.0,
    };
    let theme = glass_shell::Theme::glass();
    let size = Size::new(300.0, 28.0);
    let draw = |focused| {
        glass_shell::render::layer(
            title_bar(&theme, "Notes", focused, (300.0, 28.0), controls),
            size,
        )
    };
    let at = |bar: &[u8], x: usize, y: usize| -> [u8; 4] {
        bar[(y * 300 + x) * 4..(y * 300 + x) * 4 + 4]
            .try_into()
            .unwrap()
    };
    let focused = draw(true);
    // Close, minimize and zoom where the compositor hit-tests them.
    assert_eq!(
        at(&focused, 16, 14),
        [0x57, 0x5F, 0xFF, 0xFF],
        "close is red"
    );
    assert_eq!(at(&focused, 36, 14), [0x2E, 0xBC, 0xFE, 0xFF], "minimize");
    assert_eq!(at(&focused, 56, 14), [0x40, 0xC8, 0x28, 0xFF], "zoom");
    // Round top corners show what's behind; the bar itself is opaque.
    assert_eq!(at(&focused, 0, 0)[3], 0);
    assert_eq!(at(&focused, 150, 27)[3], 0xFF);
    // Without the keyboard the controls are grey.
    let unfocused = draw(false);
    assert_ne!(at(&unfocused, 16, 14), at(&focused, 16, 14));
}
