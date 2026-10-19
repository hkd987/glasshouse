//! The org's private catalog as devices receive it (AD-5, DV-10, DV-11),
//! and catalog-first search (GA-2).
//!
//! A release lists only items IT approved. Each declares what it does
//! (DV-3: actions, the most sensitive data it opens, network destinations,
//! default tiers) and, per directory group, how it reaches that group's
//! members: required (pushed), available (self-install) or blocked. A user
//! sees an item when one of their groups has it required or available and
//! none blocks it: blocked wins, as the stricter rule does in policy.

use std::collections::BTreeMap;

use glass_types::{Label, Tier};
use serde::{Deserialize, Serialize};

/// What kind of thing an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Agent,
    App,
    /// A recorded flow (AD-5).
    Flow,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::App => "app",
            Self::Flow => "recorded flow",
        }
    }
}

/// How an item reaches a group's members (DV-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallMode {
    /// Pushed to their machines without asking.
    Required,
    /// They may install it themselves.
    Available,
    /// Not for them, whatever another group says.
    Blocked,
}

/// What an item declares it does (DV-3). The OS holds it to this: it can
/// never exceed the declaration, whatever policy allows (ADR 0008).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declared {
    /// Action patterns it calls, such as `files.read` or `mail.*`.
    #[serde(default)]
    pub actions: Vec<String>,
    /// The tier the publisher suggests for an action, where it matters.
    #[serde(default)]
    pub default_tiers: BTreeMap<String, Tier>,
    /// The most sensitive data it opens.
    #[serde(default = "default_label")]
    pub max_label: Label,
    /// Network destinations (`host:port`). Empty: none beyond the model
    /// gateway.
    #[serde(default)]
    pub egress: Vec<String>,
}

fn default_label() -> Label {
    Label::Internal
}

impl Default for Declared {
    /// Declares nothing: no actions, internal data at most, no network.
    fn default() -> Self {
        Self {
            actions: Vec::new(),
            default_tiers: BTreeMap::new(),
            max_label: default_label(),
            egress: Vec::new(),
        }
    }
}

/// One approved item in the org's catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogItem {
    /// Stable id, such as `contract-review`. For agents, the agent id.
    pub id: String,
    pub kind: ItemKind,
    pub name: String,
    pub version: String,
    pub publisher: String,
    /// The publisher proved who they are (DV-1).
    #[serde(default)]
    pub publisher_verified: bool,
    /// Built by the org and signed with its own key: never listed outside
    /// the org (DV-10).
    #[serde(default)]
    pub internal: bool,
    /// One line on what it's for.
    pub summary: String,
    /// Words people use for the need it meets, for search.
    #[serde(default)]
    pub keywords: Vec<String>,
    pub declares: Declared,
    /// Directory group → how it reaches that group (DV-11).
    #[serde(default)]
    pub groups: BTreeMap<String, InstallMode>,
    /// Devices stay on this version until IT moves the pin (DV-12).
    #[serde(default)]
    pub pinned: bool,
}

impl CatalogItem {
    /// How the item reaches someone in `groups`, or `None` when it doesn't.
    /// Blocked anywhere wins; then required over available.
    pub fn mode_for<S: AsRef<str>>(&self, groups: &[S]) -> Option<InstallMode> {
        let modes = groups
            .iter()
            .filter_map(|g| self.groups.get(g.as_ref()).copied());
        let mut best = None;
        for mode in modes {
            match mode {
                InstallMode::Blocked => return None,
                InstallMode::Required => best = Some(InstallMode::Required),
                InstallMode::Available if best.is_none() => best = Some(InstallMode::Available),
                InstallMode::Available => {}
            }
        }
        best
    }

    /// Whether the item may ever be listed outside the org, such as on a
    /// public exchange (DV-20): never for the org's own items (DV-10).
    pub fn publicly_listable(&self) -> bool {
        !self.internal
    }
}

/// A catalog release's payload: the approved items.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogRelease {
    pub items: Vec<CatalogItem>,
}

/// A search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Match<'a> {
    pub item: &'a CatalogItem,
    pub mode: InstallMode,
    /// Higher is closer to the need.
    pub score: u32,
}

/// Words that say nothing about the need.
const STOP: &[&str] = &[
    "a",
    "an",
    "and",
    "any",
    "app",
    "apps",
    "are",
    "as",
    "at",
    "be",
    "build",
    "can",
    "do",
    "does",
    "for",
    "from",
    "get",
    "help",
    "i",
    "in",
    "is",
    "it",
    "keep",
    "let",
    "make",
    "me",
    "my",
    "need",
    "of",
    "on",
    "or",
    "our",
    "so",
    "some",
    "something",
    "that",
    "the",
    "their",
    "this",
    "to",
    "tool",
    "us",
    "want",
    "way",
    "we",
    "when",
    "which",
    "with",
    "would",
];

/// Lowercase words of three or more letters, plurals folded.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(&w.as_str()))
        .map(|w| fold(&w))
        .collect()
}

/// Folds an English plural to its singular, roughly; both sides of a
/// comparison fold the same way, which is what matters.
fn fold(word: &str) -> String {
    if word.chars().count() <= 4 {
        return word.to_owned();
    }
    if let Some(stem) = word.strip_suffix("ies") {
        return format!("{stem}y");
    }
    for suffix in ["sses", "xes", "ches", "shes", "zes"] {
        if word.ends_with(suffix) {
            return word[..word.len() - 2].to_owned();
        }
    }
    match word.strip_suffix('s') {
        Some(stem) if !stem.ends_with('s') => stem.to_owned(),
        _ => word.to_owned(),
    }
}

/// Two words match when equal, or one starts the other and the shorter has
/// four letters or more ("reconcile" and "reconciliation").
fn same(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short == long || (short.chars().count() >= 4 && long.starts_with(short))
}

fn hits(need: &[String], text: &str) -> u32 {
    let have = words(text);
    let n = need
        .iter()
        .filter(|w| have.iter().any(|h| same(w, h)))
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The items someone in `groups` may install that meet `need`, best first
/// (GA-2). Items their groups don't get, or that a group blocks, are never
/// returned. An empty need lists everything they may install.
pub fn search<'a, S: AsRef<str>>(
    items: &'a [CatalogItem],
    groups: &[S],
    need: &str,
    kind: Option<ItemKind>,
    limit: usize,
) -> Vec<Match<'a>> {
    let need = words(need);
    let mut found: Vec<Match<'a>> = items
        .iter()
        .filter(|item| kind.is_none_or(|k| item.kind == k))
        .filter_map(|item| {
            let mode = item.mode_for(groups)?;
            let actions = item.declares.actions.join(" ");
            let score = 3 * hits(&need, &item.name)
                + 2 * hits(&need, &item.keywords.join(" "))
                + hits(&need, &item.summary)
                + hits(&need, &actions);
            (need.is_empty() || score > 0).then_some(Match { item, mode, score })
        })
        .collect();
    found.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.item.name.cmp(&b.item.name))
    });
    found.truncate(limit);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use InstallMode::*;

    fn item(id: &str, name: &str, summary: &str, groups: &[(&str, InstallMode)]) -> CatalogItem {
        CatalogItem {
            id: id.into(),
            kind: ItemKind::App,
            name: name.into(),
            version: "1.0.0".into(),
            publisher: "Acme IT".into(),
            publisher_verified: true,
            internal: true,
            summary: summary.into(),
            keywords: Vec::new(),
            declares: Declared {
                actions: vec!["records.read".into()],
                ..Declared::default()
            },
            groups: groups.iter().map(|(g, m)| ((*g).to_owned(), *m)).collect(),
            pinned: false,
        }
    }

    #[test]
    fn dv11_blocked_wins_then_required_over_available() {
        let it = item(
            "x",
            "X",
            "",
            &[
                ("ap-ops", Available),
                ("finance", Required),
                ("contractors", Blocked),
            ],
        );
        assert_eq!(it.mode_for(&["ap-ops"]), Some(Available));
        assert_eq!(it.mode_for(&["ap-ops", "finance"]), Some(Required));
        assert_eq!(it.mode_for(&["ap-ops", "contractors"]), None);
        assert_eq!(it.mode_for(&["everyone"]), None, "no rule: not offered");
        assert_eq!(it.mode_for::<&str>(&[]), None);
    }

    #[test]
    fn ga2_search_finds_by_need_and_only_what_the_groups_may_install() {
        let items = vec![
            item(
                "vendor-disputes",
                "Vendor Disputes",
                "Track disputes with vendors over invoices, with overdue reminders",
                &[("ap-ops", Available)],
            ),
            item(
                "close-checklist",
                "Month-end Close",
                "A checklist for the month-end close",
                &[("finance", Available)],
            ),
            item(
                "dispute-desk",
                "Dispute Desk",
                "Customer dispute tracking",
                &[("ap-ops", Blocked), ("everyone", Available)],
            ),
        ];
        let ap = ["everyone", "ap-ops"];
        let found = search(
            &items,
            &ap,
            "I need a way to track vendor disputes",
            None,
            5,
        );
        let ids: Vec<&str> = found.iter().map(|m| m.item.id.as_str()).collect();
        assert_eq!(
            ids,
            ["vendor-disputes"],
            "blocked and other groups' items hidden"
        );
        assert_eq!(found[0].mode, Available);

        let finance = ["everyone", "finance"];
        let found = search(&items, &finance, "dispute tracker", None, 5);
        let ids: Vec<&str> = found.iter().map(|m| m.item.id.as_str()).collect();
        assert_eq!(ids, ["dispute-desk"]);

        assert!(search(&items, &ap, "payroll", None, 5).is_empty());
        assert_eq!(
            search(&items, &finance, "", None, 5).len(),
            2,
            "empty need lists all"
        );
        assert!(search(&items, &ap, "disputes", Some(ItemKind::Agent), 5).is_empty());
    }

    #[test]
    fn plurals_and_stems_match() {
        assert_eq!(words("Invoices and policies"), ["invoice", "policy"]);
        assert!(same("reconcil", "reconciliation"));
        assert!(!same("cat", "catalog"), "three letters don't start a match");
    }

    #[test]
    fn dv10_the_orgs_own_items_are_never_publicly_listable() {
        let mut it = item("x", "X", "", &[]);
        assert!(!it.publicly_listable());
        it.internal = false;
        assert!(it.publicly_listable());
    }

    #[test]
    fn a_release_round_trips() {
        let release = CatalogRelease {
            items: vec![item("x", "X", "y", &[("ap-ops", Required)])],
        };
        let json = serde_json::to_string(&release).unwrap();
        assert_eq!(
            serde_json::from_str::<CatalogRelease>(&json).unwrap(),
            release
        );
    }
}
