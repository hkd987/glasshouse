//! The apps the launcher offers: those in the person's profile, which glassd
//! gives from policy by role (`apps.list`, AD-7), that can start here.
//! First-party native apps (ADR 0025); the Browser and web apps, in the
//! browser engine (UI-5, CF-15, ADR 0027); and on a managed device the admin
//! console, which opens in the Browser (ADR 0023); after the org's, the
//! person's own web apps, a remix saying which app it's based on (ADR 0032,
//! TL-5). Catalog apps join once the catalog installs on the device (DV-10,
//! DV-11).

use glass_api::{ProfileApp, ProfileAppKind};

use crate::engine;

/// An app the launcher can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// The id policy names it by in profiles.
    pub id: String,
    pub name: String,
    pub start: Start,
    /// One of the person's own apps (ADR 0032, TL-5): the installed app it
    /// remixes, whose icon the launcher shows it with, badged as theirs.
    pub based_on: Option<String>,
}

/// How an app starts, and so which windows are its.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// A native program; its windows carry `app_id`
    /// (`xdg_toplevel.set_app_id`).
    Program {
        command: &'static str,
        app_id: &'static str,
    },
    /// The Browser, at its start page or at `url`.
    Browser { url: Option<String> },
    /// A web app in the browser engine, served by the person's app bridge.
    Web,
}

/// The id profiles name the Browser by.
pub const BROWSER: &str = "browser";

/// First-party native apps: id, name, program, the app id of their windows.
pub const NATIVE: &[(&str, &str, &str, &str)] = &[
    (
        "terminal",
        "Terminal",
        "/usr/local/bin/glass-terminal",
        "glass.terminal",
    ),
    (
        "activity-monitor",
        "Activity Monitor",
        "/usr/local/bin/glass-activity",
        "glass.activity",
    ),
];

/// Every first-party native app, as a profile naming them all gets them
/// (previews and tests).
pub fn natives() -> Vec<AppEntry> {
    NATIVE
        .iter()
        .filter_map(|(id, ..)| AppEntry::native(id))
        .collect()
}

impl AppEntry {
    /// A first-party native app by id.
    pub fn native(id: &str) -> Option<AppEntry> {
        NATIVE
            .iter()
            .find(|(native, ..)| *native == id)
            .map(|&(id, name, command, app_id)| AppEntry {
                id: id.to_owned(),
                name: name.to_owned(),
                start: Start::Program { command, app_id },
                based_on: None,
            })
    }

    /// What has to be on this device for it to start.
    pub fn program(&self) -> &str {
        match &self.start {
            Start::Program { command, .. } => command,
            Start::Browser { .. } | Start::Web => engine::ENGINE,
        }
    }

    /// Whether a window carrying `app_id` is this app's. The Browser's
    /// windows are also where the console opens.
    pub fn owns_window(&self, app_id: &str) -> bool {
        match &self.start {
            Start::Program { app_id: own, .. } => *own == app_id,
            Start::Browser { url: None } => app_id == engine::BROWSER_WINDOW,
            Start::Browser { url: Some(_) } => false,
            Start::Web => engine::web_app_of_window(app_id) == Some(self.id.as_str()),
        }
    }
}

/// What the launcher offers: the apps in `profile` that can start here
/// (`installed` says whether a program is on this device), in the
/// profile's order.
pub fn offered(profile: &[ProfileApp], installed: impl Fn(&str) -> bool) -> Vec<AppEntry> {
    profile
        .iter()
        .filter_map(|app| {
            let id = app.id.as_str();
            let entry = match app.kind {
                ProfileAppKind::Web => AppEntry {
                    id: id.to_owned(),
                    name: app.name.clone(),
                    start: Start::Web,
                    based_on: app.based_on.as_ref().map(|b| b.as_str().to_owned()),
                },
                ProfileAppKind::Console => AppEntry {
                    id: id.to_owned(),
                    name: app.name.clone(),
                    start: Start::Browser {
                        url: Some(app.url.clone()?),
                    },
                    based_on: None,
                },
                ProfileAppKind::Native if id == BROWSER => AppEntry {
                    id: id.to_owned(),
                    name: app.name.clone(),
                    start: Start::Browser { url: None },
                    based_on: None,
                },
                ProfileAppKind::Native => AppEntry::native(id)?,
            };
            installed(entry.program()).then_some(entry)
        })
        .collect()
}

/// An app's window, as Alt Tab lists it (DK-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppWindow {
    /// The compositor's id for it.
    pub id: u64,
    /// The app id it carries.
    pub app_id: String,
    pub title: String,
    /// Its minimize control hid it; Alt Tab still lists it.
    pub minimized: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(apps: &[(&str, ProfileAppKind)]) -> Vec<ProfileApp> {
        apps.iter()
            .map(|&(id, kind)| ProfileApp {
                id: id.parse().unwrap(),
                name: id.to_owned(),
                kind,
                url: (kind == ProfileAppKind::Console).then(|| "https://console.example".into()),
                based_on: None,
            })
            .collect()
    }

    #[test]
    fn ad7_the_launcher_offers_only_what_the_profile_names() {
        use ProfileAppKind::*;
        let apps = profile(&[("mail", Web), ("terminal", Native), ("browser", Native)]);
        let ids: Vec<String> = offered(&apps, |_| true).into_iter().map(|a| a.id).collect();
        assert_eq!(ids, ["mail", "terminal", "browser"]);
        assert!(offered(&[], |_| true).is_empty(), "no profile, no apps");
    }

    #[test]
    fn ui5_web_apps_and_the_browser_need_the_engine_on_this_device() {
        use ProfileAppKind::*;
        let apps = profile(&[("mail", Web), ("browser", Native), ("terminal", Native)]);
        let without_engine = offered(&apps, |program| program != engine::ENGINE);
        let ids: Vec<&str> = without_engine.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["terminal"]);
    }

    #[test]
    fn ad7_the_console_opens_in_the_browser_at_its_address() {
        let apps = profile(&[("console", ProfileAppKind::Console)]);
        let console = &offered(&apps, |_| true)[0];
        assert_eq!(
            console.start,
            Start::Browser {
                url: Some("https://console.example".into())
            }
        );
    }

    #[test]
    fn tl5_a_remix_carries_the_app_it_is_based_on() {
        use ProfileAppKind::*;
        let mut apps = profile(&[("mail", Web), ("my-mail", Web), ("terminal", Native)]);
        apps[1].name = "My Mail".into();
        apps[1].based_on = Some("mail".parse().unwrap());
        let offered = offered(&apps, |_| true);
        let based_on: Vec<(&str, Option<&str>)> = offered
            .iter()
            .map(|a| (a.id.as_str(), a.based_on.as_deref()))
            .collect();
        assert_eq!(
            based_on,
            [
                ("mail", None),
                ("my-mail", Some("mail")),
                ("terminal", None)
            ]
        );
        // Its own name, and its own web app on its own origin.
        assert_eq!(offered[1].name, "My Mail");
        assert_eq!(offered[1].start, Start::Web);
        assert!(offered[1].owns_window("chrome-my-mail.localhost__-Apps"));
        assert!(!offered[1].owns_window("chrome-mail.localhost__-Apps"));
    }

    #[test]
    fn dk2_each_window_belongs_to_its_app() {
        let mail = AppEntry {
            id: "mail".into(),
            name: "Mail".into(),
            start: Start::Web,
            based_on: None,
        };
        assert!(mail.owns_window("chrome-mail.localhost__-Apps"));
        assert!(!mail.owns_window("chrome-calendar.localhost__-Apps"));
        let terminal = AppEntry::native("terminal").unwrap();
        assert!(terminal.owns_window("glass.terminal"));
        let browser = AppEntry {
            id: BROWSER.into(),
            name: "Browser".into(),
            start: Start::Browser { url: None },
            based_on: None,
        };
        assert!(browser.owns_window("chromium-browser"));
        assert!(!browser.owns_window("chrome-mail.localhost__-Apps"));
    }
}
