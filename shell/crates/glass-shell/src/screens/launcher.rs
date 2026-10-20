//! The launcher: every app on the machine, over the dimmed desktop (the
//! dock's launcher icon, or Super tapped on its own as on Windows, DK-2).
//! A click starts the app, or the arrows and Enter (UX-20); Esc or a click
//! outside closes it.

use iced_core::alignment::{Horizontal, Vertical};
use iced_core::{Color, Length};
use iced_widget::{Column, Row, button, column, container, mouse_area, stack, text};

use crate::apps::AppEntry;
use crate::theme::{hex, hexa};
use crate::{Element, Message, Step, Theme, fonts, icons, style};

/// How many apps a row of the grid holds.
const PER_ROW: usize = 6;
/// How wide a column of the grid is: the icon and the room between, so
/// columns line up whatever the names; a longer name wraps.
const COLUMN: f32 = 120.0;

/// The launcher over the desktop; `selected` is the app the keyboard has
/// selected.
pub fn overlay<'a>(
    theme: &Theme,
    apps: &[AppEntry],
    selected: Option<usize>,
) -> Element<'a, Message> {
    let mut grid = Column::new().spacing(28);
    for (r, row) in apps.chunks(PER_ROW).enumerate() {
        let mut line = Row::new();
        for (c, app) in row.iter().enumerate() {
            let tile = tile(theme, app, apps, selected == Some(r * PER_ROW + c));
            line = line.push(
                container(tile)
                    .width(Length::Fixed(COLUMN))
                    .align_x(Horizontal::Center),
            );
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

fn tile<'a>(
    theme: &Theme,
    app: &AppEntry,
    apps: &[AppEntry],
    selected: bool,
) -> Element<'a, Message> {
    let icon = icon_tile(theme, look(app, apps), hex(0x15181D), 18.0);
    button(
        column![
            icon,
            text(app.name.clone())
                .size(13)
                .font(fonts::MEDIUM)
                .color(Color::WHITE)
                .align_x(Horizontal::Center)
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

/// What an app's icon shows: its glyph, and whether it's one of the
/// person's own versions of another app.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub glyph: Glyph,
    /// A remix (ADR 0032, TL-5): its base's glyph with a badge in the
    /// corner that says this version is the person's.
    pub remix: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Glyph {
    /// One of our apps' icons, in its color, `size` pixels square.
    Drawn {
        icon: &'static icons::Icon,
        color: u32,
        size: f32,
    },
    /// A few characters: Terminal's prompt, the Calendar's date.
    Text(&'static str),
    /// An app with no icon of its own: its initial on a tile tinted for it.
    Letter { letter: char, tint: u32 },
}

/// How `app` looks in the launcher and Alt Tab's switcher; `apps` are the
/// others offered, among them the app a remix is based on.
pub fn look(app: &AppEntry, apps: &[AppEntry]) -> Look {
    match &app.based_on {
        // A remix looks like what it remixes, badged; its name is its own.
        Some(base) => {
            let name = apps
                .iter()
                .find(|a| &a.id == base)
                .map_or(base.as_str(), |a| a.name.as_str());
            Look {
                glyph: glyph(base, name),
                remix: true,
            }
        }
        None => Look {
            glyph: glyph(&app.id, &app.name),
            remix: false,
        },
    }
}

/// The glyph of the app `id`, named `name`: ours are drawn, the rest
/// get their initial.
fn glyph(id: &str, name: &str) -> Glyph {
    let drawn = |icon, color| Glyph::Drawn {
        icon,
        color,
        size: 32.0,
    };
    match id {
        "terminal" => Glyph::Text(">_"),
        "calendar" => Glyph::Text("31"),
        "activity-monitor" => Glyph::Drawn {
            icon: &icons::ACTIVITY,
            color: 0x57AB5A,
            size: 34.0,
        },
        "browser" => drawn(&icons::GLOBE, 0x2BB5B5),
        "mail" => drawn(&icons::MAIL, 0x3AA8F5),
        "chat" => drawn(&icons::CHAT, 0xF2B84B),
        "artifacts" => drawn(&icons::ARTIFACT, 0xE07BC6),
        "meet" => drawn(&icons::MEET, 0xF0707A),
        "settings" => drawn(&icons::GEAR, 0xA9B1BC),
        // IT's console: a gear in the accent, apart from Settings'.
        "console" => drawn(&icons::GEAR, 0x3D6BFF),
        "store" => drawn(&icons::BAG, 0x8F7CF2),
        _ => initial(id, name),
    }
}

/// Tints a generated icon may take, light enough on the launcher's dark
/// tiles.
const TINTS: [u32; 6] = [0x5B9DF5, 0x35B8A6, 0x6DBE5A, 0xE3A73E, 0xE8697D, 0x9D84F5];

/// An initial on a tinted tile: the name's first letter or digit (the id's
/// past `my-` if the name has none), and a tint that follows from the id,
/// so the app looks the same everywhere and each time.
pub(crate) fn initial(id: &str, name: &str) -> Glyph {
    let id_part = id.strip_prefix(glass_types::PERSONAL_PREFIX).unwrap_or(id);
    let letter = name
        .chars()
        .chain(id_part.chars())
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('?');
    // FNV-1a: stable across runs and machines, unlike std's hasher.
    let hash = id.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    Glyph::Letter {
        letter,
        tint: TINTS[hash as usize % TINTS.len()],
    }
}

/// An app's icon on a 72-pixel tile of `fill`: its glyph in the middle and,
/// on a remix, the badge in the bottom right corner.
pub(crate) fn icon_tile<'a>(
    theme: &Theme,
    look: Look,
    fill: Color,
    radius: f32,
) -> Element<'a, Message> {
    const SIDE: f32 = 72.0;
    let tile = container(glyph_view(look.glyph))
        .center(Length::Fixed(SIDE))
        .style(move |_| style::panel(fill, radius));
    if !look.remix {
        return tile.into();
    }
    // A pencil on the accent, ringed in the tile's own color so it reads
    // as a mark on the corner, apart from the glyph.
    let accent = theme.accent;
    let badge = container(icons::view(&icons::REMIX, Color::WHITE, 12.0, 12.0))
        .center(Length::Fixed(24.0))
        .style(move |_| style::outlined(accent, 12.0, 2.0, fill));
    stack![
        tile,
        container(badge)
            .width(Length::Fixed(SIDE))
            .height(Length::Fixed(SIDE))
            .padding(3)
            .align_x(Horizontal::Right)
            .align_y(Vertical::Bottom),
    ]
    .into()
}

fn glyph_view<'a>(glyph: Glyph) -> Element<'a, Message> {
    match glyph {
        Glyph::Drawn { icon, color, size } => icons::view(icon, hex(color), size, size),
        Glyph::Text(glyph) => text(glyph)
            .size(26)
            .font(fonts::MONO)
            .color(hex(0xE6E8EB))
            .into(),
        Glyph::Letter { letter, tint } => container(
            text(letter.to_string())
                .size(24)
                .font(fonts::SEMIBOLD)
                .color(hex(tint)),
        )
        .center(Length::Fixed(44.0))
        .style(move |_| style::panel(hexa(tint, 0.18), 12.0))
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::Start;

    fn web(id: &str, name: &str, based_on: Option<&str>) -> AppEntry {
        AppEntry {
            id: id.into(),
            name: name.into(),
            start: Start::Web,
            based_on: based_on.map(String::from),
        }
    }

    #[test]
    fn tl5_a_remix_shows_its_base_apps_icon_with_a_badge() {
        let mail = web("mail", "Mail", None);
        let mine = web("my-mail", "My Mail", Some("mail"));
        let apps = [mail.clone(), mine.clone()];
        assert_eq!(
            look(&mine, &apps),
            Look {
                remix: true,
                ..look(&mail, &apps)
            }
        );
        assert!(matches!(
            look(&mine, &apps).glyph,
            Glyph::Drawn { icon, .. } if *icon == icons::MAIL
        ));
        assert!(!look(&mail, &apps).remix, "the original wears no badge");
        // A remix of an app with no icon of its own wears that app's
        // initial, from its name, not its own.
        let tracker = web("dispute-tracker", "Dispute tracker", None);
        let mine = web("my-disputes", "Disputes, mine", Some("dispute-tracker"));
        let apps = [tracker.clone(), mine.clone()];
        assert_eq!(
            look(&mine, &apps),
            Look {
                remix: true,
                ..look(&tracker, &apps)
            }
        );
    }

    #[test]
    fn tl5_an_app_without_an_icon_gets_its_initial_on_a_tinted_tile_not_a_dot() {
        let tracker = web("dispute-tracker", "Dispute tracker", None);
        let Glyph::Letter { letter, tint } = look(&tracker, &[]).glyph else {
            panic!("a generated icon");
        };
        assert_eq!(letter, 'D');
        assert!(TINTS.contains(&tint));
        // The tint follows the id, so it stays when the app is renamed;
        // other ids take other tints.
        assert!(matches!(
            initial("dispute-tracker", "Renamed"),
            Glyph::Letter { letter: 'R', tint: same } if same == tint
        ));
        let tints: std::collections::HashSet<u32> = ["a", "b", "c", "d", "e", "f", "g", "h"]
            .iter()
            .filter_map(|id| match initial(id, id) {
                Glyph::Letter { tint, .. } => Some(tint),
                _ => None,
            })
            .collect();
        assert!(tints.len() > 1, "not one tint for every app");
        // One of the person's own apps built from scratch: no badge, and
        // the initial of its name, never of the `my-` its id starts with.
        let notes = web("my-standup", "standup notes", None);
        assert_eq!(
            look(&notes, &[]),
            Look {
                glyph: initial("my-standup", "standup notes"),
                remix: false
            }
        );
        assert!(matches!(
            initial("my-standup", "standup notes"),
            Glyph::Letter { letter: 'S', .. }
        ));
        assert!(matches!(
            initial("my-standup", "…"),
            Glyph::Letter { letter: 'S', .. }
        ));
    }

    #[test]
    fn chat_meet_and_artifacts_have_icons_of_their_own() {
        for (id, icon) in [
            ("chat", icons::CHAT),
            ("meet", icons::MEET),
            ("artifacts", icons::ARTIFACT),
        ] {
            let app = web(id, id, None);
            assert!(
                matches!(look(&app, &[]).glyph, Glyph::Drawn { icon: drawn, .. } if *drawn == icon),
                "{id}"
            );
        }
        // Each in its own color among our apps.
        let colors: Vec<u32> = [
            "mail",
            "chat",
            "artifacts",
            "meet",
            "store",
            "browser",
            "settings",
        ]
        .iter()
        .filter_map(|id| match glyph(id, id) {
            Glyph::Drawn { color, .. } => Some(color),
            _ => None,
        })
        .collect();
        let distinct: std::collections::HashSet<&u32> = colors.iter().collect();
        assert_eq!((colors.len(), distinct.len()), (7, 7));
    }

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
