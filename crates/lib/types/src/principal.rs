use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{AgentId, TaskId};

/// A human user of the machine, as the OS knows them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserRef {
    /// Login name. In managed fleets this maps to the Entra ID or Okta
    /// identity the device joined with (AD-1).
    pub name: String,
    pub uid: u32,
}

impl fmt::Display for UserRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// An agent acting for a user on one task: the first-class principal the
/// whole product is built around.
///
/// Every audit record, attribution tag ("by [agent] for [user]", AU-10) and
/// downstream on-behalf-of token (PA-4) is derived from this.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentPrincipal {
    pub agent: AgentId,
    pub on_behalf_of: UserRef,
    pub task: TaskId,
}

impl AgentPrincipal {
    /// The attribution line shown inline by first-party apps (AU-10).
    pub fn attribution(&self) -> String {
        format!("by {} for {}", self.agent, self.on_behalf_of)
    }
}

impl fmt::Display for AgentPrincipal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "agent:{} for user:{} (task {})",
            self.agent,
            self.on_behalf_of,
            self.task.short()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_names_agent_and_user() {
        let p = AgentPrincipal {
            agent: "invoice-agent".parse().unwrap(),
            on_behalf_of: UserRef {
                name: "demo".into(),
                uid: 1001,
            },
            task: TaskId::new(),
        };
        assert_eq!(p.attribution(), "by invoice-agent for demo");
    }
}
