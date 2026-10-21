//! The command bar (DA-1, DK-1, the Command bar artboard): opens over
//! everything with Ctrl Space, like Spotlight. Typing finds apps by name
//! and Enter opens the top one; the desktop agent answers on the left
//! (AE-3); suggestions for what's on screen and recent tasks sit on the
//! right. Recent tasks are real `task.list` data. With no app found, Enter
//! hands what was typed to the desktop agent as a task, which opens; an
//! answer in the bar itself is the artboard's, which callers pass in.

use glass_api::{TaskView, TileState, display_line};
use iced_core::alignment::Vertical;
use iced_core::{Color, Length, Padding};
use iced_widget::text::Span;
use iced_widget::{Column, button, column, container, pin, row, text, text_input};

use crate::apps::AppEntry;
use crate::screens::launcher::{self, Look};
use crate::theme::{hex, hexa};
use crate::{Element, Message, Theme, fonts, icons, style};

#[derive(Debug, Clone, PartialEq)]
pub struct CommandBar<'a> {
    pub query: String,
    /// The apps whose names match the query, best first ([`find_apps`]).
    pub hits: Vec<Hit<'a>>,
    /// The hit Enter opens: the first, until the arrows move it.
    pub selected: Option<usize>,
    pub answer: Option<Answer>,
    /// Why the desktop agent couldn't take what was typed.
    pub note: Option<&'a str>,
    /// "For what's on your screen".
    pub suggestions: Vec<String>,
    pub recent: &'a [TaskView],
    /// Which model answers and the data limit ("Claude via Amazon Bedrock ·
    /// Internal data"), from the handoff card's "Data and model".
    pub model: String,
}

/// An app the query found, as the launcher shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit<'a> {
    pub app: &'a AppEntry,
    pub look: Look,
    /// For one of the person's own versions (TL-5), the name of the app it
    /// is a version of.
    pub based_on: Option<&'a str>,
}

impl<'a> Hit<'a> {
    /// `app` as found among `apps`, the others offered.
    pub fn new(app: &'a AppEntry, apps: &'a [AppEntry]) -> Self {
        let based_on = app.based_on.as_deref().map(|base| {
            apps.iter()
                .find(|a| a.id == base)
                .map_or(base, |a| a.name.as_str())
        });
        Self {
            app,
            look: launcher::look(app, apps),
            based_on,
        }
    }
}

/// How many apps the bar lists.
pub const MOST_HITS: usize = 6;

/// The apps whose names match `query`, best first, as Spotlight finds them
/// (DK-1): the name starting with it, then one of its words, then anywhere
/// in it, then the words' initials ("am" finds Activity Monitor). Case and
/// the spaces around the query don't matter; among equals the shorter name
/// comes first, then the alphabet. Apps the person hid from search
/// (`hidden`, by id) never show. At most [`MOST_HITS`].
pub fn find_apps<'a>(apps: &'a [AppEntry], query: &str, hidden: &[String]) -> Vec<&'a AppEntry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(u8, &AppEntry)> = apps
        .iter()
        .filter(|app| !hidden.contains(&app.id))
        .filter_map(|app| rank(&app.name.to_lowercase(), &query).map(|r| (r, app)))
        .collect();
    found.sort_by(|(r1, a1), (r2, a2)| {
        r1.cmp(r2)
            .then(a1.name.len().cmp(&a2.name.len()))
            .then(a1.name.to_lowercase().cmp(&a2.name.to_lowercase()))
    });
    found
        .into_iter()
        .take(MOST_HITS)
        .map(|(_, app)| app)
        .collect()
}

/// How well `name` matches `query`, both lowercase: lower is better, none
/// if it doesn't.
fn rank(name: &str, query: &str) -> Option<u8> {
    let words = || {
        name.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
    };
    if name.starts_with(query) {
        Some(0)
    } else if words().any(|w| w.starts_with(query)) {
        Some(1)
    } else if name.contains(query) {
        Some(2)
    } else if query.chars().count() > 1
        && words()
            .filter_map(|w| w.chars().next())
            .collect::<String>()
            .starts_with(query)
    {
        Some(3)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// Paragraphs of spans; `true` marks a bold span.
    pub paragraphs: Vec<Vec<(String, bool)>>,
    pub accept: String,
    pub decline: String,
    pub sources: String,
}

/// The id of the bar's input, to give it the keyboard focus when it opens.
pub const INPUT: &str = "command-bar-input";

const WIDTH: f32 = 960.0;
/// The artboard's 320 px column is content-box: plus 20 px padding each
/// side and its 1 px rule.
const SIDE: f32 = 361.0;
const PANEL: Color = hexa(0x15181D, 0.94);
const RULE: Color = hex(0x262B32);
const CHIP: Color = hex(0x343A43);
const LABEL: Color = hex(0xA9B1BC);
const QUIET: Color = hex(0x8C95A1);
const INK: Color = hex(0xF3F5F7);

/// The command bar with its scrim, for the desktop's overlay layer (above
/// windows and tiles, below the menu bar and dock).
pub fn overlay<'a>(theme: &Theme, bar: &'a CommandBar<'a>) -> Element<'a, Message> {
    iced_widget::stack![
        column![
            iced_widget::space().height(Length::Fixed(32.0)),
            container("")
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_| style::panel(hexa(0x0E1626, 0.3), 0.0)),
        ],
        pin(view(theme, bar)).x(240.0).y(150.0),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

pub fn view<'a>(theme: &Theme, bar: &'a CommandBar<'a>) -> Element<'a, Message> {
    let accent = theme.accent;
    let esc = container(text("Esc").size(11).font(fonts::MONO).color(LABEL))
        .padding(Padding::from([3.0, 7.0]))
        .style(|_| style::outlined(Color::TRANSPARENT, 6.0, 1.0, CHIP));
    let input = text_input("Ask, or hand off a task", &bar.query)
        .id(INPUT)
        .on_input(Message::CommandChanged)
        .size(18)
        .font(fonts::SANS)
        .padding(Padding::from([7.0, 0.0]))
        .style(move |_, _| text_input::Style {
            background: iced_core::Background::Color(Color::TRANSPARENT),
            border: iced_core::Border::default(),
            icon: INK,
            placeholder: QUIET,
            value: INK,
            selection: Color { a: 0.4, ..accent },
        });
    let header = container(
        row![icons::view(&icons::SPARK, accent, 18.0, 18.0), input, esc]
            .spacing(12)
            .align_y(Vertical::Center),
    )
    .padding(Padding::from([16.0, 20.0]));

    // A side section shows only with something in it, and the side column
    // only with a section.
    let mut side = Column::new().spacing(18);
    if !bar.suggestions.is_empty() {
        side = side.push(
            column![
                section("For what's on your screen"),
                Column::with_children(bar.suggestions.iter().map(|s| {
                    button(text(s.as_str()).size(13).color(INK))
                        .on_press(Message::RunSuggestion(s.clone()))
                        .width(Length::Fill)
                        .padding(Padding::from([11.0, 12.0]))
                        .style(style::button_like(style::outlined(
                            hex(0x1E2228),
                            10.0,
                            1.0,
                            CHIP,
                        )))
                        .into()
                }))
                .spacing(8),
            ]
            .spacing(8),
        );
    }
    if !bar.recent.is_empty() {
        side = side.push(
            column![section("Recent")]
                .extend(bar.recent.iter().map(recent_row))
                .spacing(8),
        );
    }
    let main = container(if bar.hits.is_empty() {
        answer(bar)
    } else {
        hits(bar)
    })
    .width(Length::Fill)
    .padding(20);
    let body: Element<'a, Message> = if bar.suggestions.is_empty() && bar.recent.is_empty() {
        main.into()
    } else {
        // The divider is a layer over the row, so it takes the row's
        // height instead of stretching the panel.
        iced_widget::stack![
            row![main, container(side).width(Length::Fixed(SIDE)).padding(20)],
            container(rule_v()).height(Length::Fill).padding(Padding {
                left: WIDTH - SIDE,
                ..Padding::ZERO
            }),
        ]
        .into()
    };
    let footer = container(
        row![
            text(bar.model.as_str()).size(12).color(QUIET),
            iced_widget::space().width(Length::Fill),
            text("Hands off to a specialist agent when one fits")
                .size(12)
                .color(QUIET),
        ]
        .align_y(Vertical::Center),
    )
    .padding(Padding::from([10.0, 20.0]));

    container(column![header, rule_h(), body, rule_h(), footer])
        .width(Length::Fixed(WIDTH))
        .clip(true)
        .style(|_| iced_widget::container::Style {
            text_color: Some(INK),
            shadow: style::shadow(0.35, 24.0, 64.0),
            ..style::outlined(PANEL, 18.0, 1.0, hex(0x2A3038))
        })
        .into()
}

/// The apps found, the one Enter opens lit, and the agent below them.
fn hits<'a>(bar: &'a CommandBar<'a>) -> Element<'a, Message> {
    let rows = bar.hits.iter().enumerate().map(|(n, hit)| {
        let chosen = bar.selected == Some(n);
        let about = match hit.based_on {
            Some(base) => format!("Your version of {base}"),
            None => "App".to_owned(),
        };
        let open: Element<'a, Message> = if chosen {
            text("\u{21B5} Open").size(12).color(LABEL).into()
        } else {
            iced_widget::space().width(Length::Shrink).into()
        };
        let open_it = button(
            row![
                launcher::small_icon(hit.look, 32.0),
                column![
                    text(display_line(&hit.app.name))
                        .size(15)
                        .font(fonts::MEDIUM)
                        .color(INK),
                    text(about).size(12).color(QUIET),
                ]
                .spacing(1),
                iced_widget::space().width(Length::Fill),
                open,
            ]
            .spacing(12)
            .align_y(Vertical::Center),
        )
        .on_press(Message::Launch(hit.app.id.clone()))
        .width(Length::Fill)
        .padding(Padding::from([7.0, 10.0]))
        .style(move |_, status| {
            let lit = chosen || matches!(status, button::Status::Hovered | button::Status::Pressed);
            let look = style::panel(
                if lit {
                    hexa(0xFFFFFF, 0.08)
                } else {
                    Color::TRANSPARENT
                },
                10.0,
            );
            button::Style {
                background: look.background,
                text_color: INK,
                border: look.border,
                shadow: look.shadow,
                snap: true,
            }
        });
        // Beside the row, not in it: one click hides the app from search
        // (DK-1), and Settings shows it again.
        let hide = button(text("Hide").size(12))
            .on_press(Message::HideFromSearch(hit.app.id.clone()))
            .padding(Padding::from([6.0, 10.0]))
            .style(|_, status| {
                let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                button::Style {
                    background: hovered.then(|| hexa(0xFFFFFF, 0.08).into()),
                    text_color: if hovered { INK } else { QUIET },
                    border: iced_core::Border {
                        radius: 8.0.into(),
                        ..Default::default()
                    },
                    shadow: iced_core::Shadow::default(),
                    snap: true,
                }
            });
        row![open_it, hide]
            .spacing(4)
            .align_y(Vertical::Center)
            .into()
    });
    column![
        section("Apps"),
        Column::with_children(rows).spacing(2),
        iced_widget::space().height(Length::Fixed(6.0)),
        section("Desktop agent"),
        text("Keep typing to ask, or describe a task to hand off.")
            .size(14)
            .color(hex(0xD6DBE1)),
    ]
    .spacing(10)
    .into()
}

fn answer<'a>(bar: &'a CommandBar<'a>) -> Element<'a, Message> {
    let Some(answer) = bar.answer.as_ref() else {
        let typed = !bar.query.trim().is_empty();
        let mut column = column![
            section("Desktop agent"),
            text(if typed {
                "\u{21B5} Hand this to the desktop agent. It asks first wherever the \
                 rules say to."
            } else {
                "Ask anything, or describe a task to hand off."
            })
            .size(15)
            .color(hex(0xD6DBE1)),
        ]
        .spacing(14);
        if let Some(note) = bar.note {
            column = column.push(text(display_line(note)).size(14).color(hex(0xF5A65B)));
        }
        return column.into();
    };
    let mut column = column![section("Desktop agent")].spacing(14);
    for (i, paragraph) in answer.paragraphs.iter().enumerate() {
        let color = if i == 0 { INK } else { hex(0xD6DBE1) };
        let spans: Vec<Span<'a, (), iced_core::Font>> = paragraph
            .iter()
            .map(|(s, bold)| {
                iced_widget::span(s.as_str())
                    .font(if *bold { fonts::SEMIBOLD } else { fonts::SANS })
                    .color(color)
            })
            .collect();
        column = column.push(
            iced_widget::rich_text(spans)
                .size(15)
                .line_height(1.6)
                .on_link_click(|()| Message::OpenCommandBar),
        );
    }
    let accept = button(
        text(answer.accept.as_str())
            .size(14)
            .font(fonts::SEMIBOLD)
            .color(hex(0x15181D)),
    )
    .on_press(Message::AcceptAnswer)
    .padding(Padding::from([13.0, 18.0]))
    .style(style::button_like(style::panel(INK, 10.0)));
    let decline = button(
        text(answer.decline.as_str())
            .size(14)
            .font(fonts::MEDIUM)
            .color(INK),
    )
    .on_press(Message::CloseCommandBar)
    .padding(Padding::from([13.0, 16.0]))
    .style(style::button_like(style::outlined(
        Color::TRANSPARENT,
        10.0,
        1.0,
        hex(0x3A414B),
    )));
    column
        .push(row![accept, decline].spacing(8))
        .push(text(answer.sources.as_str()).size(12).color(QUIET))
        .into()
}

fn recent_row(task: &TaskView) -> Element<'_, Message> {
    let (status, color) = match task.state {
        TileState::NeedsYou => ("Needs you".to_owned(), hex(0xF5A65B)),
        TileState::Done => (
            format!("Done {}", twelve_hour(&task.updated_at.clock())),
            hex(0x6FD39A),
        ),
        TileState::Failed => ("Failed".to_owned(), hex(0xF07B7E)),
        TileState::Stopped => ("Stopped".to_owned(), LABEL),
        _ => match task.progress {
            Some(p) => (format!("{} of {}", p.done, p.total), LABEL),
            None => ("Working".to_owned(), LABEL),
        },
    };
    // An older glassd sends no title; the agent's name stands in.
    let name = if task.title.is_empty() {
        &task.agent_name
    } else {
        &task.title
    };
    row![
        text(display_line(name)).size(13).color(INK),
        iced_widget::space().width(Length::Fill),
        text(status).size(13).color(color),
    ]
    .into()
}

/// "08:56:10" as "8:56 AM".
fn twelve_hour(clock: &str) -> String {
    let hour: u32 = clock.get(..2).and_then(|h| h.parse().ok()).unwrap_or(0);
    let minute = clock.get(3..5).unwrap_or("00");
    let suffix = if hour < 12 { "AM" } else { "PM" };
    let hour = match hour % 12 {
        0 => 12,
        h => h,
    };
    format!("{hour}:{minute} {suffix}")
}

fn section<'a>(label: &'a str) -> Element<'a, Message> {
    text(label.to_uppercase())
        .size(12)
        .font(fonts::SEMIBOLD)
        .color(LABEL)
        .into()
}

fn rule_h<'a>() -> Element<'a, Message> {
    container("")
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_| style::panel(RULE, 0.0))
        .into()
}

fn rule_v<'a>() -> Element<'a, Message> {
    container("")
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(|_| style::panel(RULE, 0.0))
        .into()
}

#[cfg(test)]
mod tests {
    use super::{find_apps, twelve_hour};
    use crate::apps::{AppEntry, Start};

    fn apps(names: &[&str]) -> Vec<AppEntry> {
        names
            .iter()
            .map(|name| AppEntry {
                id: name.to_lowercase().replace(' ', "-"),
                name: (*name).to_owned(),
                start: Start::Web,
                based_on: None,
            })
            .collect()
    }

    fn names(found: &[&AppEntry]) -> Vec<String> {
        found.iter().map(|a| a.name.clone()).collect()
    }

    #[test]
    fn dk1_the_bar_finds_apps_as_spotlight_does() {
        let all = apps(&[
            "Activity Monitor",
            "Artifacts",
            "Browser",
            "Calendar",
            "Chat",
            "Docs",
            "Mail",
            "Meet",
            "My Mail",
            "Settings",
            "Store",
            "Terminal",
        ]);
        // The start of the name first, the shorter of equals first.
        assert_eq!(names(&find_apps(&all, "ma", &[])), ["Mail", "My Mail"]);
        assert_eq!(
            names(&find_apps(&all, "m", &[])),
            ["Mail", "Meet", "My Mail", "Activity Monitor", "Terminal"]
        );
        // Case and the spaces around it don't matter.
        assert_eq!(names(&find_apps(&all, "  DOC ", &[])), ["Docs"]);
        // A word of the name, then anywhere in it.
        assert_eq!(names(&find_apps(&all, "mon", &[])), ["Activity Monitor"]);
        assert_eq!(names(&find_apps(&all, "ett", &[])), ["Settings"]);
        // The words' initials, from two letters.
        assert_eq!(names(&find_apps(&all, "am", &[])), ["Activity Monitor"]);
        // Nothing for nothing, or for what no name has.
        assert!(find_apps(&all, "", &[]).is_empty());
        assert!(find_apps(&all, "   ", &[]).is_empty());
        assert!(find_apps(&all, "prep me for the call", &[]).is_empty());
        // At most six.
        assert_eq!(find_apps(&all, "a", &[]).len(), super::MOST_HITS);
        // What the person hid never shows.
        assert_eq!(names(&find_apps(&all, "ma", &["my-mail".into()])), ["Mail"]);
        assert!(find_apps(&all, "doc", &["docs".into()]).is_empty());
    }

    #[test]
    fn done_times_read_on_a_twelve_hour_clock() {
        assert_eq!(twelve_hour("08:56:10"), "8:56 AM");
        assert_eq!(twelve_hour("00:05:00"), "12:05 AM");
        assert_eq!(twelve_hour("12:00:00"), "12:00 PM");
        assert_eq!(twelve_hour("21:12:59"), "9:12 PM");
    }
}
