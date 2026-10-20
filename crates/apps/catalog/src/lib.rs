//! The `catalog` app: catalog first (GA-2). Before an agent builds an app,
//! or does by hand a job something approved already does, it asks what IT
//! has approved that the user may install.
//!
//! `catalog.search` reads the org's private catalog as this device last
//! received it (signed, ADR 0023) and returns the items meeting a need,
//! best first, with how each reaches the user (required or available) and
//! what each declares it may do (DV-3). It returns only items one of the
//! user's groups gets and none of them blocks (DV-11). The groups come
//! from the task's context, which the runtime fills from policy when the
//! task starts; nothing in the request can widen them.
//!
//! Read only, nothing leaves the machine: it runs at Auto and, like every
//! action, is audited.
//!
//! The Store's actions, `catalog.list`, `catalog.install` and
//! `catalog.remove`, are in [`store`].

mod store;

pub use store::{INSTALL, INSTALLS, LIST, REMOVE, installed, reaches};

use std::sync::{Arc, PoisonError, RwLock};

use glass_actions::{ActionDef, ActionError, ActionOutput, ActionProvider, Preview, TaskContext};
use glass_api::{AskLabels, display_line};
use glass_managed::catalog::search;
use glass_managed::{CatalogItem, CatalogRelease, InstallMode, ItemKind};
use glass_types::{ActionName, SideEffect, Tier};
use serde::Deserialize;
use serde_json::{Value, json};

pub const SEARCH: &str = "catalog.search";
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;
const MAX_NEED: usize = 500;

/// The catalog this device applied, with its release version.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub version: u64,
    pub release: CatalogRelease,
}

/// The device's catalog, shared between the sync that applies releases and
/// the action that reads them. Each release is swapped in whole.
#[derive(Debug, Clone, Default)]
pub struct CatalogHandle(Arc<RwLock<Option<Arc<Applied>>>>);

impl CatalogHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// The catalog in force, if one has arrived.
    pub fn current(&self) -> Option<Arc<Applied>> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, applied: Applied) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(applied));
    }
}

#[derive(Debug)]
pub struct CatalogProvider {
    catalog: CatalogHandle,
    /// This device is managed by an org (ADR 0023): with no catalog yet,
    /// one is on its way; unmanaged, none will come (DV-13).
    managed: bool,
}

impl CatalogProvider {
    pub fn new(catalog: CatalogHandle) -> Self {
        Self {
            catalog,
            managed: false,
        }
    }

    pub fn managed(mut self, managed: bool) -> Self {
        self.managed = managed;
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    need: String,
    #[serde(default)]
    kind: Option<ItemKind>,
    #[serde(default)]
    limit: Option<usize>,
}

impl SearchInput {
    fn parse(input: &Value) -> Result<Self, ActionError> {
        let parsed: Self = glass_actions::parse_input(input)?;
        if parsed.need.trim().is_empty() {
            return Err(ActionError::InvalidInput(
                "say in a sentence what the user needs".into(),
            ));
        }
        if parsed.need.chars().count() > MAX_NEED {
            return Err(ActionError::InvalidInput(format!(
                "describe the need in under {MAX_NEED} characters"
            )));
        }
        if parsed.limit.is_some_and(|n| n == 0 || n > MAX_LIMIT) {
            return Err(ActionError::InvalidInput(format!(
                "limit is 1 to {MAX_LIMIT}"
            )));
        }
        Ok(parsed)
    }
}

/// `catalog.install` or `catalog.remove`: one item, by id; it changes the
/// person's own list of what they installed, which undo restores.
fn item_def(name: &str, title: &str, description: &str) -> ActionDef {
    ActionDef {
        name: name.parse().expect("static name"),
        title: title.into(),
        description: description.into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The item's id, as catalog.list or catalog.search gives it" }
            },
            "required": ["id"],
            "additionalProperties": false
        }),
        side_effect: SideEffect::LocalReversible,
        default_tier: Tier::Notify,
        ask: AskLabels::default(),
    }
}

fn mode_text(mode: InstallMode) -> &'static str {
    match mode {
        InstallMode::Required => "IT installs it for them",
        InstallMode::Available => "they can install it themselves",
        // Never returned: search leaves blocked items out.
        InstallMode::Blocked => "blocked",
    }
}

/// What an item declares, in a line (DV-3).
fn declares_text(item: &CatalogItem) -> String {
    let d = &item.declares;
    let actions = if d.actions.is_empty() {
        "no actions".to_owned()
    } else {
        format!("actions {}", d.actions.join(", "))
    };
    let network = if d.egress.is_empty() {
        "no network beyond the model gateway".to_owned()
    } else {
        format!("network {}", d.egress.join(", "))
    };
    format!("{actions}; data up to {}; {network}", d.max_label)
}

impl ActionProvider for CatalogProvider {
    fn actions(&self) -> Vec<ActionDef> {
        vec![
            ActionDef {
                name: SEARCH.parse().expect("static name"),
                title: "Search the org's catalog".into(),
                description: "Catalog first: before building an app, or doing by hand a job that \
                          repeats, look for an app, agent or recorded flow IT already approved \
                          that this user may install, and suggest it instead of building a \
                          duplicate. Describe the need in a sentence. Each result says how it \
                          reaches the user and what it is allowed to do."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "need": { "type": "string", "description": "What the user needs, in a sentence, such as \"track vendor disputes with overdue reminders\"", "maxLength": MAX_NEED },
                        "kind": { "type": "string", "enum": ["agent", "app", "flow"], "description": "Only this kind of item" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT }
                    },
                    "required": ["need"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: LIST.parse().expect("static name"),
                title: "List the org's catalog".into(),
                description: "Everything the org's catalog offers this user: apps, agents and \
                          recorded flows, each with how it reaches them (required or \
                          available), whether they installed it, and what it may do."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["agent", "app", "flow"], "description": "Only this kind of item" }
                    },
                    "additionalProperties": false
                }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            item_def(
                INSTALL,
                "Install from the org's catalog",
                "Install an item the org's catalog makes available to this user: it joins the apps and agents they see. Policy still decides what may run.",
            ),
            item_def(
                REMOVE,
                "Remove an item installed from the catalog",
                "Remove an item this user installed from the org's catalog: it leaves the apps and agents they see.",
            ),
        ]
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        match action.as_str() {
            SEARCH => {}
            LIST | INSTALL | REMOVE => {
                return store::preview(
                    self.catalog.current().as_deref(),
                    ctx,
                    action.as_str(),
                    input,
                );
            }
            _ => {
                return Err(ActionError::InvalidInput(format!(
                    "catalog has no action {action}"
                )));
            }
        }
        let input = SearchInput::parse(input)?;
        let need: String = display_line(input.need.trim()).chars().take(80).collect();
        Ok(Preview {
            summary: format!("Search the org's catalog for \"{need}\""),
            targets: vec!["Org catalog".into()],
            ..Preview::default()
        })
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        if matches!(action.as_str(), LIST | INSTALL | REMOVE) {
            let mut out = store::invoke(
                self.catalog.current().as_deref(),
                ctx,
                action.as_str(),
                input,
            )?;
            if let Some(data) = out.data.as_object_mut() {
                data.insert("managed".into(), json!(self.managed));
            }
            return Ok(out);
        }
        let input = SearchInput::parse(input)?;
        let Some(catalog) = self.catalog.current() else {
            return Ok(ActionOutput {
                text: "No catalog from IT has reached this computer yet, so there is nothing \
                       approved to suggest."
                    .into(),
                data: json!({ "catalog_version": null, "items": [] }),
                changes: Vec::new(),
            });
        };
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT);
        let found = search(
            &catalog.release.items,
            &ctx.groups,
            &input.need,
            input.kind,
            limit,
        );
        let items: Vec<Value> = found
            .iter()
            .map(|m| {
                let item = m.item;
                json!({
                    "id": item.id,
                    "kind": item.kind,
                    "name": item.name,
                    "version": item.version,
                    "publisher": item.publisher,
                    "publisher_verified": item.publisher_verified,
                    "internal": item.internal,
                    "install": m.mode,
                    "summary": item.summary,
                    "declares": item.declares,
                })
            })
            .collect();
        let text = if found.is_empty() {
            format!(
                "Nothing approved that this user may install matches \"{}\". Building it is \
                 the next step if they still want it.",
                input.need.trim()
            )
        } else {
            let mut text = format!(
                "{} approved item{} this user may install match{}. Suggest {} before building \
                 anything:",
                found.len(),
                if found.len() == 1 { "" } else { "s" },
                if found.len() == 1 { "es" } else { "" },
                if found.len() == 1 { "it" } else { "one" },
            );
            for (n, m) in found.iter().enumerate() {
                let item = m.item;
                text.push_str(&format!(
                    "\n{}. {} ({} v{}, by {}{}): {}. {}. Declares {}.",
                    n + 1,
                    item.name,
                    item.kind.as_str(),
                    item.version,
                    item.publisher,
                    if item.internal {
                        ", built by the org"
                    } else if item.publisher_verified {
                        ", verified publisher"
                    } else {
                        ""
                    },
                    item.summary,
                    mode_text(m.mode),
                    declares_text(item),
                ));
            }
            text
        };
        Ok(ActionOutput {
            text,
            data: json!({ "catalog_version": catalog.version, "items": items }),
            changes: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use glass_actions::testing::context;
    use glass_managed::Declared;
    use glass_types::Label;

    use super::*;

    fn item(id: &str, name: &str, summary: &str, groups: &[(&str, InstallMode)]) -> CatalogItem {
        CatalogItem {
            id: id.parse().unwrap(),
            kind: ItemKind::App,
            name: name.into(),
            version: "1.2.0".into(),
            publisher: "Acme IT".into(),
            publisher_verified: true,
            internal: true,
            summary: summary.into(),
            keywords: vec!["dispute".into()],
            declares: Declared {
                actions: vec!["records.read".into(), "mail.draft".into()],
                default_tiers: BTreeMap::new(),
                max_label: Label::Internal,
                egress: Vec::new(),
            },
            groups: groups.iter().map(|(g, m)| ((*g).to_owned(), *m)).collect(),
            pinned: false,
            provenance: Default::default(),
            app: None,
        }
    }

    fn provider() -> CatalogProvider {
        let handle = CatalogHandle::new();
        handle.set(Applied {
            version: 7,
            release: CatalogRelease {
                items: vec![
                    item(
                        "vendor-disputes",
                        "Vendor Disputes",
                        "Track vendor disputes with overdue reminders",
                        &[("ap-ops", InstallMode::Available)],
                    ),
                    item(
                        "legal-disputes",
                        "Legal Disputes",
                        "Dispute tracking for Legal",
                        &[("legal", InstallMode::Required)],
                    ),
                    item(
                        "dispute-bot",
                        "Dispute Bot",
                        "Disputes, automated",
                        &[
                            ("everyone", InstallMode::Available),
                            ("ap-ops", InstallMode::Blocked),
                        ],
                    ),
                ],
            },
        });
        CatalogProvider::new(handle)
    }

    fn search_as(p: &CatalogProvider, groups: &[&str], need: &str) -> ActionOutput {
        let mut ctx = context(std::path::Path::new("/home/demo"));
        ctx.groups = groups.iter().map(|g| (*g).to_owned()).collect();
        p.invoke(&ctx, &SEARCH.parse().unwrap(), &json!({ "need": need }))
            .unwrap()
    }

    fn ids(out: &ActionOutput) -> Vec<String> {
        out.data["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn ga2_dv11_search_shows_only_what_the_users_groups_may_install() {
        let p = provider();
        let ap = search_as(&p, &["everyone", "ap-ops"], "track vendor disputes");
        assert_eq!(
            ids(&ap),
            ["vendor-disputes"],
            "another group's item and a blocked one hidden"
        );
        assert_eq!(ap.data["catalog_version"], json!(7));
        assert_eq!(ap.data["items"][0]["install"], json!("available"));
        assert!(
            ap.text.contains("Suggest it before building"),
            "{}",
            ap.text
        );

        let legal = search_as(&p, &["everyone", "legal"], "disputes");
        assert_eq!(
            ids(&legal),
            ["dispute-bot", "legal-disputes"],
            "equal scores by name"
        );
        assert!(
            legal.text.contains("IT installs it for them"),
            "{}",
            legal.text
        );

        let nobody = search_as(&p, &[], "disputes");
        assert!(ids(&nobody).is_empty(), "no groups, nothing offered");
        assert!(nobody.text.contains("Nothing approved"), "{}", nobody.text);
    }

    #[test]
    fn dv3_results_carry_the_declared_privileges() {
        let out = search_as(&provider(), &["ap-ops"], "vendor disputes");
        let declares = &out.data["items"][0]["declares"];
        assert_eq!(declares["actions"], json!(["records.read", "mail.draft"]));
        assert_eq!(declares["max_label"], json!("internal"));
        assert!(
            out.text.contains(
                "Declares actions records.read, mail.draft; data up to internal; no network"
            ),
            "{}",
            out.text
        );
    }

    #[test]
    fn ga2_with_no_catalog_yet_it_says_so() {
        let p = CatalogProvider::new(CatalogHandle::new());
        let out = search_as(&p, &["everyone"], "anything");
        assert_eq!(out.data["catalog_version"], Value::Null);
        assert!(out.text.contains("No catalog from IT"), "{}", out.text);
    }

    #[test]
    fn the_request_cant_widen_the_groups_and_bad_input_is_refused() {
        let p = provider();
        let ctx = context(std::path::Path::new("/home/demo"));
        let action: ActionName = SEARCH.parse().unwrap();
        // There is no groups field to pass.
        assert!(matches!(
            p.preview(&ctx, &action, &json!({ "need": "x", "groups": ["ap-ops"] })),
            Err(ActionError::InvalidInput(_))
        ));
        assert!(p.preview(&ctx, &action, &json!({ "need": "  " })).is_err());
        assert!(
            p.preview(&ctx, &action, &json!({ "need": "x", "limit": 0 }))
                .is_err()
        );
        let preview = p
            .preview(&ctx, &action, &json!({ "need": "track\u{202E}disputes" }))
            .unwrap();
        assert!(preview.summary.starts_with("Search the org's catalog for"));
        assert!(!preview.summary.contains('\u{202E}'), "display-safe");
        assert!(preview.reads.is_empty() && preview.writes.is_empty());
    }

    fn as_in(
        p: &CatalogProvider,
        home: &std::path::Path,
        groups: &[&str],
        action: &str,
        input: Value,
    ) -> Result<ActionOutput, ActionError> {
        let mut ctx = context(home);
        ctx.owner = glass_platform::fs::Owner {
            uid: glass_platform::users::effective_uid(),
            gid: 0,
        };
        ctx.groups = groups.iter().map(|g| (*g).to_owned()).collect();
        let action: ActionName = action.parse().unwrap();
        p.preview(&ctx, &action, &input)?;
        p.invoke(&ctx, &action, &input)
    }

    #[test]
    fn dv11_installing_writes_the_persons_own_list_which_undo_restores() {
        let home = tempfile::tempdir().unwrap();
        let p = provider();
        let groups = ["everyone", "ap-ops"];
        let out = as_in(
            &p,
            home.path(),
            &groups,
            INSTALL,
            json!({ "id": "vendor-disputes" }),
        )
        .unwrap();
        assert_eq!(out.text, "Installed Vendor Disputes");
        // One change, on the file in their home: undo restores it.
        assert_eq!(out.changes.len(), 1);
        assert_eq!(out.changes[0].paths, [home.path().join(INSTALLS)]);
        assert!(out.changes[0].reversible);
        assert!(installed(home.path()).contains(&"vendor-disputes".parse().unwrap()));

        let listed = as_in(&p, home.path(), &groups, LIST, json!({})).unwrap();
        assert_eq!(listed.data["items"][0]["installed"], json!(true));
        assert_eq!(listed.data["managed"], json!(false));

        let again = as_in(
            &p,
            home.path(),
            &groups,
            INSTALL,
            json!({ "id": "vendor-disputes" }),
        )
        .unwrap();
        assert!(again.changes.is_empty(), "nothing changed, nothing to undo");
        let removed = as_in(
            &p,
            home.path(),
            &groups,
            REMOVE,
            json!({ "id": "vendor-disputes" }),
        )
        .unwrap();
        assert_eq!(removed.text, "Removed Vendor Disputes");
        assert!(installed(home.path()).is_empty());
    }

    #[test]
    fn ga13_the_store_lists_who_made_an_org_app_and_what_it_remixes() {
        let home = tempfile::tempdir().unwrap();
        let manifest: glass_api::AppManifest = serde_json::from_value(json!({
            "id": "mail-with-write", "name": "Mail, with Write", "version": "1",
            "actions": ["mail.list"], "labels": ["internal"],
        }))
        .unwrap();
        let mut made = item(
            "mail-with-write",
            "Mail, with Write",
            "Write instead of New message",
            &[("everyone", InstallMode::Available)],
        );
        made.publisher = "Dana Ruiz".into();
        made.app = Some(glass_managed::studio::OrgApp {
            manifest,
            bundle_sha256: "00".repeat(32),
            source_sha256: "11".repeat(32),
            made_by: "dana".into(),
            based_on: Some("mail@0.1.0".into()),
        });
        let handle = CatalogHandle::new();
        handle.set(Applied {
            version: 8,
            release: CatalogRelease {
                items: vec![
                    made,
                    item(
                        "vendor-disputes",
                        "Vendor Disputes",
                        "Track vendor disputes",
                        &[("everyone", InstallMode::Available)],
                    ),
                ],
            },
        });
        let p = CatalogProvider::new(handle);
        let listed = as_in(&p, home.path(), &["everyone"], LIST, json!({})).unwrap();
        let items = &listed.data["items"];
        assert_eq!(items[0]["id"], "mail-with-write");
        assert_eq!(
            items[0]["app"],
            json!({ "made_by": "dana", "based_on": "mail@0.1.0" })
        );
        // Not the bundle's digests or manifest: the Store needn't see them.
        assert!(items[0]["app"].get("bundle_sha256").is_none());
        assert_eq!(items[1]["app"], Value::Null, "not an app people made");
    }

    #[test]
    fn dv11_only_what_is_available_to_their_groups_installs() {
        let home = tempfile::tempdir().unwrap();
        let p = provider();
        // Another group's item, and one their group blocks.
        assert!(
            as_in(
                &p,
                home.path(),
                &["everyone"],
                INSTALL,
                json!({ "id": "vendor-disputes" })
            )
            .is_err()
        );
        assert!(
            as_in(
                &p,
                home.path(),
                &["everyone", "ap-ops"],
                INSTALL,
                json!({ "id": "dispute-bot" })
            )
            .is_err()
        );
        // Required: IT installs it; it can't be removed either.
        let err = as_in(
            &p,
            home.path(),
            &["legal"],
            REMOVE,
            json!({ "id": "legal-disputes" }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("IT installs"), "{err}");
        assert!(!home.path().join(INSTALLS).exists());
    }

    #[test]
    fn dv11_reaches_narrows_and_leaves_what_the_catalog_doesnt_list_to_policy() {
        let p = provider();
        let catalog = p.catalog.current();
        let catalog = catalog.as_deref();
        let none = std::collections::BTreeSet::new();
        let id = |s: &str| -> glass_types::AgentId { s.parse().unwrap() };
        assert!(reaches(catalog, &["everyone"], &none, &id("not-listed")));
        assert!(
            reaches(catalog, &["legal"], &none, &id("legal-disputes")),
            "required"
        );
        assert!(
            !reaches(catalog, &["ap-ops"], &none, &id("vendor-disputes")),
            "not installed"
        );
        let mine: std::collections::BTreeSet<_> =
            [id("vendor-disputes"), id("legal-disputes")].into();
        assert!(reaches(catalog, &["ap-ops"], &mine, &id("vendor-disputes")));
        assert!(
            !reaches(catalog, &["everyone"], &mine, &id("legal-disputes")),
            "never another group's"
        );
        assert!(
            reaches(None, &["everyone"], &none, &id("vendor-disputes")),
            "no catalog: policy's"
        );
    }
}
