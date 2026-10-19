//! Who is in the org: people and the groups policy applies to (AD-1).
//!
//! Today a local file, `directory.toml` in the console's state directory;
//! later SCIM from Entra ID or Okta behind the same [`Directory`] trait.
//! Roles are groups: the members of [`ADMINS`] may sign in to the console,
//! and the same group's apps in policy put IT's tools in their profiles;
//! the groups policy's support roles name sign in to remote help (ADR 0028).

use std::path::Path;

use anyhow::Context;
use glass_managed::{DirectoryRelease, Person};
use serde::{Deserialize, Serialize};

/// The group whose members administer the org.
pub const ADMINS: &str = "it-admins";
/// The group policy applies to everyone in.
pub const EVERYONE: &str = glass_policy::EVERYONE;
/// The starter org's mail domain.
pub const STARTER_DOMAIN: &str = "acme.example";

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
    /// Where mail to them goes; devices offer it as people type (AD-1).
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub department: String,
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

    /// The people devices get (AD-1): what it takes to address them, by
    /// login.
    fn release(&self) -> DirectoryRelease {
        let mut people: Vec<Person> = self
            .users()
            .iter()
            .map(|u| Person {
                login: u.name.clone(),
                name: if u.display.is_empty() {
                    u.name.clone()
                } else {
                    u.display.clone()
                },
                email: u.email.clone(),
                title: u.title.clone(),
                department: u.department.clone(),
            })
            .collect();
        people.sort_by(|a, b| a.login.cmp(&b.login));
        DirectoryRelease { people }
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
        dir.release()
            .check()
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        Ok(dir)
    }

    /// A small org to start from: a few people, AP and Legal, and IT with
    /// its admins and a person in each support tier's group (CF-81).
    /// `admin` (the person running the console in development) is an admin.
    pub fn starter(admin: &str) -> Self {
        let user = |name: &str, display: &str, title: &str, department: &str| User {
            name: name.into(),
            display: display.into(),
            email: format!("{name}@{STARTER_DOMAIN}"),
            title: title.into(),
            department: department.into(),
        };
        let mut users = vec![
            user("dana", "Dana Ruiz", "AP lead", "Finance"),
            user("sam", "Sam Lee", "AP analyst", "Finance"),
            user("priya", "Priya Shah", "Counsel", "Legal"),
            user("mei", "Mei Chen", "Controller", "Finance"),
            user("pat", "Pat Okafor", "IT administrator", "IT"),
            user("jordan", "Jordan Lee", "Help desk analyst", "IT"),
            user("alex", "Alex Kim", "Desktop support engineer", "IT"),
            user("sasha", "Sasha Novak", "Security analyst", "IT"),
        ];
        if !users.iter().any(|u| u.name == admin) {
            users.push(user(admin, "", "", "IT"));
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
                group(
                    "it-helpdesk",
                    "IT Help Desk",
                    "Entra ID group IT-HelpDesk",
                    &["jordan"],
                ),
                group(
                    "desktop-support",
                    "Desktop Support",
                    "Entra ID group Desktop-Support",
                    &["alex"],
                ),
                group(
                    "it-security",
                    "IT Security",
                    "Entra ID group IT-Security",
                    &["sasha"],
                ),
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

    #[test]
    fn ad1_users_carry_addresses_and_a_duplicate_email_is_refused() {
        let dir = LocalDirectory::starter("pat");
        let release = dir.release();
        assert_eq!(release.check(), Ok(()));
        let priya = release.person("priya").unwrap();
        assert_eq!(
            (
                priya.name.as_str(),
                priya.email.as_str(),
                priya.title.as_str()
            ),
            ("Priya Shah", "priya@acme.example", "Counsel")
        );

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("directory.toml");
        let mut twice = dir.clone();
        twice.users[1].email = twice.users[0].email.clone();
        std::fs::write(&path, toml::to_string(&twice).unwrap()).unwrap();
        let refused = LocalDirectory::load(&path).unwrap_err().to_string();
        assert!(refused.contains("two people's email"), "{refused}");
        let mut bad = dir;
        bad.users[0].email = "dana at acme".into();
        std::fs::write(&path, toml::to_string(&bad).unwrap()).unwrap();
        assert!(LocalDirectory::load(&path).is_err());
    }
}
