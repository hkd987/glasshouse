//! First-run setup (the Setup artboard, UX-2): the steps down a dark side
//! panel, the current one on the right. A personal computer's setup is
//! three steps: your account, how agents reach AI models, and ready.

use iced_core::alignment::{Horizontal, Vertical};
use iced_core::{Background, Border, Color, Length, Padding, border};
use iced_widget::{Column, button, column, container, row, text, text_input};

use crate::theme::hex;
use crate::{Element, Message, Theme, fonts, icons, style};

/// The side panel's colors.
const PANEL: Color = hex(0x15181D);
const PANEL_INK: Color = hex(0xF3F5F7);
const PANEL_MUTED: Color = hex(0xA9B1BC);
const DONE: Color = hex(0x2E7D4F);
/// The page's.
const PAGE: Color = hex(0xF4F6F8);
const INK: Color = hex(0x14171C);
const MUTED: Color = hex(0x5F6875);
const EDGE: Color = hex(0xD5DBE2);
const CARD: Color = hex(0xFFFFFF);
const WARN: Color = hex(0xB4432B);

/// Ids of the fields, to give them the keyboard focus.
pub const NAME: &str = "setup-name";
pub const PASSWORD: &str = "setup-password";
pub const CONFIRM: &str = "setup-confirm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Account,
    Models,
    Ready,
}

/// How agents reach AI models, as the person chose it (MP-1 offers
/// Bedrock first; HX-1 is Claude Code's plugin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelChoice {
    Bedrock,
    ClaudeCode,
    ChatGpt,
    ApiKey,
}

impl ModelChoice {
    pub const ALL: [ModelChoice; 4] = [
        ModelChoice::Bedrock,
        ModelChoice::ClaudeCode,
        ModelChoice::ChatGpt,
        ModelChoice::ApiKey,
    ];

    /// The id setup records it by.
    pub fn id(self) -> &'static str {
        match self {
            ModelChoice::Bedrock => "bedrock",
            ModelChoice::ClaudeCode => "claude-code",
            ModelChoice::ChatGpt => "chatgpt",
            ModelChoice::ApiKey => "api-key",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ModelChoice::Bedrock => "Amazon Bedrock",
            ModelChoice::ClaudeCode => "Claude Code with your Claude plan",
            ModelChoice::ChatGpt => "Sign in with ChatGPT",
            ModelChoice::ApiKey => "Anthropic or OpenAI API key",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            ModelChoice::Bedrock => {
                "Claude and other models in your own AWS account. Connect it in Settings."
            }
            ModelChoice::ClaudeCode => {
                "Sign in to Claude Code after setup. The Glasshouse plugin lets it drive your desktop."
            }
            ModelChoice::ChatGpt => "Use your ChatGPT plan's included usage.",
            ModelChoice::ApiKey => "Add a key in Settings after setup. You pay for what you use.",
        }
    }
}

/// What the setup screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupScreen {
    pub step: Step,
    pub full_name: String,
    /// The account name the full name gives.
    pub login: String,
    pub password: String,
    pub confirm: String,
    pub models: Option<ModelChoice>,
    /// What's wrong with the answers, shown above the buttons.
    pub message: Option<String>,
    /// The account is being created.
    pub busy: bool,
}

pub fn view<'a>(_theme: &Theme, screen: &SetupScreen, size: (f32, f32)) -> Element<'a, Message> {
    let side = side_panel(screen.step);
    let page = match screen.step {
        Step::Account => account(screen),
        Step::Models => models(screen),
        Step::Ready => ready(screen),
    };
    let message = screen.message.clone().map(|m| text(m).size(14).color(WARN));
    let back = (screen.step != Step::Account).then(|| {
        button(text("Back").size(15).color(INK))
            .on_press_maybe((!screen.busy).then_some(Message::SetupBack))
            .padding(Padding::from([12.0, 20.0]))
            .style(style::button_like(style::outlined(CARD, 10.0, 1.0, EDGE)))
    });
    let next_label = match (screen.step, screen.busy) {
        (Step::Ready, true) => "Setting up…",
        (Step::Ready, false) => "Start using Glasshouse",
        _ => "Continue",
    };
    let next = button(
        text(next_label)
            .size(15)
            .font(fonts::MEDIUM)
            .color(Color::WHITE),
    )
    .on_press_maybe((!screen.busy).then_some(Message::SetupNext))
    .padding(Padding::from([12.0, 24.0]))
    .style(style::button_like(style::panel(INK, 10.0)));
    let mut buttons = row![].align_y(Vertical::Center);
    if let Some(back) = back {
        buttons = buttons.push(back);
    }
    buttons = buttons
        .push(iced_widget::space().width(Length::Fill))
        .push(next);

    let mut body = Column::new().push(page).spacing(24).max_width(840.0);
    if let Some(message) = message {
        body = body.push(message);
    }
    let content = column![
        body,
        iced_widget::space().height(Length::Fill),
        container(buttons).max_width(840.0),
    ]
    .padding(Padding {
        top: 64.0,
        bottom: 56.0,
        left: 72.0,
        right: 72.0,
    })
    .height(Length::Fill);

    row![
        side,
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| style::panel(PAGE, 0.0)),
    ]
    .width(Length::Fixed(size.0))
    .height(Length::Fixed(size.1))
    .into()
}

fn side_panel<'a>(current: Step) -> Element<'a, Message> {
    let steps = [
        (Step::Account, "Your account", "Your name and a password"),
        (Step::Models, "AI models", "How agents reach AI"),
        (Step::Ready, "Ready", "Your desktop"),
    ];
    let order = |s: Step| {
        steps
            .iter()
            .position(|(step, _, _)| *step == s)
            .unwrap_or(0)
    };
    let mut list = Column::new().spacing(8);
    for (n, (step, title, detail)) in steps.into_iter().enumerate() {
        let done = order(step) < order(current);
        let here = step == current;
        // A drawn check: the fonts the system ships have no ✓.
        let mark: Element<'a, Message> = if done {
            icons::view(&icons::CHECK, PANEL_INK, 14.0, 14.0)
        } else {
            text((n + 1).to_string())
                .size(13)
                .font(fonts::MEDIUM)
                .color(if here { INK } else { PANEL_INK })
                .into()
        };
        let badge =
            container(mark)
                .center(Length::Fixed(28.0))
                .style(move |_| match (done, here) {
                    (true, _) => style::panel(DONE, 14.0),
                    (false, true) => style::panel(PANEL_INK, 14.0),
                    (false, false) => style::outlined(PANEL, 14.0, 1.0, PANEL_MUTED),
                });
        let words = column![
            text(title).size(16).font(fonts::MEDIUM).color(PANEL_INK),
            text(detail).size(13).color(PANEL_MUTED),
        ]
        .spacing(2);
        list = list.push(
            container(row![badge, words].spacing(14).align_y(Vertical::Center))
                .padding(Padding::from([12.0, 14.0]))
                .width(Length::Fill)
                .style(move |_| {
                    if here {
                        style::panel(hex(0x22262D), 10.0)
                    } else {
                        style::panel(Color::TRANSPARENT, 10.0)
                    }
                }),
        );
    }
    let footer =
        text("Joining an organization? Ask IT to enroll this computer, and setup asks nothing.")
            .size(13)
            .color(PANEL_MUTED);
    container(
        column![
            column![
                text("Welcome to").size(14).color(PANEL_MUTED),
                text("Glasshouse")
                    .size(34)
                    .font(fonts::SEMIBOLD)
                    .color(PANEL_INK),
            ]
            .spacing(6),
            list,
            iced_widget::space().height(Length::Fill),
            footer,
        ]
        .spacing(48),
    )
    .padding(Padding::from([56.0, 28.0]))
    .width(Length::Fixed(380.0))
    .height(Length::Fill)
    .style(|_| style::panel(PANEL, 0.0))
    .into()
}

fn heading<'a>(title: &'a str, detail: String) -> Element<'a, Message> {
    column![
        text(title).size(30).font(fonts::SEMIBOLD).color(INK),
        text(detail).size(16).color(MUTED),
    ]
    .spacing(10)
    .into()
}

/// A labeled field; Enter in it sends `on_submit` (the next field, or on
/// the last one, Continue).
fn field<'a>(
    label: &'a str,
    id: &'static str,
    value: &str,
    secure: bool,
    on_input: fn(String) -> Message,
    on_submit: Message,
) -> Element<'a, Message> {
    let input = text_input("", value)
        .id(id)
        .secure(secure)
        .on_input(on_input)
        .on_submit(on_submit)
        .size(16)
        .font(fonts::SANS)
        .padding(Padding::from([12.0, 14.0]))
        .width(Length::Fixed(420.0))
        .style(|_, status| text_input::Style {
            background: Background::Color(CARD),
            border: Border {
                radius: border::Radius::from(10.0),
                width: 1.0,
                color: match status {
                    text_input::Status::Focused { .. } => hex(0x3D6BFF),
                    _ => EDGE,
                },
            },
            icon: INK,
            placeholder: MUTED,
            value: INK,
            selection: Color {
                a: 0.3,
                ..hex(0x3D6BFF)
            },
        });
    column![text(label).size(14).font(fonts::MEDIUM).color(INK), input]
        .spacing(8)
        .into()
}

fn account<'a>(screen: &SetupScreen) -> Element<'a, Message> {
    let hint = if screen.full_name.trim().is_empty() {
        "You'll sign in with this name and password.".to_owned()
    } else {
        format!("Your account name will be {}.", screen.login)
    };
    column![
        heading(
            "Who will use this computer?",
            "Setup asks three things, then you're on your desktop.".into()
        ),
        column![
            field(
                "Your name",
                NAME,
                &screen.full_name,
                false,
                Message::SetupName,
                Message::SetupFocus(PASSWORD)
            ),
            text(hint).size(13).color(MUTED),
        ]
        .spacing(8),
        field(
            "Password",
            PASSWORD,
            &screen.password,
            true,
            Message::SetupPassword,
            Message::SetupFocus(CONFIRM)
        ),
        field(
            "Password again",
            CONFIRM,
            &screen.confirm,
            true,
            Message::SetupConfirm,
            Message::SetupNext
        ),
    ]
    .spacing(22)
    .into()
}

fn models<'a>(screen: &SetupScreen) -> Element<'a, Message> {
    let mut options = Column::new().spacing(12);
    for choice in ModelChoice::ALL {
        let chosen = screen.models == Some(choice);
        let dot = container(
            container(iced_widget::space())
                .width(Length::Fixed(10.0))
                .height(Length::Fixed(10.0))
                .style(move |_| style::panel(if chosen { INK } else { Color::TRANSPARENT }, 5.0)),
        )
        .center(Length::Fixed(20.0))
        .style(move |_| style::outlined(CARD, 10.0, 1.5, if chosen { INK } else { MUTED }));
        let words = column![
            text(choice.title()).size(16).font(fonts::MEDIUM).color(INK),
            text(choice.detail()).size(13).color(MUTED),
        ]
        .spacing(4);
        options = options.push(
            button(row![dot, words].spacing(16).align_y(Vertical::Center))
                .on_press(Message::SetupModels(choice))
                .padding(Padding::from([16.0, 20.0]))
                .width(Length::Fill)
                .style(style::button_like(style::outlined(
                    CARD,
                    12.0,
                    if chosen { 2.0 } else { 1.0 },
                    if chosen { INK } else { EDGE },
                ))),
        );
    }
    column![
        heading(
            "How should agents reach AI models?",
            "Pick one now or later in Settings. Agents can't use a model until one is set up."
                .into()
        ),
        options,
    ]
    .spacing(28)
    .into()
}

fn ready<'a>(screen: &SetupScreen) -> Element<'a, Message> {
    let models = screen
        .models
        .map_or("You'll choose in Settings", ModelChoice::title);
    let line = |label: &'a str, value: String| {
        row![
            container(text(label).size(14).color(MUTED)).width(Length::Fixed(160.0)),
            text(value).size(15).color(INK),
        ]
        .align_y(Vertical::Center)
    };
    column![
        heading(
            "You're ready",
            "Agents work in a sandbox of their own: you see what they do, approve what they send, \
             and can undo what they change."
                .into()
        ),
        container(
            column![
                line(
                    "Account",
                    format!("{} ({})", screen.full_name.trim(), screen.login)
                ),
                line("AI models", models.to_owned()),
            ]
            .spacing(14)
        )
        .padding(24)
        .width(Length::Fill)
        .style(|_| style::outlined(CARD, 12.0, 1.0, EDGE)),
    ]
    .spacing(28)
    .align_x(Horizontal::Left)
    .into()
}
