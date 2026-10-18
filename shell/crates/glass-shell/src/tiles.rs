//! An agent tile (AW-1 to AW-4, "Tile states"), drawn from the same
//! `TaskView` that `task.list` returns. Needs-you tiles carry the question
//! and its two answers; working tiles show the three-line feed (actions
//! agents) or the screen (screen agents), the caption and progress.

use glass_api::{AgentMode, TaskView, TileState};
use glass_types::Timestamp;
use iced_core::alignment::Vertical;
use iced_core::{Color, Length, Padding};
use iced_widget::{Column, button, column, container, row, text};

use crate::theme::{TileColors, hex};
use crate::{Element, Message, Theme, fonts, icons, style};

/// Tile width in the stack at the top right.
pub const WIDTH: f32 = 352.0;

/// Height of a screen agent's thumbnail.
pub const THUMBNAIL_HEIGHT: f32 = 176.0;

/// A tile. `thumbnail` is a screen agent's display; `None` draws a
/// placeholder.
pub fn view<'a>(
    theme: &Theme,
    task: &'a TaskView,
    now: Timestamp,
    thumbnail: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let t = theme.tile;
    let needs = task.state == TileState::NeedsYou;
    let screen = task.mode == AgentMode::Screen && !needs;

    let mut body: Column<'a, Message, iced_widget::Theme, iced_widget::Renderer> =
        column![header(theme, task, now)].spacing(10);
    if let (true, Some(ask)) = (needs, task.needs.as_ref()) {
        body = body
            .push(
                text(&ask.summary)
                    .size(15)
                    .font(fonts::MEDIUM)
                    .color(t.text)
                    .line_height(1.4),
            )
            .push(text(&task.caption).size(12).color(t.muted))
            .push(answers(t, ask.id));
    } else if screen {
        let display = thumbnail.unwrap_or_else(placeholder_thumbnail);
        body = body.push(display).push(caption_row(theme, task));
    } else {
        if !task.feed.is_empty() {
            body = body.push(feed(theme, task));
        }
        body = body.push(caption_row(theme, task));
    }

    let ring = if needs {
        (2.0, theme.attention)
    } else if screen {
        (1.0, t.outline)
    } else {
        (0.0, Color::TRANSPARENT)
    };
    let padding = if screen {
        Padding {
            top: 8.0,
            right: 8.0,
            bottom: 14.0,
            left: 8.0,
        }
    } else {
        Padding::from(14.0)
    };
    let tile = container(body)
        .width(Length::Fixed(WIDTH))
        .padding(padding)
        .style(move |_| iced_widget::container::Style {
            text_color: Some(t.text),
            shadow: style::shadow(0.30, 14.0, 32.0),
            ..style::outlined(t.background, 14.0, ring.0, ring.1)
        });
    button(tile)
        .padding(0)
        .on_press(Message::OpenTask(task.id))
        .style(style::button_like(iced_widget::container::Style::default()))
        .into()
}

fn header<'a>(theme: &Theme, task: &'a TaskView, now: Timestamp) -> Element<'a, Message> {
    let t = theme.tile;
    let needs = task.state == TileState::NeedsYou;
    let color = state_color(theme, task.state);
    let dot = container("")
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |_| style::dot(color, 8.0));
    let mut name = row![
        dot,
        text(&task.agent_name)
            .size(13)
            .font(fonts::SEMIBOLD)
            .color(t.text)
    ]
    .spacing(8)
    .align_y(Vertical::Center);
    if !needs {
        let mode = match task.mode {
            AgentMode::Actions => "Actions",
            AgentMode::Screen => "Screen",
        };
        name = name.push(
            container(text(mode).size(11).font(fonts::MEDIUM).color(t.muted))
                .padding(Padding::from([2.0, 8.0]))
                .style(move |_| style::outlined(Color::TRANSPARENT, 999.0, 1.0, t.chip_border)),
        );
    }
    let right: Element<'a, Message> = if needs {
        text("NEEDS YOU")
            .size(11)
            .font(fonts::SEMIBOLD)
            .color(theme.attention)
            .into()
    } else if task.mode == AgentMode::Screen {
        icons::view(&icons::EXPAND, t.muted, 16.0, 16.0)
    } else {
        text(elapsed(task.started_at, now))
            .size(12)
            .color(t.muted)
            .into()
    };
    let header =
        row![name, iced_widget::space().width(Length::Fill), right].align_y(Vertical::Center);
    if task.mode == AgentMode::Screen && !needs {
        container(header)
            .padding(Padding {
                top: 6.0,
                right: 6.0,
                bottom: 0.0,
                left: 6.0,
            })
            .into()
    } else {
        header.into()
    }
}

fn answers<'a>(t: TileColors, approval: glass_types::ApprovalId) -> Element<'a, Message> {
    let review = button(
        container(text("Review").size(13).font(fonts::MEDIUM).color(t.text))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .on_press(Message::Review(approval))
    .width(Length::Fill)
    .height(Length::Fixed(44.0))
    .style(style::button_like(style::outlined(
        Color::TRANSPARENT,
        10.0,
        1.0,
        t.button_border,
    )));
    let approve = button(
        container(
            text("Approve")
                .size(13)
                .font(fonts::SEMIBOLD)
                .color(t.background),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill),
    )
    .on_press(Message::Approve(approval))
    .width(Length::Fill)
    .height(Length::Fixed(44.0))
    .style(style::button_like(style::panel(t.text, 10.0)));
    container(row![review, approve].spacing(8))
        .padding(Padding {
            top: 2.0,
            ..Padding::ZERO
        })
        .into()
}

/// The last three feed lines, verb in its own column; the newest is lit
/// and ends in the accent cursor (AW-3).
fn feed<'a>(theme: &Theme, task: &'a TaskView) -> Element<'a, Message> {
    let t = theme.tile;
    let last = task.feed.len().saturating_sub(1);
    let lines = task.feed.iter().enumerate().map(|(i, line)| {
        let newest = i == last && task.state.is_live();
        let (verb, target) = if line.verb.is_empty() {
            (String::new(), line.text.clone())
        } else {
            (line.verb.clone(), line.target.clone())
        };
        let mut target_row = row![text(target).size(12).font(fonts::MONO).color(if newest {
            t.text
        } else {
            t.feed_text
        })]
        .align_y(Vertical::Center);
        if newest {
            let accent = theme.accent;
            target_row = target_row.push(
                container("")
                    .width(Length::Fixed(7.0))
                    .height(Length::Fixed(13.0))
                    .style(move |_| style::panel(accent, 0.0)),
            );
            target_row = target_row.spacing(4);
        }
        row![
            container(text(verb).size(12).font(fonts::MONO).color(if newest {
                t.feed_verb
            } else {
                t.feed_text
            }))
            .width(Length::Fixed(40.0)),
            target_row,
        ]
        .spacing(10)
        .into()
    });
    container(Column::with_children(lines).spacing(6))
        .width(Length::Fill)
        .padding(Padding::from([10.0, 12.0]))
        .style(move |_| style::panel(t.feed_background, 8.0))
        .into()
}

fn caption_row<'a>(theme: &Theme, task: &'a TaskView) -> Element<'a, Message> {
    let t = theme.tile;
    let mut caption = row![text(&task.caption).size(13).color(t.caption)].align_y(Vertical::Center);
    let mut column = column![].spacing(8);
    if let Some(p) = task.progress {
        caption = caption.push(iced_widget::space().width(Length::Fill)).push(
            text(format!("{} / {}", p.done, p.total))
                .size(13)
                .font(fonts::MONO)
                .color(t.muted),
        );
        column = column.push(caption).push(progress(theme, p.done, p.total));
    } else {
        column = column.push(caption);
    }
    if task.mode == AgentMode::Screen {
        container(column).padding(Padding::from([0.0, 6.0])).into()
    } else {
        column.into()
    }
}

fn progress<'a>(theme: &Theme, done: u32, total: u32) -> Element<'a, Message> {
    let fraction = if total == 0 {
        0.0
    } else {
        done as f32 / total as f32
    };
    let accent = theme.accent;
    let track = theme.tile.track;
    let filled = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16;
    container(row![
        container("")
            .width(Length::FillPortion(filled.max(1)))
            .height(Length::Fixed(4.0))
            .style(move |_| style::panel(accent, 2.0)),
        iced_widget::space().width(Length::FillPortion((1000 - filled).max(1))),
    ])
    .width(Length::Fill)
    .height(Length::Fixed(4.0))
    .style(move |_| style::panel(track, 2.0))
    .into()
}

/// A screen agent's tile shows its virtual display (UI-3). Until the
/// compositor streams it, a neutral sketch of a window stands in.
pub fn placeholder_thumbnail<'a>() -> Element<'a, Message> {
    let line = |w: f32, color: u32| {
        container("")
            .width(Length::Fixed(w))
            .height(Length::Fixed(6.0))
            .style(move |_| style::panel(hex(color), 3.0))
    };
    container(column![
        container("")
            .height(Length::Fixed(20.0))
            .width(Length::Fill)
            .style(|_| style::panel(hex(0x2B3440), 0.0)),
        column![
            line(120.0, 0xB9C2CC),
            line(200.0, 0xD9DFE5),
            line(160.0, 0xD9DFE5)
        ]
        .spacing(8)
        .padding(12),
    ])
    .width(Length::Fill)
    .height(Length::Fixed(THUMBNAIL_HEIGHT))
    .clip(true)
    .style(|_| style::panel(hex(0xEEF1F4), 8.0))
    .into()
}

/// The dot color for a tile state: calm blue while working, orange only
/// when the user is needed.
pub fn state_color(theme: &Theme, state: TileState) -> Color {
    match state {
        TileState::NeedsYou => theme.attention,
        TileState::Failed => hex(0xE5484D),
        TileState::Done => hex(0x2E7D4F),
        TileState::Paused | TileState::Stopped | TileState::Starting => theme.tile.muted,
        TileState::Working | TileState::YouAreDriving => theme.accent,
    }
}

/// "4 min", "1 h 5 min", "just now".
pub fn elapsed(started: Timestamp, now: Timestamp) -> String {
    let minutes = now.seconds_since(started) / 60;
    match minutes {
        0 => "just now".into(),
        m if m < 60 => format!("{m} min"),
        m => format!("{} h {} min", m / 60, m % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_reads_like_the_tile() {
        let t0 = Timestamp::from_unix_seconds(1_000_000).unwrap_or_else(Timestamp::now);
        let at =
            |s: i64| Timestamp::from_unix_seconds(1_000_000 + s).unwrap_or_else(Timestamp::now);
        assert_eq!(elapsed(t0, at(30)), "just now");
        assert_eq!(elapsed(t0, at(240)), "4 min");
        assert_eq!(elapsed(t0, at(3900)), "1 h 5 min");
    }
}
