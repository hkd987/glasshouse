use std::path::{Component, Path, PathBuf};

use glass_platform::fs::Owner;
use glass_types::{AgentPrincipal, Label};

use crate::ActionError;

/// A folder the task was given, under the name the agent sees.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// The directory groups the user was in when the task started, as
    /// policy resolved them, `everyone` included. Set by the runtime, never
    /// from anything the agent sent, for actions that show only what the
    /// user's groups may see, such as `catalog.search` (GA-2, DV-11).
    pub groups: Vec<String>,
    /// The most sensitive label of what the task has read so far (DP-11),
    /// set by the runtime: what it puts in the shared space is labeled at
    /// least this (ADR 0034 §5).
    pub data_label: Option<Label>,
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

/// The longest path an agent may name, in bytes: what a path may be on the
/// file systems underneath.
pub const MAX_PATH_BYTES: usize = 4096;
/// The most folders deep a path an agent names may go. Deeper than anyone
/// files things, and shallow enough that walking a tree made through
/// actions (to remove it, or to bring it back) is never a stack's worth of
/// calls.
pub const MAX_PATH_DEPTH: usize = 64;
/// How much of what an agent sent an error quotes back. The error is kept
/// in the task's activity and the audit log, so it has a size whatever the
/// input's was.
const QUOTED: usize = 200;

impl TaskContext {
    /// Resolves `<grant>/<path>`. Rejects absolute host paths, `..`,
    /// unknown grants, and paths longer or deeper than a path may be,
    /// before any I/O happens.
    pub fn resolve(&self, agent_path: &str) -> Result<Resolved<'_>, ActionError> {
        if agent_path.len() > MAX_PATH_BYTES {
            return Err(ActionError::InvalidInput(format!(
                "a path of {} bytes is longer than a path may be ({MAX_PATH_BYTES})",
                agent_path.len()
            )));
        }
        let trimmed = agent_path.trim().trim_start_matches('/');
        let path = Path::new(trimmed);
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
                Component::CurDir => {}
                _ => {
                    return Err(ActionError::OutOfScope(format!(
                        "{:?} is not a path inside a granted folder",
                        glass_api::clip(agent_path, QUOTED)
                    )));
                }
            }
        }
        // The granted folder's name, then what is in it.
        if parts.len() > MAX_PATH_DEPTH + 1 {
            return Err(ActionError::InvalidInput(format!(
                "a path {} folders deep is deeper than a path may be ({MAX_PATH_DEPTH})",
                parts.len() - 1
            )));
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
                    "{:?} is not a folder this task was given (granted: {})",
                    glass_api::clip(grant_name, QUOTED),
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
    fn a_path_has_a_length_and_a_depth_and_an_error_quotes_little_of_it() {
        let ctx = context(Path::new("/home/demo"));
        // As deep as a path may go, and one deeper.
        let deepest = format!("Reports/{}x.txt", "d/".repeat(MAX_PATH_DEPTH - 1));
        assert!(ctx.resolve(&deepest).is_ok());
        let deeper = format!("Reports/{}x.txt", "d/".repeat(MAX_PATH_DEPTH));
        assert!(
            matches!(ctx.resolve(&deeper), Err(ActionError::InvalidInput(why)) if why.contains("deeper")),
        );
        // Longer than a path may be: refused before it is read.
        let long = format!("Reports/{}", "x".repeat(MAX_PATH_BYTES));
        assert!(
            matches!(ctx.resolve(&long), Err(ActionError::InvalidInput(why)) if why.contains("longer")),
        );
        // What an agent sent megabytes of comes back as a line: the error
        // is kept in the task's activity and the audit log.
        for sent in [
            format!("../{}", "A".repeat(3000)),
            format!("{}/x", "B".repeat(3000)),
        ] {
            let Err(ActionError::OutOfScope(why)) = ctx.resolve(&sent) else {
                panic!("resolved");
            };
            assert!(why.len() < 400 && why.contains("more characters"), "{why}");
        }
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
