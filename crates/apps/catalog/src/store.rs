//! The Store's actions (DV-10, DV-11): what the org's catalog offers a
//! person, and their own choices from it.
//!
//! - `catalog.list`: every item one of their groups gets and none blocks,
//!   with how it reaches them and whether they installed it. Read only.
//! - `catalog.install` and `catalog.remove`: an item IT made *available*
//!   joins, or leaves, the apps and agents they see. *Required* items are
//!   theirs already; nothing else can be installed.
//!
//! The choices are a file in the person's home (`INSTALLS`), written
//! through their confined home as them, so undo restores it like any file
//! a call changed. Installing only narrows what reaches a person: an
//! available item stays out until installed. Policy still decides who may
//! run anything (ADR 0023), so writing the file by hand gains nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use glass_actions::{ActionError, ActionOutput, Change, Preview, TaskContext};
use glass_api::ChangeKind;
use glass_managed::catalog::search;
use glass_managed::{CatalogItem, InstallMode, ItemKind};
use glass_platform::fs::ConfinedDir;
use glass_types::AgentId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Applied, declares_text};

pub const LIST: &str = "catalog.list";
pub const INSTALL: &str = "catalog.install";
pub const REMOVE: &str = "catalog.remove";

/// A person's choices from the catalog, under their home.
pub const INSTALLS: &str = ".local/share/glasshouse/catalog/installed.json";
/// More than any catalog offers one person.
const MAX_INSTALLS_BYTES: u64 = 64 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Installs {
    installed: BTreeSet<AgentId>,
}

/// What `home`'s person installed from the catalog. A missing or
/// unreadable file is nothing installed.
pub fn installed(home: &Path) -> BTreeSet<AgentId> {
    ConfinedDir::open(home)
        .and_then(|dir| dir.read(Path::new(INSTALLS), MAX_INSTALLS_BYTES))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Installs>(&bytes).ok())
        .map(|i| i.installed)
        .unwrap_or_default()
}

/// Whether the catalog lets `id` reach someone in `groups` who installed
/// `installed`. Ids the catalog doesn't list are left to policy alone;
/// required items reach everyone their groups name; available ones once
/// installed; anything else (another group's, blocked) not at all. It only
/// ever narrows what policy allows.
pub fn reaches<S: AsRef<str>>(
    catalog: Option<&Applied>,
    groups: &[S],
    installed: &BTreeSet<AgentId>,
    id: &AgentId,
) -> bool {
    let Some(item) = catalog.and_then(|c| c.release.items.iter().find(|i| &i.id == id)) else {
        return true;
    };
    match item.mode_for(groups) {
        Some(InstallMode::Required) => true,
        Some(InstallMode::Available) => installed.contains(id),
        Some(InstallMode::Blocked) | None => false,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListInput {
    #[serde(default)]
    kind: Option<ItemKind>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemRef {
    id: AgentId,
}

fn path(ctx: &TaskContext) -> PathBuf {
    ctx.home.join(INSTALLS)
}

/// The item `id`, offered to this person as `mode`.
fn offered<'a>(
    catalog: Option<&'a Applied>,
    ctx: &TaskContext,
    id: &AgentId,
) -> Result<(&'a CatalogItem, InstallMode), ActionError> {
    let refused = || {
        ActionError::InvalidInput(format!(
            "{id} isn't in what your organization's catalog offers you"
        ))
    };
    let item = catalog
        .and_then(|c| c.release.items.iter().find(|i| &i.id == id))
        .ok_or_else(refused)?;
    let mode = item.mode_for(&ctx.groups).ok_or_else(refused)?;
    Ok((item, mode))
}

pub(crate) fn preview(
    catalog: Option<&Applied>,
    ctx: &TaskContext,
    action: &str,
    input: &Value,
) -> Result<Preview, ActionError> {
    if action == LIST {
        let _: ListInput = glass_actions::parse_input(input)?;
        return Ok(Preview {
            summary: "List what the org's catalog offers you".into(),
            targets: vec!["Org catalog".into()],
            ..Preview::default()
        });
    }
    let ItemRef { id } = glass_actions::parse_input(input)?;
    let (item, mode) = offered(catalog, ctx, &id)?;
    if mode == InstallMode::Required {
        return Err(ActionError::InvalidInput(format!(
            "IT installs {} for you; there's nothing to {}",
            item.name,
            if action == INSTALL {
                "install"
            } else {
                "remove"
            }
        )));
    }
    let folder = path(ctx)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Ok(Preview {
        summary: format!(
            "{} {} ({}, by {})",
            if action == INSTALL {
                "Install"
            } else {
                "Remove"
            },
            item.name,
            item.kind.as_str(),
            item.publisher
        ),
        targets: vec![item.name.clone()],
        items: vec![format!("Declares {}", declares_text(item))],
        reads: Vec::new(),
        writes: vec![path(ctx)],
        snapshot_roots: vec![folder],
        label: None,
    })
}

pub(crate) fn invoke(
    catalog: Option<&Applied>,
    ctx: &TaskContext,
    action: &str,
    input: &Value,
) -> Result<ActionOutput, ActionError> {
    if action == LIST {
        let ListInput { kind } = glass_actions::parse_input(input)?;
        return Ok(list(catalog, ctx, kind));
    }
    let ItemRef { id } = glass_actions::parse_input(input)?;
    let (item, mode) = offered(catalog, ctx, &id)?;
    if mode == InstallMode::Required {
        return Err(ActionError::InvalidInput(format!(
            "IT installs {} for you",
            item.name
        )));
    }
    let home = ConfinedDir::open(&ctx.home).map_err(|e| ActionError::io("home folder", e))?;
    let mut installs = Installs {
        installed: installed(&ctx.home),
    };
    let install = action == INSTALL;
    let changed = if install {
        installs.installed.insert(id.clone())
    } else {
        installs.installed.remove(&id)
    };
    if changed {
        if let Some(folder) = Path::new(INSTALLS).parent()
            && !home.exists(folder)
        {
            home.create_dir_all_as(folder, ctx.owner)
                .map_err(|e| ActionError::io("catalog folder", e))?;
        }
        let text =
            serde_json::to_vec_pretty(&installs).map_err(|e| ActionError::Failed(e.to_string()))?;
        home.write(Path::new(INSTALLS), &text, Some(ctx.owner))
            .map_err(|e| ActionError::io("saving what you installed", e))?;
    }
    let done = match (install, changed) {
        (true, true) => format!("Installed {}", item.name),
        (true, false) => format!("{} was installed already", item.name),
        (false, true) => format!("Removed {}", item.name),
        (false, false) => format!("{} wasn't installed", item.name),
    };
    Ok(ActionOutput {
        text: done,
        data: json!({ "id": id, "installed": install }),
        changes: if changed {
            vec![Change {
                kind: ChangeKind::Modified,
                target: format!("Catalog: {}", item.name),
                reversible: true,
                paths: vec![path(ctx)],
            }]
        } else {
            Vec::new()
        },
    })
}

/// Everything the catalog offers this person, by name.
fn list(catalog: Option<&Applied>, ctx: &TaskContext, kind: Option<ItemKind>) -> ActionOutput {
    let Some(catalog) = catalog else {
        return ActionOutput {
            text: "No catalog from IT has reached this computer yet.".into(),
            data: json!({ "catalog_version": null, "items": [] }),
            changes: Vec::new(),
        };
    };
    let mine = installed(&ctx.home);
    let found = search(&catalog.release.items, &ctx.groups, "", kind, usize::MAX);
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
                "installed": m.mode == InstallMode::Required || mine.contains(&item.id),
                "summary": item.summary,
                "declares": item.declares,
                // An app someone at the org made (ADR 0032, R2): who, and
                // what it remixes (`mail@0.1.0`), for the Store to say so.
                "app": item.app.as_ref().map(|a| json!({
                    "made_by": a.made_by,
                    "based_on": a.based_on,
                })),
            })
        })
        .collect();
    let text = match found.len() {
        0 => "Your organization's catalog offers you nothing yet.".to_owned(),
        n => format!(
            "Your organization's catalog offers you {n} item{}.",
            if n == 1 { "" } else { "s" }
        ),
    };
    ActionOutput {
        text,
        data: json!({ "catalog_version": catalog.version, "items": items }),
        changes: Vec::new(),
    }
}
