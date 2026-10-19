//! The system menu's windows, over the desktop as the command bar is:
//! About Glasshouse, and Force Quit, which stops a running agent at once
//! (apps join it when they have windows). A click outside or Esc closes
//! either.

use glass_api::{TaskView, TileState};
use glass_types::TaskId;
use iced_core::alignment::{Horizontal, Vertical};
use iced_core::{Color, Length, Padding, mouse};
use iced_widget::text::Wrapping;
use iced_widget::{Column, button, column, container, mouse_area, row, stack, text};

use crate::theme::{hex, hexa};
use crate::{Element, Message, Power, Theme, fonts, icons, style, tiles};

const INK: Color = hex(0xF3F5F7);
const MUTED: Color = hex(0xA9B1BC);
const PANEL: Color = hex(0x15181D);
const EDGE: Color = hex(0x2A3038);

/// What About Glasshouse shows: this build and this computer. Never the
/// base system's name (BR-1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct About {
    /// "0.1.0".
    pub version: String,
    /// The source revision it was built from, "9cf8edb".
    pub build: String,
    /// "Intel(R) Core(TM) Ultra 7 155H".
    pub processor: String,
    pub memory_gb: u64,
    pub computer: String,
}

/// The Force Quit window's content.
pub struct ForceQuit<'a> {
    /// The agents that are still running, oldest first.
    pub running: Vec<&'a TaskView>,
    pub chosen: Option<TaskId>,
}

/// About Glasshouse, centered over a dimmed desktop.
pub fn about_overlay<'a>(theme: &Theme, about: &'a About) -> Element<'a, Message> {
    let mut version = format!("Version {}", about.version);
    if !about.build.is_empty() {
        version.push_str(&format!(" ({})", about.build));
    }
    let fact = |label: &'static str, value: String| {
        row![
            container(text(label).size(13).color(MUTED))
                .width(Length::Fixed(120.0))
                .align_x(Horizontal::Right),
            text(value).size(13).color(INK),
        ]
        .spacing(14)
    };
    let mut facts = Column::new().spacing(8);
    if !about.processor.is_empty() {
        facts = facts.push(fact("Processor", about.processor.clone()));
    }
    if about.memory_gb > 0 {
        facts = facts.push(fact("Memory", format!("{} GB", about.memory_gb)));
    }
    if !about.computer.is_empty() {
        facts = facts.push(fact("Computer name", about.computer.clone()));
    }
    let body = column![
        icons::view(&icons::MARK, INK, 72.0, 72.0),
        text("Glasshouse").size(24).font(fonts::SEMIBOLD).color(INK),
        text(version).size(13).color(MUTED),
        divider(),
        facts,
        divider(),
        row![
            iced_widget::space().width(Length::Fill),
            push_button(theme, "Close", Some(Message::CloseSystemWindow), true),
        ],
    ]
    .spacing(12)
    .align_x(Horizontal::Center);
    centered(window(body, 380.0), Message::CloseSystemWindow)
}

/// The keyboard shortcuts (UX-20), in their groups.
pub fn shortcuts_overlay<'a>(theme: &Theme) -> Element<'a, Message> {
    let mut groups = Column::new().spacing(14);
    for (group, shortcuts) in crate::shortcuts::SHEET {
        let mut lines = Column::new().spacing(6);
        for shortcut in *shortcuts {
            let keys = container(text(shortcut.keys).size(12).font(fonts::MEDIUM).color(INK))
                .padding(Padding::from([2, 8]))
                .style(|_| style::panel(hexa(0xFFFFFF, 0.10), 6.0));
            lines = lines.push(
                row![
                    container(keys).width(Length::Fixed(130.0)),
                    text(shortcut.does).size(13).color(INK),
                ]
                .spacing(12)
                .align_y(Vertical::Center),
            );
        }
        groups = groups.push(
            column![
                text(*group).size(12).font(fonts::SEMIBOLD).color(MUTED),
                lines
            ]
            .spacing(8),
        );
    }
    let body = column![
        text("Keyboard Shortcuts")
            .size(18)
            .font(fonts::SEMIBOLD)
            .color(INK),
        divider(),
        groups,
        divider(),
        row![
            iced_widget::space().width(Length::Fill),
            push_button(theme, "Close", Some(Message::CloseSystemWindow), true),
        ],
    ]
    .spacing(12);
    centered(window(body, 460.0), Message::CloseSystemWindow)
}

/// Force Quit: the running agents, one to choose, and the button that
/// stops it.
pub fn force_quit_overlay<'a>(theme: &Theme, quit: &ForceQuit<'a>) -> Element<'a, Message> {
    let list: Element<'a, Message> = if quit.running.is_empty() {
        container(text("Nothing is running.").size(13).color(MUTED))
            .padding(14)
            .into()
    } else {
        let rows = quit.running.iter().map(|&task| {
            let chosen = quit.chosen == Some(task.id);
            let color = tiles::state_color(theme, task.state);
            let title = if task.title.is_empty() {
                &task.agent_name
            } else {
                &task.title
            };
            let line = row![
                container("")
                    .width(Length::Fixed(8.0))
                    .height(Length::Fixed(8.0))
                    .style(move |_| style::dot(color, 8.0)),
                text(task.agent_name.as_str())
                    .size(13)
                    .font(fonts::MEDIUM)
                    .wrapping(Wrapping::None),
                text(title.as_str())
                    .size(13)
                    .color(if chosen { INK } else { MUTED })
                    .wrapping(Wrapping::None)
                    .width(Length::Fill),
                text(state_word(task.state))
                    .size(12)
                    .color(if chosen { INK } else { MUTED }),
            ]
            .spacing(10)
            .align_y(Vertical::Center);
            let accent = theme.accent;
            button(line)
                .on_press(Message::ChooseToQuit(task.id))
                .width(Length::Fill)
                .padding(Padding::from([7.0, 10.0]))
                .style(move |_, status| {
                    let hover = matches!(status, button::Status::Hovered);
                    let background = if chosen {
                        accent
                    } else if hover {
                        hexa(0xFFFFFF, 0.06)
                    } else {
                        Color::TRANSPARENT
                    };
                    button::Style {
                        background: Some(background.into()),
                        text_color: INK,
                        border: iced_core::Border {
                            radius: 6.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                })
                .into()
        });
        Column::with_children(rows).spacing(2).padding(4).into()
    };
    let list = container(list)
        .width(Length::Fill)
        .style(|_| style::outlined(hexa(0x000000, 0.25), 8.0, 1.0, EDGE));
    let body = column![
        text("Force Quit").size(17).font(fonts::SEMIBOLD).color(INK),
        text("Choose an agent and click Force Quit. It stops at once, and what it changed can still be undone.")
            .size(13)
            .color(MUTED),
        list,
        row![
            iced_widget::space().width(Length::Fill),
            push_button(theme, "Cancel", Some(Message::CloseSystemWindow), false),
            push_button(
                theme,
                "Force Quit",
                quit.chosen.map(|_| Message::ForceQuit),
                true
            ),
        ]
        .spacing(8),
    ]
    .spacing(12);
    centered(window(body, 480.0), Message::CloseSystemWindow)
}

/// "Restart now?" or "Shut down now?", with what it means for the agents
/// still running.
pub fn power_overlay<'a>(theme: &Theme, power: Power, running: usize) -> Element<'a, Message> {
    let (title, go) = match power {
        Power::Restart => ("Restart now?", "Restart"),
        Power::ShutDown => ("Shut down now?", "Shut Down"),
    };
    let agents = match running {
        0 => "No agents are running.".to_owned(),
        1 => "The agent that's running will stop. What it changed can still be undone.".to_owned(),
        n => format!(
            "The {n} agents that are running will stop. What they changed can still be undone."
        ),
    };
    let body = column![
        text(title).size(17).font(fonts::SEMIBOLD).color(INK),
        text(agents).size(13).color(MUTED),
        row![
            iced_widget::space().width(Length::Fill),
            push_button(theme, "Cancel", Some(Message::CloseSystemWindow), false),
            push_button(theme, go, Some(Message::ConfirmPower), true),
        ]
        .spacing(8),
    ]
    .spacing(12);
    centered(window(body, 400.0), Message::CloseSystemWindow)
}

/// The desktop's last frame while the computer restarts or shuts down: the
/// screen it leaves up until the machine goes.
pub fn leaving_overlay<'a>(power: Power) -> Element<'a, Message> {
    let words = match power {
        Power::Restart => "Restarting…",
        Power::ShutDown => "Shutting down…",
    };
    container(
        column![
            icons::view(&icons::MARK, INK, 64.0, 64.0),
            text(words).size(17).font(fonts::MEDIUM).color(INK),
        ]
        .spacing(18)
        .align_x(Horizontal::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Horizontal::Center)
    .align_y(Vertical::Center)
    .style(|_| style::panel(hex(0x0B0E13), 0.0))
    .into()
}

/// How a running agent's state reads in the list.
fn state_word(state: TileState) -> &'static str {
    match state {
        TileState::Starting => "Starting",
        TileState::NeedsYou => "Needs you",
        TileState::Paused => "Paused",
        TileState::YouAreDriving => "You're driving",
        TileState::Working => "Working",
        TileState::Done => "Done",
        TileState::Failed => "Failed",
        TileState::Stopped => "Stopped",
    }
}

fn window<'a>(body: impl Into<Element<'a, Message>>, width: f32) -> Element<'a, Message> {
    container(body)
        .padding(20)
        .width(Length::Fixed(width))
        .style(|_| container::Style {
            shadow: style::shadow(0.45, 18.0, 48.0),
            ..style::outlined(PANEL, 14.0, 1.0, EDGE)
        })
        .into()
}

/// `window` in the middle of the screen over a dimmed desktop; a click on
/// the dimmed part sends `outside`.
fn centered<'a>(window: Element<'a, Message>, outside: Message) -> Element<'a, Message> {
    stack![
        mouse_area(
            container("")
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_| style::panel(hexa(0x0E1626, 0.35), 0.0)),
        )
        .on_press(outside),
        // Idle over the window, so a click on it doesn't reach the dimmed
        // part beneath (a stack hands the layer below a levitating cursor).
        container(mouse_area(window).interaction(mouse::Interaction::Idle))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn divider<'a>() -> Element<'a, Message> {
    container("")
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_| style::panel(hexa(0xFFFFFF, 0.08), 0.0))
        .into()
}

/// A dialog button; `primary` in the accent. Without a message it's
/// dimmed and does nothing.
fn push_button<'a>(
    theme: &Theme,
    label: &'static str,
    message: Option<Message>,
    primary: bool,
) -> Element<'a, Message> {
    let accent = theme.accent;
    let enabled = message.is_some();
    button(text(label).size(13).font(fonts::MEDIUM))
        .on_press_maybe(message)
        .padding(Padding::from([6.0, 16.0]))
        .style(move |_, status| {
            let fill = match (primary, enabled) {
                (true, true) => accent,
                (true, false) => Color { a: 0.35, ..accent },
                (false, _) => hexa(0xFFFFFF, 0.1),
            };
            let fill = if matches!(status, button::Status::Hovered) && enabled {
                Color {
                    a: (fill.a * 0.85).max(0.2),
                    ..fill
                }
            } else {
                fill
            };
            button::Style {
                background: Some(fill.into()),
                text_color: if enabled { INK } else { hexa(0xFFFFFF, 0.5) },
                border: iced_core::Border {
                    radius: 7.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}
