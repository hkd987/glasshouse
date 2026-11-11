//! The Store's actions (DV-10, DV-11): what the org's catalog offers a
//! person, and their own choices from it.
//!
//! - `catalog.list`: every item one of their groups gets and none blocks,
//!   with how it reaches them and whether they installed it. Read only.
//! - `catalog.install` and `catalog.remove`: an item IT made *available*
//!   joins, or leaves, the apps and agents they see. *Required* items are
//!   theirs already; nothing else can be installed.
//!
//! - `catalog.patch_install`: a kernel patch IT reviewed and made
//!   available, for this computer's hardware (ADR 0057, slice 3), fetched
//!   from the console, checked against the signed catalog's digest and set
//!   up to try at the next restart. The person's own: policy makes it Never
//!   for every agent. It's the whole computer's, not the person's list, so
//!   undo doesn't take it back; Settings takes it out.
//!
//! The choices are a file in the person's home (`INSTALLS`), written
//! through their confined home as them, so undo restores it like any file
//! a call changed. Installing only narrows what reaches a person: an
//! available item stays out until installed. Policy still decides who may
//! run anything (ADR 0023), so writing the file by hand gains nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glass_actions::{ActionError, ActionOutput, Change, Preview, TaskContext};
use glass_api::{ChangeKind, TrialState};
use glass_managed::catalog::search;
use glass_managed::{CatalogItem, InstallMode, ItemKind};
use glass_platform::fs::ConfinedDir;
use glass_types::AgentId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use glass_managed::hwreview::OrgPatch;

use crate::{Applied, CatalogHandle, OrgPatches, declares_text};

pub const LIST: &str = "catalog.list";
pub const INSTALL: &str = "catalog.install";
pub const REMOVE: &str = "catalog.remove";
pub const PATCH_INSTALL: &str = "catalog.patch_install";

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
    read_installs(home).unwrap_or_default()
}

/// What they installed, as their file has it. The file is theirs to edit:
/// one that no longer reads (a typo) is set aside beside itself and they
/// start again, where the next install used to write over it with only
/// the app being installed. One that can't be read at all is an error.
fn read_installs(home: &Path) -> std::io::Result<BTreeSet<AgentId>> {
    Ok(ConfinedDir::open(home)?
        .read_json_or_set_aside::<Installs>(Path::new(INSTALLS), MAX_INSTALLS_BYTES)?
        .map(|installs| installs.installed)
        .unwrap_or_default())
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

/// A kernel patch the catalog makes available to this person, for this
/// computer, and where it is here.
fn patch_offered<'a>(
    catalog: Option<&'a Applied>,
    patches: Option<&Arc<dyn OrgPatches>>,
    ctx: &TaskContext,
    id: &AgentId,
) -> Result<(&'a CatalogItem, &'a OrgPatch, Arc<dyn OrgPatches>), ActionError> {
    let (item, mode) = offered(catalog, ctx, id)?;
    let patch = item
        .patch
        .as_ref()
        .filter(|_| item.kind == ItemKind::HardwarePatch)
        .ok_or_else(|| ActionError::InvalidInput(format!("{} isn't a kernel patch", item.name)))?;
    let patches = patches
        .cloned()
        .ok_or_else(|| ActionError::Failed("this computer takes no kernel patches".into()))?;
    if mode != InstallMode::Available || !patches.fits(patch) {
        return Err(ActionError::InvalidInput(format!(
            "{} isn't for this computer's hardware",
            item.name
        )));
    }
    Ok((item, patch, patches))
}

pub(crate) fn preview(
    handle: &CatalogHandle,
    ctx: &TaskContext,
    action: &str,
    input: &Value,
) -> Result<Preview, ActionError> {
    let current = handle.current();
    let catalog = current.as_deref();
    if action == LIST {
        let _: ListInput = glass_actions::parse_input(input)?;
        return Ok(Preview {
            summary: "List what the org's catalog offers you".into(),
            targets: vec!["Org catalog".into()],
            ..Preview::default()
        });
    }
    let ItemRef { id } = glass_actions::parse_input(input)?;
    if action == PATCH_INSTALL {
        let (item, patch, _) = patch_offered(catalog, handle.patches().as_ref(), ctx, &id)?;
        return Ok(Preview {
            summary: format!("Install kernel patch {}: {}", patch.patch, item.summary),
            targets: vec![format!("Kernel patch {}", patch.patch)],
            items: vec![
                format!("Reviewed by your organization ({})", patch.reviewed_by),
                "It changes the kernel, which runs outside every sandbox".into(),
                "The next restart tries it once; if the computer doesn't start, or crashes \
                 before a day of use, it goes back by itself"
                    .into(),
            ],
            ..Preview::default()
        });
    }
    let (item, mode) = offered(catalog, ctx, &id)?;
    if item.kind == ItemKind::HardwarePatch {
        return Err(ActionError::InvalidInput(format!(
            "{} is a kernel patch: the Store installs it ({PATCH_INSTALL})",
            item.name
        )));
    }
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
        texts: Vec::new(),
        reads: Vec::new(),
        writes: vec![path(ctx)],
        snapshot_roots: vec![folder],
        label: None,
    })
}

pub(crate) fn invoke(
    handle: &CatalogHandle,
    ctx: &TaskContext,
    action: &str,
    input: &Value,
) -> Result<ActionOutput, ActionError> {
    let current = handle.current();
    let catalog = current.as_deref();
    if action == LIST {
        let ListInput { kind } = glass_actions::parse_input(input)?;
        return Ok(list(catalog, handle.patches().as_ref(), ctx, kind));
    }
    let ItemRef { id } = glass_actions::parse_input(input)?;
    if action == PATCH_INSTALL {
        return install_patch(catalog, handle.patches().as_ref(), ctx, &id);
    }
    let (item, mode) = offered(catalog, ctx, &id)?;
    if item.kind == ItemKind::HardwarePatch {
        return Err(ActionError::InvalidInput(format!(
            "{} is a kernel patch: the Store installs it ({PATCH_INSTALL})",
            item.name
        )));
    }
    if mode == InstallMode::Required {
        return Err(ActionError::InvalidInput(format!(
            "IT installs {} for you",
            item.name
        )));
    }
    let home = ConfinedDir::open(&ctx.home).map_err(|e| ActionError::io("home folder", e))?;
    let mut installs = Installs {
        installed: read_installs(&ctx.home)
            .map_err(|e| ActionError::io("what you installed", e))?,
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

/// Sets up a kernel patch from the catalog to try at the next restart.
fn install_patch(
    catalog: Option<&Applied>,
    patches: Option<&Arc<dyn OrgPatches>>,
    ctx: &TaskContext,
    id: &AgentId,
) -> Result<ActionOutput, ActionError> {
    let (item, patch, patches) = patch_offered(catalog, patches, ctx, id)?;
    if let Some(state) = patches.state(patch)
        && (state.open() || state == TrialState::Kept)
    {
        return Err(ActionError::InvalidInput(format!(
            "{} is on this computer already: {}",
            item.name,
            state.label()
        )));
    }
    let trial = patches.install(id, patch).map_err(ActionError::Failed)?;
    Ok(ActionOutput {
        text: format!(
            "Kernel patch {} is set up: restart to try it. If the computer doesn't start, or \
             crashes before a day of use, it goes back by itself; Settings takes it out.",
            trial.id
        ),
        data: json!({ "id": id, "installed": true, "state": trial.state }),
        changes: vec![Change {
            kind: ChangeKind::Created,
            target: format!("Kernel patch {}, to try at the next restart", trial.id),
            reversible: false,
            paths: Vec::new(),
        }],
    })
}

/// Everything the catalog offers this person, by name: kernel patches
/// only those for this computer.
fn list(
    catalog: Option<&Applied>,
    patches: Option<&Arc<dyn OrgPatches>>,
    ctx: &TaskContext,
    kind: Option<ItemKind>,
) -> ActionOutput {
    let Some(catalog) = catalog else {
        return ActionOutput {
            text: "No catalog from IT has reached this computer yet.".into(),
            data: json!({ "catalog_version": null, "items": [] }),
            changes: Vec::new(),
        };
    };
    let mine = installed(&ctx.home);
    let mut found = match kind {
        Some(ItemKind::HardwarePatch) => Vec::new(),
        _ => search(&catalog.release.items, &ctx.groups, "", kind, usize::MAX),
    };
    if let Some(patches) = patches
        && kind.is_none_or(|k| k == ItemKind::HardwarePatch)
    {
        let all = search(
            &catalog.release.items,
            &ctx.groups,
            "",
            Some(ItemKind::HardwarePatch),
            usize::MAX,
        );
        found.extend(all.into_iter().filter(|m| {
            m.mode == InstallMode::Available
                && m.item.patch.as_ref().is_some_and(|p| patches.fits(p))
        }));
    }
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
                "installed": match (&item.patch, patches) {
                    (Some(p), Some(patches)) => patches
                        .state(p)
                        .is_some_and(|s| s.open() || s == TrialState::Kept),
                    _ => m.mode == InstallMode::Required || mine.contains(&item.id),
                },
                "summary": item.summary,
                "declares": item.declares,
                // An app someone at the org made (ADR 0032, R2): who, and
                // what it remixes (`mail@0.1.0`), for the Store to say so.
                "app": item.app.as_ref().map(|a| json!({
                    "made_by": a.made_by,
                    "based_on": a.based_on,
                })),
                // A kernel patch IT reviewed (ADR 0057): what it's for, and
                // where it is on this computer.
                "patch": item.patch.as_ref().map(|p| json!({
                    "patch": p.patch,
                    "targets": p.targets,
                    "kernel": p.kernel,
                    "bytes": p.bytes,
                    "reviewed_by": p.reviewed_by,
                    "state": patches.and_then(|patches| patches.state(p)),
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
