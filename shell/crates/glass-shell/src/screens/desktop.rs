//! The desktop (DK-1, the Desktop artboard): a menu bar across the top, a
//! dock at the bottom, windows with their controls on the left, and the
//! agent tiles stacked at the top right, needs-you first (AW-1).

use glass_api::{TaskView, TileState};
use glass_types::Timestamp;
use iced_core::alignment::{Horizontal, Vertical};
use iced_core::{Color, Length, Padding};
use iced_widget::text::Wrapping;
use iced_widget::{
    Column, Row, button, column, container, image, mouse_area, pin, row, scrollable, stack, text,
};
use time::OffsetDateTime;
use time::macros::format_description;

use crate::theme::{Surface, hex, hexa};
use crate::{Element, Message, Power, Theme, fonts, icons, style, tiles, wallpaper};

/// What the desktop shows, besides windows.
#[derive(Debug, Clone, PartialEq)]
pub struct Desktop<'a> {
    /// From `task.list`.
    pub tasks: &'a [TaskView],
    pub now: Timestamp,
    /// The menu bar clock, in the user's locale ("Mon Oct 19  2:14 PM").
    pub clock: String,
    /// The focused app, bold in the menu bar, and its menus.
    pub app: String,
    pub menus: Vec<String>,
    /// The calendar's dock icon: month and day.
    pub date: (String, u8),
    /// Which agent tiles show over the desktop.
    pub tiles: Tiles<'a>,
    /// False leaves the wallpaper out, for a screen that keeps it as its
    /// backdrop (`render::Screen::set_backdrop`).
    pub wallpaper: bool,
    /// The menu under the mark at the left of the menu bar, when it's open:
    /// the name of the person signed in, for "Sign out <name>".
    pub system_menu: Option<String>,
    /// Whether the system menu's Lock Screen can be chosen: the sign-in
    /// screen started this session and can check the password to unlock it.
    pub lockable: bool,
    /// Whether the Trash at the right end of the dock has anything in it.
    pub trash_full: bool,
}

/// Which agent tiles the desktop shows. They don't stay on screen: a change
/// worth knowing about shows as a banner for a few seconds, like a macOS
/// notification, and every agent is a click away in the agents menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Tiles<'a> {
    /// None: the screen is the person's.
    Hidden,
    /// Notifications: these tiles at the top right, needs-you first (AW-1).
    Banners(Vec<&'a TaskView>),
    /// The agents menu, opened from the menu bar's agents pill: every agent,
    /// needs-you first, each opening its expanded view.
    Menu,
}

impl<'a> Desktop<'a> {
    /// The desktop at `now`, in the user's local time: the menu bar clock
    /// and the calendar's dock icon follow it. With no app open the menu
    /// bar's left side is just the mark: the desktop isn't an app, and the
    /// mark's menu is where things start.
    pub fn live(tasks: &'a [TaskView], now: OffsetDateTime) -> Self {
        Desktop {
            tasks,
            now: Timestamp::from_unix_seconds(now.unix_timestamp()).unwrap_or_else(Timestamp::now),
            clock: clock(now),
            app: String::new(),
            menus: Vec::new(),
            date: (
                clock_part(now, format_description!("[month repr:short]")).to_uppercase(),
                now.day(),
            ),
            tiles: Tiles::Hidden,
            wallpaper: true,
            system_menu: None,
            lockable: false,
            trash_full: false,
        }
    }
}

/// The menu bar clock: "Mon Oct 19  2:14 PM".
pub fn clock(at: OffsetDateTime) -> String {
    clock_part(
        at,
        format_description!(
            "[weekday repr:short] [month repr:short] [day padding:none]  [hour repr:12 padding:none]:[minute] [period]"
        ),
    )
}

fn clock_part(
    at: OffsetDateTime,
    format: &[time::format_description::BorrowedFormatItem<'_>],
) -> String {
    at.format(format).unwrap_or_default()
}

/// An app window: the shell draws its frame (controls on the left, the
/// title centered); the app draws the content.
pub struct Window<'a> {
    pub title: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub content: Element<'a, Message>,
}

/// Room at the top right for the tile stack.
const TILES_TOP: f32 = 48.0;
const TILES_RIGHT: f32 = 24.0;

/// The desktop. `overlay` (the command bar) goes over windows and tiles,
/// under the menu bar and dock.
pub fn view<'a>(
    theme: &Theme,
    desktop: &Desktop<'a>,
    windows: Vec<Window<'a>>,
    thumbnail: impl Fn(&'a TaskView) -> Option<Element<'a, Message>>,
    overlay: Option<Element<'a, Message>>,
    size: (f32, f32),
) -> Element<'a, Message> {
    let mut layers: Vec<Element<'a, Message>> = Vec::new();
    if desktop.wallpaper {
        layers.push(
            image(wallpaper::handle(&theme.wallpaper, size.0, size.1))
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced_core::ContentFit::Fill)
                .into(),
        );
    }
    for window in windows {
        layers.push(
            pin(frame(
                theme,
                window.title,
                window.content,
                window.width,
                window.height,
            ))
            .x(window.x)
            .y(window.y)
            .into(),
        );
    }
    // The same layers every frame, filled or not: frames are compared layer
    // by layer (`render::Screen`), and one missing would shift the full-screen
    // dock layer onto another and redraw everything when a banner goes.
    let nothing = || iced_widget::space().into();
    let (behind_tiles, tiles): (Element<'a, Message>, Element<'a, Message>) = match &desktop.tiles {
        // A click anywhere else closes a menu, as a menu does.
        _ if desktop.system_menu.is_some() => (
            mouse_area(
                iced_widget::space()
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .on_press(Message::ToggleSystemMenu)
            .into(),
            pin(system_menu(
                theme,
                desktop.system_menu.as_deref().unwrap_or_default(),
                desktop.lockable,
            ))
            .x(SYSTEM_MENU_LEFT)
            .y(MENU_BAR_HEIGHT + 2.0)
            .into(),
        ),
        Tiles::Hidden => (nothing(), nothing()),
        Tiles::Banners(shown) => {
            let mut shown = shown.clone();
            shown.sort_by_key(|t| (t.state != TileState::NeedsYou, t.started_at.unix_seconds()));
            let banners = shown
                .into_iter()
                .map(|task| tiles::view(theme, task, desktop.now, thumbnail(task)));
            let banners = pin(mouse_area(Column::with_children(banners).spacing(12))
                .on_enter(Message::HoldBanners)
                .on_exit(Message::LetBannersGo))
            .x(size.0 - TILES_RIGHT - tiles::WIDTH)
            .y(TILES_TOP);
            (nothing(), banners.into())
        }
        Tiles::Menu => (
            mouse_area(
                iced_widget::space()
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .on_press(Message::CloseAgents)
            .into(),
            pin(agents_menu(theme, desktop, &thumbnail, size.1))
                .x(size.0 - TILES_RIGHT - tiles::WIDTH - MENU_PADDING)
                .y(TILES_TOP - MENU_PADDING)
                .into(),
        ),
    };
    layers.push(behind_tiles);
    layers.push(tiles);
    layers.push(overlay.unwrap_or_else(nothing));
    layers.push(menu_bar(theme, desktop));
    layers.push(
        container(dock(theme, desktop))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Bottom)
            .padding(Padding {
                bottom: 10.0,
                ..Padding::ZERO
            })
            .into(),
    );
    iced_widget::Stack::with_children(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Needs-you tiles first, then the rest oldest first, so the newest sits at
/// the bottom (AW-1). Ended tiles that were dismissed aren't in the list.
pub fn ordered(tasks: &[TaskView]) -> Vec<&TaskView> {
    let mut ordered: Vec<&TaskView> = tasks.iter().collect();
    ordered.sort_by_key(|t| (t.state != TileState::NeedsYou, t.started_at.unix_seconds()));
    ordered
}

/// Room around the tiles inside the agents menu.
const MENU_PADDING: f32 = 12.0;
/// Room the menu leaves for the dock.
pub const DOCK_ROOM: f32 = 96.0;

/// The agents menu: every agent's tile, needs-you first, in a panel that
/// scrolls when they don't fit.
fn agents_menu<'a>(
    theme: &Theme,
    desktop: &Desktop<'a>,
    thumbnail: &impl Fn(&'a TaskView) -> Option<Element<'a, Message>>,
    height: f32,
) -> Element<'a, Message> {
    let live = desktop.tasks.iter().filter(|t| t.state.is_live()).count();
    let waiting = desktop
        .tasks
        .iter()
        .filter(|t| t.state == TileState::NeedsYou)
        .count();
    let summary = match (live, waiting) {
        (0, _) => "None running".to_owned(),
        (n, 0) => format!("{n} running"),
        (n, w) => format!(
            "{n} running · {w} need{} you",
            if w == 1 { "s" } else { "" }
        ),
    };
    let header = row![
        text("Agents")
            .size(13)
            .font(fonts::SEMIBOLD)
            .color(hex(0xF3F5F7)),
        iced_widget::space().width(Length::Fill),
        text(summary).size(12).color(hex(0xA9B1BC)),
    ]
    .align_y(Vertical::Center);
    let body: Element<'a, Message> = if desktop.tasks.is_empty() {
        text("No agents yet. Press Ctrl Space to hand one a task.")
            .size(13)
            .color(hex(0xC9D1DA))
            .into()
    } else {
        let tiles = ordered(desktop.tasks)
            .into_iter()
            .map(|task| tiles::view(theme, task, desktop.now, thumbnail(task)));
        scrollable(Column::with_children(tiles).spacing(10))
            .height(Length::Shrink)
            .into()
    };
    container(column![header, body].spacing(10))
        .padding(MENU_PADDING)
        .width(Length::Fixed(tiles::WIDTH + MENU_PADDING * 2.0))
        .max_height(height - TILES_TOP - DOCK_ROOM)
        .style(|_| container::Style {
            shadow: style::shadow(0.35, 18.0, 40.0),
            ..style::outlined(hex(0x15181D), 18.0, 1.0, hex(0x262B32))
        })
        .into()
}

fn menu_bar<'a>(theme: &Theme, desktop: &Desktop<'a>) -> Element<'a, Message> {
    let bar = theme.menu_bar;
    let label = |s: String, font| text(s).size(13).font(font).color(bar.text);
    let mut menus = Row::new().spacing(20).align_y(Vertical::Center);
    if !desktop.app.is_empty() {
        menus = menus.push(label(desktop.app.clone(), fonts::SEMIBOLD));
    }
    for menu in &desktop.menus {
        menus = menus.push(label(menu.clone(), fonts::SANS));
    }
    // The mark opens the system menu; its highlight reaches past the mark,
    // which stays where it was drawn before it was a button.
    let open = desktop.system_menu.is_some();
    let mark = button(
        container(icons::view(&icons::MARK_SMALL, bar.text, 16.0, 16.0))
            .center_x(Length::Fixed(MARK_WIDTH))
            .center_y(Length::Fixed(22.0)),
    )
    .on_press(Message::ToggleSystemMenu)
    .padding(0)
    .style(style::button_like(style::panel(
        if open { bar.fill } else { Color::TRANSPARENT },
        6.0,
    )));
    let left = row![mark, menus]
        .spacing(20.0 - MARK_REACH)
        .align_y(Vertical::Center);
    let right = row![
        agents_pill(theme, desktop.tasks, desktop.tiles == Tiles::Menu),
        icons::view(&icons::SPARK, bar.text, 16.0, 16.0),
        icons::view(&icons::WIFI, bar.text, 17.0, 17.0),
        icons::view(&icons::BATTERY, bar.text, 24.0, 13.0),
        label(desktop.clock.clone(), fonts::SANS),
    ]
    .spacing(16)
    .align_y(Vertical::Center);
    container(row![left, iced_widget::space().width(Length::Fill), right].align_y(Vertical::Center))
        .width(Length::Fill)
        .height(Length::Fixed(MENU_BAR_HEIGHT))
        .padding(Padding {
            left: 16.0 - MARK_REACH,
            right: 16.0,
            ..Padding::ZERO
        })
        .center_y(Length::Fixed(MENU_BAR_HEIGHT))
        .style(move |_| style::panel(bar.background, 0.0))
        .into()
}

pub const MENU_BAR_HEIGHT: f32 = 32.0;
/// How far the mark's highlight reaches either side of it.
const MARK_REACH: f32 = 6.0;
const MARK_WIDTH: f32 = 16.0 + MARK_REACH * 2.0;
const SYSTEM_MENU_LEFT: f32 = 6.0;

/// The system menu, under the mark, as a Mac's Apple menu: About
/// Glasshouse and Check for Updates (there when updates ship); what can be
/// started from anywhere (an agent's task, for now; apps join it when they
/// have windows); Force Quit; Restart and Shut Down, each asked first;
/// signing out (Shift Super Q, as Shift Command Q on a Mac). Sleep, locking
/// the screen and switching users wait for the compositor (ADR 0019).
fn system_menu<'a>(theme: &Theme, person: &str, lockable: bool) -> Element<'a, Message> {
    let sign_out = if person.is_empty() {
        "Sign out…".to_owned()
    } else {
        format!("Sign out {person}…")
    };
    // A line without a message is shown but can't be chosen yet.
    let sections: [&[(String, &'static str, Option<Message>)]; 5] = [
        &[
            ("About Glasshouse".to_owned(), "", Some(Message::OpenAbout)),
            // Updates arrive as whole images (BS-4, EP-1), which don't
            // ship yet.
            ("Check for Updates…".to_owned(), "", None),
        ],
        &[(
            "Hand an agent a task…".to_owned(),
            "Ctrl Space",
            Some(Message::OpenCommandBar),
        )],
        &[(
            "Force Quit…".to_owned(),
            "Alt Super Esc",
            Some(Message::OpenForceQuit),
        )],
        &[
            (
                "Restart…".to_owned(),
                "",
                Some(Message::AskPower(Power::Restart)),
            ),
            (
                "Shut Down…".to_owned(),
                "",
                Some(Message::AskPower(Power::ShutDown)),
            ),
        ],
        // Paired as on a Mac: Lock Screen (Control Command Q) over Log Out.
        &[
            (
                "Lock Screen".to_owned(),
                "Ctrl Super Q",
                lockable.then_some(Message::LockScreen),
            ),
            (sign_out, "Shift Super Q", Some(Message::SignOut)),
        ],
    ];
    // As wide as its longest line, as a menu is. Every line fills that
    // width so the highlight does; the width itself is reckoned from the
    // text, since a filling line would otherwise take the whole screen.
    let width = sections
        .iter()
        .flat_map(|section| section.iter())
        .map(|(label, shortcut, _)| {
            label.chars().count() as f32 * 7.2 + shortcut.chars().count() as f32 * 6.6
        })
        .fold(0.0, f32::max)
        + 32.0
        + 20.0;
    let mut items = Column::new()
        .spacing(4)
        .width(Length::Fixed(width.max(240.0)));
    for (n, section) in sections.iter().enumerate() {
        if n > 0 {
            items = items.push(
                container("")
                    .width(Length::Fill)
                    .height(Length::Fixed(1.0))
                    .style(|_| style::panel(hexa(0xFFFFFF, 0.1), 0.0)),
            );
        }
        for (label, shortcut, message) in section.iter() {
            items = items.push(menu_item(theme, label.clone(), shortcut, message.clone()));
        }
    }
    container(items)
        .padding(5)
        .style(|_| container::Style {
            shadow: style::shadow(0.35, 12.0, 30.0),
            ..style::outlined(hex(0x15181D), 10.0, 1.0, hex(0x262B32))
        })
        .into()
}

/// One line of a menu: what it does, its shortcut at the right.
fn menu_item<'a>(
    theme: &Theme,
    label: String,
    shortcut: &'static str,
    message: Option<Message>,
) -> Element<'a, Message> {
    button(
        row![
            text(label).size(13).wrapping(Wrapping::None),
            iced_widget::space().width(Length::Fill),
            // Translucent, so it reads on the menu and on the highlight.
            text(shortcut)
                .size(12)
                .color(hexa(0xFFFFFF, 0.6))
                .wrapping(Wrapping::None),
        ]
        .spacing(32)
        .align_y(Vertical::Center),
    )
    .on_press_maybe(message)
    .width(Length::Fill)
    .padding(Padding::from([5.0, 10.0]))
    .style(style::menu_item(hex(0xF3F5F7), theme.accent))
    .into()
}

/// "● ● ● 3 agents": one dot per agent, orange for any that need you. A
/// click opens the agents menu.
fn agents_pill<'a>(theme: &Theme, tasks: &[TaskView], open: bool) -> Element<'a, Message> {
    let count = tasks.iter().filter(|t| t.state.is_live()).count();
    let mut dots = Row::new().spacing(6).align_y(Vertical::Center);
    for color in status_dots(theme, tasks) {
        dots = dots.push(dot(color, 7.0));
    }
    let bar = theme.menu_bar;
    // Highlighted while its menu is open, as a menu bar item is.
    let fill = if open {
        Color {
            a: (bar.fill.a + 0.25).min(1.0),
            ..bar.fill
        }
    } else {
        bar.fill
    };
    button(
        container(
            row![dots, text(pill_label(count)).size(13).color(bar.text)]
                .spacing(6)
                .align_y(Vertical::Center),
        )
        .padding(Padding::from([0.0, 9.0]))
        .center_y(Length::Fixed(22.0)),
    )
    .on_press(Message::ToggleAgents)
    .padding(0)
    .style(style::button_like(style::panel(fill, 11.0)))
    .into()
}

/// The pill counts the agents still at work; ended ones stay in the agents
/// menu until dismissed, so none running isn't "0 agents".
fn pill_label(running: usize) -> String {
    match running {
        0 => "No agents running".to_owned(),
        1 => "1 agent".to_owned(),
        n => format!("{n} agents"),
    }
}

/// Up to three dots for the live agents, the ones that need you last so
/// the orange one stands at the end.
fn status_dots(theme: &Theme, tasks: &[TaskView]) -> Vec<Color> {
    let mut live: Vec<&TaskView> = tasks.iter().filter(|t| t.state.is_live()).collect();
    live.sort_by_key(|t| (t.state == TileState::NeedsYou, t.started_at.unix_seconds()));
    let skip = live.len().saturating_sub(3);
    live.into_iter()
        .skip(skip)
        .map(|t| tiles::state_color(theme, t.state))
        .collect()
}

fn dot<'a>(color: Color, size: f32) -> Element<'a, Message> {
    container("")
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_| style::dot(color, size))
        .into()
}

/// The window frame: title bar with the controls on the left (DK-1).
fn frame<'a>(
    theme: &Theme,
    title: String,
    content: Element<'a, Message>,
    width: f32,
    height: f32,
) -> Element<'a, Message> {
    let w = theme.window;
    let controls = row![
        dot(w.controls, 12.0),
        dot(w.controls, 12.0),
        dot(w.controls, 12.0)
    ]
    .spacing(8);
    let title_bar = container(
        stack![
            container(controls)
                .height(Length::Fill)
                .padding(Padding::from([0.0, 14.0]))
                .align_y(Vertical::Center),
            container(text(title).size(13).font(fonts::MEDIUM).color(w.text))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fixed(44.0))
    .style(move |_| style::panel(w.title_bar, 0.0));
    let divider = container("")
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(move |_| style::panel(w.divider, 0.0));
    container(column![title_bar, divider, content])
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .clip(true)
        .style(move |_| iced_widget::container::Style {
            text_color: Some(w.text),
            shadow: style::shadow(0.12, 12.0, 32.0),
            ..style::outlined(w.background, 12.0, 1.0, w.border)
        })
        .into()
}

/// The dock: apps, then the agents button with a badge for asks waiting.
fn dock<'a>(theme: &Theme, desktop: &Desktop<'a>) -> Element<'a, Message> {
    let d: Surface = theme.dock;
    let app = |fill: Color, content: Element<'a, Message>| -> Element<'a, Message> {
        container(content)
            .center(Length::Fixed(52.0))
            .style(move |_| style::panel(fill, 14.0))
            .into()
    };
    let glyph = |icon: &icons::Icon, color: Color, s: f32| icons::view(icon, color, s, s);
    let white = hex(0xFFFFFF);
    let (month, day) = (&desktop.date.0, desktop.date.1);
    let calendar = column![
        text(month.clone())
            .size(9)
            .font(fonts::SEMIBOLD)
            .color(hex(0xC53030)),
        text(day.to_string())
            .size(22)
            .font(fonts::MEDIUM)
            .color(hex(0x14171C))
            .line_height(1.0),
    ]
    .align_x(Horizontal::Center);
    // Every item keeps a slot under it for the running indicator, so open
    // and closed apps line up.
    let slot = |tile: Element<'a, Message>, running: bool| -> Element<'a, Message> {
        let indicator = if running {
            hex(0x14171C)
        } else {
            Color::TRANSPARENT
        };
        column![tile, dot(indicator, 4.0)]
            .spacing(4)
            .align_x(Horizontal::Center)
            .into()
    };
    let needs = desktop
        .tasks
        .iter()
        .filter(|t| t.state == TileState::NeedsYou)
        .count();
    let mut agent_dots = Row::new().spacing(4).align_y(Vertical::Center);
    let mut colors = status_dots(theme, desktop.tasks);
    if colors.is_empty() {
        // No agent running: the icon's three dots, dimmed, not an empty tile.
        colors = vec![hexa(0xFFFFFF, 0.28); 3];
    }
    for color in colors {
        agent_dots = agent_dots.push(dot(color, 7.0));
    }
    let agents = app(hex(0x15181D), agent_dots.into());
    let agents: Element<'a, Message> = if needs > 0 {
        let attention = theme.attention;
        // The badge counts the asks waiting; it overlaps the corner.
        stack![
            container(agents).padding(Padding {
                right: 6.0,
                ..Padding::ZERO
            }),
            container(
                container(
                    text(needs.to_string())
                        .size(12)
                        .font(fonts::SEMIBOLD)
                        .color(hex(0x1A1206))
                )
                .center(Length::Fixed(20.0))
                .style(move |_| style::panel(attention, 10.0)),
            )
            .width(Length::Fixed(58.0))
            .align_x(Horizontal::Right),
        ]
        .into()
    } else {
        agents
    };
    // Clicking it opens the agents menu, as the menu bar's pill does.
    let agents: Element<'a, Message> = button(agents)
        .on_press(Message::ToggleAgents)
        .padding(0)
        .style(style::button_like(style::panel(Color::TRANSPARENT, 0.0)))
        .into();
    let separator = container(
        container("")
            .width(Length::Fixed(1.0))
            .height(Length::Fixed(44.0))
            .style(move |_| style::panel(d.fill, 0.0)),
    )
    .center_y(Length::Fixed(52.0));
    // Files always first and the Trash always last, as a Mac's Finder and
    // Trash; the apps between are the person's to arrange (once apps have
    // windows), and the agents sit with the Trash past the separator.
    let trash = if desktop.trash_full {
        &icons::TRASH_FULL
    } else {
        &icons::TRASH
    };
    let items = row![
        slot(
            app(hex(0x2F7BEA), glyph(&icons::FOLDER, white, 26.0)),
            false
        ),
        slot(
            button(app(
                hex(0xE9ECEF),
                glyph(&icons::LAUNCHER, hex(0x5F6875), 24.0)
            ))
            .on_press(Message::ToggleLauncher)
            .padding(0)
            .style(style::button_like(style::panel(Color::TRANSPARENT, 0.0)))
            .into(),
            false
        ),
        slot(app(hex(0x1E9BF0), glyph(&icons::MAIL, white, 26.0)), false),
        slot(app(white, calendar.into()), false),
        slot(app(hex(0x0E8A8A), glyph(&icons::GLOBE, white, 26.0)), true),
        slot(app(hex(0x6E56E8), glyph(&icons::BAG, white, 26.0)), false),
        slot(app(hex(0x5F6875), glyph(&icons::GEAR, white, 26.0)), false),
        separator,
        agents,
        slot(
            app(hexa(0xFFFFFF, 0.55), glyph(trash, hex(0x5F6875), 26.0)),
            false
        ),
    ]
    .spacing(10)
    .align_y(Vertical::Top);
    container(items)
        .padding(Padding {
            top: 10.0,
            right: 10.0,
            bottom: 0.0,
            left: 10.0,
        })
        .height(Length::Fixed(72.0))
        .style(move |_| iced_widget::container::Style {
            shadow: style::shadow(0.25, 10.0, 30.0),
            ..style::outlined(d.background, 22.0, 1.0, d.border)
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    #[test]
    fn dk1_the_bare_desktop_names_no_app_in_the_menu_bar() {
        let desktop = Desktop::live(&[], OffsetDateTime::UNIX_EPOCH);
        assert!(desktop.app.is_empty());
        assert!(desktop.menus.is_empty());
    }

    #[test]
    fn aw1_needs_you_tiles_come_first_then_oldest_to_newest() {
        let tasks = fixtures::tasks();
        let names: Vec<&str> = ordered(&tasks)
            .iter()
            .map(|t| t.agent_name.as_str())
            .collect();
        assert_eq!(names, ["Expense agent", "Meeting prep", "Invoice agent"]);
    }

    #[test]
    fn a_live_desktop_shows_the_local_time_and_date() {
        let tasks = fixtures::tasks();
        let afternoon = time::macros::datetime!(2026-10-19 14:14 UTC);
        let live = Desktop::live(&tasks, afternoon);
        assert_eq!(live.clock, "Mon Oct 19  2:14 PM");
        assert_eq!(live.date, ("OCT".into(), 19));
        assert_eq!(live.now.unix_seconds(), afternoon.unix_timestamp());
        let morning = afternoon.replace_hour(9).expect("valid");
        assert_eq!(clock(morning), "Mon Oct 19  9:14 AM");
    }

    #[test]
    fn the_pill_counts_running_agents_in_words() {
        assert_eq!(pill_label(0), "No agents running");
        assert_eq!(pill_label(1), "1 agent");
        assert_eq!(pill_label(3), "3 agents");
    }

    #[test]
    fn the_status_dots_put_the_orange_one_last() {
        let theme = Theme::glass();
        let dots = status_dots(&theme, &fixtures::tasks());
        assert_eq!(dots.len(), 3);
        assert_eq!(dots[2], theme.attention);
        assert_eq!(dots[0], theme.accent);
    }
}
