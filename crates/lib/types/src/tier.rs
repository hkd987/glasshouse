use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The approval tier an action runs under (PRD "Approval tiers").
///
/// Variants are ordered from least to most strict, so `max` picks the
/// stricter of two tiers. Users and task scopes may only move an action to a
/// stricter tier, never a looser one (DA-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Runs with no prompt. Logged and undoable.
    Auto,
    /// Runs, then shows a quiet line in the tile feed. Undoable.
    Notify,
    /// The task goes to "Needs you" and waits for the user.
    Ask,
    /// Blocked, and listed under "Can't touch".
    Never,
}

impl Tier {
    pub const ALL: [Tier; 4] = [Tier::Auto, Tier::Notify, Tier::Ask, Tier::Never];

    /// The stricter of two tiers.
    pub fn stricter(self, other: Tier) -> Tier {
        self.max(other)
    }

    /// Whether the action may run without the user answering a prompt.
    pub fn runs_unattended(self) -> bool {
        matches!(self, Tier::Auto | Tier::Notify)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Auto => "auto",
            Tier::Notify => "notify",
            Tier::Ask => "ask",
            Tier::Never => "never",
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Tier {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Tier::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or_else(|| format!("unknown tier {s:?}; expected auto, notify, ask or never"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_by_strictness() {
        assert!(Tier::Auto < Tier::Notify);
        assert!(Tier::Notify < Tier::Ask);
        assert!(Tier::Ask < Tier::Never);
        assert_eq!(Tier::Notify.stricter(Tier::Ask), Tier::Ask);
        assert_eq!(Tier::Never.stricter(Tier::Auto), Tier::Never);
    }

    #[test]
    fn parses_and_displays() {
        for t in Tier::ALL {
            assert_eq!(t.to_string().parse::<Tier>().unwrap(), t);
        }
        assert!("sometimes".parse::<Tier>().is_err());
    }
}
