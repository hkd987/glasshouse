use std::path::{Component, Path, PathBuf};

use glass_platform::fs::Owner;
use glass_types::AgentPrincipal;

use crate::ActionError;

/// A folder the task was given, under the name the agent sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The first path segment agents use, such as `Invoices`.
    pub name: String,
    /// Absolute host path.
    pub root: PathBuf,
    pub writable: bool,
}

/// What providers know about the task they act for.
#[derive(Debug, Clone)]
pub struct TaskContext {
    pub principal: AgentPrincipal,
    /// Who owns what the broker creates: the user, never root.
    pub owner: Owner,
    /// The user's home, where first-party apps keep their data.
    pub home: PathBuf,
    pub grants: Vec<Grant>,
}

/// An agent-visible path resolved against the task's grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved<'a> {
    pub grant: &'a Grant,
    /// Path within the grant; empty for the grant itself.
    pub rel: PathBuf,
    /// Where it lives on the host (lexically; I/O still goes through
    /// `ConfinedDir`, which enforces the boundary).
    pub host: PathBuf,
    /// The path as the agent should see it in results.
    pub display: String,
}

impl TaskContext {
    /// Resolves `<grant>/<path>`. Rejects absolute host paths, `..` and
    /// unknown grants before any I/O happens.
    pub fn resolve(&self, agent_path: &str) -> Result<Resolved<'_>, ActionError> {
        let trimmed = agent_path.trim().trim_start_matches('/');
        let path = Path::new(trimmed);
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
                Component::CurDir => {}
                _ => {
                    return Err(ActionError::OutOfScope(format!(
                        "{agent_path:?} is not a path inside a granted folder"
                    )));
                }
            }
        }
        let Some((grant_name, rest)) = parts.split_first() else {
            return Err(ActionError::InvalidInput(
                "path names no granted folder".into(),
            ));
        };
        let grant = self
            .grants
            .iter()
            .find(|g| &g.name == grant_name)
            .ok_or_else(|| {
                ActionError::OutOfScope(format!(
                    "{grant_name:?} is not a folder this task was given (granted: {})",
                    self.grant_names()
                ))
            })?;
        let rel: PathBuf = rest.iter().collect();
        let display = std::iter::once(grant_name.as_str())
            .chain(rest.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("/");
        Ok(Resolved {
            host: grant.root.join(&rel),
            grant,
            rel,
            display,
        })
    }

    /// Like [`resolve`](Self::resolve), but requires a writable grant and a
    /// path below the grant root.
    pub fn resolve_writable(&self, agent_path: &str) -> Result<Resolved<'_>, ActionError> {
        let resolved = self.resolve(agent_path)?;
        if !resolved.grant.writable {
            return Err(ActionError::OutOfScope(format!(
                "{} is read-only for this task",
                resolved.grant.name
            )));
        }
        if resolved.rel.as_os_str().is_empty() {
            return Err(ActionError::InvalidInput(
                "path must name something inside the folder".into(),
            ));
        }
        Ok(resolved)
    }

    pub fn grant_names(&self) -> String {
        if self.grants.is_empty() {
            "none".into()
        } else {
            self.grants
                .iter()
                .map(|g| g.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::context;

    #[test]
    fn resolves_grant_relative_paths() {
        let ctx = context(Path::new("/home/demo"));
        let r = ctx.resolve("Invoices/2026/inv-1.txt").unwrap();
        assert_eq!(r.host, Path::new("/home/demo/Invoices/2026/inv-1.txt"));
        assert_eq!(r.display, "Invoices/2026/inv-1.txt");
        assert_eq!(ctx.resolve("/Reports").unwrap().rel, PathBuf::new());
    }

    #[test]
    fn refuses_escapes_and_unknown_grants() {
        let ctx = context(Path::new("/home/demo"));
        assert!(matches!(
            ctx.resolve("Invoices/../../etc/passwd"),
            Err(ActionError::OutOfScope(_))
        ));
        assert!(matches!(
            ctx.resolve("Secrets/x"),
            Err(ActionError::OutOfScope(_))
        ));
        assert!(matches!(ctx.resolve(""), Err(ActionError::InvalidInput(_))));
    }

    #[test]
    fn writes_need_a_writable_grant() {
        let ctx = context(Path::new("/home/demo"));
        assert!(matches!(
            ctx.resolve_writable("Invoices/x.txt"),
            Err(ActionError::OutOfScope(_))
        ));
        assert!(ctx.resolve_writable("Reports/x.txt").is_ok());
        assert!(matches!(
            ctx.resolve_writable("Reports"),
            Err(ActionError::InvalidInput(_))
        ));
    }
}
