//! The command bar (DA-1, DK-1, the Command bar artboard): opens over
//! everything with Ctrl Space. The desktop agent answers on the left (AE-3);
//! suggestions for what's on screen and recent tasks sit on the right.
//! Recent tasks are real `task.list` data; the answer comes from the
//! desktop agent, which isn't built yet, so callers pass it in.

use glass_api::{TaskView, TileState, display_line};
use iced_core::alignment::Vertical;
use iced_core::{Color, Length, Padding};
use iced_widget::text::Span;
use iced_widget::{Column, button, column, container, pin, row, text, text_input};

use crate::theme::{hex, hexa};
use crate::{Element, Message, Theme, fonts, icons, style};

#[derive(Debug, Clone, PartialEq)]
pub struct CommandBar<'a> {
    pub query: String,
    pub answer: Option<Answer>,
    /// "For what's on your screen".
    pub suggestions: Vec<String>,
    pub recent: &'a [TaskView],
    /// Which model answers and the data limit ("Claude via Amazon Bedrock ·
    /// Internal data"), from the handoff card's "Data and model".
    pub model: String,
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

    let suggestions = column![
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
    .spacing(8);
    let recent = column![section("Recent")]
        .extend(bar.recent.iter().map(recent_row))
        .spacing(8);
    let side = column![suggestions, recent].spacing(18);

    // The divider is a layer over the row, so it takes the row's height
    // instead of stretching the panel.
    let body = iced_widget::stack![
        row![
            container(answer(bar.answer.as_ref()))
                .width(Length::Fill)
                .padding(20),
            container(side).width(Length::Fixed(SIDE)).padding(20),
        ],
        container(rule_v()).height(Length::Fill).padding(Padding {
            left: WIDTH - SIDE,
            ..Padding::ZERO
        }),
    ];
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

fn answer<'a>(answer: Option<&'a Answer>) -> Element<'a, Message> {
    let Some(answer) = answer else {
        return column![
            section("Desktop agent"),
            text("Ask anything, or describe a task to hand off.")
                .size(15)
                .color(hex(0xD6DBE1)),
        ]
        .spacing(14)
        .into();
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
    use super::twelve_hour;

    #[test]
    fn done_times_read_on_a_twelve_hour_clock() {
        assert_eq!(twelve_hour("08:56:10"), "8:56 AM");
        assert_eq!(twelve_hour("00:05:00"), "12:05 AM");
        assert_eq!(twelve_hour("12:00:00"), "12:00 PM");
        assert_eq!(twelve_hour("21:12:59"), "9:12 PM");
    }
}
