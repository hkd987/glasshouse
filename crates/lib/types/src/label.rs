use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A data-sensitivity label (PRD "Data labels").
///
/// Ordered from least to most sensitive. Content derived from a labeled
/// source inherits the label (DP-11), so combining content takes the `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Label {
    /// Can go to any approved model.
    Public,
    /// Can go to approved cloud models under enterprise terms.
    Internal,
    /// Local model or a specific private deployment only.
    Confidential,
    /// No model. Agents can't open it.
    Restricted,
}

impl Label {
    pub const ALL: [Label; 4] = [
        Label::Public,
        Label::Internal,
        Label::Confidential,
        Label::Restricted,
    ];

    /// Whether any agent may open content with this label at all.
    pub fn agent_may_open(self) -> bool {
        self != Label::Restricted
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Label::Public => "public",
            Label::Internal => "internal",
            Label::Confidential => "confidential",
            Label::Restricted => "restricted",
        }
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Label {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Label::ALL
            .into_iter()
            .find(|l| l.as_str() == s)
            .ok_or_else(|| {
                format!(
                    "unknown label {s:?}; expected public, internal, confidential or restricted"
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_content_inherits_the_stricter_label() {
        assert_eq!(
            Label::Internal.max(Label::Confidential),
            Label::Confidential
        );
        assert_eq!(Label::Public.max(Label::Public), Label::Public);
    }

    #[test]
    fn restricted_is_never_opened_by_agents() {
        assert!(!Label::Restricted.agent_may_open());
        assert!(Label::Confidential.agent_may_open());
    }

    #[test]
    fn parses_and_displays() {
        for l in Label::ALL {
            assert_eq!(l.to_string().parse::<Label>().unwrap(), l);
        }
    }
}
