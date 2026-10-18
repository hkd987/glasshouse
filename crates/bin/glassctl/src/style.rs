//! Terminal styling. Colors when stdout is a terminal, or when
//! `CLICOLOR_FORCE` is set (for captured output such as screenshots), and
//! never when `NO_COLOR` is set.

use std::io::IsTerminal;
use std::sync::OnceLock;

use glass_api::TileState;

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let forced = std::env::var("CLICOLOR_FORCE").is_ok_and(|v| !v.is_empty() && v != "0");
        std::env::var_os("NO_COLOR").is_none() && (forced || std::io::stdout().is_terminal())
    })
}

fn paint(code: &str, text: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

pub fn bold(text: &str) -> String {
    paint("1", text)
}

pub fn dim(text: &str) -> String {
    paint("2", text)
}

pub fn red(text: &str) -> String {
    paint("31", text)
}

pub fn green(text: &str) -> String {
    paint("32", text)
}

pub fn orange(text: &str) -> String {
    paint("33", text)
}

pub fn blue(text: &str) -> String {
    paint("34", text)
}

/// The tile's state marker: calm blue while working, orange only when the
/// user is needed (PRD "Tile states").
pub fn state_marker(state: TileState) -> String {
    match state {
        TileState::Starting => dim("○"),
        TileState::Working => blue("●"),
        TileState::NeedsYou => orange("◉"),
        TileState::Paused => dim("‖"),
        TileState::YouAreDriving => blue("◎"),
        TileState::Done => green("✓"),
        TileState::Failed => red("✗"),
        TileState::Stopped => dim("■"),
    }
}

pub fn state_label(state: TileState) -> String {
    match state {
        TileState::NeedsYou => orange(state.label()),
        TileState::Failed => red(state.label()),
        TileState::Done => green(state.label()),
        TileState::YouAreDriving => blue("You're driving · paused"),
        other => other.label().to_owned(),
    }
}

pub fn clear_screen() -> &'static str {
    if enabled() { "\x1b[2J\x1b[H" } else { "" }
}
