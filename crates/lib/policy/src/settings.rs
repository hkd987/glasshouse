//! Device settings IT manages per group (UX-1, AD-2): one table of what a
//! computer can be set to, shared by policy checks, the admin console's
//! Settings page and the device's Settings app.
//!
//! A group sets a setting either as a default the person may change or
//! locked. A person's setting comes from the first of their groups, in the
//! policy's order, that sets it, then from `everyone`, then from the table.
//! Settings that apply to the whole computer (the browser's) come from
//! `everyone` only: a computer has one browser policy whoever uses it.

use serde::Serialize;

use crate::EVERYONE;
use crate::document::{Managed, PolicyDocument, SettingValue};

/// Whom a setting is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Each person's own, set per group.
    Person,
    /// The whole computer's, set for everyone.
    Device,
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
                "Shows 14:00 instead of 2:00 PM in the menu bar.",
                Scope::Person,
                false,
            ),
            SettingDef {
                id: "browser.home_page",
                section: "Browser",
                title: "Home page",
                help: "Opens with the browser and its home button.",
                kind: Kind::Url,
                scope: Scope::Device,
                default: SettingValue::Text(String::new()),
            },
            SettingDef {
                id: "browser.blocked_sites",
                section: "Browser",
                title: "Blocked sites",
                help: "Sites nobody can open on this computer, such as example.com or *.games.example.",
                kind: Kind::Sites,
                scope: Scope::Device,
                default: SettingValue::List(Vec::new()),
            },
            toggle(
                "browser.restore_tabs",
                "Browser",
                "Reopen last session's tabs",
                "Otherwise the browser starts at the home page.",
                Scope::Device,
                true,
            ),
            toggle(
                "browser.camera",
                "Privacy",
                "Camera for websites and apps",
                "Meetings and calls need it; each site still asks the person.",
                Scope::Device,
                true,
            ),
            toggle(
                "browser.microphone",
                "Privacy",
                "Microphone for websites and apps",
                "Meetings and calls need it; each site still asks the person.",
                Scope::Device,
                true,
            ),
        ]
    })
}

pub fn def(id: &str) -> Option<&'static SettingDef> {
    table().iter().find(|d| d.id == id)
}

/// Where a person's setting came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "from", content = "group", rename_all = "snake_case")]
pub enum Source {
    /// A group of theirs.
    Group(String),
    Everyone,
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
            let from_group = (def.scope == Scope::Person)
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

/// The computer's own settings: those for the whole computer, from
/// `everyone`.
pub fn device(doc: &PolicyDocument) -> Vec<Resolved> {
    resolve(doc, "")
        .into_iter()
        .filter(|r| def(r.id).is_some_and(|d| d.scope == Scope::Device))
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
        (_, SettingValue::List(l)) if l.is_empty() => "none".into(),
        (_, SettingValue::List(l)) if l.len() <= 3 => l.join(", "),
        (_, SettingValue::List(l)) => format!("{} sites", l.len()),
    }
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
        (Kind::Toggle, _) => Err("on or off".into()),
        (Kind::Number { .. }, _) => Err("a number".into()),
        (Kind::Url, _) => Err("a web address".into()),
        (Kind::Sites, _) => Err("a list of sites".into()),
    }
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
            bad("\"browser.home_page\" = { value = \"https://intranet.example\" }")
                .contains("set for everyone")
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
}
