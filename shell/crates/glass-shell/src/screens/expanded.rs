//! The expanded view (AW-10 to AW-16, the Expanded artboard): what a tile
//! opens into. A header with the agent, its state and the controls that
//! apply (AW-11); the agent's work on the left with the Now bar (AW-12);
//! Activity and Changes on the right (AW-13) over what it can't touch
//! (AW-14); and the timeline with its snapshots and Roll back (AW-15).
//!
//! It is drawn from data the shell already holds: the tile's `TaskView`,
//! kept current by glassd's events, and the `TaskDetail` from `task.get`.
//! The artboard's left pane is a screen agent's live screen; screen agents
//! can't stream one yet, so the left pane shows what the person needs from
//! the data instead: the ask's items while the task needs them, the
//! receipt once it has ended, and otherwise its feed, request and plan.

use glass_api::{
    ActivityEntry, ActivityKind, ApprovalView, ChangeEntry, ChangeKind, Receipt, SnapshotMarker,
    TaskDetail, TaskView, TileState, UndoReport,
};
use glass_types::{SnapshotId, Timestamp};
use iced_core::alignment::Vertical;
use iced_core::{Background, Border, Color, Length, Padding};
use iced_widget::scrollable::{Direction, Scrollbar};
use iced_widget::{Column, Row, button, column, container, row, scrollable, text};
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

use crate::theme::TileColors;
use crate::{Element, Message, Theme, fonts, icons, style, tiles};

/// What the expanded view shows. Everything is borrowed from the shell's
/// state, so building it costs nothing.
#[derive(Debug, Clone, Copy)]
pub struct Expanded<'a> {
    /// The tile, current to the last event.
    pub task: &'a TaskView,
    /// From `task.get`: `None` until it answers.
    pub detail: Option<&'a TaskDetail>,
    pub tab: Tab,
    /// The snapshot the person picked; the latest one otherwise.
    pub selected: Option<SnapshotId>,
    pub rollback: Option<&'a Rollback>,
    /// Now, in the person's time zone: times of day are shown in its
    /// offset, and a running task's timeline ends here.
    pub now: OffsetDateTime,
}

/// The right column's tabs (AW-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Activity,
    Changes,
}

/// Rolling back to a snapshot (AW-15): `task.undo` with `dry_run` first, so
/// the person sees what would change before anything does.
#[derive(Debug, Clone, PartialEq)]
pub enum Rollback {
    /// Asking what rolling back would change.
    Checking {
        to: SnapshotId,
    },
    /// What it would change, waiting for the person to confirm.
    Preview {
        to: SnapshotId,
        report: UndoReport,
    },
    RollingBack {
        to: SnapshotId,
    },
    /// What it changed.
    Done {
        to: SnapshotId,
        report: UndoReport,
    },
    Failed {
        to: SnapshotId,
        message: String,
    },
}

impl Rollback {
    pub fn to(&self) -> SnapshotId {
        match self {
            Rollback::Checking { to }
            | Rollback::Preview { to, .. }
            | Rollback::RollingBack { to }
            | Rollback::Done { to, .. }
            | Rollback::Failed { to, .. } => *to,
        }
    }

    /// Whether a call is out for it, so nothing should start another.
    pub fn in_flight(&self) -> bool {
        matches!(
            self,
            Rollback::Checking { .. } | Rollback::RollingBack { .. }
        )
    }
}

/// A header control (AW-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause,
    Resume,
    Stop,
    TakeOver,
    HandBack,
}

impl Control {
    pub fn label(self) -> &'static str {
        match self {
            Control::Pause => "Pause",
            Control::Resume => "Resume",
            Control::Stop => "Stop",
            Control::TakeOver => "Take over",
            Control::HandBack => "Hand back",
        }
    }

    fn icon(self) -> &'static icons::Icon {
        match self {
            Control::Pause => &icons::PAUSE,
            Control::Resume => &icons::RESUME,
            Control::Stop => &icons::STOP,
            Control::TakeOver => &icons::POINTER,
            Control::HandBack => &icons::HAND_BACK,
        }
    }

    /// Take over and Hand back are the header's primary button.
    fn primary(self) -> bool {
        matches!(self, Control::TakeOver | Control::HandBack)
    }
}

/// The header's controls for a task (AW-11), each with the message it
/// sends, or `None` when it's shown but doesn't apply right now. Pause
/// turns into Resume while paused, Take over into Hand back while the
/// person drives; a task that has ended has none.
pub fn controls(task: &TaskView) -> Vec<(Control, Option<Message>)> {
    if !task.state.is_live() {
        return Vec::new();
    }
    let id = task.id;
    let hold = match task.state {
        TileState::Paused => (Control::Resume, Some(Message::Resume(id))),
        // Driving already holds the agent; Hand back lets it go on.
        TileState::YouAreDriving => (Control::Pause, None),
        _ => (Control::Pause, Some(Message::Pause(id))),
    };
    let take = if task.state == TileState::YouAreDriving {
        (Control::HandBack, Some(Message::HandBack(id)))
    } else {
        // The person's own harness drives an attended task already.
        let attended = task.driver_socket.is_some();
        (
            Control::TakeOver,
            (!attended).then_some(Message::TakeOver(id)),
        )
    };
    vec![hold, (Control::Stop, Some(Message::Stop(id))), take]
}

/// The snapshot Roll back returns to: the one picked, or the latest.
pub fn target(
    snapshots: &[SnapshotMarker],
    selected: Option<SnapshotId>,
) -> Option<&SnapshotMarker> {
    selected
        .and_then(|id| snapshots.iter().find(|s| s.id == id))
        .or_else(|| snapshots.last())
}

/// The state chip: "Working · 12 of 40".
pub fn chip(task: &TaskView) -> String {
    match task.progress {
        Some(p) => format!("{} · {} of {}", task.state.label(), p.done, p.total),
        None => task.state.label().to_owned(),
    }
}

/// The line under the name: "Reconciling March invoices · on behalf of you
/// · started 2:02 PM", and when it ended.
pub fn caption(task: &TaskView, offset: UtcOffset) -> String {
    let mut parts = Vec::new();
    if !task.caption.trim().is_empty() {
        parts.push(task.caption.trim().to_owned());
    }
    // A shell sees only its own user's tasks.
    parts.push("on behalf of you".to_owned());
    parts.push(format!("started {}", time_of_day(task.started_at, offset)));
    let ended = match task.state {
        TileState::Done => Some("finished"),
        TileState::Failed => Some("failed"),
        TileState::Stopped => Some("stopped"),
        _ => None,
    };
    if let Some(ended) = ended {
        parts.push(format!("{ended} {}", time_of_day(task.updated_at, offset)));
    }
    parts.join(" · ")
}

/// "2:02 PM" in the person's time zone.
pub fn time_of_day(at: Timestamp, offset: UtcOffset) -> String {
    local(at, offset)
        .format(format_description!(
            "[hour repr:12 padding:none]:[minute] [period]"
        ))
        .unwrap_or_default()
}

/// "2:02", for the activity log's time column.
fn short_time(at: Timestamp, offset: UtcOffset) -> String {
    local(at, offset)
        .format(format_description!("[hour repr:12 padding:none]:[minute]"))
        .unwrap_or_default()
}

fn local(at: Timestamp, offset: UtcOffset) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(at.unix_seconds())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(offset)
}

/// The menu bar's menus while an agent is expanded.
pub const MENUS: [&str; 4] = ["Agent", "View", "Window", "Help"];

/// Where the view sits on the desktop: between the menu bar and the dock.
const INSET: Padding = Padding {
    top: 43.0,
    right: 47.0,
    bottom: 91.0,
    left: 47.0,
};
const HEADER: f32 = 76.0;
const FOOTER: f32 = 84.0;
/// The right column, without its rule.
const SIDE: f32 = 360.0;
/// Rows kept in the Activity and Changes lists. Long tasks keep their whole
/// log in the audit trail; the view draws the latest.
const MOST_ROWS: usize = 200;
/// Files listed per group on the roll back card.
const MOST_FILES: usize = 6;

/// The expanded view in its place on the desktop, for the desktop's overlay
/// layer (over windows and tiles, under the menu bar and dock).
pub fn overlay<'a>(theme: &Theme, state: &Expanded<'a>) -> Element<'a, Message> {
    container(view(theme, state))
        .padding(INSET)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The expanded view's panel.
pub fn view<'a>(theme: &Theme, state: &Expanded<'a>) -> Element<'a, Message> {
    let t = theme.tile;
    let body = row![
        container(column![main(theme, state), now_bar(t, state)].spacing(12))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20),
        rule_v(t),
        side(theme, state),
    ]
    .height(Length::Fill);
    container(column![
        header(theme, state),
        rule_h(t),
        body,
        rule_h(t),
        footer(theme, state),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .style(move |_| iced_widget::container::Style {
        text_color: Some(t.text),
        shadow: style::shadow(0.35, 18.0, 48.0),
        ..style::outlined(t.background, 16.0, 1.0, t.outline)
    })
    .into()
}

fn header<'a>(theme: &Theme, state: &Expanded<'a>) -> Element<'a, Message> {
    let t = theme.tile;
    let task = state.task;
    let collapse = button(
        container(icons::view(&icons::COLLAPSE, t.text, 16.0, 16.0))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .on_press(Message::CloseTask)
    .width(Length::Fixed(44.0))
    .height(Length::Fixed(44.0))
    .padding(0)
    .style(style::button_like(style::panel(t.raised, 10.0)));
    let color = tiles::state_color(theme, task.state);
    let chip = container(
        row![
            container("")
                .width(Length::Fixed(6.0))
                .height(Length::Fixed(6.0))
                .style(move |_| style::dot(color, 6.0)),
            text(chip(task)).size(13).font(fonts::MEDIUM).color(t.text),
        ]
        .spacing(7)
        .align_y(Vertical::Center),
    )
    .padding(Padding::from([3.0, 11.0]))
    .style(move |_| style::outlined(t.raised, 999.0, 1.0, t.chip_border));
    let title = column![
        row![
            text(task.agent_name.as_str())
                .size(18)
                .font(fonts::SEMIBOLD)
                .color(t.text),
            chip,
        ]
        .spacing(10)
        .align_y(Vertical::Center),
        text(caption(task, state.now.offset()))
            .size(14)
            .color(t.muted)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(4);
    let mut buttons = Row::new().spacing(8).align_y(Vertical::Center);
    for (control, message) in controls(task) {
        buttons = buttons.push(control_button(theme, control, message));
    }
    // The title takes what the buttons leave, so a long caption never
    // pushes them off.
    container(
        row![
            collapse,
            container(title).width(Length::Fill).clip(true),
            buttons
        ]
        .spacing(12)
        .align_y(Vertical::Center),
    )
    .padding(Padding {
        top: 0.0,
        right: 20.0,
        bottom: 0.0,
        left: 12.0,
    })
    .width(Length::Fill)
    .center_y(Length::Fixed(HEADER))
    .into()
}

fn control_button<'a>(
    theme: &Theme,
    control: Control,
    message: Option<Message>,
) -> Element<'a, Message> {
    let t = theme.tile;
    let enabled = message.is_some();
    let ink = match (control.primary(), enabled) {
        (_, false) => Color { a: 0.4, ..t.text },
        (true, true) => Color::WHITE,
        (false, true) => t.text,
    };
    let look = match (control.primary(), enabled) {
        (true, true) => style::panel(theme.accent, 8.0),
        (true, false) => style::panel(t.raised, 8.0),
        (false, _) => style::outlined(Color::TRANSPARENT, 8.0, 1.0, t.chip_border),
    };
    button(
        container(
            row![
                icons::view(control.icon(), ink, 15.0, 15.0),
                text(control.label())
                    .size(15)
                    .font(if control.primary() {
                        fonts::SEMIBOLD
                    } else {
                        fonts::MEDIUM
                    })
                    .color(ink),
            ]
            .spacing(9)
            .align_y(Vertical::Center),
        )
        .center_y(Length::Fill),
    )
    .on_press_maybe(message)
    .height(Length::Fixed(44.0))
    .padding(Padding::from([0.0, 18.0]))
    .style(style::button_like(look))
    .into()
}

/// The left pane: the roll back card while one is open, else what the
/// person needs from the agent right now.
fn main<'a>(theme: &Theme, state: &Expanded<'a>) -> Element<'a, Message> {
    let content = if let Some(rollback) = state.rollback {
        rollback_card(theme, state, rollback)
    } else {
        match (state.task.state, state.task.needs.as_ref()) {
            (TileState::NeedsYou, Some(ask)) => ask_pane(theme, state, ask),
            (s, _) if !s.is_live() => match state.task.receipt.as_ref() {
                Some(receipt) => receipt_pane(theme, state, receipt),
                None => work_pane(theme, state),
            },
            _ => work_pane(theme, state),
        }
    };
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .into()
}

/// A working agent: what it's doing, what it was asked and its plan.
fn work_pane<'a>(theme: &Theme, state: &Expanded<'a>) -> Element<'a, Message> {
    let t = theme.tile;
    let task = state.task;
    let mut pane = Column::new().spacing(20);
    if task.state == TileState::YouAreDriving {
        let accent = theme.accent;
        pane = pane.push(
            container(
                column![
                    text("You have control")
                        .size(15)
                        .font(fonts::SEMIBOLD)
                        .color(t.text),
                    text(
                        "The agent is paused at a safe point. Make your changes, then \
                         Hand back: it says what you changed and carries on from there."
                    )
                    .size(14)
                    .color(t.caption)
                    .line_height(1.4),
                ]
                .spacing(6),
            )
            .width(Length::Fill)
            .padding(Padding::from([14.0, 16.0]))
            .style(move |_| style::outlined(t.raised, 12.0, 1.0, accent)),
        );
    }
    let count = task.progress.map(|p| -> Element<'a, Message> {
        text(format!("{} of {}", p.done, p.total))
            .size(13)
            .font(fonts::MONO)
            .color(t.muted)
            .into()
    });
    let mut doing = column![
        row![
            section(t, "What it's doing"),
            iced_widget::space().width(Length::Fill),
        ]
        .extend(count)
        .align_y(Vertical::Center),
    ]
    .spacing(12);
    if task.feed.is_empty() {
        doing = doing.push(text(task.caption.as_str()).size(14).color(t.caption));
    } else {
        doing = doing.push(feed(theme, task));
    }
    if let Some(p) = task.progress {
        doing = doing.push(progress(theme, p.done, p.total));
    }
    pane = pane.push(
        container(doing)
            .width(Length::Fill)
            .padding(Padding::from([16.0, 18.0]))
            .style(move |_| style::panel(t.raised, 12.0)),
    );
    if let Some(detail) = state.detail {
        if !detail.prompt.trim().is_empty() {
            pane = pane.push(
                column![
                    section(t, "You asked"),
                    text(detail.prompt.as_str())
                        .size(15)
                        .color(t.caption)
                        .line_height(1.45),
                ]
                .spacing(8),
            );
        }
        if !detail.plan.is_empty() {
            let steps = detail.plan.iter().enumerate().map(|(i, step)| {
                row![
                    container(
                        text((i + 1).to_string())
                            .size(13)
                            .font(fonts::MONO)
                            .color(t.muted)
                    )
                    .width(Length::Fixed(20.0)),
                    text(step.as_str()).size(14).color(t.text),
                ]
                .spacing(8)
                .into()
            });
            pane = pane.push(
                column![section(t, "Plan"), Column::with_children(steps).spacing(8)].spacing(10),
            );
        }
    }
    quiet_scroll(t, pane.into())
}

/// The agent's last feed lines, verb in its own column; the newest is lit
/// and ends in the accent cursor while the agent runs (AW-3).
fn feed<'a>(theme: &Theme, task: &'a TaskView) -> Element<'a, Message> {
    let t = theme.tile;
    let last = task.feed.len().saturating_sub(1);
    let lines = task.feed.iter().enumerate().map(|(i, line)| {
        let newest = i == last && task.state.is_live();
        let (verb, target) = if line.verb.is_empty() {
            ("", line.text.as_str())
        } else {
            (line.verb.as_str(), line.target.as_str())
        };
        let mut target_row = row![text(target).size(14).font(fonts::MONO).color(if newest {
            t.text
        } else {
            t.feed_text
        })]
        .spacing(4)
        .align_y(Vertical::Center);
        if newest {
            let accent = theme.accent;
            target_row = target_row.push(
                container("")
                    .width(Length::Fixed(8.0))
                    .height(Length::Fixed(16.0))
                    .style(move |_| style::panel(accent, 0.0)),
            );
        }
        row![
            container(text(verb).size(14).font(fonts::MONO).color(if newest {
                t.feed_verb
            } else {
                t.feed_text
            }))
            .width(Length::Fixed(56.0)),
            target_row,
        ]
        .spacing(10)
        .into()
    });
    Column::with_children(lines).spacing(8).into()
}

fn progress<'a>(theme: &Theme, done: u32, total: u32) -> Element<'a, Message> {
    let fraction = if total == 0 {
        0.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    };
    let filled = (fraction * 1000.0).round() as u16;
    let accent = theme.accent;
    let track = theme.tile.track;
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

/// A task that needs the person: the ask, everything it covers, and its
/// own answers (DA-12). The list is the review; approving answers once.
fn ask_pane<'a>(
    theme: &Theme,
    state: &Expanded<'a>,
    ask: &'a ApprovalView,
) -> Element<'a, Message> {
    let t = theme.tile;
    let offset = state.now.offset();
    let asked = row![
        text("NEEDS YOU")
            .size(12)
            .font(fonts::SEMIBOLD)
            .color(theme.attention),
        text(format!("asked {}", time_of_day(ask.requested_at, offset)))
            .size(12)
            .color(t.muted),
    ]
    .spacing(8);
    let items = ask.items.iter().map(|item| {
        container(text(item.as_str()).size(14).color(t.text))
            .width(Length::Fill)
            .padding(Padding::from([11.0, 14.0]))
            .style(move |_| style::panel(t.raised, 8.0))
            .into()
    });
    let count = match ask.items.len() {
        1 => "1 item".to_owned(),
        n => format!("{n} items"),
    };
    let list = column![
        row![
            section(t, &ask.labels.review),
            text(count).size(12).color(t.muted)
        ]
        .spacing(8)
        .align_y(Vertical::Center),
        quiet_scroll(t, Column::with_children(items).spacing(6).into()),
    ]
    .spacing(10)
    .height(Length::Fill);
    let decline = button(
        container(text("Decline").size(14).font(fonts::MEDIUM).color(t.text))
            .center_y(Length::Fill),
    )
    .on_press(Message::Decline(ask.id))
    .height(Length::Fixed(44.0))
    .padding(Padding::from([0.0, 18.0]))
    .style(style::button_like(style::outlined(
        Color::TRANSPARENT,
        10.0,
        1.0,
        t.button_border,
    )));
    let approve = button(
        container(
            text(ask.labels.approve.as_str())
                .size(14)
                .font(fonts::SEMIBOLD)
                .color(t.background),
        )
        .center_y(Length::Fill),
    )
    .on_press(Message::Approve(ask.id))
    .height(Length::Fixed(44.0))
    .padding(Padding::from([0.0, 20.0]))
    .style(style::button_like(style::panel(t.text, 10.0)));
    column![
        asked,
        text(ask.summary.as_str())
            .size(22)
            .font(fonts::MEDIUM)
            .color(t.text),
        iced_widget::space().height(Length::Fixed(4.0)),
        list,
        row![
            text(format!("Asks because of {}", ask.rule))
                .size(12)
                .color(t.muted)
                .width(Length::Fill),
            decline,
            approve,
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    ]
    .spacing(12)
    .into()
}

/// A task that ended: what it did, what's safe, what to look at.
fn receipt_pane<'a>(
    theme: &Theme,
    state: &Expanded<'a>,
    receipt: &'a Receipt,
) -> Element<'a, Message> {
    let t = theme.tile;
    let task = state.task;
    let when = time_of_day(task.updated_at, state.now.offset());
    let title = match task.state {
        TileState::Done => format!("Finished at {when}"),
        TileState::Failed => format!("Failed at {when}"),
        _ => format!("Stopped at {when}"),
    };
    let mut pane = column![text(title).size(22).font(fonts::MEDIUM).color(t.text)].spacing(12);
    if let Some(failure) = &task.failure {
        pane = pane.push(text(failure.as_str()).size(15).color(t.text));
    }
    // What the agent told the person (AE-3, ADR 0041), first: it's what
    // they asked for.
    if let Some(answer) = state.detail.and_then(|d| d.answer.as_deref()) {
        pane = pane.push(
            container(
                Column::with_children(blocks(answer).into_iter().map(|block| {
                    match block {
                        Block::Heading(h) => {
                            text(h).size(16).font(fonts::MEDIUM).color(t.text).into()
                        }
                        Block::Paragraph(p) => {
                            text(p).size(15).color(t.text).line_height(1.5).into()
                        }
                        Block::Item { mark, text: item } => row![
                            text(mark)
                                .size(15)
                                .color(t.caption)
                                .width(Length::Fixed(22.0)),
                            text(item).size(15).color(t.text).line_height(1.5),
                        ]
                        .into(),
                        Block::Code(line) => {
                            text(line).size(13).font(fonts::MONO).color(t.text).into()
                        }
                    }
                }))
                .spacing(10),
            )
            .width(Length::Fill)
            .padding(Padding::from([14.0, 16.0]))
            .style(move |_| style::panel(t.raised, 12.0)),
        );
    }
    pane = pane.push(text(receipt.outcome()).size(15).color(t.caption));
    let stat = |label: &'static str, n: u32| -> Element<'a, Message> {
        container(
            column![
                text(label).size(12).color(t.muted),
                text(n.to_string()).size(22).font(fonts::MONO).color(t.text),
            ]
            .spacing(4),
        )
        .width(Length::Fill)
        .padding(Padding::from([12.0, 14.0]))
        .style(move |_| style::panel(t.raised, 10.0))
        .into()
    };
    pane = pane.push(
        row![
            stat("Actions", receipt.actions),
            stat("Changes", receipt.changes),
            stat("Can be undone", receipt.undoable),
            stat("Left the machine", receipt.left_machine),
        ]
        .spacing(8),
    );
    if !receipt.flagged.is_empty() {
        let flagged = receipt.flagged.iter().map(|line| {
            container(text(line.as_str()).size(14).color(t.text))
                .width(Length::Fill)
                .padding(Padding::from([11.0, 14.0]))
                .style(move |_| style::panel(t.raised, 8.0))
                .into()
        });
        pane = pane.push(
            column![
                section(t, "Flagged for you"),
                Column::with_children(flagged).spacing(6)
            ]
            .spacing(10),
        );
    }
    quiet_scroll(t, pane.into())
}

/// A piece of an answer as it's shown. Agents are asked for plain text,
/// but models write Markdown anyway, and Claude Code's answers are
/// Markdown: its headings, lists and code show as such, never as `#`, `-`
/// and backticks.
#[derive(Debug, PartialEq)]
enum Block {
    Heading(String),
    /// Lines joined where a paragraph wraps.
    Paragraph(String),
    /// A list item and its mark: "•", or its number ("2.").
    Item {
        mark: String,
        text: String,
    },
    /// A line of a code block, as written.
    Code(String),
}

fn blocks(answer: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut code = false;
    let flush = |paragraph: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(inline(&paragraph.join(" "))));
            paragraph.clear();
        }
    };
    for line in answer.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            flush(&mut paragraph, &mut blocks);
            code = !code;
            continue;
        }
        if code {
            blocks.push(Block::Code(line.trim_end().to_owned()));
            continue;
        }
        let heading = trimmed
            .trim_start_matches('#')
            .strip_prefix(' ')
            .filter(|_| {
                trimmed.starts_with('#')
                    && trimmed.len() - trimmed.trim_start_matches('#').len() <= 6
            });
        let number = trimmed.split_once(['.', ')']).filter(|(n, rest)| {
            !n.is_empty()
                && n.len() <= 3
                && n.bytes().all(|b| b.is_ascii_digit())
                && rest.starts_with(' ')
        });
        if trimmed.is_empty() || trimmed.chars().all(|c| c == '-' || c == '*') && trimmed.len() >= 3
        {
            flush(&mut paragraph, &mut blocks);
        } else if let Some(heading) = heading {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading(inline(heading.trim())));
        } else if let Some(item) = ["- ", "* ", "+ ", "• "]
            .iter()
            .find_map(|m| trimmed.strip_prefix(m))
        {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Item {
                mark: "•".into(),
                text: inline(item.trim()),
            });
        } else if let Some((n, rest)) = number {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Item {
                mark: format!("{n}."),
                text: inline(rest.trim()),
            });
        } else if let (Some(Block::Item { text, .. }), true, true) = (
            blocks.last_mut(),
            paragraph.is_empty(),
            line.starts_with([' ', '\t']),
        ) {
            // An item's text wrapped onto an indented line.
            text.push(' ');
            text.push_str(&inline(trimmed));
        } else {
            paragraph.push(trimmed);
        }
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

/// Markdown's inline marks taken off: bold, code, and a link as its text
/// and address.
fn inline(text: &str) -> String {
    let mut out = text.replace("**", "").replace("__", "").replace('`', "");
    while let Some(open) = out.find('[') {
        let Some(close) = out[open..].find("](").map(|i| open + i) else {
            break;
        };
        let Some(end) = out[close..].find(')').map(|i| close + i) else {
            break;
        };
        let (label, url) = (&out[open + 1..close], &out[close + 2..end]);
        let shown = if label == url {
            label.to_owned()
        } else {
            format!("{label} ({url})")
        };
        out.replace_range(open..=end, &shown);
    }
    out
}

/// Rolling back (AW-15): what it would change, then what it changed.
fn rollback_card<'a>(
    theme: &Theme,
    state: &Expanded<'a>,
    rollback: &'a Rollback,
) -> Element<'a, Message> {
    let t = theme.tile;
    let offset = state.now.offset();
    let snapshots = state.detail.map_or(&[][..], |d| d.snapshots.as_slice());
    let when = snapshots
        .iter()
        .find(|s| s.id == rollback.to())
        .map_or_else(|| "the snapshot".to_owned(), |s| time_of_day(s.at, offset));
    let quiet = |s: String| text(s).size(14).color(t.caption).line_height(1.4);
    let mut card = Column::new().spacing(14);
    let mut buttons = Row::new().spacing(8);
    match rollback {
        Rollback::Checking { .. } => {
            card = card
                .push(heading(t, format!("Roll back to {when}")))
                .push(quiet("Checking what it would change…".to_owned()));
        }
        Rollback::Preview { report, .. } => {
            card = card
                .push(heading(t, format!("Roll back to {when}?")))
                .push(quiet(if is_empty(report) {
                    "Nothing has changed since then, so there's nothing to undo.".to_owned()
                } else {
                    "Nothing has changed yet. Rolling back:".to_owned()
                }));
            card = report_lists(theme, card, report, false);
            if matches!(state.task.state, TileState::Working | TileState::NeedsYou) {
                card = card.push(quiet(
                    "The agent pauses first, and waits until you resume it.".to_owned(),
                ));
            }
            buttons = buttons
                .push(secondary(t, "Cancel", Message::CancelRollBack))
                .push(primary(t, "Roll back", Message::ConfirmRollBack));
        }
        Rollback::RollingBack { .. } => {
            card = card
                .push(heading(t, format!("Rolling back to {when}…")))
                .push(quiet("Putting files back.".to_owned()));
        }
        Rollback::Done { report, .. } => {
            card = card.push(heading(t, format!("Rolled back to {when}")));
            if is_empty(report) {
                card = card.push(quiet("Nothing needed undoing.".to_owned()));
            }
            card = report_lists(theme, card, report, true);
            buttons = buttons.push(primary(t, "Done", Message::CancelRollBack));
        }
        Rollback::Failed { message, .. } => {
            card = card
                .push(heading(t, format!("Couldn't roll back to {when}")))
                .push(quiet(message.clone()));
            buttons = buttons.push(secondary(t, "Close", Message::CancelRollBack));
        }
    }
    column![quiet_scroll(t, card.into()), buttons]
        .spacing(16)
        .into()
}

fn is_empty(report: &UndoReport) -> bool {
    report.restored.is_empty() && report.removed.is_empty() && report.not_undone.is_empty()
}

/// The report's three lists: files put back, files removed, and what stays.
fn report_lists<'a>(
    theme: &Theme,
    mut card: Column<'a, Message, iced_widget::Theme, iced_widget::Renderer>,
    report: &'a UndoReport,
    done: bool,
) -> Column<'a, Message, iced_widget::Theme, iced_widget::Renderer> {
    let t = theme.tile;
    let groups: [(&str, &str, &'a [String], Color); 3] = [
        (
            if done { "Put back" } else { "Puts back" },
            "file",
            &report.restored,
            t.text,
        ),
        (
            if done { "Removed" } else { "Removes" },
            "file",
            &report.removed,
            t.text,
        ),
        (
            if done { "Not undone" } else { "Can't undo" },
            "change",
            &report.not_undone,
            theme.attention,
        ),
    ];
    for (title, noun, lines, color) in groups {
        if lines.is_empty() {
            continue;
        }
        let n = lines.len();
        let mut list = column![
            text(format!(
                "{title} {n} {noun}{}",
                if n == 1 { "" } else { "s" }
            ))
            .size(13)
            .font(fonts::SEMIBOLD)
            .color(color)
        ]
        .spacing(6);
        for line in lines.iter().take(MOST_FILES) {
            list = list.push(
                text(line.as_str())
                    .size(13)
                    .font(fonts::MONO)
                    .color(t.caption),
            );
        }
        if n > MOST_FILES {
            list = list.push(
                text(format!("and {} more", n - MOST_FILES))
                    .size(13)
                    .color(t.muted),
            );
        }
        card = card.push(
            container(list)
                .width(Length::Fill)
                .padding(Padding::from([12.0, 14.0]))
                .style(move |_| style::panel(t.raised, 10.0)),
        );
    }
    card
}

/// The Now bar (AW-12): the current step in a full sentence.
fn now_bar<'a>(t: TileColors, state: &Expanded<'a>) -> Element<'a, Message> {
    let now = state
        .detail
        .map_or(state.task.caption.as_str(), |d| d.now.as_str());
    container(
        row![
            text("Now").size(14).color(t.muted),
            text(now).size(14).color(t.text)
        ]
        .spacing(6),
    )
    .width(Length::Fill)
    .padding(Padding::from([15.0, 16.0]))
    .style(move |_| style::panel(t.raised, 10.0))
    .into()
}

/// The most room Can't touch takes: six lines.
const CANT_TOUCH_HEIGHT: f32 = 110.0;

/// The right column: Activity or Changes over Can't touch.
fn side<'a>(theme: &Theme, state: &Expanded<'a>) -> Element<'a, Message> {
    let t = theme.tile;
    let changes = state.detail.map_or(0, |d| d.changes.len());
    let tab = |label: String, tab: Tab| -> Element<'a, Message> {
        let on = state.tab == tab;
        button(
            text(label)
                .size(14)
                .font(fonts::MEDIUM)
                .color(if on { t.text } else { t.muted }),
        )
        .on_press(Message::ShowTab(tab))
        .padding(Padding::from([7.0, 14.0]))
        .style(style::button_like(style::panel(
            if on { t.rule } else { Color::TRANSPARENT },
            8.0,
        )))
        .into()
    };
    let tabs = row![
        tab("Activity".to_owned(), Tab::Activity),
        tab(format!("Changes · {changes}"), Tab::Changes),
    ]
    .spacing(4);
    let list = match (state.detail, state.tab) {
        (None, _) => text("Loading…").size(14).color(t.muted).into(),
        (Some(detail), Tab::Activity) => activity(theme, state, detail),
        (Some(detail), Tab::Changes) => changes_list(theme, state, detail),
    };
    let mut column = column![tabs, container(list).height(Length::Fill)].spacing(14);
    if let Some(detail) = state.detail.filter(|d| !d.cant_touch.is_empty()) {
        column = column.push(
            container(
                column![
                    text("Can't touch")
                        .size(14)
                        .font(fonts::MEDIUM)
                        .color(t.text),
                    // An agent with narrow rules can't touch dozens of
                    // things: they scroll, so Activity keeps its room.
                    container(
                        scroll(
                            t,
                            text(detail.cant_touch.join(", "))
                                .size(13)
                                .color(t.muted)
                                .line_height(1.4)
                                .into(),
                        )
                        .height(Length::Shrink),
                    )
                    .max_height(CANT_TOUCH_HEIGHT),
                ]
                .spacing(4),
            )
            .width(Length::Fill)
            .padding(Padding::from([12.0, 14.0]))
            .style(move |_| style::panel(t.raised, 10.0)),
        );
    }
    container(column)
        .width(Length::Fixed(SIDE))
        .height(Length::Fill)
        .padding(Padding {
            top: 18.0,
            right: 16.0,
            bottom: 16.0,
            left: 16.0,
        })
        .into()
}

/// The activity log, newest at the bottom, snapshots marked ◇ and the one
/// Roll back would return to ◆ (AW-13).
fn activity<'a>(
    theme: &Theme,
    state: &Expanded<'a>,
    detail: &'a TaskDetail,
) -> Element<'a, Message> {
    let t = theme.tile;
    let offset = state.now.offset();
    let target = target(&detail.snapshots, state.selected).map(|s| s.id);
    // Snapshot lines and timeline markers are recorded together, in order.
    let mut markers = detail.snapshots.iter();
    let rows: Vec<(&'a ActivityEntry, Option<SnapshotId>)> = detail
        .activity
        .iter()
        .map(|entry| {
            let marker = (entry.kind == ActivityKind::Snapshot)
                .then(|| markers.next().map(|m| m.id))
                .flatten();
            (entry, marker)
        })
        .collect();
    let skip = rows.len().saturating_sub(MOST_ROWS);
    let last = rows.len().saturating_sub(1);
    let working = state.task.state == TileState::Working;
    let lines = rows
        .into_iter()
        .enumerate()
        .skip(skip)
        .map(|(i, (entry, marker))| {
            let selected = marker.is_some() && marker == target;
            let time = time_column(t, entry.at, offset);
            let words = text(if selected {
                format!("{} · selected", entry.text)
            } else {
                entry.text.clone()
            })
            .size(14)
            .line_height(1.15)
            .color(t.text);
            let content: Element<'a, Message> = if entry.kind == ActivityKind::Snapshot {
                let icon = if selected {
                    &icons::SNAPSHOT_SELECTED
                } else {
                    &icons::SNAPSHOT
                };
                row![icons::view(icon, t.text, 12.0, 12.0), words]
                    .spacing(8)
                    .align_y(Vertical::Center)
                    .into()
            } else if i == last && working {
                let accent = theme.accent;
                row![
                    container("")
                        .width(Length::Fixed(6.0))
                        .height(Length::Fixed(6.0))
                        .style(move |_| style::dot(accent, 6.0)),
                    words
                ]
                .spacing(9)
                .align_y(Vertical::Center)
                .into()
            } else {
                words.into()
            };
            let line = row![time, content].spacing(12);
            let look = if selected {
                style::outlined(t.raised, 8.0, 1.0, t.chip_border)
            } else {
                style::panel(Color::TRANSPARENT, 8.0)
            };
            button(line)
                .on_press_maybe(marker.map(Message::SelectSnapshot))
                .width(Length::Fill)
                .padding(Padding::from([10.0, 8.0]))
                .style(style::button_like(look))
                .into()
        });
    log_scroll(t, Column::with_children(lines).spacing(2).into())
}

/// What the agent changed, newest at the bottom, with the changes that
/// can't be undone flagged (AW-13).
fn changes_list<'a>(
    theme: &Theme,
    state: &Expanded<'a>,
    detail: &'a TaskDetail,
) -> Element<'a, Message> {
    let t = theme.tile;
    if detail.changes.is_empty() {
        return text("Nothing changed yet.").size(14).color(t.muted).into();
    }
    let offset = state.now.offset();
    let skip = detail.changes.len().saturating_sub(MOST_ROWS);
    let lines = detail.changes.iter().skip(skip).map(|change| {
        let flag: Option<(&str, Color)> = if change.undone {
            Some(("Undone", t.muted))
        } else if !change.reversible {
            Some(("Can't be undone", theme.attention))
        } else {
            None
        };
        let mut about = row![
            text(format!("{} · {}", kind(change), change.action))
                .size(12)
                .color(t.muted)
        ]
        .spacing(6);
        if let Some((label, color)) = flag {
            about = about
                .push(text("·").size(12).color(t.muted))
                .push(text(label).size(12).font(fonts::MEDIUM).color(color));
        }
        let what = column![
            text(change.target.as_str())
                .size(14)
                .line_height(1.15)
                .color(if change.undone { t.muted } else { t.text }),
            about,
        ]
        .spacing(3)
        .width(Length::Fill);
        let line = row![time_column(t, change.at, offset), what].spacing(12);
        container(line)
            .width(Length::Fill)
            .padding(Padding::from([9.0, 8.0]))
            .into()
    });
    log_scroll(t, Column::with_children(lines).spacing(2).into())
}

/// "2:02" in a log's time column.
fn time_column<'a>(t: TileColors, at: Timestamp, offset: UtcOffset) -> Element<'a, Message> {
    container(
        text(short_time(at, offset))
            .size(12)
            .font(fonts::MONO)
            .color(t.muted),
    )
    .width(Length::Fixed(36.0))
    .padding(Padding {
        top: 1.0,
        ..Padding::ZERO
    })
    .into()
}

fn kind(change: &ChangeEntry) -> &'static str {
    match change.kind {
        ChangeKind::Created => "Created",
        ChangeKind::Modified => "Changed",
        ChangeKind::Deleted => "Deleted",
        ChangeKind::Moved => "Moved",
        ChangeKind::Sent => "Sent",
    }
}

/// The timeline (AW-15): from the start to now, a marker per snapshot, lit
/// up to the one Roll back would return to; and the Roll back button.
fn footer<'a>(theme: &Theme, state: &Expanded<'a>) -> Element<'a, Message> {
    let t = theme.tile;
    let task = state.task;
    let offset = state.now.offset();
    let snapshots = state.detail.map_or(&[][..], |d| d.snapshots.as_slice());
    let target = target(snapshots, state.selected);
    let start = task.started_at.unix_seconds();
    let end = if task.state.is_live() {
        state.now.unix_timestamp()
    } else {
        task.updated_at.unix_seconds()
    }
    .max(start + 1);
    let place = |at: Timestamp| (at.unix_seconds() - start) as f64 / (end - start) as f64;

    // The track is segments between markers, each as wide as the time it
    // covers; segments before the target are lit.
    let mut track = Row::new().align_y(Vertical::Center).height(28);
    let mut from = 0.0_f64;
    let mut lit = target.is_some();
    let accent = theme.accent;
    let segment = |share: f64, color: Color| {
        let portion = (share.clamp(0.0, 1.0) * 1000.0).round() as u16;
        container("")
            .width(Length::FillPortion(portion.max(1)))
            .height(Length::Fixed(4.0))
            .style(move |_| style::panel(color, 0.0))
    };
    // The line runs on under each marker: lit on both sides, or only on
    // the left of the target.
    let half = |color: Color| {
        container("")
            .width(Length::Fill)
            .height(Length::Fixed(4.0))
            .style(move |_| style::panel(color, 0.0))
    };
    for marker in snapshots {
        let at = place(marker.at).clamp(from, 1.0);
        let before = if lit { accent } else { t.rail };
        track = track.push(segment(at - from, before));
        let selected = target.is_some_and(|s| s.id == marker.id);
        if selected {
            lit = false;
        }
        let after = if lit { accent } else { t.rail };
        // ◆ for the target; ◇ filled with the panel's color for the rest, so
        // the line doesn't show through.
        let diamond: Element<'a, Message> = if selected {
            icons::view(&icons::SNAPSHOT_SELECTED, t.text, 16.0, 16.0)
        } else {
            iced_widget::stack![
                icons::view(&icons::SNAPSHOT_SELECTED, t.background, 12.0, 12.0),
                icons::view(&icons::SNAPSHOT, t.text, 12.0, 12.0),
            ]
            .into()
        };
        let cell = iced_widget::stack![
            container(row![half(before), half(after)])
                .center_y(Length::Fill)
                .width(Length::Fill),
            container(diamond)
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        ];
        track = track.push(
            button(cell)
                .on_press(Message::SelectSnapshot(marker.id))
                .width(Length::Fixed(18.0))
                .height(Length::Fixed(20.0))
                .padding(0)
                .style(style::button_like(style::panel(Color::TRANSPARENT, 0.0))),
        );
        from = at;
    }
    let tail = if snapshots.is_empty() { accent } else { t.rail };
    track = track.push(segment(1.0 - from, if lit { accent } else { tail }));
    // Now: a knob with a soft ring.
    let halo = Color { a: 0.55, ..t.rail };
    track = track.push(
        container("")
            .width(Length::Fixed(28.0))
            .height(Length::Fixed(28.0))
            .style(move |_| style::outlined(t.text, 14.0, 4.0, halo)),
    );
    let end_label = if task.state.is_live() {
        "Now".to_owned()
    } else {
        time_of_day(task.updated_at, offset)
    };
    let labels = row![
        text(time_of_day(task.started_at, offset))
            .size(11)
            .font(fonts::MONO)
            .color(t.muted),
        iced_widget::space().width(Length::Fill),
        text(end_label).size(11).color(t.muted),
    ];
    let mut footer = row![
        container(text("Timeline").size(14).color(t.muted)).width(Length::Fixed(84.0)),
        column![track, labels].spacing(2).width(Length::Fill),
    ]
    .align_y(Vertical::Center);
    if let Some(target) = target {
        // While the roll back card is open, its own buttons go on.
        let busy = state.rollback.is_some();
        let ink = if busy { t.muted } else { t.text };
        footer = footer
            .push(iced_widget::space().width(Length::Fixed(10.0)))
            .push(
                button(
                    container(
                        row![
                            icons::view(&icons::ROLL_BACK, ink, 15.0, 15.0),
                            text(format!("Roll back to {}", time_of_day(target.at, offset)))
                                .size(15)
                                .font(fonts::MEDIUM)
                                .color(ink),
                        ]
                        .spacing(9)
                        .align_y(Vertical::Center),
                    )
                    .center_y(Length::Fill),
                )
                .on_press_maybe((!busy).then_some(Message::RollBack))
                .height(Length::Fixed(44.0))
                .padding(Padding::from([0.0, 18.0]))
                .style(style::button_like(style::outlined(
                    Color::TRANSPARENT,
                    8.0,
                    1.0,
                    t.chip_border,
                ))),
            );
    }
    container(footer)
        .width(Length::Fill)
        .padding(Padding::from([0.0, 20.0]))
        .center_y(Length::Fixed(FOOTER))
        .into()
}

fn section<'a>(t: TileColors, label: &str) -> Element<'a, Message> {
    text(label.to_uppercase())
        .size(12)
        .font(fonts::SEMIBOLD)
        .color(t.muted)
        .into()
}

fn heading<'a>(t: TileColors, words: String) -> Element<'a, Message> {
    text(words)
        .size(22)
        .font(fonts::MEDIUM)
        .color(t.text)
        .into()
}

fn primary<'a>(t: TileColors, label: &'a str, message: Message) -> Element<'a, Message> {
    button(
        container(
            text(label)
                .size(14)
                .font(fonts::SEMIBOLD)
                .color(t.background),
        )
        .center_y(Length::Fill),
    )
    .on_press(message)
    .height(Length::Fixed(44.0))
    .padding(Padding::from([0.0, 20.0]))
    .style(style::button_like(style::panel(t.text, 10.0)))
    .into()
}

fn secondary<'a>(t: TileColors, label: &'a str, message: Message) -> Element<'a, Message> {
    button(container(text(label).size(14).font(fonts::MEDIUM).color(t.text)).center_y(Length::Fill))
        .on_press(message)
        .height(Length::Fixed(44.0))
        .padding(Padding::from([0.0, 18.0]))
        .style(style::button_like(style::outlined(
            Color::TRANSPARENT,
            10.0,
            1.0,
            t.button_border,
        )))
        .into()
}

/// A log that scrolls when it's longer than its room, showing its newest
/// lines at the bottom.
fn log_scroll<'a>(t: TileColors, content: Element<'a, Message>) -> Element<'a, Message> {
    scroll(t, content).anchor_bottom().into()
}

/// A list that scrolls when it's longer than its room.
fn quiet_scroll<'a>(t: TileColors, content: Element<'a, Message>) -> Element<'a, Message> {
    scroll(t, content).into()
}

/// A thin scroller, only there when the content overflows.
fn scroll<'a>(
    t: TileColors,
    content: Element<'a, Message>,
) -> iced_widget::Scrollable<'a, Message, iced_widget::Theme, iced_widget::Renderer> {
    scrollable(content)
        .direction(Direction::Vertical(
            Scrollbar::new().width(4).scroller_width(4).margin(0),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_, _| {
            let rail = scrollable::Rail {
                background: None,
                border: Border::default(),
                scroller: scrollable::Scroller {
                    background: Background::Color(t.chip_border),
                    border: Border {
                        radius: 2.0.into(),
                        ..Border::default()
                    },
                },
            };
            scrollable::Style {
                container: iced_widget::container::Style::default(),
                vertical_rail: rail,
                horizontal_rail: rail,
                gap: None,
                auto_scroll: scrollable::AutoScroll {
                    background: Background::Color(t.raised),
                    border: Border::default(),
                    shadow: iced_core::Shadow::default(),
                    icon: t.text,
                },
            }
        })
}

fn rule_h<'a>(t: TileColors) -> Element<'a, Message> {
    container("")
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(move |_| style::panel(t.rule, 0.0))
        .into()
}

fn rule_v<'a>(t: TileColors) -> Element<'a, Message> {
    container("")
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(move |_| style::panel(t.rule, 0.0))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn task(state: TileState) -> TaskView {
        TaskView {
            state,
            ..fixtures::invoice_detail().view
        }
    }

    #[test]
    fn aw11_controls_follow_the_tile_state() {
        let names = |state| -> Vec<(Control, bool)> {
            controls(&task(state))
                .into_iter()
                .map(|(c, m)| (c, m.is_some()))
                .collect()
        };
        use Control::*;
        for state in [TileState::Starting, TileState::Working, TileState::NeedsYou] {
            assert_eq!(
                names(state),
                [(Pause, true), (Stop, true), (TakeOver, true)],
                "{state:?}"
            );
        }
        assert_eq!(
            names(TileState::Paused),
            [(Resume, true), (Stop, true), (TakeOver, true)]
        );
        // While the person drives, Pause doesn't apply and Take over turns
        // into Hand back.
        assert_eq!(
            names(TileState::YouAreDriving),
            [(Pause, false), (Stop, true), (HandBack, true)]
        );
        for state in [TileState::Done, TileState::Failed, TileState::Stopped] {
            assert!(names(state).is_empty(), "{state:?}");
        }
    }

    #[test]
    fn aw11_an_attended_task_cant_be_taken_over_from_the_header() {
        let mut attended = task(TileState::Working);
        attended.driver_socket = Some("/var/run/glasshouse/driver.sock".into());
        let take = controls(&attended)
            .into_iter()
            .find(|(c, _)| *c == Control::TakeOver);
        assert_eq!(take, Some((Control::TakeOver, None)));
    }

    #[test]
    fn aw10_header_reads_like_the_artboard() {
        let detail = fixtures::invoice_detail();
        assert_eq!(chip(&detail.view), "Working · 12 of 40");
        assert_eq!(
            caption(&detail.view, UtcOffset::UTC),
            "Reconciling March invoices in AP Invoice Entry · on behalf of you · started 2:02 PM"
        );
        let mut done = detail.view.clone();
        done.state = TileState::Done;
        assert!(caption(&done, UtcOffset::UTC).ends_with("started 2:02 PM · finished 2:14 PM"));
    }

    #[test]
    fn aw15_roll_back_targets_the_picked_snapshot_or_the_latest() {
        let detail = fixtures::invoice_detail();
        let snapshots = &detail.snapshots;
        assert_eq!(target(snapshots, None).map(|s| s.id), Some(snapshots[2].id));
        assert_eq!(
            target(snapshots, Some(snapshots[0].id)).map(|s| s.id),
            Some(snapshots[0].id)
        );
        // A snapshot that's gone falls back to the latest.
        assert_eq!(
            target(snapshots, Some(SnapshotId::new())).map(|s| s.id),
            Some(snapshots[2].id)
        );
        assert!(target(&[], None).is_none());
    }

    #[test]
    fn ae3_an_answer_reads_as_paragraphs() {
        assert_eq!(
            blocks("You have 3 files.\nOne is new.\n\n\nplan.txt is the newest.\n"),
            [
                Block::Paragraph("You have 3 files. One is new.".into()),
                Block::Paragraph("plan.txt is the newest.".into()),
            ]
        );
    }

    #[test]
    fn ae3_markdown_in_an_answer_shows_as_what_it_means() {
        let answer = "## What I found\n\
            Two files in **LaunchNotes**:\n\
            - `plan.txt`: ship by\n  November 14.\n\
            * todo.txt: call Ana.\n\n\
            ---\n\
            1. Draft the mail\n\
            2) Book the room, see [the wiki](https://wiki.example/room)\n\
            ```\nlet x = 1;\n  x + 1\n```\n\
            Total: 3 * 4 = 12. #3 is next.";
        let item = |mark: &str, text: &str| Block::Item {
            mark: mark.into(),
            text: text.into(),
        };
        assert_eq!(
            blocks(answer),
            [
                Block::Heading("What I found".into()),
                Block::Paragraph("Two files in LaunchNotes:".into()),
                item("•", "plan.txt: ship by November 14."),
                item("•", "todo.txt: call Ana."),
                item("1.", "Draft the mail"),
                item(
                    "2.",
                    "Book the room, see the wiki (https://wiki.example/room)"
                ),
                Block::Code("let x = 1;".into()),
                Block::Code("  x + 1".into()),
                Block::Paragraph("Total: 3 * 4 = 12. #3 is next.".into()),
            ]
        );
    }

    #[test]
    fn times_of_day_follow_the_persons_offset() {
        let at = fixtures::invoice_detail().view.started_at;
        assert_eq!(time_of_day(at, UtcOffset::UTC), "2:02 PM");
        let chicago = UtcOffset::from_hms(-5, 0, 0).expect("valid");
        assert_eq!(time_of_day(at, chicago), "9:02 AM");
        assert_eq!(short_time(at, chicago), "9:02");
    }
}
