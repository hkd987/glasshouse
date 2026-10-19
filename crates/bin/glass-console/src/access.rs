//! Who may do what in the console (CF-80, CF-81, ADR 0028). IT's admins
//! run the org; support staff hold the roles policy names for their
//! directory groups and reach remote help only. Every route needs an admin
//! unless the server names it as a support route.
//!
//! Roles come from the newest policy the pilot ring runs, so a change to
//! them takes effect once IT publishes it. Being an admin gives no support
//! tools: only a role naming one of their groups does.

use std::collections::BTreeSet;

use glass_policy::document::{Support, SupportRole, SupportTool};

use crate::directory::{Directory, EVERYONE};

/// What a route needs of whoever calls it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// One of IT's admins: every route's default.
    Admin,
    /// Anyone holding a support role.
    Support,
}

/// What someone signed in may do.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Access {
    pub admin: bool,
    /// The support roles their directory groups hold.
    pub roles: Vec<SupportRole>,
}

impl Access {
    pub fn of(user: &str, directory: &dyn Directory, support: &Support) -> Self {
        let roles = support
            .roles
            .iter()
            .filter(|role| {
                // Policy refuses an everyone role; never one here either.
                role.groups
                    .iter()
                    .any(|g| g != EVERYONE && directory.members(g).contains(&user))
            })
            .cloned()
            .collect();
        Self {
            admin: directory.is_admin(user),
            roles,
        }
    }

    /// Whether they may use the console at all.
    pub fn any(&self) -> bool {
        self.admin || !self.roles.is_empty()
    }

    pub fn allows(&self, need: Need) -> bool {
        match need {
            Need::Admin => self.admin,
            Need::Support => !self.roles.is_empty(),
        }
    }

    /// Every tool their roles give them.
    pub fn tools(&self) -> BTreeSet<SupportTool> {
        self.roles
            .iter()
            .flat_map(|r| r.tools.iter().copied())
            .collect()
    }

    /// What the console calls them: "IT admin", their roles' names, or both.
    pub fn title(&self) -> String {
        let mut names: Vec<&str> = Vec::new();
        if self.admin {
            names.push("IT admin");
        }
        names.extend(self.roles.iter().map(|r| r.name.as_str()));
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::LocalDirectory;

    fn tiers() -> Support {
        glass_policy::Policy::from_toml(&format!(
            "{}\n{}",
            glass_policy::DEFAULT_POLICY,
            glass_policy::SUPPORT_TIERS
        ))
        .unwrap()
        .support()
        .clone()
    }

    #[test]
    fn cf81_roles_come_from_directory_groups_and_admins_get_no_tools_by_being_admins() {
        let dir = LocalDirectory::starter("root");
        let support = tiers();
        let jordan = Access::of("jordan", &dir, &support);
        assert!(!jordan.admin && jordan.allows(Need::Support) && !jordan.allows(Need::Admin));
        assert_eq!(
            jordan.tools().into_iter().collect::<Vec<_>>(),
            [SupportTool::View, SupportTool::Control]
        );
        assert_eq!(jordan.title(), "IT Help Desk");

        let pat = Access::of("pat", &dir, &support);
        assert!(pat.allows(Need::Admin) && !pat.allows(Need::Support));
        assert!(pat.tools().is_empty(), "separation of duties");

        let dana = Access::of("dana", &dir, &support);
        assert!(!dana.any());
        // No roles in force: support staff are nobody yet.
        assert!(!Access::of("jordan", &dir, &Support::default()).any());
    }
}
