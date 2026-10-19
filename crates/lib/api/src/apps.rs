//! The app bridge (ADR 0024, UI-5): what an app runtime says to glassd on
//! the app socket, and the manifest every app ships.
//!
//! An app is a principal like an agent: its calls run through the same
//! pipeline, attributed "by [app] for [user]". glassd reads each app's
//! manifest from the app's installed copy, never from the app as it runs,
//! and refuses any action the manifest doesn't declare (DV-3). The app
//! socket carries only the [`app_methods`]; tasks, approvals and undo stay
//! on the control socket, so an app can never answer its own asks.

use glass_types::{ActionName, AgentId, Label, SideEffect, TaskId, Tier};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::DisplaySafe;

/// Where `glassd` serves the app bridge unless configured otherwise.
pub const DEFAULT_APP_SOCKET: &str = "/var/run/glasshouse/apps.sock";
/// Environment variable app runtimes read to find the app socket.
pub const APP_SOCKET_ENV: &str = "GLASS_APP_SOCKET";

/// The loopback port a person's app bridge serves their apps on (ADR 0027):
/// fixed per person, because an app's origin, `http://<app>.localhost:<port>`,
/// includes it and the browser keeps each app's offline data by origin; and
/// different for each person signed in. Below the kernel's ephemeral ports.
pub fn app_bridge_port(uid: u32) -> u16 {
    // 7300 to 8299; uid 1001 serves on 7301.
    7300 + (uid % 1000) as u16
}

/// Methods on the app socket. Params → result types are noted on each.
pub mod app_methods {
    /// [`crate::AppOpen`] → [`crate::AppSession`]. Opens a session for an
    /// installed app, for the connecting user. It lasts until
    /// [`APP_CLOSE`] or until this connection closes.
    pub const APP_OPEN: &str = "app.open";
    /// [`crate::AppCall`] → [`crate::AppCallResult`]. Runs one action
    /// through the pipeline. An ask holds the call until the user answers
    /// on the desktop; refusals are results, not errors.
    pub const APP_CALL: &str = "app.call";
    /// [`crate::AppSessionRef`] → [`crate::AppStatus`]: whether a call is
    /// waiting for the user, so the app can say so.
    pub const APP_STATUS: &str = "app.status";
    /// [`crate::AppSessionRef`] → `()`.
    pub const APP_CLOSE: &str = "app.close";
    /// [`crate::AppOpen`] → [`crate::AppFiles`]: what one of the caller's
    /// own apps serves now (ADR 0032). The bridge serves those from here,
    /// never from a folder.
    pub const APP_FILES: &str = "app.files";
}

/// An app in the caller's profile (`apps.list`), which is what the
/// launcher offers. Policy decides profiles per group (role-based
/// delivery), and glassd opens no app outside the caller's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileApp {
    pub id: AgentId,
    pub name: String,
    pub kind: ProfileAppKind,
    /// For the admin console: its address, from the device's managed
    /// settings. It reads from the console's servers, not from this device.
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAppKind {
    /// A web app installed on this device (ADR 0024).
    Web,
    /// A native app, such as Activity Monitor (ADR 0025).
    Native,
    /// The org's admin console, served from the console's own servers.
    Console,
}

/// What an app declares (UI-5, DV-3). glassd reads it from the app's
/// installed copy and enforces it: an app can never exceed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppManifest {
    /// The app's identity, a slug like `mail`. Apps and catalog agents
    /// share one namespace, since both act as "by [id] for [user]".
    pub id: AgentId,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// The actions the app calls. The OS refuses any other.
    pub actions: Vec<ActionName>,
    /// Powers beyond actions.
    #[serde(default)]
    pub permissions: Vec<AppPermission>,
    /// The data labels the app touches; the most sensitive bounds what its
    /// calls may open.
    pub labels: Vec<Label>,
    /// Network destinations (`host:port`). None are allowed yet: apps
    /// reach the world only through actions.
    #[serde(default)]
    pub network: Vec<String>,
}

/// A power an app may declare beyond its actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppPermission {
    /// Keep what it loaded on the device and show it without a connection
    /// (UI-5: apps work offline).
    Offline,
    /// Camera, microphone and a WebRTC connection, for meetings only
    /// (CF-21, ADR 0030 §6): an app must declare `meet.join` to have it,
    /// so Chat never does. Until our engine build (CF-15) enforces it per
    /// origin, it is checked here and in `apps-lint`.
    Media,
}

impl AppManifest {
    /// Checks what a manifest declares, before glassd installs it.
    pub fn check(&self) -> Result<(), String> {
        let line = |field: &str, text: &str, max: usize| {
            let ok = !text.trim().is_empty()
                && text.chars().count() <= max
                && !text.chars().any(char::is_control);
            if ok {
                Ok(())
            } else {
                Err(format!(
                    "{}: {field} must be one line of 1 to {max} characters",
                    self.id
                ))
            }
        };
        line("name", &self.name, 40)?;
        line("version", &self.version, 32)?;
        if self.actions.is_empty() {
            return Err(format!(
                "{}: declares no actions; an app reaches the system only through them",
                self.id
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        if let Some(twice) = self.actions.iter().find(|a| !seen.insert(*a)) {
            return Err(format!("{}: declares {twice} twice", self.id));
        }
        if self.labels.is_empty() {
            return Err(format!(
                "{}: declare the data labels the app touches",
                self.id
            ));
        }
        if self.labels.contains(&Label::Restricted) {
            return Err(format!("{}: no app may open restricted data", self.id));
        }
        if !self.network.is_empty() {
            return Err(format!(
                "{}: network destinations aren't supported yet; apps reach the world through actions",
                self.id
            ));
        }
        if self.may(AppPermission::Media) && !self.actions.iter().any(|a| a.as_str() == "meet.join")
        {
            return Err(format!(
                "{}: media is for meetings: only an app that declares meet.join may have it",
                self.id
            ));
        }
        Ok(())
    }

    /// The most sensitive label the app declares.
    pub fn max_label(&self) -> Label {
        self.labels.iter().copied().max().unwrap_or(Label::Public)
    }

    pub fn declares(&self, action: &ActionName) -> bool {
        self.actions.contains(action)
    }

    pub fn may(&self, permission: AppPermission) -> bool {
        self.permissions.contains(&permission)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppOpen {
    pub app: AgentId,
}

/// What one of a person's own apps serves (ADR 0032): the files glassd
/// built from the version they chose, by name (`index.html`, `app.js`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppFiles {
    pub app: AgentId,
    pub version: u32,
    pub files: std::collections::BTreeMap<String, String>,
}

/// An open app, for one user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSession {
    /// Pass it to every call. Valid on the connection that opened it only.
    pub session: TaskId,
    pub app: AgentId,
    pub name: String,
    pub version: String,
    /// Who the app acts for.
    pub user: String,
    /// What the app may call, and how each runs for this user today.
    pub actions: Vec<AppAction>,
    #[serde(default)]
    pub permissions: Vec<AppPermission>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppAction {
    pub name: ActionName,
    /// "Send emails"
    pub title: String,
    /// Ask means the user approves each call on the desktop, not in the app.
    pub tier: Tier,
    pub side_effect: SideEffect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppCall {
    pub session: TaskId,
    pub action: ActionName,
    #[serde(default)]
    pub input: Value,
}

/// What a call did. A refusal (undeclared, blocked by policy, declined by
/// the user) is `ok: false` with a sentence saying why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppCallResult {
    pub ok: bool,
    pub text: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSessionRef {
    pub session: TaskId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppStatus {
    /// False once the session ended (closed, stopped from the desktop).
    pub live: bool,
    /// A call waiting for the user's answer on the desktop.
    #[serde(default)]
    pub waiting: Option<AppWaiting>,
    /// Ask again with this as `since` to hear of the next change.
    #[serde(default)]
    pub cursor: u64,
    /// Whose data changed for the person since `since`: this app's id when
    /// its did, such as a new chat message (ADR 0030). Ids only, never
    /// content: the app reads what changed with one audited call.
    #[serde(default)]
    pub changed: Vec<String>,
}

/// `app.status`: the session's state, waiting up to `wait_secs` (at most
/// [`APP_STATUS_MAX_WAIT_SECS`]) for the person's data in this app to
/// change after `since`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppStatusRequest {
    pub session: TaskId,
    #[serde(default)]
    pub wait_secs: u64,
    #[serde(default)]
    pub since: u64,
}

/// The longest an `app.status` waits for a change.
pub const APP_STATUS_MAX_WAIT_SECS: u64 = 25;

/// What the user is being asked, so the app can show it is waiting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppWaiting {
    /// "Send email: Re: Renewal terms"
    pub summary: String,
    pub items: Vec<String>,
}

impl DisplaySafe for AppWaiting {
    fn make_display_safe(&mut self) {
        let Self { summary, items } = self;
        summary.make_display_safe();
        items.make_display_safe();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> AppManifest {
        serde_json::from_value(serde_json::json!({
            "id": "mail", "name": "Mail", "version": "0.1.0",
            "actions": ["mail.list", "mail.send"], "permissions": ["offline"],
            "labels": ["public", "internal"]
        }))
        .unwrap()
    }

    #[test]
    fn dv3_manifests_declare_actions_labels_and_powers() {
        let m = manifest();
        assert!(m.check().is_ok());
        assert_eq!(m.max_label(), Label::Internal);
        assert!(m.declares(&"mail.send".parse().unwrap()));
        assert!(!m.declares(&"files.delete".parse().unwrap()));
        assert!(m.may(AppPermission::Offline));
    }

    #[test]
    fn cf21_only_a_meeting_app_has_the_camera_and_microphone() {
        let mut m = manifest();
        m.permissions.push(AppPermission::Media);
        assert!(
            m.check()
                .unwrap_err()
                .contains("only an app that declares meet.join")
        );
        m.actions.push("meet.join".parse().unwrap());
        assert!(m.check().is_ok());
    }

    #[test]
    fn dv3_manifests_that_overreach_are_refused() {
        let mut m = manifest();
        m.labels.push(Label::Restricted);
        assert!(m.check().is_err());
        let mut m = manifest();
        m.network.push("example.com:443".into());
        assert!(m.check().is_err());
        let mut m = manifest();
        m.actions.push("mail.list".parse().unwrap());
        assert!(m.check().unwrap_err().contains("twice"));
        let mut m = manifest();
        m.name = "Mail\nApproved".into();
        assert!(m.check().is_err());
        // Unknown fields and patterns are refused as the manifest is read.
        assert!(
            serde_json::from_value::<AppManifest>(serde_json::json!({
                "id": "mail", "name": "Mail", "version": "1", "actions": ["mail.*"], "labels": ["public"]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<AppManifest>(serde_json::json!({
                "id": "mail", "name": "Mail", "version": "1", "actions": ["mail.list"],
                "labels": ["public"], "tiers": { "mail.send": "auto" }
            }))
            .is_err()
        );
    }
}
