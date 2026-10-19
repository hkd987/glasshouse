//! Who is in the org: people and the groups policy applies to (AD-1).
//!
//! Today a local file, `directory.toml` in the console's state directory;
//! later SCIM from Entra ID or Okta behind the same [`Directory`] trait.
//! Roles are groups: the members of [`ADMINS`] may sign in to the console,
//! and the same group's apps in policy put IT's tools in their profiles.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// The group whose members administer the org.
pub const ADMINS: &str = "it-admins";
/// The group policy applies to everyone in.
pub const EVERYONE: &str = glass_policy::EVERYONE;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    /// The id policy uses, such as `ap-ops`.
    pub name: String,
    /// What IT calls it: "AP Operations".
    #[serde(default)]
    pub display: String,
    /// Where it comes from, such as "Entra ID group AP-Ops".
    #[serde(default)]
    pub source: String,
    /// Login names. Ignored for `everyone`, which is every user.
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct User {
    /// Login name, as devices know them.
    pub name: String,
    #[serde(default)]
    pub display: String,
}

/// The org's people and groups.
pub trait Directory: Send + Sync {
    fn users(&self) -> &[User];
    fn groups(&self) -> &[Group];

    fn group(&self, name: &str) -> Option<&Group> {
        self.groups().iter().find(|g| g.name == name)
    }

    /// The members of `group`: everyone for `everyone`.
    fn members(&self, group: &str) -> Vec<&str> {
        if group == EVERYONE {
            return self.users().iter().map(|u| u.name.as_str()).collect();
        }
        self.group(group)
            .map(|g| g.members.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    fn is_admin(&self, user: &str) -> bool {
        self.group(ADMINS)
            .is_some_and(|g| g.members.iter().any(|m| m == user))
    }

    fn display_name(&self, user: &str) -> String {
        self.users()
            .iter()
            .find(|u| u.name == user && !u.display.is_empty())
            .map_or_else(|| user.to_owned(), |u| u.display.clone())
    }

    /// What IT calls a group.
    fn group_display(&self, name: &str) -> String {
        match self.group(name) {
            Some(g) if !g.display.is_empty() => g.display.clone(),
            _ if name == EVERYONE => "Everyone".into(),
            _ => name.to_owned(),
        }
    }
}

/// The directory as a file (`directory.toml`): `[[user]]` and `[[group]]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDirectory {
    #[serde(default, rename = "user")]
    pub users: Vec<User>,
    #[serde(default, rename = "group")]
    pub groups: Vec<Group>,
}

impl LocalDirectory {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let dir: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        for group in &dir.groups {
            if let Some(stranger) = group
                .members
                .iter()
                .find(|m| !dir.users.iter().any(|u| &u.name == *m))
            {
                anyhow::bail!("group {} lists {stranger}, who isn't a user", group.name);
            }
        }
        Ok(dir)
    }

    /// A small org to start from: a few people, AP and Legal, and IT.
    /// `admin` (the person running the console in development) is in IT.
    pub fn starter(admin: &str) -> Self {
        let user = |name: &str, display: &str| User {
            name: name.into(),
            display: display.into(),
        };
        let mut users = vec![
            user("dana", "Dana Ruiz"),
            user("sam", "Sam Lee"),
            user("priya", "Priya Shah"),
            user("mei", "Mei Chen"),
            user("pat", "Pat Okafor"),
        ];
        if !users.iter().any(|u| u.name == admin) {
            users.push(user(admin, ""));
        }
        let group = |name: &str, display: &str, source: &str, members: &[&str]| Group {
            name: name.into(),
            display: display.into(),
            source: source.into(),
            members: members.iter().map(|m| (*m).to_owned()).collect(),
        };
        let mut admins = vec!["pat"];
        if admin != "pat" {
            admins.push(admin);
        }
        Self {
            users,
            groups: vec![
                group(
                    "ap-ops",
                    "AP Operations",
                    "Entra ID group AP-Ops",
                    &["dana", "sam"],
                ),
                group(
                    "legal-ops",
                    "Legal Operations",
                    "Entra ID group Legal-Ops",
                    &["priya"],
                ),
                group(ADMINS, "IT admins", "Entra ID group IT-Admins", &admins),
            ],
        }
    }
}

impl Directory for LocalDirectory {
    fn users(&self) -> &[User] {
        &self.users
    }

    fn groups(&self) -> &[Group] {
        &self.groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ad1_groups_resolve_members_and_admins_are_a_role() {
        let dir = LocalDirectory::starter("root");
        assert_eq!(dir.members("ap-ops"), ["dana", "sam"]);
        assert_eq!(dir.members(EVERYONE).len(), dir.users.len());
        assert!(dir.is_admin("pat") && dir.is_admin("root"));
        assert!(!dir.is_admin("dana"));
        assert_eq!(dir.group_display("ap-ops"), "AP Operations");
        assert_eq!(dir.group_display(EVERYONE), "Everyone");

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("directory.toml");
        std::fs::write(&path, toml::to_string(&dir).unwrap()).unwrap();
        assert_eq!(LocalDirectory::load(&path).unwrap(), dir);
        std::fs::write(&path, "[[group]]\nname = \"x\"\nmembers = [\"ghost\"]\n").unwrap();
        assert!(LocalDirectory::load(&path).is_err());
    }
}
