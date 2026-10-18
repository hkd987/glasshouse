use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Error for an action name that is not `app.verb` shaped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid action name {0:?}: use 2-4 dot-separated segments of lowercase letters, digits and underscores"
)]
pub struct InvalidActionName(String);

/// The name of an app action in the registry, such as `files.write` or
/// `mail.send` (NV-1).
///
/// The first segment names the app that declares the action. Policy rules
/// match names with patterns such as `files.*` (see `glass-policy`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ActionName(String);

impl ActionName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The app that declares this action (the first segment).
    pub fn app(&self) -> &str {
        self.0.split('.').next().unwrap_or_default()
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }
}

impl TryFrom<String> for ActionName {
    type Error = InvalidActionName;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let segments: Vec<&str> = value.split('.').collect();
        let valid = (2..=4).contains(&segments.len())
            && segments.iter().all(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(InvalidActionName(value))
        }
    }
}

impl FromStr for ActionName {
    type Err = InvalidActionName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<ActionName> for String {
    fn from(name: ActionName) -> Self {
        name.0
    }
}

impl fmt::Display for ActionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What an action does to the world, as declared by the app (NV-1).
///
/// This decides how the action can be undone (PRD "Undo") and is used to warn
/// when policy puts an irreversible action below the Ask tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    /// Reads only. Nothing to undo.
    None,
    /// Changes local state that snapshots or an app transaction log restore.
    LocalReversible,
    /// Leaves the machine or can't be undone: sent mail, posted records,
    /// payments, deletes outside snapshotted data.
    Irreversible,
}

impl SideEffect {
    pub fn is_reversible(self) -> bool {
        !matches!(self, SideEffect::Irreversible)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_app_verb_names() {
        let name: ActionName = "files.write".parse().unwrap();
        assert_eq!(name.app(), "files");
        assert!(ActionName::from_str("mail.send_batch").is_ok());
        assert!(ActionName::from_str("crm.invoice.create").is_ok());
    }

    #[test]
    fn rejects_malformed_names() {
        for bad in [
            "files",
            "files.",
            ".write",
            "Files.write",
            "a.b.c.d.e",
            "files.*",
            "a-b.c",
        ] {
            assert!(
                ActionName::from_str(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }
}
