//! The apps the launcher offers. First-party native apps for now (ADR
//! 0025); web apps join once the browser engine is in the image, and catalog
//! apps once the catalog is on the device (DV-10, DV-11).

/// An app the launcher can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    pub id: &'static str,
    pub name: &'static str,
    /// The program that opens its window.
    pub command: &'static str,
}

/// Every first-party app the launcher knows; the desktop offers those that
/// are installed.
pub const FIRST_PARTY: &[AppEntry] = &[
    AppEntry {
        id: "glass.terminal",
        name: "Terminal",
        command: "/usr/local/bin/glass-terminal",
    },
    AppEntry {
        id: "glass.activity",
        name: "Activity Monitor",
        command: "/usr/local/bin/glass-activity",
    },
];
