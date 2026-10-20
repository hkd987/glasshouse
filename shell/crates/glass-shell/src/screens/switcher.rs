//! Alt Tab's switcher (DK-2): the open windows, the one last used first,
//! over the desktop. Tab moves to the next, Shift Tab back; letting go of
//! Alt raises the one chosen, Esc keeps things as they were.

use iced_core::alignment::Horizontal;
use iced_core::{Color, Length};
use iced_widget::{Row, column, container, text};

use crate::apps::{AppEntry, AppWindow};
use crate::screens::launcher;
use crate::theme::{hex, hexa};
use crate::{Element, Message, Theme, fonts, style};

/// The longest title a card shows whole.
const TITLE: usize = 22;

pub fn overlay<'a>(
    theme: &Theme,
    windows: &[AppWindow],
    chosen: u64,
    apps: &[AppEntry],
) -> Element<'a, Message> {
    let cards = windows.iter().fold(Row::new().spacing(12), |row, w| {
        // The app's icon, as the launcher shows it; a window of no app
        // offered gets its title's initial.
        let look = match apps.iter().find(|a| a.owns_window(&w.app_id)) {
            Some(app) => launcher::look(app, apps),
            None => launcher::Look {
                glyph: launcher::initial(&w.app_id, &w.title),
                remix: false,
            },
        };
        row.push(card(theme, w, look, w.id == chosen))
    });
    let panel = container(cards)
        .padding(18)
        .style(|_| style::panel(hexa(0x15181D, 0.94), 18.0));
    container(panel).center(Length::Fill).into()
}

fn card<'a>(
    theme: &Theme,
    window: &AppWindow,
    look: launcher::Look,
    chosen: bool,
) -> Element<'a, Message> {
    let icon = launcher::icon_tile(theme, look, hex(0x0B0D10), 16.0);
    let title = if window.title.chars().count() > TITLE {
        let short: String = window.title.chars().take(TITLE - 1).collect();
        format!("{}…", short.trim_end())
    } else {
        window.title.clone()
    };
    let look = if chosen {
        style::panel(hexa(0xFFFFFF, 0.12), 12.0).border(iced_core::Border {
            color: hex(0x4C8DFF),
            width: 2.0,
            radius: 12.0.into(),
        })
    } else {
        style::panel(Color::TRANSPARENT, 12.0)
    };
    container(
        column![
            icon,
            text(title).size(13).font(fonts::MEDIUM).color(Color::WHITE)
        ]
        .spacing(10)
        .align_x(Horizontal::Center),
    )
    .width(Length::Fixed(176.0))
    .padding(12)
    .align_x(Horizontal::Center)
    .style(move |_| look)
    .into()
}
