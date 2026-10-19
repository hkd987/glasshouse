//! The launcher: every app on the machine, over the dimmed desktop (the
//! dock's launcher icon, or Super tapped on its own as on Windows, DK-2).
//! A click starts the app; Esc or a click outside closes it.

use iced_core::alignment::Horizontal;
use iced_core::{Color, Length};
use iced_widget::{Column, Row, button, column, container, mouse_area, text};

use crate::apps::AppEntry;
use crate::theme::{hex, hexa};
use crate::{Element, Message, Theme, fonts, icons, style};

/// How many apps a row of the grid holds.
const PER_ROW: usize = 6;

pub fn overlay<'a>(theme: &Theme, apps: &[AppEntry]) -> Element<'a, Message> {
    let mut grid = Column::new().spacing(28);
    for row in apps.chunks(PER_ROW) {
        let mut line = Row::new().spacing(36);
        for app in row {
            line = line.push(tile(theme, app));
        }
        grid = grid.push(line);
    }
    let empty = apps
        .is_empty()
        .then(|| text("No apps yet").size(15).color(Color::WHITE).into());
    let content: Element<'a, Message> = empty.unwrap_or_else(|| grid.into());
    // A click anywhere but an app closes it.
    mouse_area(
        container(content)
            .center(Length::Fill)
            .style(|_| style::panel(hexa(0x0B0D10, 0.55), 0.0)),
    )
    .on_press(Message::ToggleLauncher)
    .into()
}

fn tile<'a>(_theme: &Theme, app: &AppEntry) -> Element<'a, Message> {
    let icon = container(icon(app.id))
        .center(Length::Fixed(72.0))
        .style(|_| style::panel(hex(0x15181D), 18.0));
    button(
        column![
            icon,
            text(app.name)
                .size(13)
                .font(fonts::MEDIUM)
                .color(Color::WHITE)
        ]
        .spacing(8)
        .align_x(Horizontal::Center),
    )
    .on_press(Message::Launch(app.id.to_owned()))
    .padding(6)
    .style(style::button_like(style::panel(Color::TRANSPARENT, 12.0)))
    .into()
}

/// An app's icon: Terminal's prompt, Activity Monitor's pulse; a dot for
/// the rest.
pub(crate) fn icon<'a>(id: &str) -> Element<'a, Message> {
    let glyph = match id {
        "terminal" => ">_",
        "activity-monitor" => return icons::view(&icons::ACTIVITY, hex(0x57AB5A), 34.0, 34.0),
        _ => "•",
    };
    text(glyph)
        .size(26)
        .font(fonts::MONO)
        .color(hex(0xE6E8EB))
        .into()
}
