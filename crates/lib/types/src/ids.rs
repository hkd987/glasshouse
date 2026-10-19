use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Error for an identifier that fails to parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} id: {value:?}")]
pub struct InvalidId {
    kind: &'static str,
    value: String,
}

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// A new, time-ordered identifier.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// The last eight hex digits, for display and names. These are
            /// random bits; the leading digits of a v7 id are a timestamp and
            /// repeat for ids made within about a minute of each other.
            pub fn short(&self) -> String {
                let simple = self.0.simple().to_string();
                simple[simple.len() - 8..].to_owned()
            }

            /// Whether `reference` names this id: the full id (with or without
            /// hyphens) or a trailing part of at least six hex digits, such as
            /// the [`short`](Self::short) form.
            pub fn matches_ref(&self, reference: &str) -> bool {
                let reference = reference.replace('-', "").to_ascii_lowercase();
                let simple = self.0.simple().to_string();
                reference == simple || (reference.len() >= 6 && simple.ends_with(&reference))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = InvalidId;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self).map_err(|_| InvalidId {
                    kind: $kind,
                    value: s.to_owned(),
                })
            }
        }
    };
}

uuid_id!(
    /// One delegated task: a plain-language request turned into a scoped plan
    /// and run by exactly one agent in its own sandbox.
    TaskId,
    "task"
);
uuid_id!(
    /// One pending or answered approval ("Needs you") on a task.
    ApprovalId,
    "approval"
);
uuid_id!(
    /// One point-in-time snapshot of a task's data, used for rollback.
    SnapshotId,
    "snapshot"
);

/// Error for an agent id that is not a valid slug.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid agent id {0:?}: use 1-64 lowercase letters, digits and dashes")]
pub struct InvalidAgentId(String);

/// The catalog identity of an agent, such as `invoice-agent`.
///
/// This names the agent *kind* that IT approved and published (AD-5). A
/// running instance is identified by its [`TaskId`] and acts as an
/// [`crate::AgentPrincipal`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

/// Ids of the apps people make for themselves start with this (ADR 0032):
/// nothing IT publishes, installs or names in policy may, so a person's app
/// never takes on another's identity or its rules.
pub const PERSONAL_PREFIX: &str = "my-";

impl AgentId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// One person's own app (ADR 0032), not the org's.
    pub fn is_personal(&self) -> bool {
        self.0.starts_with(PERSONAL_PREFIX)
    }
}

impl TryFrom<String> for AgentId {
    type Error = InvalidAgentId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = (1..=64).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !value.starts_with('-')
            && !value.ends_with('-');
        if valid {
            Ok(Self(value))
        } else {
            Err(InvalidAgentId(value))
        }
    }
}

impl FromStr for AgentId {
    type Err = InvalidAgentId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids_round_trip_and_match_by_reference() {
        let id = TaskId::new();
        let parsed: TaskId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
        assert!(id.matches_ref(&id.short()));
        assert!(id.matches_ref(&id.to_string()));
        assert!(!id.matches_ref(""));
        assert!(
            !id.matches_ref(&id.short()[4..]),
            "too short to be unambiguous"
        );
    }

    #[test]
    fn short_ids_differ_for_ids_made_together() {
        let ids: Vec<TaskId> = (0..64).map(|_| TaskId::new()).collect();
        let shorts: std::collections::HashSet<String> = ids.iter().map(TaskId::short).collect();
        assert_eq!(shorts.len(), ids.len());
    }

    #[test]
    fn task_ids_are_time_ordered() {
        let a = TaskId::new();
        let b = TaskId::new();
        assert!(a < b);
    }

    #[test]
    fn agent_ids_are_slugs() {
        assert!(AgentId::from_str("invoice-agent").is_ok());
        assert!(AgentId::from_str("claude-code").is_ok());
        for bad in [
            "",
            "Invoice",
            "a b",
            "-lead",
            "trail-",
            "x".repeat(65).as_str(),
        ] {
            assert!(
                AgentId::from_str(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn agent_id_deserialization_validates() {
        assert!(serde_json::from_str::<AgentId>("\"ok-agent\"").is_ok());
        assert!(serde_json::from_str::<AgentId>("\"Not OK\"").is_err());
    }
}
