//! The browser engine (UI-5, CF-15; ADR 0027): how the desktop starts the
//! Browser and web apps' windows, and which app a window of it belongs to.
//!
//! One engine, two profiles per person, so browsing and apps never share
//! cookies, storage or extensions: the Browser's, and one for every web
//! app, each app a window on its own origin, `http://<app>.localhost:<port>`,
//! served by the person's app bridge on their own port
//! (`glass_api::app_bridge_port`).

use std::path::Path;

/// The engine, from the browser.third-party list until our own build.
pub const ENGINE: &str = "/usr/local/share/chromium/chrome";

/// The app id the Browser's windows carry.
pub const BROWSER_WINDOW: &str = "chromium-browser";

/// The Browser's profile, under the person's home.
pub const BROWSER_PROFILE: &str = ".config/glasshouse/browser";
/// Every web app's profile, under the person's home.
pub const APPS_PROFILE: &str = ".local/share/glasshouse/app-runtime";

/// What a new profile starts with, before the engine first opens it: the
/// desktop's frame around its windows (the compositor's title bar and
/// controls, DK-1) rather than the engine's own. The engine fills in the
/// rest; a profile that exists already is left as it is.
pub const NEW_PROFILE_PREFERENCES: &str = r#"{"browser":{"custom_chrome_frame":false}}"#;

/// The file a new profile's preferences go in, under its folder.
pub const PREFERENCES_FILE: &str = "Default/Preferences";

/// Flags every window of the engine gets: our compositor; software
/// drawing into shared memory (the compositor composites in software, so
/// GPU rendering would only be copied back); none of the engine's own
/// first-run, default-browser and background services; passwords kept in
/// the profile (there's no desktop keyring to ask).
fn common(home: &Path, profile: &str) -> Vec<String> {
    vec![
        "--ozone-platform=wayland".into(),
        "--disable-gpu".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-networking".into(),
        "--password-store=basic".into(),
        format!("--user-data-dir={}", home.join(profile).display()),
    ]
}

/// The Browser, at its start page or at `url`.
pub fn browser(home: &Path, url: Option<&str>) -> Vec<String> {
    let mut args = common(home, BROWSER_PROFILE);
    args.extend(url.map(str::to_owned));
    args
}

/// A window of web app `id`, from the bridge on `port`.
pub fn web_app(home: &Path, id: &str, port: u16) -> Vec<String> {
    let mut args = common(home, APPS_PROFILE);
    args.push(format!("--app=http://{id}.localhost:{port}/"));
    args
}

/// The web app a window of the engine belongs to: its app id is
/// `chrome-<app>.localhost__-<profile>`.
pub fn web_app_of_window(app_id: &str) -> Option<&str> {
    let rest = app_id.strip_prefix("chrome-")?;
    let (host, _profile) = rest.split_once("__-")?;
    host.strip_suffix(".localhost")
        .filter(|id| !id.is_empty() && !id.contains('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui5_each_web_app_is_a_window_on_its_own_origin_in_the_apps_profile() {
        let home = Path::new("/home/dana");
        let args = web_app(home, "mail", 7301);
        assert!(args.contains(&"--app=http://mail.localhost:7301/".to_owned()));
        assert!(args.contains(
            &"--user-data-dir=/home/dana/.local/share/glasshouse/app-runtime".to_owned()
        ));
        // The Browser keeps its own profile: browsing never shares an app's storage.
        let browser = browser(home, Some("https://console.example.com"));
        assert!(
            browser.contains(&"--user-data-dir=/home/dana/.config/glasshouse/browser".to_owned())
        );
        assert_eq!(
            browser.last().map(String::as_str),
            Some("https://console.example.com")
        );
        assert!(!browser.iter().any(|a| a.starts_with("--app=")));
    }

    #[test]
    fn ui5_a_window_names_its_web_app() {
        assert_eq!(
            web_app_of_window("chrome-mail.localhost__-Default"),
            Some("mail")
        );
        assert_eq!(
            web_app_of_window("chrome-calendar.localhost__-Default"),
            Some("calendar")
        );
        assert_eq!(web_app_of_window("chromium-browser"), None);
        assert_eq!(web_app_of_window("chrome-example.com__-Default"), None);
        assert_eq!(web_app_of_window("chrome-a.b.localhost__-Default"), None);
        assert_eq!(web_app_of_window("glass.terminal"), None);
    }
}
