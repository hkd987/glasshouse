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
pub mod diff;
pub mod store;
pub mod testing;
mod tried;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext, parse_input,
};
use glass_api::{AppManifest, AppPermission, ChangeKind};
use glass_managed::catalog::relevance;
use glass_managed::studio::{ShareLink, ShareTo, SharedBase, Submission, digest};
use glass_platform::sandbox::ToolRunner;
use glass_types::{ActionName, AgentId, Label, PERSONAL_PREFIX, SideEffect, Tier, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};

pub use bundle::Bundler;
pub use checks::Findings;
pub use store::{Base, Record, Store};

pub const BASES: &str = "apps.bases";
pub const SOURCE: &str = "apps.source";
pub const SAVE: &str = "apps.save";
pub const CREATE: &str = "apps.create";
pub const VERSIONS: &str = "apps.versions";
pub const REVERT: &str = "apps.revert";
pub const SHARE: &str = "apps.share";
pub const ACTIONS: &str = "apps.actions";

/// What the studio needs from the rest of the system, which glassd gives it
/// once the runtime is up.
pub trait Surroundings: Send + Sync + fmt::Debug {
    /// The apps in `login`'s profile: the ones they may remix.
    fn profile(&self, login: &str) -> BTreeSet<AgentId>;
    /// Whether IT lets people remix and make apps here (TL-6).
    fn remixing(&self) -> bool;
    /// Why an app of `login`'s declaring `manifest` couldn't work for them:
    /// actions that don't exist or that their policy never allows an app
    /// they make, labels beyond theirs (GA-5). Empty when it fits.
    fn refusals(&self, login: &str, manifest: &AppManifest) -> Vec<String>;
    /// What the org's catalog offers someone in `groups` for `need`, best
    /// first: id and name (GA-2).
    fn offers(&self, groups: &[String], need: &str) -> Vec<(AgentId, String)>;
    /// What an app `login` makes may call: each action on this computer
    /// their policy allows in such an app, with how it runs there, what it
    /// takes and what it returns (GA-1).
    fn capabilities(&self, login: &str) -> Vec<Capability>;
}

/// One action an app someone makes may call ([`Surroundings::capabilities`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Capability {
    pub name: ActionName,
    pub title: String,
    pub description: String,
    pub side_effect: SideEffect,
    /// How it runs when their app calls it: Auto, Notify or Ask.
    pub tier: Tier,
    /// Its input's JSON Schema.
    pub input: Value,
    /// Its result data's shape ([`glass_actions::shape`]), where declared.
    pub returns: Option<Value>,
}

/// Where things are, from glassd's configuration.
#[derive(Debug, Clone)]
pub struct StudioPaths {
    /// glassd's own folder for the apps people make (root only).
    pub store: PathBuf,
    /// The installed apps: each one's manifest and `dist/`.
    pub installed: PathBuf,
    /// Our apps' source, which people remix: `apps/<app>/src` and the SDK
    /// (`sdk/`: `package.json`, `src/`, `template/`). A folder of its own,
    /// which glassd removes when IT turns remixing off (TL-6).
    pub source: PathBuf,
    /// esbuild.
    pub bundler: PathBuf,
    /// glass-app-try, which tries each version saved (GA-1, ADR 0049):
    /// statically linked, alone in its folder, run in the bundler's jail.
    /// None, or not there, and nothing is tried.
    pub tryout: Option<PathBuf>,
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
    /// The org's console, and this computer's name in its fleet: where
    /// sharing goes (R2). Unset on a computer no org manages.
    share: OnceLock<(Arc<dyn ShareLink>, String)>,
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
                share: OnceLock::new(),
                saving: Mutex::new(()),
            }),
        }
    }

    /// A studio that has nothing and builds nothing, for listing actions.
    pub fn offline() -> Self {
        Self::new(StudioPaths {
            store: "/nonexistent".into(),
            installed: "/nonexistent".into(),
            source: "/nonexistent".into(),
            bundler: "/nonexistent".into(),
            tryout: None,
        })
    }

    /// Bundles with `tools`' confinement, in the store's build folder.
    pub fn set_tools(&self, tools: Arc<dyn ToolRunner>) {
        let _ = self.inner.bundler.set(
            Bundler::new(
                tools,
                self.inner.paths.bundler.clone(),
                self.inner.paths.source.join("sdk"),
                self.inner.paths.store.join(".build"),
            )
            .trying_with(self.inner.paths.tryout.clone()),
        );
    }

    pub fn set_surroundings(&self, surroundings: Arc<dyn Surroundings>) {
        let _ = self.inner.surroundings.set(surroundings);
    }

    /// Shares go to the org's console through `link`, from computer
    /// `device` (R2, GA-8).
    pub fn set_share(&self, link: Arc<dyn ShareLink>, device: String) {
        let _ = self.inner.share.set((link, device));
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

    /// An installed app's manifest and source, if its source is here.
    fn installed(&self, id: &AgentId) -> Option<(AppManifest, BTreeMap<String, String>)> {
        let dir = self.inner.paths.installed.join(id.as_str());
        let manifest: AppManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).ok()?).ok()?;
        let source = store::read_tree(
            &self.inner.paths.source.join("apps").join(id.as_str()),
            "src",
        )
        .ok()?;
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

    /// TL-6: nothing is remixed or made while IT has it off.
    fn allowed(&self) -> Result<(), ActionError> {
        if self.studio.surroundings()?.remixing() {
            Ok(())
        } else {
            Err(ActionError::Failed(
                "IT turned remixing apps off for your organization's computers, so apps can't \
                 be remixed or made here"
                    .into(),
            ))
        }
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
    /// One file, as text: an agent that can't take a whole app at once
    /// reads it a file at a time.
    #[serde(default)]
    path: Option<String>,
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
    /// What it declares; what it starts from declares by default.
    #[serde(default)]
    manifest: Option<Declares>,
    /// The whole source, or
    #[serde(default)]
    files: Option<BTreeMap<String, String>>,
    /// changes to what it starts from: an agent changes a line without
    /// sending the rest of the app again.
    #[serde(default)]
    edits: Vec<Edit>,
    note: String,
}

/// One change to a file: `old`, found exactly once, becomes `new`. An
/// empty `old` on a path that isn't there makes the file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    path: String,
    #[serde(default)]
    old: String,
    new: String,
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

/// Who a share is for, as an agent names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Audience {
    Team,
    Org,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareInput {
    app: AgentId,
    to: Audience,
    /// The person's group, for `to: team`.
    #[serde(default)]
    team: Option<String>,
    /// What colleagues see it as in the Store; its own name by default.
    #[serde(default)]
    name: Option<String>,
    note: String,
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

/// A name as an id's words: lowercase letters and digits joined by `-`,
/// 40 at most, without a leading `my-`. Empty when the name has no letters
/// or digits.
pub fn slug(name: &str) -> String {
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
    slug.trim_matches('-').to_owned()
}

/// The first of `stem`, `stem-2`, `stem-3`… that isn't `taken`.
pub fn unique_id(stem: &str, taken: &[AgentId]) -> Option<AgentId> {
    (1..100)
        .map(|n| {
            if n == 1 {
                stem.to_owned()
            } else {
                format!("{stem}-{n}")
            }
        })
        .filter_map(|id| id.parse::<AgentId>().ok())
        .find(|id| !taken.contains(id))
}

/// A name's id among `login`'s apps: `my-` and the name's words.
fn id_for(name: &str, taken: &[AgentId]) -> Result<AgentId, ActionError> {
    let slug = slug(name);
    let stem = format!(
        "{PERSONAL_PREFIX}{}",
        if slug.is_empty() { "app" } else { &slug }
    );
    unique_id(&stem, taken).ok_or_else(|| invalid("pick another name: that one is taken"))
}

impl StudioProvider {
    fn plan_save(&self, ctx: &TaskContext, input: SaveInput) -> Result<Plan, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        let store = self.studio.store();
        // What the save starts from: its manifest and source, read only
        // when the input leaves either out.
        let partial = input.manifest.is_none() || input.files.is_none();
        let (app, version, base, base_actions, default_name, from) = match (&input.app, &input.base)
        {
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
                let from = match partial {
                    true => Some((
                        store
                            .manifest(login, app, latest)
                            .ok_or_else(|| invalid(format!("{app}'s manifest can't be read")))?,
                        store
                            .source(login, app, latest)
                            .map_err(|e| ActionError::io("its source", e))?,
                    )),
                    false => None,
                };
                (
                    app.clone(),
                    latest + 1,
                    record.based_on,
                    base_actions,
                    record.name,
                    from,
                )
            }
            (None, Some(base)) => {
                let surroundings = self.studio.surroundings()?;
                let Some((manifest, files)) = self.studio.installed(base) else {
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
                    .as_ref()
                    .and_then(|m| m.name.clone())
                    .filter(|n| n.trim() != manifest.name)
                    .unwrap_or_else(|| format!("My {}", manifest.name));
                // A remix they undid, of the same app by the same name, goes
                // on where it was: no `my-mail-2` beside a hidden `my-mail`.
                let chosen = store::chosen(&ctx.home);
                let undone = store.apps(login).into_iter().find(|id| {
                    !chosen.contains_key(id)
                        && store
                            .versions(login, id)
                            .last()
                            .and_then(|&v| store.record(login, id, v))
                            .is_some_and(|r| {
                                r.name == name && r.based_on.is_some_and(|b| b.id == *base)
                            })
                });
                // Beside another app of theirs by that name, the next free
                // "My Mail 2": two "My Mail"s in the launcher can't be told
                // apart.
                let name = match &undone {
                    Some(_) => name,
                    None => {
                        let names: Vec<String> = store
                            .apps(login)
                            .iter()
                            .filter_map(|id| {
                                let latest = *store.versions(login, id).last()?;
                                Some(store.record(login, id, latest)?.name)
                            })
                            .collect();
                        (1..)
                            .map(|n| match n {
                                1 => name.clone(),
                                n => format!("{name} {n}"),
                            })
                            .find(|candidate| !names.contains(candidate))
                            .unwrap_or(name)
                    }
                };
                let app = match undone {
                    Some(app) => app,
                    None => id_for(&name, &store.apps(login))?,
                };
                let based_on = Base {
                    id: manifest.id.clone(),
                    name: manifest.name.clone(),
                    version: manifest.version.clone(),
                };
                let base_actions = manifest.actions.clone();
                let from = partial.then_some((manifest, files));
                (app, 1, Some(based_on), base_actions, name, from)
            }
            _ => {
                return Err(invalid(
                    "name either app (a new version of an app of yours) or base (an app to remix)",
                ));
            }
        };
        let name = match &input.app {
            Some(_) => input
                .manifest
                .as_ref()
                .and_then(|m| m.name.clone())
                .unwrap_or(default_name),
            None => default_name,
        };
        let (declares, files) = match (input.manifest, input.files, from) {
            (Some(declares), Some(files), _) => (declares, files),
            (declares, files, Some((from_manifest, from_files))) => (
                declares.unwrap_or_else(|| Declares::of(&from_manifest)),
                files.unwrap_or(from_files),
            ),
            (_, _, None) => unreachable!("read when the input leaves something out"),
        };
        let files = match (input.edits.is_empty(), files) {
            (true, files) => files,
            (false, files) => edited(files, &input.edits)?,
        };
        Ok(Plan {
            manifest: manifest(&app, &name, version, declares),
            app,
            name,
            version,
            base,
            base_actions,
            need: None,
            declined: Vec::new(),
            files,
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
        // An app meets the need when it has two of its words, or its only
        // one: a word in common ("drafts", "shows") is chance, and refusing
        // for it sent the agent back to the person over Settings and Chat.
        let enough = relevance(&input.need, &input.need).clamp(1, 2);
        let missed: Vec<String> = offered
            .iter()
            .map(|(id, name)| (id, format!("{name} ({id}) in your organization's catalog")))
            .chain(
                remixable
                    .iter()
                    .filter(|b| b.score >= enough && b.remixable)
                    .map(|b| (&b.id, format!("{} ({}), which you can remix", b.name, b.id))),
            )
            .filter(|(id, _)| !input.declined.contains(id))
            .map(|(_, text)| text)
            .collect();
        if !missed.is_empty() {
            return Err(invalid(format!(
                "Something already meets this need: {}. Suggest it first. If the person \
                 turns it down, or already asked for an app of their own rather than a change \
                 to it, name it in declined.",
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
        // Tried as it opens and as each button is pressed (ADR 0049); what
        // went wrong is the agent's to fix, and the version is kept anyway.
        let tried = self.try_version(login, &manifest, bundler, &dist);
        if let Some(tried) = &tried {
            found
                .notes
                .extend(tried.problems.iter().map(|p| format!("when tried: {p}")));
        }
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
        for note in found
            .notes
            .iter()
            .filter(|n| !n.starts_with("when tried: "))
        {
            text.push_str(&format!(" Note: {note}."));
        }
        if let Some(tried) = &tried {
            text.push_str("\n\n");
            text.push_str(&tried.text);
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
                "tried": tried.as_ref().map(|t| json!({ "problems": t.problems })),
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

impl Declares {
    /// What `manifest` declares, its name left to the save.
    fn of(manifest: &AppManifest) -> Self {
        Self {
            name: None,
            description: Some(manifest.description.clone()),
            actions: manifest.actions.clone(),
            permissions: manifest.permissions.clone(),
            labels: manifest.labels.clone(),
        }
    }
}

/// `files` with `edits` made, in order; an edit that doesn't fit exactly
/// says why, so the agent reads the file again rather than guessing.
fn edited(
    mut files: BTreeMap<String, String>,
    edits: &[Edit],
) -> Result<BTreeMap<String, String>, ActionError> {
    for (n, edit) in edits.iter().enumerate() {
        let which = format!("edit {} ({})", n + 1, edit.path);
        match files.get_mut(&edit.path) {
            None if edit.old.is_empty() => {
                files.insert(edit.path.clone(), edit.new.clone());
            }
            None => {
                return Err(invalid(format!(
                    "{which}: there's no such file; an empty old makes a new one"
                )));
            }
            Some(_) if edit.old.is_empty() => {
                return Err(invalid(format!(
                    "{which}: the file is there; old is the text to replace"
                )));
            }
            Some(text) => match text.matches(edit.old.as_str()).count() {
                1 => *text = text.replacen(edit.old.as_str(), &edit.new, 1),
                0 => {
                    return Err(invalid(format!(
                        "{which}: old isn't in the file; read it again with apps.source and its path"
                    )));
                }
                times => {
                    return Err(invalid(format!(
                        "{which}: old is in the file {times} times; include more of the text around it"
                    )));
                }
            },
        }
    }
    Ok(files)
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
impl StudioProvider {
    /// Tries a built version (ADR 0049): what it showed and did with sample
    /// data, and its problems. None when nothing tries apps here.
    fn try_version(
        &self,
        login: &str,
        manifest: &AppManifest,
        bundler: &Bundler,
        dist: &bundle::Dist,
    ) -> Option<tried::Tried> {
        let capabilities = self.studio.surroundings().ok()?.capabilities(login);
        let today =
            Timestamp::now().date_at(glass_platform::system::utc_offset_secs().unwrap_or(0));
        let permissions = manifest
            .permissions
            .iter()
            .filter_map(|p| serde_json::to_value(p).ok()?.as_str().map(str::to_owned))
            .collect();
        let setup = tried::setup(
            manifest.id.as_str(),
            &manifest.name,
            login,
            permissions,
            &manifest.actions,
            &capabilities,
            &today,
        );
        Some(match bundler.try_out(dist, &setup)? {
            Ok(report) => tried::tell(&report, &manifest.actions, &capabilities),
            Err(why) => tried::Tried {
                text: format!("It couldn't be tried here: {why}."),
                problems: Vec::new(),
            },
        })
    }
}

/// The most actions `apps.actions` shows in full at once.
const MAX_DETAILED: usize = 12;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionsInput {
    #[serde(default)]
    need: Option<String>,
    #[serde(default)]
    actions: Vec<ActionName>,
}

/// How an action runs when an app calls it, in the person's words.
fn runs(tier: Tier) -> &'static str {
    match tier {
        Tier::Auto => "runs without asking",
        Tier::Notify => "runs and tells the person",
        Tier::Ask => "asks the person first, each time",
        Tier::Never => "never runs",
    }
}

fn does(side_effect: SideEffect) -> &'static str {
    match side_effect {
        SideEffect::None => "reads",
        SideEffect::LocalReversible => "changes things on this computer, undoably",
        SideEffect::Irreversible => "can't be undone (it sends, posts or deletes)",
        SideEffect::Disclose => "sends what it's given to an AI model, as policy routes it",
    }
}

impl StudioProvider {
    /// `apps.actions` (GA-1): what an app the person makes may call.
    fn capabilities(
        &self,
        ctx: &TaskContext,
        input: ActionsInput,
    ) -> Result<ActionOutput, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        let mut all = self.studio.surroundings()?.capabilities(login);
        if input.actions.len() > MAX_DETAILED {
            return Err(invalid(format!(
                "ask for at most {MAX_DETAILED} actions at once"
            )));
        }
        if !input.actions.is_empty() {
            let mut text = Vec::new();
            let mut data = Vec::new();
            for name in &input.actions {
                let Some(c) = all.iter().find(|c| &c.name == name) else {
                    text.push(format!(
                        "{name}: not something an app of yours can call here (apps.actions lists what is)"
                    ));
                    continue;
                };
                let takes = glass_actions::shape::typescript(&c.input);
                let gives = c.returns.as_ref().map_or_else(
                    || {
                        "not described yet: call it once with your own tool, if you have it, to see"
                            .to_owned()
                    },
                    glass_actions::shape::typescript,
                );
                text.push(format!(
                    "{} ({}): {}. It {}; in an app it {}.\nconst result = await bridge.call(\"{}\", input), where input is:\n{takes}\nresult.ok is false with result.text saying why when it's refused or fails; otherwise result.data is:\n{gives}",
                    c.name, c.title, c.description.trim_end_matches('.'), does(c.side_effect), runs(c.tier), c.name
                ));
                data.push(json!({
                    "name": c.name, "title": c.title, "description": c.description,
                    "side_effect": c.side_effect, "tier": c.tier,
                    "input": c.input, "returns": c.returns,
                }));
            }
            return Ok(ActionOutput {
                text: text.join("\n\n"),
                data: json!({ "actions": data }),
                changes: Vec::new(),
            });
        }
        if let Some(need) = input.need.as_deref().filter(|n| !n.trim().is_empty()) {
            // The closest first, as apps.bases ranks; the rest after, by name.
            let score = |c: &Capability| {
                relevance(
                    need,
                    &format!(
                        "{} {} {}",
                        c.name.as_str().replace(['.', '_'], " "),
                        c.title,
                        c.description
                    ),
                )
            };
            all.sort_by(|a, b| {
                score(b)
                    .cmp(&score(a))
                    .then(a.name.as_str().cmp(b.name.as_str()))
            });
        }
        let lines: Vec<String> = all
            .iter()
            .map(|c| {
                format!(
                    "{}: {} ({}; {}{})",
                    c.name,
                    c.title,
                    does(c.side_effect),
                    runs(c.tier),
                    if c.returns.is_some() {
                        ""
                    } else {
                        "; its data isn't described yet"
                    }
                )
            })
            .collect();
        Ok(ActionOutput {
            text: format!(
                "An app you make may call these, each declared in its manifest's actions. Ask \
                 for some by name (actions) to see what they take and what their data is.\n{}",
                lines.join("\n")
            ),
            data: json!({ "actions": all.iter().map(|c| json!({
                "name": c.name, "title": c.title, "side_effect": c.side_effect, "tier": c.tier,
                "described": c.returns.is_some(),
            })).collect::<Vec<_>>() }),
            changes: Vec::new(),
        })
    }
}

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
            let sdk = &self.studio.inner.paths.source.join("sdk");
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
            let label = format!("{} version {version}", manifest.name);
            if let Some(path) = &input.path {
                return one_file(&app, &label, &files, path);
            }
            return Ok(ActionOutput {
                text: format!(
                    "{label}: {}. Save a new version with apps.save {{\"app\": \"{app}\"}}.",
                    listing(&files)
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
        let label = format!("{} {}", manifest.name, manifest.version);
        if let Some(path) = &input.path {
            return one_file(&app, &label, &files, path);
        }
        Ok(ActionOutput {
            text: format!(
                "{label}: {}. Change them and save your version with apps.save {{\"base\": \"{app}\"}}.",
                listing(&files)
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

impl StudioProvider {
    /// What sharing `input` would send: the submission and who it's for.
    fn submission(&self, ctx: &TaskContext, input: &ShareInput) -> Result<Submission, ActionError> {
        let login = ctx.principal.on_behalf_of.name.as_str();
        let Some((_, device)) = self.studio.inner.share.get() else {
            return Err(ActionError::Failed(
                "this computer isn't managed by an organization, so there is no store to share to"
                    .into(),
            ));
        };
        let version = *store::chosen(&ctx.home)
            .get(&input.app)
            .ok_or_else(|| invalid(format!("{} isn't an app of yours", input.app)))?;
        let store = self.studio.store();
        let (Some(manifest), Some(record)) = (
            store.manifest(login, &input.app, version),
            store.record(login, &input.app, version),
        ) else {
            return Err(invalid(format!("{} has no version {version}", input.app)));
        };
        let files = store
            .source(login, &input.app, version)
            .map_err(|e| ActionError::io("its source", e))?;
        let share = match input.to {
            Audience::Org => ShareTo::Org,
            Audience::Team => {
                let group = input
                    .team
                    .clone()
                    .ok_or_else(|| invalid("name the team (one of your groups) to share with"))?;
                if !ctx.groups.contains(&group) {
                    return Err(invalid(format!(
                        "{group} isn't one of your groups; share with your own team or the whole organization"
                    )));
                }
                ShareTo::Team { group }
            }
        };
        if input.note.trim().is_empty() {
            return Err(invalid(
                "say in a sentence what it's for: reviewers and colleagues read it",
            ));
        }
        let mut manifest = manifest;
        if let Some(name) = input.name.as_deref().map(str::trim) {
            manifest.name = name.to_owned();
            manifest
                .check()
                .map_err(|why| invalid(format!("that name won't do: {why}")))?;
        }
        Ok(Submission {
            app: input.app.clone(),
            name: manifest.name.clone(),
            named: input.name.is_some(),
            version,
            maker: login.to_owned(),
            device: device.clone(),
            manifest,
            files,
            based_on: record.based_on.map(|b| SharedBase {
                id: b.id,
                version: b.version,
            }),
            note: input.note.trim().to_owned(),
            share,
            notes: record.notes,
        })
    }

    fn share(&self, ctx: &TaskContext, input: ShareInput) -> Result<ActionOutput, ActionError> {
        let submission = self.submission(ctx, &input)?;
        let Some((link, _)) = self.studio.inner.share.get() else {
            return Err(ActionError::Failed("there is no store to share to".into()));
        };
        let submitted = link.submit(&submission).map_err(ActionError::Failed)?;
        Ok(ActionOutput {
            text: submitted.text.clone(),
            data: json!({
                "app": submission.app,
                "version": submission.version,
                "id": submitted.id,
                "to": submission.share,
            }),
            changes: Vec::new(),
        })
    }
}

fn audience(share: &ShareTo) -> String {
    match share {
        ShareTo::Team { group } => format!("the {group} team"),
        ShareTo::Org => "everyone in your organization".into(),
    }
}

/// An app's files by path and length, for an agent choosing what to read.
fn listing(files: &BTreeMap<String, String>) -> String {
    let each: Vec<String> = files
        .iter()
        .map(|(path, text)| format!("{path} ({} lines)", text.lines().count()))
        .collect();
    format!("{} files: {}", files.len(), each.join(", "))
}

/// One of an app's files: the result's text is the file, exactly, for an
/// agent to copy an edit's `old` from.
fn one_file(
    app: &AgentId,
    label: &str,
    files: &BTreeMap<String, String>,
    path: &str,
) -> Result<ActionOutput, ActionError> {
    let Some(text) = files.get(path) else {
        return Err(invalid(format!(
            "{label} has no {path}; it has {}",
            files.keys().cloned().collect::<Vec<_>>().join(", ")
        )));
    };
    Ok(ActionOutput {
        text: text.clone(),
        data: json!({ "app": app, "of": label, "path": path, "lines": text.lines().count() }),
        changes: Vec::new(),
    })
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
        texts: Vec::new(),
        reads: Vec::new(),
        snapshot_roots: file.parent().map(Path::to_path_buf).into_iter().collect(),
        writes: vec![file],
        label: None,
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
                ACTIONS,
                "What an app can do",
                "The actions an app this person makes may call, with how each runs (without \
                 asking, telling them, or asking them first). With actions, each one's full \
                 detail: what it takes and, as TypeScript, what its result's data is. Read it \
                 before writing an app's calls: the app gets exactly that data.",
                SideEffect::None,
                Tier::Auto,
                json!({
                    "type": "object",
                    "properties": {
                        "need": { "type": "string", "description": "What the app is for: the closest actions first." },
                        "actions": { "type": "array", "items": { "type": "string" }, "maxItems": MAX_DETAILED, "description": "Actions to show in full, such as calendar.list." }
                    },
                    "additionalProperties": false
                }),
            ),
            def(
                SOURCE,
                "An app's source",
                "An app's TypeScript and CSS source and what its manifest declares, to change \
                 and save as the person's own version; or, with template, the starting point \
                 for a new app. Its text lists the files; with path, the result is that one \
                 file's text, exactly, to make edits from.",
                SideEffect::None,
                Tier::Auto,
                json!({
                    "type": "object",
                    "properties": {
                        "app": app,
                        "version": { "type": "integer", "minimum": 1 },
                        "template": { "type": "boolean" },
                        "path": { "type": "string", "description": "One file, such as src/main.ts." }
                    },
                    "additionalProperties": false
                }),
            ),
            def(
                SAVE,
                "Save an app of your own",
                "Saves a remix of an installed app (base) or a new version of an app the person \
                 made (app), from the whole source (files) or from edits to what it starts \
                 from; manifest is what that declares unless sent. glassd checks the source and \
                 what it declares, bundles it and keeps every version; the person can go back \
                 to any. Refusals say what to fix. apps.actions says what each action an app \
                 calls takes and returns.",
                SideEffect::LocalReversible,
                Tier::Notify,
                json!({
                    "type": "object",
                    "properties": {
                        "app": app,
                        "base": app,
                        "manifest": manifest_schema(),
                        "files": files_schema(),
                        "edits": {
                            "type": "array",
                            "description": "Changes to the source it starts from, in order, instead of files.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": { "type": "string", "description": "Such as src/main.ts." },
                                    "old": { "type": "string", "description": "Text in the file, exactly as apps.source gave it, found once. Empty makes a new file." },
                                    "new": { "type": "string", "description": "What it becomes." }
                                },
                                "required": ["path", "new"],
                                "additionalProperties": false
                            }
                        },
                        "note": { "type": "string", "description": "What changed, in a sentence." }
                    },
                    "required": ["note"],
                    "additionalProperties": false
                }),
            ),
            def(
                CREATE,
                "Make a new app",
                "Makes a new app from the template (apps.source with template) when nothing in \
                 the catalog or apps.bases meets the need. Refused while something does, unless \
                 the person turned it down (declined). Before writing its calls, apps.actions \
                 says what each action takes and what its result's data is, exactly.",
                SideEffect::LocalReversible,
                Tier::Ask,
                json!({
                    "type": "object",
                    "properties": {
                        "need": { "type": "string", "description": "What the app is for, as searched in the catalog." },
                        "declined": { "type": "array", "items": { "type": "string" }, "description": "Ids of what the person turned down for this: what they said not to use or change (\"a new app, not a change to Calendar\" turns down calendar)." },
                        "manifest": manifest_schema(),
                        "files": files_schema(),
                        "note": { "type": "string" }
                    },
                    "required": ["need", "manifest", "files", "note"],
                    "additionalProperties": false
                }),
            ),
            def(
                SHARE,
                "Share an app through the store",
                "Sends one of the person's own apps, at the version they use, to their \
                 organization's store for review, for their team (one of their groups) or \
                 everyone. IT sees what changed and what it can do; once approved, colleagues \
                 install it from the Store. Nothing reaches anyone without that review.",
                SideEffect::Irreversible,
                Tier::Ask,
                json!({
                    "type": "object",
                    "properties": {
                        "app": app,
                        "to": { "enum": ["team", "org"] },
                        "team": { "type": "string", "description": "One of the person's groups, for to: team." },
                        "name": { "type": "string", "description": "What colleagues see it as in the Store, for what it does (\"Mail, with Write\"); its own name by default. Leave it out for a new version of an app already in the Store: it keeps the name it has there." },
                        "note": { "type": "string", "description": "What it's for, in a sentence." }
                    },
                    "required": ["app", "to", "note"],
                    "additionalProperties": false
                }),
            )
            .asking("Review the share", "Share"),
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
        self.allowed()?;
        match action.as_str() {
            BASES | SOURCE | VERSIONS | ACTIONS => Ok(Preview {
                summary: match action.as_str() {
                    BASES => "List apps you can start from".into(),
                    SOURCE => "Read an app's source".into(),
                    ACTIONS => "List what an app can do".into(),
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
            SHARE => {
                let input: ShareInput = parse_input(input)?;
                let submission = self.submission(ctx, &input)?;
                let mut items = vec![
                    format!(
                        "Sends its source to your organization's store, for {}",
                        audience(&submission.share)
                    ),
                    format!("Does: {}", names(&submission.manifest.actions)),
                ];
                if let Some(base) = &submission.based_on {
                    items.push(format!("Based on {} {}", base.id, base.version));
                }
                items.push(format!("Note: {}", submission.note));
                Ok(Preview {
                    summary: format!("Share {} for review", submission.name),
                    targets: vec![submission.name.clone()],
                    items,
                    ..Preview::default()
                })
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
        self.allowed()?;
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
            ACTIONS => self.capabilities(ctx, parse_input(input)?),
            REVERT => self.revert(ctx, parse_input(input)?),
            SHARE => self.share(ctx, parse_input(input)?),
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }
}

#[cfg(test)]
mod tests;
