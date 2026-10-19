//! The browser engine (UI-5, CF-15; ADR 0027): how the desktop starts the
//! Browser and web apps' windows, and which app a window of it belongs to.
//!
//! One engine per person, with two profiles, so browsing and apps never
//! share cookies, storage or extensions: the Browser's, and one for every
//! web app, each app a window on its own origin,
//! `http://<app>.localhost:<port>`, served by the person's app bridge on
//! their own port (`glass_api::app_bridge_port`).
//!
//! The desktop starts the engine when the person signs in, with no window
//! ([`warm`]), and keeps it running: an app's window is then handed to a
//! running engine instead of waiting for one to start (UI-7).

use std::path::Path;

/// The engine, from the browser.third-party list until our own build.
pub const ENGINE: &str = "/usr/local/share/chromium/chrome";

/// The app id the Browser's windows carry.
pub const BROWSER_WINDOW: &str = "chromium-browser";

/// The engine's data, under the person's home: both profiles.
pub const DATA: &str = ".local/share/glasshouse/engine";
/// The Browser's profile, in [`DATA`].
pub const BROWSER_PROFILE: &str = "Browser";
/// Every web app's profile, in [`DATA`].
pub const APPS_PROFILE: &str = "Apps";
/// Both, for setting them up before the engine first opens them.
pub const PROFILES: [&str; 2] = [APPS_PROFILE, BROWSER_PROFILE];

/// What a new profile starts with, before the engine first opens it: the
/// desktop's frame around its windows (the compositor's title bar and
/// controls, DK-1) rather than the engine's own. The engine fills in the
/// rest; a profile that exists already is left as it is.
pub const NEW_PROFILE_PREFERENCES: &str = r#"{"browser":{"custom_chrome_frame":false}}"#;

/// The file a new profile's preferences go in, under its folder.
pub const PREFERENCES_FILE: &str = "Preferences";

/// Flags every start of the engine gets: our compositor; software drawing
/// into shared memory (the compositor composites in software, so GPU
/// rendering would only be copied back); none of the engine's own
/// first-run, default-browser and background services; passwords kept in
/// the profile (there's no desktop keyring to ask); `profile` of the
/// person's engine data.
fn common(home: &Path, profile: &str) -> Vec<String> {
    vec![
        "--ozone-platform=wayland".into(),
        "--disable-gpu".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-networking".into(),
        "--password-store=basic".into(),
        format!("--user-data-dir={}", home.join(DATA).display()),
        format!("--profile-directory={profile}"),
    ]
}

/// The engine with no window, kept running for the session: started with
/// its control pipe on descriptors 3 (read) and 4 (write), it runs until
/// that pipe closes rather than until its last window does. Whoever starts
/// it holds the pipe and never writes to it, so nothing else can drive
/// the engine through it; the desktop closing it (signing out) ends the
/// engine. It opens the apps' profile, where most windows go.
pub fn warm(home: &Path) -> Vec<String> {
    let mut args = common(home, APPS_PROFILE);
    args.push("--no-startup-window".into());
    args.push("--remote-debugging-pipe".into());
    args
}

/// The socket a running engine answers new windows on, in [`DATA`], and
/// the lock naming its process (`<host>-<pid>`): the engine is ready for
/// windows once both are there and the lock names it.
pub const READY_FILE: &str = "SingletonSocket";
pub const LOCK_FILE: &str = "SingletonLock";

/// What a running engine is asked on its socket ([`READY_FILE`]) to open
/// a window with, without starting another copy of it: `START`, the folder
/// relative paths are from, then the command line that would have started
/// one (`args`, after the program), each ended by a NUL but the last. The
/// engine answers [`TAKEN`] when it has the window to open; the folder
/// holding the socket is the person's own (`DATA`, mode 0700).
pub fn window_request(cwd: &Path, args: &[String]) -> Vec<u8> {
    let mut parts = vec![
        "START".to_owned(),
        cwd.display().to_string(),
        ENGINE.to_owned(),
    ];
    parts.extend(args.iter().cloned());
    parts.join("\0").into_bytes()
}

/// A running engine's answer to [`window_request`] when it takes the window.
pub const TAKEN: &[u8] = b"ACK";

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
/// `chrome-<app>.localhost__-<profile>` (`-Apps`).
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
        assert!(
            args.contains(&"--user-data-dir=/home/dana/.local/share/glasshouse/engine".to_owned())
        );
        assert!(args.contains(&"--profile-directory=Apps".to_owned()));
        // The Browser keeps its own profile: browsing never shares an app's storage.
        let browser = browser(home, Some("https://console.example.com"));
        assert!(browser.contains(&"--profile-directory=Browser".to_owned()));
        assert_eq!(
            browser.last().map(String::as_str),
            Some("https://console.example.com")
        );
        assert!(!browser.iter().any(|a| a.starts_with("--app=")));
    }

    #[test]
    fn ui7_the_engine_waits_with_no_window_until_its_pipe_closes() {
        let args = warm(Path::new("/home/dana"));
        for flag in [
            "--no-startup-window",
            "--remote-debugging-pipe",
            "--profile-directory=Apps",
        ] {
            assert!(args.contains(&flag.to_owned()), "{flag}");
        }
        // Never a debugging port: any local process could connect to one.
        assert!(
            !args
                .iter()
                .any(|a| a.starts_with("--remote-debugging-port"))
        );
    }

    #[test]
    fn ui7_a_running_engine_is_asked_for_a_window_in_its_own_words() {
        let home = Path::new("/home/dana");
        let request = window_request(home, &web_app(home, "mail", 7301));
        let parts: Vec<&[u8]> = request.split(|b| *b == 0).collect();
        assert_eq!(parts[0], b"START");
        assert_eq!(parts[1], b"/home/dana");
        assert_eq!(parts[2], ENGINE.as_bytes());
        assert_eq!(
            parts.last(),
            Some(&&b"--app=http://mail.localhost:7301/"[..])
        );
        assert_ne!(request.last(), Some(&0), "no NUL after the last");
    }

    #[test]
    fn ui5_a_window_names_its_web_app() {
        assert_eq!(
            web_app_of_window("chrome-mail.localhost__-Apps"),
            Some("mail")
        );
        assert_eq!(
            web_app_of_window("chrome-calendar.localhost__-Apps"),
            Some("calendar")
        );
        assert_eq!(web_app_of_window("chromium-browser"), None);
        assert_eq!(web_app_of_window("chrome-example.com__-Default"), None);
        assert_eq!(web_app_of_window("chrome-a.b.localhost__-Default"), None);
        assert_eq!(web_app_of_window("glass.terminal"), None);
    }
}
