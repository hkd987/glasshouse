//! The keyboard shortcuts, as the cheat sheet lists them (UX-20): Super /
//! or the system menu's Keyboard Shortcuts. The desktop's own are tested
//! against what it does with them (`Live::shortcut`), so the sheet can't
//! drift from the keys.

/// Keys and what they do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub keys: &'static str,
    pub does: &'static str,
}

const fn key(keys: &'static str, does: &'static str) -> Shortcut {
    Shortcut { keys, does }
}

/// Every shortcut, in groups.
pub const SHEET: &[(&str, &[Shortcut])] = &[
    (
        "Desktop",
        &[
            key("Super", "Open the launcher"),
            key("Ctrl Space", "Hand an agent a task"),
            key("Alt Tab", "Switch windows; Shift goes back"),
            key("Super /", "Show these shortcuts"),
            key("Esc", "Close a menu, window or switcher"),
        ],
    ),
    (
        "Session",
        &[
            key("Super L", "Lock the screen"),
            key("Ctrl Super Q", "Lock the screen"),
            key("Shift Super Q", "Sign out"),
            key("Alt Super Esc", "Force quit an agent"),
            key("Ctrl Shift Esc", "Force quit an agent"),
        ],
    ),
    (
        "In apps",
        &[
            key("Tab", "Next field; Shift Tab, the one before"),
            key("Ctrl Shift C", "Copy (Terminal)"),
            key("Ctrl Shift V", "Paste (Terminal)"),
            key("Ctrl C", "Copy what's selected, or interrupt (Terminal)"),
        ],
    ),
];

/// Whether the sheet lists these keys.
pub fn listed(keys: &str) -> bool {
    SHEET
        .iter()
        .flat_map(|(_, shortcuts)| shortcuts.iter())
        .any(|s| s.keys == keys)
}
