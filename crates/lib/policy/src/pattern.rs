use std::fmt;
use std::str::FromStr;

use glass_types::ActionName;
use serde::{Deserialize, Serialize};

/// A pattern over action names, as written in policy files.
///
/// Three shapes are allowed:
/// - an exact name: `mail.send`
/// - a subtree: `files.*` matches every action under `files.`
/// - everything: `*`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ActionPattern {
    raw: String,
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Kind {
    Exact,
    Subtree(String),
    Everything,
}

/// How specific a pattern is. Larger is more specific.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Specificity {
    literal_segments: usize,
    exact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid action pattern {0:?}: use an action name, `app.*`, or `*`")]
pub struct InvalidPattern(String);

impl ActionPattern {
    pub fn matches(&self, action: &ActionName) -> bool {
        match &self.kind {
            Kind::Exact => self.raw == action.as_str(),
            Kind::Subtree(prefix) => action
                .as_str()
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with('.')),
            Kind::Everything => true,
        }
    }

    pub fn specificity(&self) -> Specificity {
        match &self.kind {
            Kind::Exact => Specificity {
                literal_segments: self.raw.split('.').count(),
                exact: true,
            },
            Kind::Subtree(prefix) => Specificity {
                literal_segments: prefix.split('.').count(),
                exact: false,
            },
            Kind::Everything => Specificity {
                literal_segments: 0,
                exact: false,
            },
        }
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Whether every action this pattern matches is also matched by `other`.
    pub fn is_covered_by(&self, other: &ActionPattern) -> bool {
        match (&self.kind, &other.kind) {
            (_, Kind::Everything) => true,
            (Kind::Everything, _) => false,
            (Kind::Exact, _) => {
                ActionName::from_str(&self.raw).is_ok_and(|name| other.matches(&name))
            }
            (Kind::Subtree(mine), Kind::Subtree(theirs)) => {
                mine == theirs || mine.starts_with(&format!("{theirs}."))
            }
            (Kind::Subtree(_), Kind::Exact) => false,
        }
    }
}

/// Picks the most specific pattern in `entries` that matches `action`.
///
/// Ties cannot happen between distinct valid patterns: two patterns with the
/// same specificity that both match an action are identical.
pub fn most_specific<'a, T>(
    entries: impl IntoIterator<Item = (&'a ActionPattern, T)>,
    action: &ActionName,
) -> Option<(&'a ActionPattern, T)> {
    entries
        .into_iter()
        .filter(|(p, _)| p.matches(action))
        .max_by_key(|(p, _)| p.specificity())
}

impl TryFrom<String> for ActionPattern {
    type Error = InvalidPattern;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        let kind = if raw == "*" {
            Kind::Everything
        } else if let Some(prefix) = raw.strip_suffix(".*") {
            // The prefix must itself be well formed: one to three segments.
            let probe = format!("{prefix}.x");
            if ActionName::from_str(&probe).is_err() {
                return Err(InvalidPattern(raw));
            }
            Kind::Subtree(prefix.to_owned())
        } else if ActionName::from_str(&raw).is_ok() {
            Kind::Exact
        } else {
            return Err(InvalidPattern(raw));
        };
        Ok(Self { raw, kind })
    }
}

// Ordered by text so patterns can key the sorted maps in policy documents.
impl Ord for ActionPattern {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl PartialOrd for ActionPattern {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl FromStr for ActionPattern {
    type Err = InvalidPattern;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<ActionPattern> for String {
    fn from(p: ActionPattern) -> Self {
        p.raw
    }
}

impl fmt::Display for ActionPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ActionPattern {
        s.parse().unwrap()
    }

    fn a(s: &str) -> ActionName {
        s.parse().unwrap()
    }

    #[test]
    fn matching() {
        assert!(p("mail.send").matches(&a("mail.send")));
        assert!(!p("mail.send").matches(&a("mail.send_batch")));
        assert!(p("mail.*").matches(&a("mail.send")));
        assert!(p("crm.invoice.*").matches(&a("crm.invoice.create")));
        assert!(!p("mail.*").matches(&a("mailer.send")));
        assert!(p("*").matches(&a("files.read")));
    }

    #[test]
    fn rejects_bad_patterns() {
        for bad in [
            "",
            "mail",
            "mail.",
            "*.send",
            "mail.*.x",
            "Mail.*",
            "a.b.c.d.*",
        ] {
            assert!(
                ActionPattern::from_str(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn most_specific_wins() {
        let rules = [(p("*"), 0), (p("mail.*"), 1), (p("mail.send"), 2)];
        let pick = |name| most_specific(rules.iter().map(|(p, v)| (p, *v)), &a(name));
        assert_eq!(pick("mail.send").map(|(_, v)| v), Some(2));
        assert_eq!(pick("mail.draft").map(|(_, v)| v), Some(1));
        assert_eq!(pick("files.read").map(|(_, v)| v), Some(0));
    }

    #[test]
    fn coverage() {
        assert!(p("mail.send").is_covered_by(&p("mail.*")));
        assert!(p("crm.invoice.*").is_covered_by(&p("crm.*")));
        assert!(p("mail.*").is_covered_by(&p("*")));
        assert!(!p("mail.*").is_covered_by(&p("mail.send")));
        assert!(!p("*").is_covered_by(&p("mail.*")));
    }
}
