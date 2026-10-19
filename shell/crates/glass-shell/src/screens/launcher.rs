//! The launcher: every app on the machine, over the dimmed desktop (the
//! dock's launcher icon, or Super tapped on its own as on Windows, DK-2).
//! A click starts the app, or the arrows and Enter (UX-20); Esc or a click
//! outside closes it.

use iced_core::alignment::Horizontal;
use iced_core::{Color, Length};
use iced_widget::{Column, Row, button, column, container, mouse_area, text};

use crate::apps::AppEntry;
use crate::theme::{hex, hexa};
use crate::{Element, Message, Step, Theme, fonts, icons, style};

/// How many apps a row of the grid holds.
const PER_ROW: usize = 6;

/// The launcher over the desktop; `selected` is the app the keyboard has
/// selected.
pub fn overlay<'a>(
    theme: &Theme,
    apps: &[AppEntry],
    selected: Option<usize>,
) -> Element<'a, Message> {
    let mut grid = Column::new().spacing(28);
    for (r, row) in apps.chunks(PER_ROW).enumerate() {
        let mut line = Row::new().spacing(36);
        for (c, app) in row.iter().enumerate() {
            line = line.push(tile(theme, app, selected == Some(r * PER_ROW + c)));
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

/// Where an arrow, Home or End moves the selection in the grid of `n` apps:
/// along a row or up and down a column, stopping at the edges. With none
/// selected yet, End selects the last app and any other key the first.
pub fn step(at: Option<usize>, n: usize, step: Step) -> Option<usize> {
    let last = n.checked_sub(1)?;
    let Some(at) = at else {
        return Some(if step == Step::Last { last } else { 0 });
    };
    Some(match step {
        Step::Left => at.saturating_sub(1),
        Step::Right => (at + 1).min(last),
        Step::Up => at.checked_sub(PER_ROW).unwrap_or(at),
        Step::Down if at + PER_ROW <= last => at + PER_ROW,
        Step::Down => at,
        Step::First => 0,
        Step::Last => last,
    })
}

fn tile<'a>(_theme: &Theme, app: &AppEntry, selected: bool) -> Element<'a, Message> {
    let icon = container(icon(&app.id))
        .center(Length::Fixed(72.0))
        .style(|_| style::panel(hex(0x15181D), 18.0));
    button(
        column![
            icon,
            text(app.name.clone())
                .size(13)
                .font(fonts::MEDIUM)
                .color(Color::WHITE)
        ]
        .spacing(8)
        .align_x(Horizontal::Center),
    )
    .on_press(Message::Launch(app.id.clone()))
    .padding(6)
    .style(style::button_like(style::panel(
        if selected {
            hexa(0xFFFFFF, 0.16)
        } else {
            Color::TRANSPARENT
        },
        12.0,
    )))
    .into()
}

/// An app's icon: Terminal's prompt, Activity Monitor's pulse, and the
/// dock's glyphs for the Browser, Mail, Settings, the console and the Store;
/// a dot for the rest.
pub(crate) fn icon<'a>(id: &str) -> Element<'a, Message> {
    let drawn = |icon: &icons::Icon, color| icons::view(icon, hex(color), 32.0, 32.0);
    let glyph = match id {
        "terminal" => ">_",
        "activity-monitor" => return icons::view(&icons::ACTIVITY, hex(0x57AB5A), 34.0, 34.0),
        "browser" => return drawn(&icons::GLOBE, 0x2BB5B5),
        "mail" => return drawn(&icons::MAIL, 0x3AA8F5),
        "settings" => return drawn(&icons::GEAR, 0xA9B1BC),
        // IT's console: a gear in the accent, apart from Settings'.
        "console" => return drawn(&icons::GEAR, 0x3D6BFF),
        "store" => return drawn(&icons::BAG, 0x8F7CF2),
        "calendar" => "31",
        _ => "•",
    };
    text(glyph)
        .size(26)
        .font(fonts::MONO)
        .color(hex(0xE6E8EB))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ux20_arrows_move_along_rows_and_down_columns_and_stop_at_the_edges() {
        let n = PER_ROW + 2;
        assert_eq!(step(None, n, Step::Right), Some(0), "the first key selects");
        assert_eq!(step(None, n, Step::Last), Some(n - 1));
        assert_eq!(step(Some(0), n, Step::Left), Some(0));
        assert_eq!(step(Some(0), n, Step::Right), Some(1));
        assert_eq!(step(Some(1), n, Step::Down), Some(1 + PER_ROW));
        assert_eq!(step(Some(1 + PER_ROW), n, Step::Up), Some(1));
        // Nothing under the third app: Down stays.
        assert_eq!(step(Some(2), n, Step::Down), Some(2));
        assert_eq!(step(Some(n - 1), n, Step::Right), Some(n - 1));
        assert_eq!(
            step(None, 0, Step::Down),
            None,
            "no apps, nothing to select"
        );
    }
}
