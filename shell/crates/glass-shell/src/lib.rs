//! The Glasshouse desktop shell's UI (PRD UI-4, DK-1, ADR 0009), built with
//! iced on the tiny-skia software renderer.
//!
//! - [`theme`]: design tokens; one [`Theme`] restyles everything (UX-10).
//! - [`screens`]: boot, sign in, the desktop with its menu bar, dock and
//!   agent tiles, the command bar, and a tile's expanded view.
//! - [`tiles`]: an agent tile drawn from a `glass_api::TaskView`, the same
//!   data `glassctl` shows.
//! - [`greeter`]: the sign-in screen's state, for the greeter (ADR 0019).
//! - [`live`]: the live desktop's state, kept current from glassd's events,
//!   and what the person's clicks and keys ask of glassd.
//! - [`render`]: draws any screen, once for previews and snapshot tests, or
//!   frame after frame with its UI state kept ([`render::Screen`]).
//!
//! The shell is a control-API client: it knows `glass-api` types and
//! nothing of the runtime behind them.

pub mod fixtures;
pub mod fonts;
pub mod greeter;
pub mod icons;
pub mod live;
pub mod render;
pub mod screens;
pub mod session;
mod style;
pub mod theme;
pub mod tiles;
pub mod wallpaper;

use glass_types::{ApprovalId, SnapshotId, TaskId};

pub use iced_core::Size;
pub use session::Power;
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
    /// Open a tile's expanded view (`task.get`, AW-10).
    OpenTask(TaskId),
    /// The needs-you tile's review button: the expanded view, showing the
    /// items being approved.
    Review(ApprovalId),
    /// `approval.answer`, approve once.
    Approve(ApprovalId),
    /// `approval.answer`, deny.
    Decline(ApprovalId),
    /// Shrink the expanded view back to its tile (AW-16).
    CloseTask,
    /// The expanded view's right column: Activity or Changes (AW-13).
    ShowTab(screens::expanded::Tab),
    /// The expanded view's header (AW-11): `task.pause`, `task.resume`,
    /// `task.stop`, `task.take_over` and `task.hand_back`.
    Pause(TaskId),
    Resume(TaskId),
    Stop(TaskId),
    TakeOver(TaskId),
    HandBack(TaskId),
    /// Pick a snapshot on the timeline or in the activity log (AW-15).
    SelectSnapshot(SnapshotId),
    /// "Roll back to 2:11 PM": first asks what it would change.
    RollBack,
    /// Roll back for real, after seeing what it would change.
    ConfirmRollBack,
    /// Close the roll back card.
    CancelRollBack,
    /// The menu bar's agents pill: open or close the agents menu.
    ToggleAgents,
    /// A click outside the agents menu, or Esc: close it.
    CloseAgents,
    /// The pointer is on the banners: they stay until it leaves, as macOS
    /// keeps a notification you're reading.
    HoldBanners,
    LetBannersGo,
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
    /// The sign-in screen's "Switch user": the next person who can sign in.
    SwitchUser,
    /// End the session and go back to the sign-in screen (Shift Super Q, or
    /// the system menu).
    SignOut,
    /// Lock the screen until the person's password is given again (Ctrl
    /// Super Q, Super L, or the system menu); agents keep working.
    LockScreen,
    /// The mark at the left of the menu bar: open or close the system menu.
    ToggleSystemMenu,
    /// The system menu's About Glasshouse.
    OpenAbout,
    /// The system menu's Force Quit… (Alt Super Esc, or Ctrl Shift Esc).
    OpenForceQuit,
    /// Choose the agent Force Quit would stop.
    ChooseToQuit(TaskId),
    /// Stop the chosen agent at once.
    ForceQuit,
    /// Close About, Force Quit or a Restart or Shut Down question.
    CloseSystemWindow,
    /// Restart… or Shut Down… in the system menu: asks first.
    AskPower(Power),
    /// The answer to that question: go ahead.
    ConfirmPower,
}
