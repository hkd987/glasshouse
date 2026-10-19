//! Studio: remixing and making apps (GA-1 to GA-5, GA-12, TL-5, ADR 0032).
//!
//! Every web app ships its source. A person, usually through their agent,
//! remixes one into an app of their own or builds a new one from the SDK's
//! template; glassd checks, bundles and keeps every version, and the app is
//! theirs alone until they share it through the store (slice R2).
//!
//! - `apps.bases` (Auto, reads): what they may start from for a need.
//! - `apps.source` (Auto, reads): an app's files, or the template's.
//! - `apps.save` (Notify): a remix, or a new version of an app of theirs.
//! - `apps.create` (Ask): a new app from the template, once the catalog and
//!   the bases had nothing for the need, or the person turned it down.
//! - `apps.versions` (Auto) and `apps.revert` (Notify): undo is choosing an
//!   earlier version; versions never change.
//!
//! An app's id starts with `my-` ([`glass_types::PERSONAL_PREFIX`]): it has
//! its own origin and never the rules policy gives the app it remixes.

pub mod bundle;
pub mod checks;
pub mod store;
pub mod testing;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext, parse_input,
};
use glass_api::{AppManifest, AppPermission, ChangeKind};
use glass_managed::catalog::relevance;
use glass_platform::sandbox::Sandbox;
use glass_types::{ActionName, AgentId, Label, PERSONAL_PREFIX, SideEffect, Tier, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub use bundle::Bundler;
pub use checks::Findings;
pub use store::{Base, Record, Store};

pub const BASES: &str = "apps.bases";
pub const SOURCE: &str = "apps.source";
pub const SAVE: &str = "apps.save";
pub const CREATE: &str = "apps.create";
pub const VERSIONS: &str = "apps.versions";
pub const REVERT: &str = "apps.revert";

/// What the studio needs from the rest of the system, which glassd gives it
/// once the runtime is up.
pub trait Surroundings: Send + Sync + fmt::Debug {
    /// The apps in `login`'s profile: the ones they may remix.
    fn profile(&self, login: &str) -> BTreeSet<AgentId>;
    /// Why an app of `login`'s declaring `manifest` couldn't work for them:
    /// actions that don't exist or that their policy never allows an app
    /// they make, labels beyond theirs (GA-5). Empty when it fits.
    fn refusals(&self, login: &str, manifest: &AppManifest) -> Vec<String>;
    /// What the org's catalog offers someone in `groups` for `need`, best
    /// first: id and name (GA-2).
    fn offers(&self, groups: &[String], need: &str) -> Vec<(AgentId, String)>;
}

/// Where things are, from glassd's configuration.
#[derive(Debug, Clone)]
pub struct StudioPaths {
    /// glassd's own folder for the apps people make (root only).
    pub store: PathBuf,
    /// The installed apps: each one's manifest, `src/` and `dist/`.
    pub installed: PathBuf,
    /// The SDK's installed copy: `package.json`, `src/` and `template/`.
    pub sdk: PathBuf,
    /// esbuild.
    pub bundler: PathBuf,
}

/// The studio: the store of apps people make and how they're built. Cheap
/// to clone; glassd wires in the sandbox and its surroundings at start.
#[derive(Debug, Clone)]
pub struct Studio {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    paths: StudioPaths,
    store: Store,
    bundler: OnceLock<Bundler>,
    surroundings: OnceLock<Arc<dyn Surroundings>>,
    /// One save at a time on this computer: versions are numbered.
    saving: Mutex<()>,
}

/// One of a person's apps as they use it now.
#[derive(Debug, Clone)]
pub struct OwnApp {
    pub manifest: AppManifest,
    pub record: Record,
}

impl Studio {
    pub fn new(paths: StudioPaths) -> Self {
        Self {
            inner: Arc::new(Inner {
                store: Store::new(&paths.store),
                paths,
                bundler: OnceLock::new(),
                surroundings: OnceLock::new(),
                saving: Mutex::new(()),
            }),
        }
    }

    /// A studio that has nothing and builds nothing, for listing actions.
    pub fn offline() -> Self {
        Self::new(StudioPaths {
            store: "/nonexistent".into(),
            installed: "/nonexistent".into(),
            sdk: "/nonexistent".into(),
            bundler: "/nonexistent".into(),
        })
    }

    /// Bundles with `sandbox`'s confinement, in the store's build folder.
    pub fn set_sandbox(&self, sandbox: Arc<dyn Sandbox>) {
        let _ = self.inner.bundler.set(Bundler::new(
            sandbox,
            self.inner.paths.bundler.clone(),
            self.inner.paths.sdk.clone(),
            self.inner.paths.store.join(".build"),
        ));
    }

    pub fn set_surroundings(&self, surroundings: Arc<dyn Surroundings>) {
        let _ = self.inner.surroundings.set(surroundings);
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// `login`'s apps at the versions they chose (in `home`).
    pub fn own_apps(&self, login: &str, home: &Path) -> Vec<OwnApp> {
        store::chosen(home)
            .into_iter()
            .filter_map(|(app, version)| {
                Some(OwnApp {
                    manifest: self.inner.store.manifest(login, &app, version)?,
                    record: self.inner.store.record(login, &app, version)?,
                })
            })
            .filter(|own| own.record.owner == login && own.manifest.id.is_personal())
            .collect()
    }

    /// What `login`'s `app` serves now: its version and files.
    pub fn served(
        &self,
        login: &str,
        home: &Path,
        app: &AgentId,
    ) -> Option<(u32, BTreeMap<String, String>)> {
        let version = *store::chosen(home).get(app)?;
        let dist = self.inner.store.dist(login, app, version).ok()?;
        Some((version, dist))
    }

    /// Every app made on this computer, at its latest version, for IT's
    /// inventory (GA-10).
    pub fn inventory(&self) -> Vec<(Record, AppManifest)> {
        let store = &self.inner.store;
        store
            .owners()
            .into_iter()
            .flat_map(|owner| {
                store.apps(&owner).into_iter().filter_map(move |app| {
                    let latest = *store.versions(&owner, &app).last()?;
                    Some((
                        store.record(&owner, &app, latest)?,
                        store.manifest(&owner, &app, latest)?,
                    ))
                })
            })
            .collect()
    }

    fn surroundings(&self) -> Result<&Arc<dyn Surroundings>, ActionError> {
        self.inner
            .surroundings
            .get()
            .ok_or_else(|| ActionError::Failed("apps can't be made here yet".into()))
    }

    /// An installed app's manifest and source, if it has its source.
    fn installed(&self, id: &AgentId) -> Option<(AppManifest, BTreeMap<String, String>)> {
        let dir = self.inner.paths.installed.join(id.as_str());
        let manifest: AppManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).ok()?).ok()?;
        let source = store::read_tree(&dir, "src").ok()?;
        (manifest.id == *id && !id.is_personal()).then_some((manifest, source))
    }

    /// The installed apps that have their source, by id.
    fn installed_ids(&self) -> Vec<AgentId> {
        let mut ids: Vec<AgentId> = std::fs::read_dir(&self.inner.paths.installed)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
            .filter(|id: &AgentId| !id.is_personal())
            .collect();
        ids.sort();
        ids
    }
}

/// The studio's actions.
#[derive(Debug, Clone)]
pub struct StudioProvider {
    studio: Studio,
}

impl StudioProvider {
    pub fn new(studio: Studio) -> Self {
        Self { studio }
    }
}

fn def(
    name: &str,
    title: &str,
    description: &str,
    side_effect: SideEffect,
    tier: Tier,
    schema: Value,
) -> ActionDef {
    ActionDef {
        name: name.parse().expect("static action name"),
        title: title.into(),
        description: description.into(),
        input_schema: schema,
        side_effect,
        default_tier: tier,
        ask: Default::default(),
    }
}

fn manifest_schema() -> Value {
    json!({
        "type": "object",
        "description": "What the app declares: the OS lets it do only this.",
        "properties": {
            "name": { "type": "string", "description": "The app's name, one line." },
            "description": { "type": "string" },
            "actions": { "type": "array", "items": { "type": "string" }, "description": "Every action the app calls, such as mail.list. Nothing else will run." },
            "permissions": { "type": "array", "items": { "enum": ["offline", "media"] } },
            "labels": { "type": "array", "items": { "enum": ["public", "internal", "confidential"] }, "description": "The data labels it touches." }
        },
        "required": ["actions", "labels"],
        "additionalProperties": false
    })
}

fn files_schema() -> Value {
    json!({
        "type": "object",
        "description": "The app's source, path → text: src/main.ts (required), src/app.css and any other .ts or .css under src/. The page is made from the manifest.",
        "additionalProperties": { "type": "string" }
    })
}

/// What an app declares, as an agent sends it: the studio sets its id and
/// version.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declares {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    actions: Vec<ActionName>,
    #[serde(default)]
    permissions: Vec<AppPermission>,
    labels: Vec<Label>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BasesInput {
    #[serde(default)]
    need: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceInput {
    #[serde(default)]
    app: Option<AgentId>,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    template: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveInput {
    /// An app of theirs: the save is its next version.
    #[serde(default)]
    app: Option<AgentId>,
    /// An installed app: the save is a new app remixing it.
    #[serde(default)]
    base: Option<AgentId>,
    manifest: Declares,
    files: BTreeMap<String, String>,
    note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    need: String,
    #[serde(default)]
    declined: Vec<AgentId>,
    manifest: Declares,
    files: BTreeMap<String, String>,
    note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionsInput {
    #[serde(default)]
    app: Option<AgentId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevertInput {
    app: AgentId,
    version: u32,
}

/// What a save will be, worked out from its input alone.
#[derive(Debug)]
struct Plan {
    app: AgentId,
    name: String,
    version: u32,
    base: Option<Base>,
    base_actions: Vec<ActionName>,
    need: Option<String>,
    declined: Vec<AgentId>,
    manifest: AppManifest,
    files: BTreeMap<String, String>,
    note: String,
}

fn invalid(why: impl Into<String>) -> ActionError {
    ActionError::InvalidInput(why.into())
}

/// A name's id among `login`'s apps: `my-` and the name's words.
fn id_for(name: &str, taken: &[AgentId]) -> Result<AgentId, ActionError> {
    let mut slug = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    // "My Mail" is my-mail, not my-my-mail.
    let slug = slug.strip_prefix("my-").unwrap_or(slug);
    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_matches('-');
    let stem = format!(
        "{PERSONAL_PREFIX}{}",
        if slug.is_empty() { "app" } else { slug }
    );
    (1..100)
        .map(|n| {
            if n == 1 {
                stem.clone()
            } else {
                format!("{stem}-{n}")
            }
        })
        .filter_map(|id| id.parse::<AgentId>().ok())
        .find(|id| !taken.contains(id))
        .ok_or_else(|| invalid("pick another name: that one is taken"))
}

fn digest(files: &BTreeMap<String, String>) -> String {
    let mut hash = Sha256::new();
    for (path, text) in files {
        hash.update(path.as_bytes());
        hash.update([0]);
        hash.update(text.as_bytes());
        hash.update([0]);
    }
    hex::encode(hash.finalize())
}

impl StudioProvider {
    fn plan_save(&self, ctx: &TaskContext, input: SaveInput) -> Result<Plan, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        let store = self.studio.store();
        let (app, version, base, base_actions, default_name) = match (&input.app, &input.base) {
            (Some(app), None) => {
                let versions = store.versions(login, app);
                let Some(&latest) = versions.last() else {
                    return Err(invalid(format!(
                        "{app} isn't an app of yours; remix an app with base, or make one with apps.create"
                    )));
                };
                let record = store
                    .record(login, app, latest)
                    .ok_or_else(|| invalid(format!("{app}'s versions can't be read")))?;
                let base_actions = record
                    .based_on
                    .as_ref()
                    .and_then(|b| self.studio.installed(&b.id))
                    .map(|(m, _)| m.actions)
                    .unwrap_or_default();
                (
                    app.clone(),
                    latest + 1,
                    record.based_on,
                    base_actions,
                    record.name,
                )
            }
            (None, Some(base)) => {
                let surroundings = self.studio.surroundings()?;
                let Some((manifest, _)) = self.studio.installed(base) else {
                    return Err(invalid(format!(
                        "{base} isn't an app installed here with its source; apps.bases lists what you can remix"
                    )));
                };
                if !surroundings.profile(login).contains(base) {
                    return Err(invalid(format!(
                        "{} isn't one of your apps, so it can't be remixed for you",
                        manifest.name
                    )));
                }
                // Never the base's own name: both are in the launcher.
                let name = input
                    .manifest
                    .name
                    .clone()
                    .filter(|n| n.trim() != manifest.name)
                    .unwrap_or_else(|| format!("My {}", manifest.name));
                let app = id_for(&name, &store.apps(login))?;
                let based_on = Base {
                    id: manifest.id.clone(),
                    name: manifest.name.clone(),
                    version: manifest.version.clone(),
                };
                (app, 1, Some(based_on), manifest.actions, name)
            }
            _ => {
                return Err(invalid(
                    "name either app (a new version of an app of yours) or base (an app to remix)",
                ));
            }
        };
        let name = match &input.app {
            Some(_) => input.manifest.name.clone().unwrap_or(default_name),
            None => default_name,
        };
        Ok(Plan {
            manifest: manifest(&app, &name, version, input.manifest),
            app,
            name,
            version,
            base,
            base_actions,
            need: None,
            declined: Vec::new(),
            files: input.files,
            note: input.note,
        })
    }

    fn plan_create(&self, ctx: &TaskContext, input: CreateInput) -> Result<Plan, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        if input.need.trim().is_empty() {
            return Err(invalid(
                "say what the app is for (need): the store is searched for it first",
            ));
        }
        let Some(name) = input.manifest.name.clone() else {
            return Err(invalid("a new app needs a name"));
        };
        // GA-2, GA-12: the store first, then what can be remixed; a new app
        // only when the person turned those down.
        let offered = self.studio.surroundings()?.offers(&ctx.groups, &input.need);
        let remixable = self.bases(ctx, &input.need)?;
        let missed: Vec<String> = offered
            .iter()
            .map(|(id, name)| (id, format!("{name} ({id}) in your organization's catalog")))
            .chain(
                remixable
                    .iter()
                    .filter(|b| b.score > 0 && b.remixable)
                    .map(|b| (&b.id, format!("{} ({}), which you can remix", b.name, b.id))),
            )
            .filter(|(id, _)| !input.declined.contains(id))
            .map(|(_, text)| text)
            .collect();
        if !missed.is_empty() {
            return Err(invalid(format!(
                "Something already meets this need: {}. Suggest it first; if the person still \
                 wants their own app, name what they turned down in declined.",
                missed.join("; ")
            )));
        }
        let app = id_for(&name, &self.studio.store().apps(login))?;
        Ok(Plan {
            manifest: manifest(&app, &name, 1, input.manifest),
            app,
            name,
            version: 1,
            base: None,
            base_actions: Vec::new(),
            need: Some(input.need),
            declined: input.declined,
            files: input.files,
            note: input.note,
        })
    }

    /// Checks, bundles and saves `plan`, then makes it the version the
    /// person uses.
    fn save(&self, ctx: &TaskContext, plan: Plan) -> Result<ActionOutput, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        let _one_at_a_time = self
            .studio
            .inner
            .saving
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // Numbered under the lock: another save may have taken it.
        let version = self
            .studio
            .store()
            .versions(login, &plan.app)
            .last()
            .map_or(plan.version, |latest| plan.version.max(latest + 1));
        let mut manifest = plan.manifest.clone();
        manifest.version = version.to_string();
        let mut found = Findings::default();
        if let Err(why) = manifest.check() {
            found.refusals.push(why);
        }
        found
            .refusals
            .extend(self.studio.surroundings()?.refusals(login, &manifest));
        let source = checks::check_source(&manifest, &plan.files);
        found.refusals.extend(source.refusals);
        found.notes.extend(source.notes);
        if !found.passed() {
            return Err(refused(&found));
        }
        let bundler = self
            .studio
            .inner
            .bundler
            .get()
            .ok_or_else(|| ActionError::Failed("apps can't be built here yet".into()))?;
        let dist = bundler
            .bundle(&manifest, &plan.files)
            .map_err(|f| refused(&f))?;
        let beyond_base: Vec<ActionName> = match &plan.base {
            Some(_) => manifest
                .actions
                .iter()
                .filter(|a| !plan.base_actions.contains(a))
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        let record = Record {
            app: plan.app.clone(),
            name: plan.name.clone(),
            version,
            owner: login.to_owned(),
            based_on: plan.base.clone(),
            note: plan.note.trim().to_owned(),
            saved_at: Timestamp::now(),
            saved_by: ctx.principal.agent.clone(),
            source_sha256: digest(&plan.files),
            notes: found.notes.clone(),
            need: plan.need.clone(),
            declined: plan.declined.clone(),
            beyond_base: beyond_base.clone(),
        };
        self.studio
            .store()
            .save(&record, &manifest, &plan.files, &dist)
            .map_err(|e| ActionError::Failed(format!("saving {}: {e}", plan.name)))?;
        let mut chosen = store::chosen(&ctx.home);
        chosen.insert(plan.app.clone(), version);
        store::choose(&ctx.home, ctx.owner, chosen)
            .map_err(|e| ActionError::io("choosing the new version", e))?;

        let based = plan
            .base
            .as_ref()
            .map(|b| format!(", your version of {} {}", b.name, b.version))
            .unwrap_or_default();
        let mut text = format!(
            "Saved {} version {version}{based}. It's in your launcher; only you can open it.",
            plan.name
        );
        if !beyond_base.is_empty() {
            text.push_str(&format!(
                " It does more than {}: {}.",
                plan.base.as_ref().map_or("its base", |b| b.name.as_str()),
                names(&beyond_base)
            ));
        }
        for note in &found.notes {
            text.push_str(&format!(" Note: {note}."));
        }
        Ok(ActionOutput {
            text,
            data: json!({
                "app": plan.app,
                "name": plan.name,
                "version": version,
                "based_on": plan.base,
                "beyond_base": beyond_base,
                "notes": found.notes,
            }),
            changes: vec![Change {
                kind: if version == 1 {
                    ChangeKind::Created
                } else {
                    ChangeKind::Modified
                },
                target: format!("App: {} version {version}", plan.name),
                reversible: true,
                paths: vec![ctx.home.join(store::MINE)],
            }],
        })
    }
}

fn names(actions: &[ActionName]) -> String {
    actions
        .iter()
        .map(ActionName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn refused(found: &Findings) -> ActionError {
    invalid(format!(
        "Not saved; fix these and save again: {}",
        found.refusals.join("; ")
    ))
}

fn manifest(app: &AgentId, name: &str, version: u32, declares: Declares) -> AppManifest {
    AppManifest {
        id: app.clone(),
        name: name.trim().to_owned(),
        version: version.to_string(),
        description: declares.description.unwrap_or_default(),
        actions: declares.actions,
        permissions: declares.permissions,
        labels: declares.labels,
        network: Vec::new(),
    }
}

/// Something a person may start from.
#[derive(Debug, Clone)]
struct BaseOffer {
    id: AgentId,
    name: String,
    version: String,
    description: String,
    yours: bool,
    remixable: bool,
    why_not: Option<String>,
    score: u32,
}

impl StudioProvider {
    /// What `ctx`'s person may start from, best match for `need` first:
    /// installed apps in their profile with their source, and their own.
    fn bases(&self, ctx: &TaskContext, need: &str) -> Result<Vec<BaseOffer>, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        let profile = self.studio.surroundings()?.profile(login);
        let mut offers: Vec<BaseOffer> = self
            .studio
            .installed_ids()
            .into_iter()
            .filter(|id| profile.contains(id))
            .filter_map(|id| {
                let (manifest, source) = self.studio.installed(&id)?;
                let found = checks::check_source(&manifest, &source);
                Some(BaseOffer {
                    score: relevance(need, &format!("{} {}", manifest.name, manifest.description)),
                    id,
                    name: manifest.name,
                    version: manifest.version,
                    description: manifest.description,
                    yours: false,
                    remixable: found.passed(),
                    why_not: found.refusals.first().cloned(),
                })
            })
            .collect();
        for own in self.studio.own_apps(login, &ctx.home) {
            offers.push(BaseOffer {
                score: relevance(
                    need,
                    &format!(
                        "{} {} {}",
                        own.manifest.name, own.manifest.description, own.record.note
                    ),
                ),
                id: own.manifest.id,
                name: own.manifest.name,
                version: own.manifest.version,
                description: own.manifest.description,
                yours: true,
                remixable: true,
                why_not: None,
            });
        }
        offers.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
        Ok(offers)
    }

    fn source(&self, ctx: &TaskContext, input: SourceInput) -> Result<ActionOutput, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        if input.template {
            let sdk = &self.studio.inner.paths.sdk;
            let manifest: Value = serde_json::from_slice(
                &std::fs::read(sdk.join("template/manifest.json"))
                    .map_err(|e| ActionError::io("the template", e))?,
            )
            .map_err(|e| ActionError::Failed(format!("the template: {e}")))?;
            let files = store::read_tree(&sdk.join("template"), "src")
                .map_err(|e| ActionError::io("the template", e))?;
            return Ok(ActionOutput {
                text: format!(
                    "The template for a new app: {} files. Change them and make the app with \
                     apps.create once the catalog and apps.bases have nothing for the need.",
                    files.len()
                ),
                data: json!({ "template": true, "manifest": manifest, "files": files }),
                changes: Vec::new(),
            });
        }
        let Some(app) = input.app else {
            return Err(invalid("name an app, or ask for the template"));
        };
        if app.is_personal() {
            let store = self.studio.store();
            let version = match input.version {
                Some(v) => v,
                None => *store
                    .versions(login, &app)
                    .last()
                    .ok_or_else(|| invalid(format!("{app} isn't an app of yours")))?,
            };
            let (Some(manifest), Some(record)) = (
                store.manifest(login, &app, version),
                store.record(login, &app, version),
            ) else {
                return Err(invalid(format!("{app} has no version {version}")));
            };
            let files = store
                .source(login, &app, version)
                .map_err(|e| ActionError::io("its source", e))?;
            return Ok(ActionOutput {
                text: format!(
                    "{} version {version}: {} files. Save a new version with apps.save {{\"app\": \"{app}\"}}.",
                    manifest.name,
                    files.len()
                ),
                data: json!({
                    "app": app, "name": manifest.name, "version": version, "yours": true,
                    "based_on": record.based_on, "manifest": declares_json(&manifest), "files": files,
                }),
                changes: Vec::new(),
            });
        }
        let surroundings = self.studio.surroundings()?;
        let (manifest, files) = self
            .studio
            .installed(&app)
            .filter(|_| surroundings.profile(login).contains(&app))
            .ok_or_else(|| invalid(format!("{app} isn't an app of yours with its source here")))?;
        Ok(ActionOutput {
            text: format!(
                "{} {}: {} files. Change them and save your version with apps.save {{\"base\": \"{app}\"}}.",
                manifest.name,
                manifest.version,
                files.len()
            ),
            data: json!({
                "app": app, "name": manifest.name, "version": manifest.version, "yours": false,
                "manifest": declares_json(&manifest), "files": files,
            }),
            changes: Vec::new(),
        })
    }

    fn versions(
        &self,
        ctx: &TaskContext,
        input: VersionsInput,
    ) -> Result<ActionOutput, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        let store = self.studio.store();
        let chosen = store::chosen(&ctx.home);
        let Some(app) = input.app else {
            let apps: Vec<Value> = self
                .studio
                .own_apps(login, &ctx.home)
                .into_iter()
                .map(|own| record_json(&own.record, true))
                .collect();
            return Ok(ActionOutput {
                text: match apps.len() {
                    0 => "You haven't made any apps yet.".to_owned(),
                    n => format!(
                        "You have {n} app{} of your own.",
                        if n == 1 { "" } else { "s" }
                    ),
                },
                data: json!({ "apps": apps }),
                changes: Vec::new(),
            });
        };
        let versions: Vec<Value> = store
            .versions(login, &app)
            .into_iter()
            .rev()
            .filter_map(|v| store.record(login, &app, v))
            .map(|r| record_json(&r, chosen.get(&app) == Some(&r.version)))
            .collect();
        if versions.is_empty() {
            return Err(invalid(format!("{app} isn't an app of yours")));
        }
        Ok(ActionOutput {
            text: format!(
                "{} version{}; you use {}.",
                versions.len(),
                if versions.len() == 1 { "" } else { "s" },
                chosen
                    .get(&app)
                    .map_or("none of them".to_owned(), |v| format!("version {v}"))
            ),
            data: json!({ "app": app, "versions": versions }),
            changes: Vec::new(),
        })
    }

    fn revert(&self, ctx: &TaskContext, input: RevertInput) -> Result<ActionOutput, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        let record = self
            .studio
            .store()
            .record(login, &input.app, input.version)
            .ok_or_else(|| invalid(format!("{} has no version {}", input.app, input.version)))?;
        let mut chosen = store::chosen(&ctx.home);
        chosen.insert(input.app.clone(), input.version);
        store::choose(&ctx.home, ctx.owner, chosen)
            .map_err(|e| ActionError::io("choosing the version", e))?;
        Ok(ActionOutput {
            text: format!("{} is back to version {}.", record.name, record.version),
            data: json!({ "app": input.app, "version": input.version }),
            changes: vec![Change {
                kind: ChangeKind::Modified,
                target: format!("App: {} version {}", record.name, record.version),
                reversible: true,
                paths: vec![ctx.home.join(store::MINE)],
            }],
        })
    }
}

fn declares_json(manifest: &AppManifest) -> Value {
    json!({
        "name": manifest.name,
        "description": manifest.description,
        "actions": manifest.actions,
        "permissions": manifest.permissions,
        "labels": manifest.labels,
    })
}

fn record_json(record: &Record, current: bool) -> Value {
    json!({
        "app": record.app,
        "name": record.name,
        "version": record.version,
        "current": current,
        "based_on": record.based_on,
        "note": record.note,
        "saved_at": record.saved_at,
        "saved_by": record.saved_by,
        "beyond_base": record.beyond_base,
    })
}

/// The person's choices file and its folder, for a call that changes it.
fn choice_preview(ctx: &TaskContext, summary: String, items: Vec<String>) -> Preview {
    let file = ctx.home.join(store::MINE);
    Preview {
        summary,
        targets: vec!["Your apps".into()],
        items,
        reads: Vec::new(),
        snapshot_roots: file.parent().map(Path::to_path_buf).into_iter().collect(),
        writes: vec![file],
    }
}

impl ActionProvider for StudioProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let app =
            json!({ "type": "string", "description": "An app's id, such as mail or my-mail." });
        vec![
            def(
                BASES,
                "Apps to start from",
                "Lists the apps this person can remix into their own (built-in apps with their \
                 source, and apps they made), best match for the need first. Use it after \
                 catalog.search found nothing to install, before making a new app.",
                SideEffect::None,
                Tier::Auto,
                json!({ "type": "object", "properties": { "need": { "type": "string" } }, "additionalProperties": false }),
            ),
            def(
                SOURCE,
                "An app's source",
                "An app's TypeScript and CSS source and what its manifest declares, to change \
                 and save as the person's own version; or, with template, the starting point \
                 for a new app.",
                SideEffect::None,
                Tier::Auto,
                json!({
                    "type": "object",
                    "properties": {
                        "app": app,
                        "version": { "type": "integer", "minimum": 1 },
                        "template": { "type": "boolean" }
                    },
                    "additionalProperties": false
                }),
            ),
            def(
                SAVE,
                "Save an app of your own",
                "Saves a remix of an installed app (base) or a new version of an app the person \
                 made (app). glassd checks the source and what it declares, bundles it and keeps \
                 every version; the person can go back to any. Refusals say what to fix.",
                SideEffect::LocalReversible,
                Tier::Notify,
                json!({
                    "type": "object",
                    "properties": {
                        "app": app,
                        "base": app,
                        "manifest": manifest_schema(),
                        "files": files_schema(),
                        "note": { "type": "string", "description": "What changed, in a sentence." }
                    },
                    "required": ["manifest", "files", "note"],
                    "additionalProperties": false
                }),
            ),
            def(
                CREATE,
                "Make a new app",
                "Makes a new app from the template when nothing in the catalog or apps.bases \
                 meets the need. Refused while something does, unless the person turned it \
                 down (declined).",
                SideEffect::LocalReversible,
                Tier::Ask,
                json!({
                    "type": "object",
                    "properties": {
                        "need": { "type": "string", "description": "What the app is for, as searched in the catalog." },
                        "declined": { "type": "array", "items": { "type": "string" }, "description": "Ids the person turned down." },
                        "manifest": manifest_schema(),
                        "files": files_schema(),
                        "note": { "type": "string" }
                    },
                    "required": ["need", "manifest", "files", "note"],
                    "additionalProperties": false
                }),
            ),
            def(
                VERSIONS,
                "Your apps and their versions",
                "Without app: the apps this person made. With app: its versions, newest first, \
                 and which one they use.",
                SideEffect::None,
                Tier::Auto,
                json!({ "type": "object", "properties": { "app": app }, "additionalProperties": false }),
            ),
            def(
                REVERT,
                "Use an earlier version",
                "Goes back to an earlier version of an app the person made.",
                SideEffect::LocalReversible,
                Tier::Notify,
                json!({
                    "type": "object",
                    "properties": { "app": app, "version": { "type": "integer", "minimum": 1 } },
                    "required": ["app", "version"],
                    "additionalProperties": false
                }),
            ),
        ]
    }

    fn prepare(&self, ctx: &TaskContext) -> Result<(), ActionError> {
        // The choices' folder exists before a call changes it, so the
        // snapshot before that call has something to cover.
        let home = glass_platform::fs::ConfinedDir::open(&ctx.home)
            .map_err(|e| ActionError::io("home folder", e))?;
        if let Some(folder) = Path::new(store::MINE).parent()
            && !home.exists(folder)
        {
            home.create_dir_all_as(folder, ctx.owner)
                .map_err(|e| ActionError::io("your apps' folder", e))?;
        }
        Ok(())
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        match action.as_str() {
            BASES | SOURCE | VERSIONS => Ok(Preview {
                summary: match action.as_str() {
                    BASES => "List apps you can start from".into(),
                    SOURCE => "Read an app's source".into(),
                    _ => "List your apps' versions".into(),
                },
                targets: vec!["Your apps".into()],
                ..Preview::default()
            }),
            SAVE => {
                let plan = self.plan_save(ctx, parse_input(input)?)?;
                let based = plan
                    .base
                    .as_ref()
                    .map(|b| format!(" (your version of {})", b.name))
                    .unwrap_or_default();
                let mut items = vec![format!("Does: {}", names(&plan.manifest.actions))];
                let beyond: Vec<ActionName> = plan
                    .manifest
                    .actions
                    .iter()
                    .filter(|a| plan.base.is_some() && !plan.base_actions.contains(a))
                    .cloned()
                    .collect();
                if !beyond.is_empty() {
                    items.push(format!("Beyond what it's based on: {}", names(&beyond)));
                }
                items.push(format!("Change: {}", plan.note.trim()));
                Ok(choice_preview(
                    ctx,
                    format!("Save {}{based}", plan.name),
                    items,
                ))
            }
            CREATE => {
                let plan = self.plan_create(ctx, parse_input(input)?)?;
                let mut items = vec![
                    format!("For: {}", plan.need.as_deref().unwrap_or_default()),
                    format!("Does: {}", names(&plan.manifest.actions)),
                ];
                if !plan.declined.is_empty() {
                    items.push(format!(
                        "You chose this over: {}",
                        plan.declined
                            .iter()
                            .map(AgentId::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                Ok(choice_preview(
                    ctx,
                    format!("Make a new app, {}", plan.name),
                    items,
                ))
            }
            REVERT => {
                let input: RevertInput = parse_input(input)?;
                Ok(choice_preview(
                    ctx,
                    format!("Use version {} of {}", input.version, input.app),
                    Vec::new(),
                ))
            }
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        match action.as_str() {
            BASES => {
                let BasesInput { need } = parse_input(input)?;
                let bases = self.bases(ctx, &need)?;
                let list: Vec<Value> = bases
                    .iter()
                    .map(|b| {
                        json!({
                            "id": b.id, "name": b.name, "version": b.version,
                            "description": b.description, "yours": b.yours,
                            "remixable": b.remixable, "why_not": b.why_not, "match": b.score,
                        })
                    })
                    .collect();
                let text = match bases
                    .iter()
                    .find(|b| b.remixable && (b.score > 0 || need.is_empty()))
                {
                    Some(best) if !need.is_empty() => format!(
                        "{} can be remixed for this: read it with apps.source {{\"app\": \"{}\"}}.",
                        best.name, best.id
                    ),
                    _ if need.is_empty() => format!("{} apps you can start from.", bases.len()),
                    _ => "Nothing here is close; a new app from the template (apps.source \
                          {\"template\": true}) may be the way."
                        .to_owned(),
                };
                Ok(ActionOutput {
                    text,
                    data: json!({ "bases": list }),
                    changes: Vec::new(),
                })
            }
            SOURCE => self.source(ctx, parse_input(input)?),
            SAVE => {
                let plan = self.plan_save(ctx, parse_input(input)?)?;
                self.save(ctx, plan)
            }
            CREATE => {
                let plan = self.plan_create(ctx, parse_input(input)?)?;
                self.save(ctx, plan)
            }
            VERSIONS => self.versions(ctx, parse_input(input)?),
            REVERT => self.revert(ctx, parse_input(input)?),
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }
}

#[cfg(test)]
mod tests;
