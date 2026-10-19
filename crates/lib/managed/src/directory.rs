//! The org's people, as devices get them (AD-1): who to address mail and
//! invitations to, for autocomplete in Mail and Calendar and for agents
//! looking someone up (`people.search`).
//!
//! Just enough to address someone: name, email, title and department. No
//! phone numbers, managers or employee ids. The console signs the whole
//! list as a release of its own kind ([`DocKind::Directory`]), versioned
//! like the catalog and with no rings: a new hire shouldn't wait for a
//! policy rollout to be found. Every change sends the whole list, so a
//! release has a ceiling ([`MAX_PEOPLE`], [`MAX_PAYLOAD_BYTES`]).
//!
//! [`DocKind::Directory`]: crate::DocKind::Directory

use std::collections::BTreeSet;
use std::sync::{Arc, PoisonError, RwLock};

use serde::{Deserialize, Serialize};

/// The most people one release carries.
pub const MAX_PEOPLE: usize = 25_000;
/// The largest release payload, in bytes: about 25,000 people, and well
/// under what a device reads in one sync.
pub const MAX_PAYLOAD_BYTES: usize = 4 << 20;
/// The longest field.
const MAX_FIELD: usize = 200;

/// A person in the org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    /// Their login name, as devices know them.
    pub login: String,
    /// What people call them: "Priya Shah".
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub department: String,
}

/// A directory release's payload: everyone, by login.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryRelease {
    pub people: Vec<Person>,
}

impl DirectoryRelease {
    /// Checks a release whole: one person per login and per email, every
    /// field one line, emails shaped like addresses, within the ceiling.
    pub fn check(&self) -> Result<(), String> {
        if self.people.len() > MAX_PEOPLE {
            return Err(format!(
                "{} people is more than a directory release carries ({MAX_PEOPLE})",
                self.people.len()
            ));
        }
        let mut logins = BTreeSet::new();
        let mut emails = BTreeSet::new();
        for p in &self.people {
            if p.login.is_empty() || !logins.insert(p.login.as_str()) {
                return Err(format!("the login {:?} is empty or used twice", p.login));
            }
            for (what, text) in [
                ("name", &p.name),
                ("email", &p.email),
                ("title", &p.title),
                ("department", &p.department),
                ("login", &p.login),
            ] {
                if text.len() > MAX_FIELD || text.chars().any(char::is_control) {
                    return Err(format!("{}: the {what} must be one short line", p.login));
                }
            }
            if !p.email.is_empty() {
                if !is_address(&p.email) {
                    return Err(format!("{}: {:?} isn't an email address", p.login, p.email));
                }
                if !emails.insert(p.email.to_lowercase()) {
                    return Err(format!("{} is two people's email", p.email));
                }
            }
        }
        Ok(())
    }

    pub fn person(&self, login: &str) -> Option<&Person> {
        self.people.iter().find(|p| p.login == login)
    }

    /// The person whose email is `email`, ignoring case: who an invitation
    /// to that address reaches in the org (CF-21).
    pub fn by_email(&self, email: &str) -> Option<&Person> {
        let email = email.trim();
        self.people
            .iter()
            .find(|p| !p.email.is_empty() && p.email.eq_ignore_ascii_case(email))
    }
}

/// Whether `email` is shaped like an address mail can go to: one `@`, a
/// dotted domain, nothing that would split or quote it in a header.
pub fn is_address(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && email.len() <= 254
        && !email.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"')
        })
}

/// The people `query` finds, best first, at most `limit`: every word of it
/// starts a word of their name, email, login, title or department. An
/// exact email or login comes first, then a name that starts with the
/// query, then first names, last names, emails and the rest; ties by name.
pub fn search<'a>(people: &'a [Person], query: &str, limit: usize) -> Vec<&'a Person> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut found: Vec<(u8, &Person)> = people
        .iter()
        .filter_map(|p| {
            let name = p.name.to_lowercase();
            let email = p.email.to_lowercase();
            let login = p.login.to_lowercase();
            let mut tokens: Vec<String> = name.split_whitespace().map(str::to_owned).collect();
            tokens.extend(
                email
                    .split(['@', '.', '_', '-', '+'])
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned),
            );
            tokens.push(email.clone());
            tokens.push(login.clone());
            tokens.extend(p.title.to_lowercase().split_whitespace().map(str::to_owned));
            tokens.extend(
                p.department
                    .to_lowercase()
                    .split_whitespace()
                    .map(str::to_owned),
            );
            if !words
                .iter()
                .all(|w| tokens.iter().any(|t| t.starts_with(w)))
            {
                return None;
            }
            let first = name.split_whitespace().next().unwrap_or_default();
            let last = name.split_whitespace().last().unwrap_or_default();
            let rank = if email == query || login == query {
                0
            } else if name.starts_with(&query) {
                1
            } else if first.starts_with(words[0]) {
                2
            } else if last.starts_with(words[0]) {
                3
            } else if email.starts_with(words[0]) {
                4
            } else {
                5
            };
            Some((rank, p))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().take(limit).map(|(_, p)| p).collect()
}

/// A directory release a device verified, with its version.
#[derive(Debug, Clone)]
pub struct Applied {
    pub version: u64,
    pub release: DirectoryRelease,
}

/// The device's directory, shared between the sync that applies releases
/// and the actions that read it. Each release is swapped in whole.
#[derive(Debug, Clone, Default)]
pub struct DirectoryHandle(Arc<RwLock<Option<Arc<Applied>>>>);

impl DirectoryHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// The directory in force, if one has arrived.
    pub fn current(&self) -> Option<Arc<Applied>> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, applied: Applied) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(applied));
    }

    /// The person with this login, if the directory in force has them.
    pub fn person(&self, login: &str) -> Option<Person> {
        self.current()
            .and_then(|d| d.release.person(login).cloned())
    }

    /// Who `login` is when they send mail or invitations: their name and
    /// address from the directory, or their login alone without one.
    pub fn sender(&self, login: &str) -> Sender {
        match self.person(login) {
            Some(p) if !p.email.is_empty() => Sender {
                name: p.name,
                email: Some(p.email),
            },
            _ => Sender {
                name: login.to_owned(),
                email: None,
            },
        }
    }
}

/// Who sends a message or an invitation (AU-10 says which agent did it for
/// them; this says who they are).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    pub name: String,
    pub email: Option<String>,
}

impl Sender {
    /// For a mail header: `Priya Shah <priya@acme.example>`, the name
    /// quoted when it has characters that would split it; the login alone
    /// without an address.
    pub fn mailbox(&self) -> String {
        let Some(email) = &self.email else {
            return self.name.clone();
        };
        let plain = self
            .name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '\'' | '!' | '#' | '&'));
        if plain {
            format!("{} <{email}>", self.name)
        } else {
            let quoted = self.name.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{quoted}\" <{email}>")
        }
    }

    /// The address replies go to: the email, or the login without one.
    pub fn address(&self) -> &str {
        self.email.as_deref().unwrap_or(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(login: &str, name: &str, email: &str, title: &str) -> Person {
        Person {
            login: login.into(),
            name: name.into(),
            email: email.into(),
            title: title.into(),
            department: String::new(),
        }
    }

    fn people() -> Vec<Person> {
        vec![
            person("priya", "Priya Shah", "priya.shah@acme.example", "Counsel"),
            person("dana", "Dana Ruiz", "dana@acme.example", "AP lead"),
            person("sam", "Sam Lee", "sam.lee@acme.example", "Analyst"),
            person("pat", "Pat Okafor", "pat@acme.example", "IT admin"),
            person("lee", "Lee Park", "lee.park@acme.example", "Designer"),
        ]
    }

    fn names(found: &[&Person]) -> Vec<String> {
        found.iter().map(|p| p.name.clone()).collect()
    }

    #[test]
    fn ad1_search_finds_by_name_email_title_and_ranks_names_first() {
        let people = people();
        assert_eq!(names(&search(&people, "pri", 8)), ["Priya Shah"]);
        assert_eq!(names(&search(&people, "shah", 8)), ["Priya Shah"]);
        assert_eq!(names(&search(&people, "counsel", 8)), ["Priya Shah"]);
        assert_eq!(
            names(&search(&people, "dana@acme.example", 8)),
            ["Dana Ruiz"]
        );
        // "Lee" is Lee Park's first name and Sam Lee's last: first names first.
        assert_eq!(names(&search(&people, "lee", 8)), ["Lee Park", "Sam Lee"]);
        assert_eq!(names(&search(&people, "LEE", 1)), ["Lee Park"]);
    }

    #[test]
    fn ad1_search_needs_every_word() {
        let people = people();
        assert_eq!(names(&search(&people, "sam an", 8)), ["Sam Lee"]);
        assert!(search(&people, "sam counsel", 8).is_empty());
        assert!(search(&people, "  ", 8).is_empty());
        assert!(search(&people, "zz", 8).is_empty());
    }

    #[test]
    fn ad1_a_release_has_one_person_per_login_and_email() {
        let ok = DirectoryRelease { people: people() };
        assert_eq!(ok.check(), Ok(()));
        let mut twice = ok.clone();
        twice
            .people
            .push(person("dana", "Other", "other@acme.example", ""));
        assert!(twice.check().is_err());
        let mut same_email = ok.clone();
        same_email
            .people
            .push(person("dana2", "Dana Two", "DANA@acme.example", ""));
        assert!(same_email.check().unwrap_err().contains("two people's"));
        for bad in [
            "dana",
            "dana@",
            "@acme.example",
            "a b@acme.example",
            "x@y",
            "a<b@c.d",
        ] {
            let mut bad_email = ok.clone();
            bad_email.people.push(person("x", "X", bad, ""));
            assert!(bad_email.check().is_err(), "{bad}");
        }
        let mut two_lines = ok;
        two_lines.people.push(person("y", "Y\nZ", "", ""));
        assert!(two_lines.check().is_err());
    }

    #[test]
    fn a_directory_release_cant_pass_for_a_catalog() {
        use crate::{DocKind, OrgKey};
        let key = OrgKey::from_seed([7; 32]);
        let payload =
            serde_json::to_string(&DirectoryRelease { people: people() }).expect("serializes");
        let signed = key.sign(DocKind::Directory, "acme", 1, payload);
        let keys = [key.public()];
        assert!(
            signed
                .open::<DirectoryRelease>(DocKind::Directory, "acme", &keys, None)
                .is_ok()
        );
        assert!(
            signed
                .open::<crate::CatalogRelease>(DocKind::Catalog, "acme", &keys, None)
                .is_err()
        );
    }

    #[test]
    fn the_handle_swaps_releases_whole() {
        let handle = DirectoryHandle::new();
        assert!(handle.current().is_none());
        handle.set(Applied {
            version: 3,
            release: DirectoryRelease { people: people() },
        });
        assert_eq!(handle.current().map(|d| d.version), Some(3));
        assert_eq!(handle.person("sam").map(|p| p.name), Some("Sam Lee".into()));
        assert!(handle.person("nobody").is_none());
    }

    #[test]
    fn au10_a_sender_is_their_name_and_address_or_their_login() {
        let handle = DirectoryHandle::new();
        assert_eq!(handle.sender("demo").mailbox(), "demo");
        let mut people = people();
        people.push(person("o", "O'Brien, Jo \"J\"", "jo@acme.example", ""));
        handle.set(Applied {
            version: 1,
            release: DirectoryRelease { people },
        });
        let priya = handle.sender("priya");
        assert_eq!(priya.mailbox(), "Priya Shah <priya.shah@acme.example>");
        assert_eq!(priya.address(), "priya.shah@acme.example");
        assert_eq!(
            handle.sender("o").mailbox(),
            "\"O'Brien, Jo \\\"J\\\"\" <jo@acme.example>"
        );
        assert_eq!(handle.sender("nobody").address(), "nobody");
    }
}
