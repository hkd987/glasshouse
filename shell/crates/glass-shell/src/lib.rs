//! The Glasshouse desktop shell's UI (PRD UI-4, DK-1, ADR 0009), built with
//! iced on the tiny-skia software renderer.
//!
//! - [`theme`]: design tokens; one [`Theme`] restyles everything (UX-10).
//! - [`screens`]: boot, sign in, and the desktop with its menu bar, dock and
//!   agent tiles.
//! - [`tiles`]: an agent tile drawn from a `glass_api::TaskView`, the same
//!   data `glassctl` shows.
//! - [`render`]: draws any screen without a display, for previews and
//!   snapshot tests; the compositor (next milestone) draws through the same
//!   path into its own buffers.
//!
//! The shell is a control-API client: it knows `glass-api` types and
//! nothing of the runtime behind them.

pub mod fixtures;
pub mod fonts;
pub mod icons;
pub mod render;
pub mod screens;
mod style;
pub mod theme;
pub mod tiles;
pub mod wallpaper;

use glass_types::{ApprovalId, TaskId};

pub use iced_core::Size;
pub use theme::Theme;

/// An element of the shell's UI.
pub type Element<'a, Message> =
    iced_core::Element<'a, Message, iced_widget::Theme, iced_widget::Renderer>;

/// Screens stacked, later ones on top, such as the command bar over the
/// desktop.
pub fn layered<'a>(layers: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    iced_widget::Stack::with_children(layers)
        .width(iced_core::Length::Fill)
        .height(iced_core::Length::Fill)
        .into()
}

/// An empty element that fills its space: a window with nothing drawn yet.
pub fn blank<'a>() -> Element<'a, Message> {
    iced_widget::space()
        .width(iced_core::Length::Fill)
        .height(iced_core::Length::Fill)
        .into()
}

/// What the user does in the shell. Each maps onto a control-API call.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Open a tile's expanded view (`task.get`).
    OpenTask(TaskId),
    /// The needs-you tile's review button: show the items being approved.
    Review(ApprovalId),
    /// `approval.answer`, approve once.
    Approve(ApprovalId),
    /// Open the command bar (Ctrl Space).
    OpenCommandBar,
    CommandChanged(String),
    /// A "for what's on your screen" suggestion.
    RunSuggestion(String),
    /// The answer's primary action, such as "Build the brief" (`task.plan`).
    AcceptAnswer,
    CloseCommandBar,
    PasswordChanged(String),
    SignIn,
}
