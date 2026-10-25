//! Device settings IT manages per group (UX-1, AD-2): one table of what a
//! computer can be set to, shared by policy checks, the admin console's
//! Settings page and the device's Settings app.
//!
//! A group sets a setting either as a default the person may change or
//! locked. A person's setting comes from the first of their groups, in the
//! policy's order, that sets it, then from `everyone`, then from the table.
//! The computer's settings that follow whoever is signed in (the browser's,
//! USB drives) resolve the same way for the person at the screen, and as
//! `everyone`'s while nobody is (ADR 0031). Those for the whole computer
//! whoever uses it (the time zone) come from `everyone` only.

use serde::Serialize;

use crate::EVERYONE;
use crate::document::{Managed, PolicyDocument, SettingValue};

/// The organization's Wi-Fi networks, by name (CF-12).
pub const WIFI_NETWORKS: &str = "wifi.networks";
/// Wi-Fi networks the computer never joins.
pub const WIFI_BLOCKED: &str = "wifi.blocked";
/// Only the organization's Wi-Fi networks may be joined.
pub const WIFI_ONLY_ORGANIZATION: &str = "wifi.only_organization";
/// The folders the desktop agent reads when asked without naming any
/// (ADR 0048).
pub const AGENT_FOLDERS: &str = "agents.folders";
/// What it reads on a computer where nobody chose: the person's Documents.
pub const AGENT_FOLDERS_DEFAULT: &str = "Documents";

/// Whom a setting is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Each person's own, set per group.
    Person,
    /// The whole computer's, set for everyone.
    Device,
    /// The computer's, for whoever is signed in at the screen: set per
    /// group, and `everyone`'s while nobody is (ADR 0031).
    Seat,
}

/// What a setting holds, and how the console and Settings app show it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    Toggle,
    Number {
        min: i64,
        max: i64,
        unit: &'static str,
    },
    /// A web address: https, or http for an intranet.
    Url,
    /// Sites, such as `example.com` or `*.games.example`.
    Sites,
    /// A time zone's name, such as `America/New_York`; empty is the
    /// computer's own.
    TimeZone,
    /// Apps, by id, such as `my-mail`.
    Apps,
    /// Folders in the person's home, such as `Documents` or
    /// `Projects/Northwind`.
    Folders,
    /// Wi-Fi networks by name, such as `Northwind Staff` (CF-12).
    Networks,
}

/// One setting a computer has.
#[derive(Debug, Clone, Serialize)]
pub struct SettingDef {
    pub id: &'static str,
    /// The Settings app's section it is in.
    pub section: &'static str,
    pub title: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub scope: Scope,
    /// What a computer does when nobody set it.
    pub default: SettingValue,
}

const fn toggle(
    id: &'static str,
    section: &'static str,
    title: &'static str,
    help: &'static str,
    scope: Scope,
    default: bool,
) -> SettingDef {
    SettingDef {
        id,
        section,
        title,
        help,
        kind: Kind::Toggle,
        scope,
        default: SettingValue::Bool(default),
    }
}

/// Every setting IT can manage, in the Settings app's order.
pub fn table() -> &'static [SettingDef] {
    static TABLE: std::sync::OnceLock<Vec<SettingDef>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        vec![
            SettingDef {
                id: "lock.after_minutes",
                section: "Lock screen",
                title: "Lock after",
                help: "Locks the screen after this long without use.",
                kind: Kind::Number {
                    min: 1,
                    max: 120,
                    unit: "minutes",
                },
                scope: Scope::Person,
                default: SettingValue::Number(10),
            },
            // The desktop blanks the screen; glassd sleeps the computer for
            // the person at it (ADR 0031). 0 is never.
            SettingDef {
                id: "display.blank_after_minutes",
                section: "Display & sleep",
                title: "Blank the screen after",
                help: "Turns the screen black after this long without use; a key or a move brings it back. 0 is never.",
                kind: Kind::Number {
                    min: 0,
                    max: 240,
                    unit: "minutes",
                },
                scope: Scope::Person,
                default: SettingValue::Number(5),
            },
            SettingDef {
                id: "power.sleep_after_minutes",
                section: "Display & sleep",
                title: "Sleep after",
                help: "Puts the computer to sleep after this long without use. Agents pause first and carry on when it wakes. 0 is never.",
                kind: Kind::Number {
                    min: 0,
                    max: 480,
                    unit: "minutes",
                },
                scope: Scope::Person,
                default: SettingValue::Number(30),
            },
            toggle(
                "clock.hour24",
                "Date & time",
                "24-hour clock",
                "Shows 14:00 instead of 2:00 PM in the menu bar and on the lock screen.",
                Scope::Person,
                false,
            ),
            // glassd sets it with the base system's tzsetup; the clock
            // follows without signing out (ADR 0031).
            SettingDef {
                id: "region.time_zone",
                section: "Date & time",
                title: "Time zone",
                help: "Such as America/New_York or Europe/London. Empty keeps the computer's own.",
                kind: Kind::TimeZone,
                scope: Scope::Device,
                default: SettingValue::Text(String::new()),
            },
            // glassd writes a devd rule that powers off any USB
            // mass-storage interface that attaches (ADR 0031).
            toggle(
                "devices.usb_storage",
                "Devices",
                "USB drives",
                "Off: USB drives and card readers don't connect, and any plugged in is turned off.",
                Scope::Seat,
                true,
            ),
            // CF-12, ADR 0059: glassd keeps the organization's networks
            // in wpa_supplicant, their passwords from wifi-secrets.toml,
            // never in policy or Settings; Settings joins only what these
            // allow.
            SettingDef {
                id: WIFI_NETWORKS,
                section: "Wi-Fi",
                title: "Your organization's networks",
                help: "Joined by themselves, ahead of any network a person joined. Their passwords are set on the computer, never shown.",
                kind: Kind::Networks,
                scope: Scope::Device,
                default: SettingValue::List(Vec::new()),
            },
            SettingDef {
                id: WIFI_BLOCKED,
                section: "Wi-Fi",
                title: "Blocked networks",
                help: "Networks this computer never joins; one it's on is left at once.",
                kind: Kind::Networks,
                scope: Scope::Device,
                default: SettingValue::List(Vec::new()),
            },
            toggle(
                WIFI_ONLY_ORGANIZATION,
                "Wi-Fi",
                "Only your organization's networks",
                "People can't join other networks, such as a café's or their home's.",
                Scope::Device,
                false,
            ),
            toggle(
                "notifications.do_not_disturb",
                "Notifications",
                "Do Not Disturb",
                "Agents' notifications stay in the menu bar instead of popping up; what needs you still waits there.",
                Scope::Person,
                false,
            ),
            SettingDef {
                id: "browser.home_page",
                section: "Browser",
                title: "Home page",
                help: "Opens with the browser and its home button.",
                kind: Kind::Url,
                scope: Scope::Seat,
                default: SettingValue::Text(String::new()),
            },
            SettingDef {
                id: "browser.blocked_sites",
                section: "Browser",
                title: "Blocked sites",
                help: "Sites that don't open, such as example.com or *.games.example.",
                kind: Kind::Sites,
                scope: Scope::Seat,
                default: SettingValue::List(Vec::new()),
            },
            toggle(
                "browser.restore_tabs",
                "Browser",
                "Reopen last session's tabs",
                "Otherwise the browser starts at the home page.",
                Scope::Seat,
                true,
            ),
            toggle(
                "browser.camera",
                "Privacy",
                "Camera for websites and apps",
                "Meetings and calls need it; each site still asks the person.",
                Scope::Seat,
                true,
            ),
            // ADR 0032, TL-6: off, glassd removes our apps' source from
            // the computer, the studio refuses and people's own apps don't
            // open; their versions are kept for when it's on again.
            toggle(
                "apps.remix",
                "Apps",
                "Remixing apps",
                "People and their agents may make their own versions of the organization's apps. Off removes the apps' source from the computer, and the versions people made don't open until it's on again.",
                Scope::Device,
                true,
            ),
            // The command bar finds apps as Spotlight does (DK-1); a person
            // hides the ones they don't want there, from the bar itself.
            SettingDef {
                id: "search.hidden_apps",
                section: "Search",
                title: "Hidden from search",
                help: "Apps Ctrl Space doesn't list. Hide one from its row in the search; show it again here. It stays in the launcher and the dock.",
                kind: Kind::Apps,
                scope: Scope::Person,
                default: SettingValue::List(Vec::new()),
            },
            // What the desktop agent reads when the person asks it from the
            // command bar or in a meeting without naming folders (ADR
            // 0048): "grab the ABC report" has somewhere to look. IT may
            // set or lock it, as any of the person's.
            SettingDef {
                id: AGENT_FOLDERS,
                section: "Agents",
                title: "Folders your agent reads",
                help: "When you ask the desktop agent something from Ctrl Space or in a meeting, it may read these folders in your home. It can't change anything in them, and it still asks before it sends anything.",
                kind: Kind::Folders,
                scope: Scope::Person,
                default: SettingValue::List(vec![AGENT_FOLDERS_DEFAULT.to_owned()]),
            },
            toggle(
                "browser.microphone",
                "Privacy",
                "Microphone for websites and apps",
                "Meetings and calls need it; each site still asks the person.",
                Scope::Seat,
                true,
            ),
            // Calendar seals it into each meeting the person organizes, and
            // every computer in a transcribed meeting adds what its own
            // person says (ADR 0045). Only IT turns it on.
            toggle(
                "meetings.transcripts",
                "Meetings",
                "Meeting transcripts",
                "Meetings people organize are transcribed: everyone is told as they join, each computer turns only its own person's words into text, and IT can read the transcript as it does chat. Agents act on what the person asks them in a meeting. Audio and video are never recorded. People may turn it off for meetings they organize, never on.",
                Scope::Person,
                false,
            ),
        ]
    })
}

pub fn def(id: &str) -> Option<&'static SettingDef> {
    table().iter().find(|d| d.id == id)
}

/// Settings only IT turns on: a person may turn one off for themselves
/// where IT turned it on, but never on where IT didn't (ADR 0045).
pub const NARROW_ONLY: &[&str] = &["meetings.transcripts"];

/// Whether a person may only turn `id` off ([`NARROW_ONLY`]).
pub fn narrow_only(id: &str) -> bool {
    NARROW_ONLY.contains(&id)
}

/// Where a person's setting came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "from", content = "group", rename_all = "snake_case")]
pub enum Source {
    /// A group of theirs.
    Group(String),
    Everyone,
    /// The computer's owner, on a computer no organization manages
    /// (ADR 0038).
    Owner,
    /// Nobody set it: the computer's default.
    Default,
}

/// A setting as it applies to someone.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resolved {
    pub id: &'static str,
    pub value: SettingValue,
    /// The person can't change it.
    pub locked: bool,
    pub source: Source,
}

/// `user`'s settings under `doc`: every setting in the table.
pub fn resolve(doc: &PolicyDocument, user: &str) -> Vec<Resolved> {
    let everyone = doc.groups.iter().find(|g| g.name == EVERYONE);
    let theirs: Vec<_> = doc
        .groups
        .iter()
        .filter(|g| g.name != EVERYONE && g.members.iter().any(|m| m == user))
        .collect();
    table()
        .iter()
        .map(|def| {
            let from_group = (def.scope != Scope::Device)
                .then(|| {
                    theirs.iter().find_map(|g| {
                        g.settings
                            .get(def.id)
                            .map(|m| (m, Source::Group(g.name.clone())))
                    })
                })
                .flatten();
            let set = from_group.or_else(|| {
                everyone
                    .and_then(|g| g.settings.get(def.id))
                    .map(|m| (m, Source::Everyone))
            });
            match set {
                Some((Managed { value, locked }, source)) => Resolved {
                    id: def.id,
                    value: value.clone(),
                    locked: *locked,
                    source,
                },
                None => Resolved {
                    id: def.id,
                    value: def.default.clone(),
                    locked: false,
                    source: Source::Default,
                },
            }
        })
        .collect()
}

/// The computer's own settings with `seat` signed in at the screen, or
/// nobody: those for the whole computer from `everyone`, and those that
/// follow whoever is signed in from their groups (ADR 0031).
pub fn computer(doc: &PolicyDocument, seat: Option<&str>) -> Vec<Resolved> {
    resolve(doc, seat.unwrap_or(""))
        .into_iter()
        .filter(|r| def(r.id).is_some_and(|d| d.scope != Scope::Person))
        .collect()
}

/// `value` in words IT reads: "on", "5 minutes", "2 sites".
pub fn describe(def: &SettingDef, value: &SettingValue) -> String {
    match (def.kind, value) {
        (_, SettingValue::Bool(on)) => if *on { "on" } else { "off" }.into(),
        (Kind::Number { min: 0, .. }, SettingValue::Number(0)) => "never".into(),
        (Kind::Number { unit, .. }, SettingValue::Number(n)) => format!("{n} {unit}"),
        (_, SettingValue::Number(n)) => n.to_string(),
        (_, SettingValue::Text(t)) if t.is_empty() => "none".into(),
        (_, SettingValue::Text(t)) => t.clone(),
        (Kind::Apps, SettingValue::List(l)) if l.len() > 3 => format!("{} apps", l.len()),
        (Kind::Folders, SettingValue::List(l)) if l.len() > 3 => format!("{} folders", l.len()),
        (Kind::Networks, SettingValue::List(l)) if l.len() > 3 => format!("{} networks", l.len()),
        (_, SettingValue::List(l)) if l.is_empty() => "none".into(),
        (_, SettingValue::List(l)) if l.len() <= 3 => l.join(", "),
        (_, SettingValue::List(l)) => format!("{} sites", l.len()),
    }
}

/// Whether `zone` is shaped like a time zone's name: `UTC`, or `Area/City`
/// (`America/Argentina/Buenos_Aires`), each part letters, digits, `_`, `+`
/// or `-`. Nothing that could leave the zone folder.
pub fn time_zone_ok(zone: &str) -> bool {
    let parts: Vec<&str> = zone.split('/').collect();
    zone.len() <= 64
        && parts.len() <= 3
        && parts.iter().all(|p| {
            p.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'))
        })
}

fn site_ok(site: &str) -> bool {
    let host = site.strip_prefix("*.").unwrap_or(site);
    !host.is_empty()
        && site.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '/' | ':'))
}

/// Why `value` can't be `def`'s, if it can't.
pub fn check(def: &SettingDef, value: &SettingValue) -> Result<(), String> {
    match (def.kind, value) {
        (Kind::Toggle, SettingValue::Bool(_)) => Ok(()),
        (Kind::Number { min, max, unit }, SettingValue::Number(n)) => {
            if (min..=max).contains(n) {
                Ok(())
            } else {
                Err(format!("{min} to {max} {unit}"))
            }
        }
        (Kind::Url, SettingValue::Text(url)) => {
            let ok = url.is_empty()
                || ((url.starts_with("https://") || url.starts_with("http://"))
                    && url.len() <= 2048
                    && !url.chars().any(|c| c.is_whitespace() || c.is_control()));
            if ok {
                Ok(())
            } else {
                Err("a web address starting https://".into())
            }
        }
        (Kind::Sites, SettingValue::List(sites)) => {
            if sites.len() > 500 {
                return Err("at most 500 sites".into());
            }
            match sites.iter().find(|s| !site_ok(s)) {
                Some(bad) => Err(format!("{bad:?} isn't a site such as example.com")),
                None => Ok(()),
            }
        }
        (Kind::Folders, SettingValue::List(folders)) => {
            if folders.len() > 20 {
                return Err("at most 20 folders".into());
            }
            match folders.iter().find(|f| !folder_ok(f)) {
                Some(bad) => Err(format!(
                    "{bad:?} isn't a folder in the home, such as Documents"
                )),
                None => Ok(()),
            }
        }
        (Kind::Apps, SettingValue::List(apps)) => {
            if apps.len() > 200 {
                return Err("at most 200 apps".into());
            }
            match apps.iter().find(|a| !app_id_ok(a)) {
                Some(bad) => Err(format!("{bad:?} isn't an app's id such as my-mail")),
                None => Ok(()),
            }
        }
        (Kind::Networks, SettingValue::List(names)) => {
            if names.len() > 64 {
                return Err("at most 64 networks".into());
            }
            match names
                .iter()
                .find(|n| n.is_empty() || n.len() > 32 || n.chars().any(char::is_control))
            {
                Some(n) => Err(format!(
                    "{n:?} isn't a Wi-Fi network's name: 1 to 32 characters"
                )),
                None => Ok(()),
            }
        }
        (Kind::TimeZone, SettingValue::Text(zone)) => {
            if zone.is_empty() || time_zone_ok(zone) {
                Ok(())
            } else {
                Err(format!(
                    "{zone:?} isn't a time zone such as America/New_York"
                ))
            }
        }
        (Kind::Toggle, _) => Err("on or off".into()),
        (Kind::Number { .. }, _) => Err("a number".into()),
        (Kind::Url, _) => Err("a web address".into()),
        (Kind::Sites, _) => Err("a list of sites".into()),
        (Kind::TimeZone, _) => Err("a time zone's name".into()),
        (Kind::Apps, _) => Err("a list of apps".into()),
        (Kind::Folders, _) => Err("a list of folders".into()),
        (Kind::Networks, _) => Err("a list of Wi-Fi networks".into()),
    }
}

/// A folder in a home, by its path there: plain names only, never `..`,
/// a root or a control character.
fn folder_ok(folder: &str) -> bool {
    !folder.is_empty()
        && folder.len() <= 200
        && !folder.starts_with('/')
        && !folder.chars().any(char::is_control)
        && folder
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// An app's id: lowercase letters, digits and `-`, as agent and app ids
/// are.
fn app_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn validate(doc: &PolicyDocument, errors: &mut Vec<String>) {
    for group in &doc.groups {
        for (id, managed) in &group.settings {
            let at = format!("group {:?} setting {id}", group.name);
            let Some(def) = def(id) else {
                errors.push(format!("{at}: no such setting"));
                continue;
            };
            if def.scope == Scope::Device && group.name != EVERYONE {
                errors.push(format!(
                    "{at}: applies to the whole computer, so it is set for everyone"
                ));
            }
            if let Err(e) = check(def, &managed.value) {
                errors.push(format!("{at}: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEFAULT_POLICY, Policy};

    fn doc(extra: &str) -> PolicyDocument {
        toml::from_str(&format!("{DEFAULT_POLICY}\n{extra}")).unwrap()
    }

    const TWO_GROUPS: &str = r#"
[[group]]
name = "ap-ops"
members = ["dana"]
[group.settings]
"lock.after_minutes" = { value = 5, locked = true }

[[group]]
name = "finance"
members = ["dana", "mei"]
[group.settings]
"lock.after_minutes" = { value = 30 }
"clock.hour24" = { value = true }
"#;

    #[test]
    fn ux1_a_persons_setting_comes_from_their_first_group_then_everyone_then_the_default() {
        let mut d = doc(TWO_GROUPS);
        let everyone = d.groups.iter_mut().find(|g| g.name == EVERYONE).unwrap();
        everyone.settings.insert(
            "browser.restore_tabs".into(),
            Managed {
                value: SettingValue::Bool(true),
                locked: false,
            },
        );
        let get =
            |user: &str, id: &str| resolve(&d, user).into_iter().find(|r| r.id == id).unwrap();
        let dana = get("dana", "lock.after_minutes");
        assert_eq!(
            (dana.value, dana.locked, dana.source),
            (
                SettingValue::Number(5),
                true,
                Source::Group("ap-ops".into())
            )
        );
        let mei = get("mei", "lock.after_minutes");
        assert_eq!((mei.value, mei.locked), (SettingValue::Number(30), false));
        assert_eq!(get("dana", "clock.hour24").value, SettingValue::Bool(true));
        assert_eq!(get("sam", "browser.restore_tabs").source, Source::Everyone);
        assert_eq!(get("sam", "lock.after_minutes").source, Source::Default);
    }

    #[test]
    fn adr0031_a_time_zone_is_a_name_that_stays_in_the_zone_folder() {
        for zone in [
            "UTC",
            "America/New_York",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+5",
        ] {
            assert!(time_zone_ok(zone), "{zone}");
        }
        for zone in [
            "",
            "../etc/passwd",
            "America/../../x",
            "/etc/localtime",
            "A/B/C/D",
            "America/New York",
        ] {
            assert!(!time_zone_ok(zone), "{zone}");
        }
        let def = def("region.time_zone").unwrap();
        assert!(
            check(def, &SettingValue::Text(String::new())).is_ok(),
            "the computer's own"
        );
        assert!(check(def, &SettingValue::Text("../x".into())).is_err());
    }

    #[test]
    fn adr0031_the_computer_follows_whoever_is_signed_in_and_everyone_while_nobody_is() {
        let policy = Policy::from_toml(&format!(
            "{DEFAULT_POLICY}
[[group]]
name = \"kiosk\"
members = [\"dana\"]
[group.settings]
\"browser.blocked_sites\" = {{ value = [\"games.example\"], locked = true }}
\"devices.usb_storage\" = {{ value = false, locked = true }}
"
        ))
        .unwrap();
        let get = |seat: Option<&str>, id: &str| {
            policy
                .computer_settings(seat)
                .into_iter()
                .find(|r| r.id == id)
                .unwrap()
        };
        let dana = get(Some("dana"), "devices.usb_storage");
        assert_eq!(
            (dana.value, dana.source),
            (SettingValue::Bool(false), Source::Group("kiosk".into()))
        );
        assert_eq!(
            get(Some("dana"), "browser.blocked_sites").value,
            SettingValue::List(vec!["games.example".into()])
        );
        // Someone else, or nobody: everyone's, here the table's.
        for seat in [Some("mei"), None] {
            assert_eq!(
                get(seat, "devices.usb_storage").value,
                SettingValue::Bool(true)
            );
        }
        // A person's own settings aren't the computer's; the time zone is.
        let ids: Vec<_> = policy
            .computer_settings(None)
            .iter()
            .map(|r| r.id)
            .collect();
        assert!(ids.contains(&"region.time_zone") && !ids.contains(&"lock.after_minutes"));
        // Dana's Settings app shows what the computer applies for them.
        assert_eq!(
            policy
                .settings_for("dana")
                .into_iter()
                .find(|r| r.id == "devices.usb_storage")
                .unwrap()
                .value,
            SettingValue::Bool(false)
        );
    }

    #[test]
    fn ux1_settings_are_checked_and_the_computers_own_are_for_everyone() {
        let bad = |extra: &str| {
            Policy::from_toml(&format!(
                "{DEFAULT_POLICY}\n[[group]]\nname = \"ap-ops\"\n[group.settings]\n{extra}\n"
            ))
            .unwrap_err()
            .to_string()
        };
        assert!(bad("\"lock.after_minutes\" = { value = 0 }").contains("1 to 120 minutes"));
        assert!(bad("\"lock.after_minutes\" = { value = true }").contains("a number"));
        assert!(bad("\"teleport.on\" = { value = true }").contains("no such setting"));
        assert!(
            bad("\"region.time_zone\" = { value = \"Asia/Tokyo\" }").contains("set for everyone")
        );
        let everyone = |id: &str, value: SettingValue| {
            let mut d = doc("");
            d.groups
                .iter_mut()
                .find(|g| g.name == EVERYONE)
                .unwrap()
                .settings
                .insert(
                    id.into(),
                    Managed {
                        value,
                        locked: true,
                    },
                );
            Policy::from_document(d).map_err(|e| e.to_string())
        };
        let sites = |s: &[&str]| SettingValue::List(s.iter().map(|x| (*x).to_owned()).collect());
        assert!(
            everyone(
                "browser.blocked_sites",
                sites(&["games.example", "*.social.example"])
            )
            .is_ok()
        );
        assert!(
            everyone("browser.blocked_sites", sites(&["no spaces please"]))
                .unwrap_err()
                .contains("isn't a site")
        );
        assert!(
            everyone(
                "browser.home_page",
                SettingValue::Text("javascript:alert(1)".into())
            )
            .unwrap_err()
            .contains("https://")
        );
    }

    #[test]
    fn dk1_hidden_from_search_is_a_persons_list_of_app_ids() {
        let def = def("search.hidden_apps").unwrap();
        assert_eq!(def.scope, Scope::Person);
        let list =
            |apps: &[&str]| SettingValue::List(apps.iter().map(|a| (*a).to_owned()).collect());
        assert!(check(def, &list(&[])).is_ok());
        assert!(check(def, &list(&["mail", "my-mail", "dispute-tracker"])).is_ok());
        assert!(check(def, &list(&["My Mail"])).is_err());
        assert!(check(def, &list(&["../etc"])).is_err());
        assert!(check(def, &list(&[""])).is_err());
        assert!(check(def, &SettingValue::Bool(true)).is_err());
        assert_eq!(describe(def, &list(&["a", "b", "c", "d"])), "4 apps");
        let many: Vec<String> = (0..201).map(|n| format!("app-{n}")).collect();
        assert!(check(def, &SettingValue::List(many)).is_err());
    }
}
