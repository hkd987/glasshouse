//! The apps the launcher offers: those installed here that are also in the
//! person's profile, which glassd gives from policy by role (`apps.list`,
//! AD-7). First-party native apps for now (ADR 0025); web apps join once the
//! browser engine is in the image, and catalog apps once the catalog is on
//! the device (DV-10, DV-11).

/// An app the launcher can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// The id policy names it by in profiles.
    pub id: &'static str,
    pub name: &'static str,
    /// The program that opens its window.
    pub command: &'static str,
}

/// Every first-party app the launcher knows; the desktop offers those that
/// are installed.
pub const FIRST_PARTY: &[AppEntry] = &[
    AppEntry {
        id: "terminal",
        name: "Terminal",
        command: "/usr/local/bin/glass-terminal",
    },
    AppEntry {
        id: "activity-monitor",
        name: "Activity Monitor",
        command: "/usr/local/bin/glass-activity",
    },
];

/// The first-party apps installed here (`installed`) that `profile`, the
/// ids glassd says the person may use, names.
pub fn offered(installed: &[AppEntry], profile: &[String]) -> Vec<AppEntry> {
    installed
        .iter()
        .filter(|app| profile.iter().any(|id| id == app.id))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ad7_the_launcher_offers_only_what_the_profile_names() {
        let profile = ["mail".to_owned(), "terminal".to_owned()];
        let ids: Vec<&str> = offered(FIRST_PARTY, &profile)
            .iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, ["terminal"]);
        assert!(offered(FIRST_PARTY, &[]).is_empty(), "no profile, no apps");
    }
}
