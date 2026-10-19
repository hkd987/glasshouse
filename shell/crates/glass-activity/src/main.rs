//! `glass-activity`: Activity Monitor, what's using the machine (UI-9). A
//! native first-party app (ADR 0025) on `glass-window`: the processors and memory, the person's
//! agents (everything in each one's jail added up) and their apps, by CPU
//! and memory, every two seconds. Stop an agent or quit an app from here.
//!
//! ```text
//! glass-activity [--socket PATH]
//! ```

mod model;
mod sampler;

use std::path::PathBuf;

use anyhow::Context;
use glass_api::{CONTROL_SOCKET_ENV, DEFAULT_CONTROL_SOCKET};
use glass_shell::theme::{hex, hexa};
use glass_shell::{Element, fonts};
use glass_types::TaskId;
use glass_window::{App, KeyPress, Wake};
use iced_core::alignment::{Horizontal, Vertical};
use iced_core::{Background, Border, Color, Length, Padding, border};
use iced_widget::{Column, Row, button, column, container, row, scrollable, text};

use crate::model::{AgentRow, AppRow, Snapshot, cpu_text, memory_text};
use crate::sampler::{Request, Sampler};

const WIDTH: u32 = 720;
const HEIGHT: u32 = 480;

/// The window's colors: the desktop's light window style.
const BACKGROUND: Color = hex(0xFFFFFF);
const PANEL: Color = hex(0xF4F6F8);
const TEXT: Color = hex(0x14171C);
const MUTED: Color = hex(0x5F6875);
const DIVIDER: Color = hex(0xE1E5EA);
const ACCENT: Color = hex(0x3D6BFF);
const TRACK: Color = hex(0xE1E5EA);

/// The name column's width; the numbers share the rest.
const NAME: f32 = 380.0;
const KIND: f32 = 110.0;
const NUMBER: f32 = 90.0;

fn main() -> anyhow::Result<()> {
    let mut socket = std::env::var_os(CONTROL_SOCKET_ENV)
        .map_or_else(|| PathBuf::from(DEFAULT_CONTROL_SOCKET), PathBuf::from);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().context("--socket needs a path")?.into(),
            "-h" | "--help" => {
                eprintln!("usage: glass-activity [--socket PATH]");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let wake = Wake::new()?;
    let uid = glass_platform::users::effective_uid();
    let sampler = Sampler::start(wake.waker()?, socket, uid)?;
    glass_window::run(Activity::new(sampler), wake)
}

/// A row the person picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picked {
    Agent(TaskId),
    App(u32),
}

#[derive(Debug, Clone)]
enum Message {
    Pick(Picked),
    /// Stop the picked agent, or quit the picked app.
    Act,
}

struct Activity {
    sampler: Sampler,
    shown: Snapshot,
    picked: Option<Picked>,
    problem: Option<String>,
}

impl Activity {
    fn new(sampler: Sampler) -> Self {
        Self {
            sampler,
            shown: Snapshot::default(),
            picked: None,
            problem: None,
        }
    }

    /// What the button would do to the picked row, if it's still there.
    fn action(&self) -> Option<(Request, &'static str)> {
        match self.picked? {
            Picked::Agent(task) => self
                .shown
                .agents
                .iter()
                .any(|a| a.task == Some(task))
                .then_some((Request::Stop(task), "Stop agent")),
            Picked::App(pid) => self
                .shown
                .apps
                .iter()
                .any(|a| a.pid == pid && a.quittable)
                .then_some((Request::Quit(pid), "Quit")),
        }
    }
}

impl App for Activity {
    type Message = Message;

    fn title(&self) -> String {
        "Activity Monitor".into()
    }

    fn app_id(&self) -> String {
        "glass.activity".into()
    }

    fn size(&self) -> (u32, u32) {
        (WIDTH, HEIGHT)
    }

    fn background(&self) -> [u8; 4] {
        [0xFF, 0xFF, 0xFF, 0xFF]
    }

    fn view(&self) -> Element<'_, Message> {
        let summary = row![
            gauge(
                "CPU",
                self.shown.cpu_percent.map_or_else(|| "…".into(), cpu_text),
                self.shown.cpu_percent.unwrap_or(0.0) / 100.0,
            ),
            gauge(
                "Memory",
                memory_summary(&self.shown),
                share(self.shown.memory.used_mb, self.shown.memory.total_mb),
            ),
            agents_card(&self.shown),
        ]
        .spacing(12);

        let mut rows = Column::new().push(header());
        rows = rows.push(section("Agents"));
        if !self.shown.agents_known {
            rows = rows.push(note(
                "The system isn't answering, so agents can't be listed.",
            ));
        } else if self.shown.agents.is_empty() {
            rows = rows.push(note("No agents are running."));
        }
        for agent in &self.shown.agents {
            rows = rows.push(agent_row(agent, self.picked));
        }
        rows = rows.push(section("Apps and processes"));
        for app in &self.shown.apps {
            rows = rows.push(app_row(app, self.picked));
        }

        let action = self.action();
        let act = button(text(action.as_ref().map_or("Quit", |(_, label)| *label)).size(13))
            .padding(Padding::from([6, 14]))
            .on_press_maybe(action.as_ref().map(|_| Message::Act))
            .style(move |_, status| {
                let on = !matches!(status, button::Status::Disabled);
                button::Style {
                    background: Some(Background::Color(if on { TEXT } else { DIVIDER })),
                    text_color: if on { Color::WHITE } else { MUTED },
                    border: Border {
                        radius: border::Radius::from(7.0),
                        ..Border::default()
                    },
                    ..button::Style::default()
                }
            });
        let status = text(self.problem.clone().unwrap_or_else(|| {
            "Updates every 2 seconds. Agents' numbers include everything in their sandbox.".into()
        }))
        .size(12)
        .color(if self.problem.is_some() {
            hex(0xC4472B)
        } else {
            MUTED
        });
        let footer =
            row![status, iced_widget::space().width(Length::Fill), act].align_y(Vertical::Center);

        container(
            column![
                summary,
                container(scrollable(rows).height(Length::Fill))
                    .height(Length::Fill)
                    .style(|_| outlined(BACKGROUND, 10.0)),
                footer,
            ]
            .spacing(12),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(PANEL)),
            ..container::Style::default()
        })
        .into()
    }

    fn update(&mut self, message: Message) {
        match message {
            Message::Pick(picked) => self.picked = Some(picked),
            Message::Act => {
                if let Some((request, _)) = self.action() {
                    self.sampler.ask(request);
                    self.picked = None;
                }
            }
        }
    }

    fn key(&mut self, press: &KeyPress) -> bool {
        // Esc lets go of the picked row.
        if press.keysym == glass_window::xkb::Keysym::Escape {
            self.picked = None;
            return true;
        }
        false
    }

    fn woken(&mut self) -> bool {
        let (snapshot, problem) = self.sampler.take();
        // The same numbers draw nothing.
        let snapshot = snapshot.filter(|s| *s != self.shown);
        let changed = snapshot.is_some() || problem.is_some();
        if let Some(snapshot) = snapshot {
            self.shown = snapshot;
        }
        if problem.is_some() {
            self.problem = problem;
        }
        changed
    }
}

fn share(part: u64, whole: u64) -> f64 {
    match whole {
        0 => 0.0,
        _ => part as f64 / whole as f64,
    }
}

fn memory_summary(shown: &Snapshot) -> String {
    match shown.memory.total_mb {
        0 => "…".into(),
        total => format!(
            "{} of {}",
            memory_text(shown.memory.used_mb * 1024),
            memory_text(total * 1024)
        ),
    }
}

fn outlined(background: Color, radius: f32) -> container::Style {
    container::Style {
        background: Some(Background::Color(background)),
        border: Border {
            radius: border::Radius::from(radius),
            width: 1.0,
            color: DIVIDER,
        },
        ..container::Style::default()
    }
}

/// A card with a label, a value and a bar for how full it is.
fn gauge<'a>(label: &'a str, value: String, fraction: f64) -> Element<'a, Message> {
    let filled = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16;
    let bar = row![
        container(iced_widget::space())
            .width(Length::FillPortion(filled.max(1)))
            .height(6)
            .style(|_| container::Style {
                background: Some(Background::Color(ACCENT)),
                border: Border {
                    radius: border::Radius::from(3.0),
                    ..Border::default()
                },
                ..container::Style::default()
            }),
        iced_widget::space()
            .width(Length::FillPortion(1000u16.saturating_sub(filled).max(1)))
            .height(6),
    ];
    card(column![
        text(label).size(12).color(MUTED),
        text(value).size(20).font(fonts::SEMIBOLD).color(TEXT),
        container(bar).style(|_| container::Style {
            background: Some(Background::Color(TRACK)),
            border: Border {
                radius: border::Radius::from(3.0),
                ..Border::default()
            },
            ..container::Style::default()
        }),
    ])
}

fn agents_card<'a>(shown: &Snapshot) -> Element<'a, Message> {
    let running = shown.agents.iter().filter(|a| a.task.is_some()).count();
    let value = match (shown.agents_known, running) {
        (false, _) => "…".to_owned(),
        (true, 1) => "1 running".to_owned(),
        (true, n) => format!("{n} running"),
    };
    // A float sum of nothing is -0.0, which would read "-0.0%".
    let cpu = shown.agents.iter().fold(0.0, |sum, a| sum + a.cpu_percent);
    card(column![
        text("Agents").size(12).color(MUTED),
        text(value).size(20).font(fonts::SEMIBOLD).color(TEXT),
        text(format!("{} of the CPU", cpu_text(cpu)))
            .size(12)
            .color(MUTED),
    ])
}

fn card<'a>(
    content: iced_widget::Column<'a, Message, iced_widget::Theme, iced_widget::Renderer>,
) -> Element<'a, Message> {
    container(content.spacing(6))
        .padding(12)
        .width(Length::Fill)
        .style(|_| outlined(BACKGROUND, 10.0))
        .into()
}

fn header<'a>() -> Element<'a, Message> {
    let label = |s: &'a str, width: f32, right: bool| {
        container(text(s).size(11).font(fonts::MEDIUM).color(MUTED))
            .width(Length::Fixed(width))
            .align_x(if right {
                Horizontal::Right
            } else {
                Horizontal::Left
            })
    };
    container(row![
        label("Name", NAME, false),
        label("Kind", KIND, false),
        label("CPU", NUMBER, true),
        label("Memory", NUMBER, true),
    ])
    .padding(Padding::from([8, 12]))
    .into()
}

fn section<'a>(title: &'a str) -> Element<'a, Message> {
    container(
        text(title.to_uppercase())
            .size(10)
            .font(fonts::SEMIBOLD)
            .color(MUTED),
    )
    .padding(Padding {
        top: 10.0,
        bottom: 4.0,
        left: 12.0,
        right: 12.0,
    })
    .width(Length::Fill)
    .style(|_| container::Style {
        border: Border {
            width: 0.0,
            ..Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

fn note<'a>(says: &'a str) -> Element<'a, Message> {
    container(text(says).size(13).color(MUTED))
        .padding(Padding::from([6, 12]))
        .into()
}

fn agent_row<'a>(agent: &AgentRow, picked: Option<Picked>) -> Element<'a, Message> {
    let name = match agent.agent.is_empty() {
        true => agent.title.clone(),
        false => format!("{} · {}", agent.title, agent.agent),
    };
    let kind = agent.state.map_or("Agent", |s| s.label());
    let pick = agent.task.map(Picked::Agent);
    line(
        name,
        kind.to_owned(),
        agent.cpu_percent,
        agent.memory_kb,
        pick,
        pick.is_some() && pick == picked,
    )
}

fn app_row<'a>(app: &AppRow, picked: Option<Picked>) -> Element<'a, Message> {
    let pick = Picked::App(app.pid);
    line(
        app.name.clone(),
        app.kind.to_owned(),
        app.cpu_percent,
        app.memory_kb,
        Some(pick),
        picked == Some(pick),
    )
}

fn line<'a>(
    name: String,
    kind: String,
    cpu: f64,
    memory_kb: u64,
    pick: Option<Picked>,
    picked: bool,
) -> Element<'a, Message> {
    let cell = |s: String, width: f32, right: bool| {
        container(
            text(s)
                .size(13)
                .color(if picked { Color::WHITE } else { TEXT })
                .wrapping(iced_core::text::Wrapping::None),
        )
        .width(Length::Fixed(width))
        .clip(true)
        .align_x(if right {
            Horizontal::Right
        } else {
            Horizontal::Left
        })
    };
    let content: Row<'a, Message, iced_widget::Theme, iced_widget::Renderer> = row![
        cell(name, NAME, false),
        cell(kind, KIND, false),
        cell(cpu_text(cpu), NUMBER, true),
        cell(memory_text(memory_kb), NUMBER, true),
    ];
    button(content)
        .padding(Padding::from([6, 12]))
        .width(Length::Fill)
        .on_press_maybe(pick.map(Message::Pick))
        .style(move |_, status| button::Style {
            background: match (picked, status) {
                (true, _) => Some(Background::Color(ACCENT)),
                (false, button::Status::Hovered) => Some(Background::Color(hexa(0x14171C, 0.04))),
                _ => None,
            },
            text_color: TEXT,
            border: Border {
                radius: border::Radius::from(6.0),
                ..Border::default()
            },
            ..button::Style::default()
        })
        .into()
}
