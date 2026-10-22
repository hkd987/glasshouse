//! The `settings` app's actions (UX-1, ADR 0029): a person's device
//! settings as IT set them for their groups, and the person's own choices
//! for the ones IT left as defaults.
//!
//! [`SettingsHandle`] is shared by glassd, which gives it each policy it
//! applies, the actions here and the desktop's `settings.get`: one place
//! that says what a person's computer is set to, and that tells whoever
//! listens when it changes. A person's choices are kept in glassd's state
//! directory, by uid, never in their home, so nothing they or their agents
//! can write decides what IT locked.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use glass_actions::{ActionDef, ActionError, ActionOutput, ActionProvider, Preview, TaskContext};
use glass_api::AskLabels;
use glass_api::settings::{SettingFrom, SettingView};
use glass_policy::Policy;
use glass_policy::document::SettingValue;
use glass_policy::settings::{self, Scope, SettingDef};
use glass_types::{ActionName, SideEffect, Tier};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const LIST: &str = "settings.list";
pub const SET: &str = "settings.set";
/// Which AI models service the computer's owner connected, never the key.
pub const MODELS: &str = "settings.models";
/// The owner connects their own models service with its key (ADR 0039).
/// The person's own, in Settings alone: never an agent's.
pub const MODELS_CONNECT: &str = "settings.models_connect";
/// The owner lets an agent ask their AI models, or not (ADR 0039). The
/// person's own, in Settings alone: never an agent's.
pub const MODELS_AGENTS: &str = "settings.models_agents";
/// The Settings app: the one place the computer's own settings change.
pub const APP: &str = "settings";

/// The services an owner can connect: id, name, and how a key starts.
pub const MODEL_SERVICES: [(&str, &str, &str); 2] = [
    ("openrouter", "OpenRouter", "sk-or-"),
    ("anthropic", "Anthropic", "sk-ant-"),
];

/// Where a computer's owner keeps the AI models they connected (ADR 0039):
/// glassd, root only. It never hands the key back.
pub trait ModelKeys: Send + Sync {
    /// The service connected, if any: `openrouter` or `anthropic`.
    fn connected(&self) -> Option<String>;
    /// Connects `service` with `key`, or forgets it with `None`.
    fn connect(&self, choice: Option<(&str, &str)>) -> Result<(), String>;
    /// How setup said agents should reach models, if it was asked.
    fn setup_choice(&self) -> Option<String>;
    /// The agents that may ask the owner's models, and which do.
    fn agents(&self) -> Vec<ModelAgent> {
        Vec::new()
    }
    /// Lets `agent` ask the owner's models, or stops it.
    fn allow(&self, agent: &str, on: bool) -> Result<(), String> {
        let _ = (agent, on);
        Err("This computer can't choose which agents use its AI models.".into())
    }
}

/// An agent, as Settings lists it under AI models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelAgent {
    pub id: String,
    pub name: String,
    /// It may ask the owner's models.
    pub on: bool,
    /// Policy lets it ask models whatever the owner chooses here.
    pub always: bool,
}

/// A setting as someone's computer applies it.
#[derive(Debug, Clone, Serialize)]
pub struct Effective {
    #[serde(flatten)]
    pub def: &'static SettingDef,
    pub value: SettingValue,
    /// IT locked it: the person can't change it.
    pub locked: bool,
    pub from: SettingFrom,
    /// The person may change it: one of their own IT left to them, or one
    /// of the computer's own on a computer they own (ADR 0038).
    pub editable: bool,
}

impl Effective {
    pub fn view(&self) -> SettingView {
        SettingView {
            id: self.def.id.to_owned(),
            value: serde_json::to_value(&self.value).unwrap_or(Value::Null),
            locked: self.locked,
            from: self.from,
            editable: self.editable,
        }
    }
}

type Listener = Box<dyn Fn(Option<u32>) + Send + Sync>;
type AppNames = Box<dyn Fn(u32) -> BTreeMap<String, String> + Send + Sync>;

struct Inner {
    /// Each person's choices: `<uid>.json`.
    dir: PathBuf,
    policy: RwLock<Option<Arc<Policy>>>,
    listeners: Mutex<Vec<Listener>>,
    /// The names of a person's apps, by id, for settings that list apps.
    app_names: RwLock<Option<AppNames>>,
    /// The computer's owner (ADR 0038), on a computer no organization
    /// manages: they set its own settings, kept in `computer.json`.
    owner: RwLock<Option<String>>,
    /// Where the owner's AI models key goes (ADR 0039).
    model_keys: RwLock<Option<Arc<dyn ModelKeys>>>,
}

/// Device settings on this computer: policy's, and each person's choices.
#[derive(Clone)]
pub struct SettingsHandle(Arc<Inner>);

impl std::fmt::Debug for SettingsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsHandle")
            .field("dir", &self.0.dir)
            .finish_non_exhaustive()
    }
}

fn group_settings(
    policy: Option<&Policy>,
) -> Vec<(String, BTreeMap<String, glass_policy::document::Managed>)> {
    policy
        .map(|p| {
            p.document()
                .groups
                .iter()
                .map(|g| (g.name.clone(), g.settings.clone()))
                .collect()
        })
        .unwrap_or_default()
}

impl SettingsHandle {
    /// Choices kept in `dir`, which glassd owns.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self(Arc::new(Inner {
            dir: dir.into(),
            policy: RwLock::new(None),
            listeners: Mutex::new(Vec::new()),
            app_names: RwLock::new(None),
            owner: RwLock::new(None),
            model_keys: RwLock::new(None),
        }))
    }

    /// Where the owner's AI models key is kept (glassd's, ADR 0039).
    pub fn set_model_keys(&self, keys: Arc<dyn ModelKeys>) {
        *self
            .0
            .model_keys
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(keys);
    }

    fn model_keys(&self) -> Option<Arc<dyn ModelKeys>> {
        self.0
            .model_keys
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// What Settings shows about AI models for `user` (ADR 0039).
    pub fn models_view(&self, user: &str) -> Value {
        let keys = self.model_keys();
        let owner = self.owner();
        json!({
            "connected": keys.as_ref().and_then(|k| k.connected()),
            "setup_choice": keys.as_ref().and_then(|k| k.setup_choice()),
            "managed": owner.is_none(),
            "editable": keys.is_some() && owner.as_deref() == Some(user),
            "services": MODEL_SERVICES.iter().map(|(id, name, _)| json!({ "id": id, "name": name })).collect::<Vec<_>>(),
            "agents": keys.as_ref().filter(|_| owner.is_some()).map(|k| k.agents()).unwrap_or_default(),
        })
    }

    /// The owner's model keys, if `user` is this computer's owner; else why
    /// not, in words.
    fn owners_keys(&self, user: &str) -> Result<Arc<dyn ModelKeys>, ActionError> {
        let Some(keys) = self.model_keys() else {
            return Err(ActionError::Failed(
                "This computer can't keep a models key.".into(),
            ));
        };
        match self.owner() {
            None => Err(ActionError::Failed(
                "Your organization sets how agents reach AI models.".into(),
            )),
            Some(owner) if owner != user => Err(ActionError::Failed(format!(
                "Only this computer's owner, {owner}, connects AI models."
            ))),
            Some(_) => Ok(keys),
        }
    }

    /// The owner lets `agent` ask their AI models, or stops it (ADR 0039).
    pub fn allow_agent(&self, user: &str, agent: &str, on: bool) -> Result<String, ActionError> {
        let keys = self.owners_keys(user)?;
        let Some(listed) = keys.agents().into_iter().find(|a| a.id == agent) else {
            return Err(ActionError::InvalidInput(format!(
                "no agent {agent:?} on this computer"
            )));
        };
        if listed.always {
            return Err(ActionError::Failed(format!(
                "{} always asks AI models: it's what it does.",
                listed.name
            )));
        }
        keys.allow(agent, on).map_err(ActionError::Failed)?;
        Ok(if on {
            format!("{} may use your AI models now.", listed.name)
        } else {
            format!("{} no longer uses your AI models.", listed.name)
        })
    }

    /// The owner connects `service` with `key`, or forgets theirs (ADR
    /// 0039). Refused, in words, to anyone else, on a managed computer,
    /// and for a key that isn't the service's.
    pub fn connect_models(
        &self,
        user: &str,
        choice: Option<(&str, &str)>,
    ) -> Result<String, ActionError> {
        let keys = self.owners_keys(user)?;
        let Some((service, key)) = choice else {
            keys.connect(None).map_err(ActionError::Failed)?;
            return Ok("AI models are disconnected; agents can't reach any now.".into());
        };
        let Some((_, name, prefix)) = MODEL_SERVICES.iter().find(|(id, _, _)| *id == service)
        else {
            return Err(ActionError::InvalidInput(format!("no service {service:?}")));
        };
        let key = key.trim();
        if !key.starts_with(prefix)
            || key.len() > 512
            || key.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(ActionError::InvalidInput(format!(
                "That isn't an {name} key: they start with {prefix}."
            )));
        }
        // A Claude sign-in token, not an API key: never ours to keep (ADR
        // 0042, ADR 0043).
        if key.starts_with("sk-ant-oat") {
            return Err(ActionError::InvalidInput(
                "That's a Claude sign-in token, not an API key. Create an API key in your \
                 Anthropic account and use that."
                    .into(),
            ));
        }
        keys.connect(Some((service, key)))
            .map_err(ActionError::Failed)?;
        Ok(format!(
            "Agents on this computer reach AI models through your {name} account."
        ))
    }

    /// Who owns this computer (ADR 0038): the person who set up a computer
    /// no organization manages, who changes its own settings as IT would
    /// on a managed one. `None` on a managed computer: IT does.
    pub fn set_owner(&self, owner: Option<String>) {
        let changed = {
            let mut slot = self.0.owner.write().unwrap_or_else(PoisonError::into_inner);
            let changed = *slot != owner;
            *slot = owner;
            changed
        };
        if changed {
            self.notify(None);
        }
    }

    pub fn owner(&self) -> Option<String> {
        self.0
            .owner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn computer_path(&self) -> PathBuf {
        self.0.dir.join("computer.json")
    }

    /// The owner's choices for the computer's own settings, those still
    /// valid; none on a computer with no owner, whatever the file says.
    fn computer_choices(&self) -> BTreeMap<String, SettingValue> {
        if self.owner().is_none() {
            return BTreeMap::new();
        }
        let Ok(text) = fs::read_to_string(self.computer_path()) else {
            return BTreeMap::new();
        };
        let all: BTreeMap<String, SettingValue> = serde_json::from_str(&text).unwrap_or_default();
        all.into_iter()
            .filter(|(id, value)| {
                settings::def(id)
                    .is_some_and(|d| d.scope != Scope::Person && settings::check(d, value).is_ok())
            })
            .collect()
    }

    /// Whether people may remix apps here (TL-6): unless IT, or the
    /// computer's owner, turned `apps.remix` off.
    pub fn remixing(&self) -> bool {
        !self
            .computer(None)
            .iter()
            .any(|r| r.id == "apps.remix" && r.value == SettingValue::Bool(false))
    }

    /// Where the names of a person's apps come from (by uid), so a setting
    /// that lists apps by id shows them as the person knows them.
    pub fn name_apps(&self, f: impl Fn(u32) -> BTreeMap<String, String> + Send + Sync + 'static) {
        *self
            .0
            .app_names
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(f));
    }

    /// The names of the apps `settings` list (DK-1: those hidden from
    /// search), as the person's computer offers them.
    fn app_names(&self, uid: u32, settings: &[Effective]) -> BTreeMap<String, String> {
        let listed: Vec<&String> = settings
            .iter()
            .filter(|e| e.def.kind == settings::Kind::Apps)
            .flat_map(|e| match &e.value {
                SettingValue::List(ids) => ids.iter().collect(),
                _ => Vec::new(),
            })
            .collect();
        if listed.is_empty() {
            return BTreeMap::new();
        }
        let names = self
            .0
            .app_names
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|f| f(uid))
            .unwrap_or_default();
        names
            .into_iter()
            .filter(|(id, _)| listed.contains(&id))
            .collect()
    }

    /// The policy in force. Tells listeners when it changes anyone's
    /// settings.
    pub fn set_policy(&self, policy: &Policy) {
        let before = {
            let mut slot = self
                .0
                .policy
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let before = group_settings(slot.as_deref());
            *slot = Some(Arc::new(policy.clone()));
            before
        };
        if before != group_settings(Some(policy)) {
            self.notify(None);
        }
    }

    /// Calls `f` with the uid whose settings changed, or `None` for
    /// everyone's.
    pub fn on_change(&self, f: impl Fn(Option<u32>) + Send + Sync + 'static) {
        self.0
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Box::new(f));
    }

    fn notify(&self, uid: Option<u32>) {
        for listener in self
            .0
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            listener(uid);
        }
    }

    fn policy(&self) -> Option<Arc<Policy>> {
        self.0
            .policy
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn choices_path(&self, uid: u32) -> PathBuf {
        self.0.dir.join(format!("{uid}.json"))
    }

    /// `uid`'s own choices, those still valid.
    fn choices(&self, uid: u32) -> BTreeMap<String, SettingValue> {
        let Ok(text) = fs::read_to_string(self.choices_path(uid)) else {
            return BTreeMap::new();
        };
        let all: BTreeMap<String, SettingValue> = serde_json::from_str(&text).unwrap_or_default();
        all.into_iter()
            .filter(|(id, value)| {
                settings::def(id)
                    .is_some_and(|d| d.scope == Scope::Person && settings::check(d, value).is_ok())
            })
            .collect()
    }

    /// What `user`'s computer is set to: IT's locked settings, then their
    /// own choices (the owner's, for the computer's own settings), then
    /// IT's defaults, then the computer's.
    pub fn person(&self, user: &str, uid: u32) -> Vec<Effective> {
        let policy = self.policy();
        let choices = self.choices(uid);
        let computer = self.computer_choices();
        let owns = self.owner().as_deref() == Some(user);
        settings::table()
            .iter()
            .map(|def| {
                let theirs = def.scope == Scope::Person || owns;
                let it = policy
                    .as_ref()
                    .and_then(|p| p.settings_for(user).into_iter().find(|r| r.id == def.id));
                // Only IT turns one of these on (ADR 0045): where it
                // didn't, it's off and the person can't change that.
                if settings::narrow_only(def.id)
                    && it.as_ref().map_or(&def.default, |r| &r.value) != &SettingValue::Bool(true)
                {
                    return Effective {
                        def,
                        value: SettingValue::Bool(false),
                        locked: false,
                        from: if it
                            .as_ref()
                            .is_some_and(|r| r.source != settings::Source::Default)
                        {
                            SettingFrom::It
                        } else {
                            SettingFrom::Default
                        },
                        editable: false,
                    };
                }
                match it {
                    Some(r) if r.locked => Effective {
                        def,
                        value: r.value,
                        locked: true,
                        from: SettingFrom::It,
                        editable: false,
                    },
                    _ if def.scope != Scope::Person && computer.contains_key(def.id) => Effective {
                        def,
                        value: computer[def.id].clone(),
                        locked: false,
                        from: if owns {
                            SettingFrom::You
                        } else {
                            SettingFrom::Owner
                        },
                        editable: owns,
                    },
                    _ if choices.contains_key(def.id) => Effective {
                        def,
                        value: choices[def.id].clone(),
                        locked: false,
                        from: SettingFrom::You,
                        editable: true,
                    },
                    Some(r) if r.source != settings::Source::Default => Effective {
                        def,
                        value: r.value,
                        locked: false,
                        from: SettingFrom::It,
                        editable: theirs,
                    },
                    _ => Effective {
                        def,
                        value: def.default.clone(),
                        locked: false,
                        from: SettingFrom::Default,
                        editable: theirs,
                    },
                }
            })
            .collect()
    }

    /// Whether `user`'s toggle `id` is on, as [`Self::person`] resolves it.
    pub fn on(&self, user: &str, uid: u32, id: &str) -> bool {
        self.person(user, uid)
            .into_iter()
            .any(|e| e.def.id == id && e.value == SettingValue::Bool(true))
    }

    /// The computer's own settings, such as the browser's, as policy sets
    /// them with `seat` signed in at the screen, or nobody (ADR 0031), and
    /// as its owner chose where policy didn't lock them (ADR 0038).
    pub fn computer(&self, seat: Option<&str>) -> Vec<settings::Resolved> {
        let mut all = self
            .policy()
            .map(|p| p.computer_settings(seat))
            .unwrap_or_else(|| {
                settings::table()
                    .iter()
                    .filter(|d| d.scope != Scope::Person)
                    .map(|d| settings::Resolved {
                        id: d.id,
                        value: d.default.clone(),
                        locked: false,
                        source: settings::Source::Default,
                    })
                    .collect()
            });
        // The owner's choice is enforced for whoever is at the screen, as
        // IT's locked ones are: the browser gets it as managed, and only
        // the owner changes it, in Settings.
        let chosen = self.computer_choices();
        for r in all.iter_mut().filter(|r| !r.locked) {
            if let Some(value) = chosen.get(r.id) {
                r.value = value.clone();
                r.locked = true;
                r.source = settings::Source::Owner;
            }
        }
        all
    }

    /// Sets `user`'s own choice for `id`, or takes it back with `None`.
    /// Refused, in words, for a setting IT locked, or one of the computer's
    /// own unless `user` owns the computer (ADR 0038).
    pub fn choose(
        &self,
        user: &str,
        uid: u32,
        id: &str,
        value: Option<SettingValue>,
    ) -> Result<Effective, ActionError> {
        let def = settings::def(id)
            .ok_or_else(|| ActionError::InvalidInput(format!("no setting {id}")))?;
        if def.scope != Scope::Person {
            return self.choose_for_computer(user, uid, def, value);
        }
        let now = self.person(user, uid);
        let current = now.iter().find(|e| e.def.id == id).expect("in the table");
        if current.locked {
            return Err(ActionError::Failed(format!(
                "IT locked {} at {}; only IT can change it.",
                def.title,
                settings::describe(def, &current.value)
            )));
        }
        if !current.editable && settings::narrow_only(id) {
            return Err(ActionError::Failed(format!(
                "{} are off where IT hasn't turned them on; only IT turns them on.",
                def.title
            )));
        }
        if let Some(v) = &value {
            settings::check(def, v)
                .map_err(|e| ActionError::InvalidInput(format!("{}: {e}", def.title)))?;
        }
        let mut choices = self.choices(uid);
        match value {
            Some(v) => {
                choices.insert(id.to_owned(), v);
            }
            None => {
                choices.remove(id);
            }
        }
        write_private(
            &self.choices_path(uid),
            &serde_json::to_vec_pretty(&choices).unwrap_or_default(),
        )
        .map_err(|e| ActionError::io("saving your settings", e))?;
        self.notify(Some(uid));
        Ok(self
            .person(user, uid)
            .into_iter()
            .find(|e| e.def.id == id)
            .expect("in the table"))
    }
}

impl SettingsHandle {
    /// The owner's choice for one of the computer's own settings (ADR
    /// 0038): it applies to whoever uses the computer.
    fn choose_for_computer(
        &self,
        user: &str,
        uid: u32,
        def: &'static SettingDef,
        value: Option<SettingValue>,
    ) -> Result<Effective, ActionError> {
        match self.owner() {
            None => {
                return Err(ActionError::Failed(format!(
                    "{} is the computer's setting; IT sets it in the admin console.",
                    def.title
                )));
            }
            Some(owner) if owner != user => {
                return Err(ActionError::Failed(format!(
                    "{} is this computer's own setting; its owner, {owner}, changes it in Settings.",
                    def.title
                )));
            }
            Some(_) => {}
        }
        let current = |handle: &Self| {
            handle
                .person(user, uid)
                .into_iter()
                .find(|e| e.def.id == def.id)
                .expect("in the table")
        };
        let now = current(self);
        if now.locked {
            return Err(ActionError::Failed(format!(
                "{} is locked at {} by this computer's policy.",
                def.title,
                settings::describe(def, &now.value)
            )));
        }
        if let Some(v) = &value {
            settings::check(def, v)
                .map_err(|e| ActionError::InvalidInput(format!("{}: {e}", def.title)))?;
        }
        let mut choices = self.computer_choices();
        match value {
            Some(v) => {
                choices.insert(def.id.to_owned(), v);
            }
            None => {
                choices.remove(def.id);
            }
        }
        write_private(
            &self.computer_path(),
            &serde_json::to_vec_pretty(&choices).unwrap_or_default(),
        )
        .map_err(|e| ActionError::io("saving the computer's settings", e))?;
        // Everyone's: the computer applies it for whoever is at the screen.
        self.notify(None);
        Ok(current(self))
    }
}

/// Writes `bytes` to `path` whole, readable by glassd only.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.new",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode_private()
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

trait PrivateMode {
    fn mode_private(&mut self) -> &mut Self;
}

impl PrivateMode for fs::OpenOptions {
    fn mode_private(&mut self) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, 0o600)
    }
}

#[derive(Debug, Clone)]
pub struct SettingsProvider {
    handle: SettingsHandle,
}

impl SettingsProvider {
    pub fn new(handle: SettingsHandle) -> Self {
        Self { handle }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectInput {
    service: Option<String>,
    #[serde(default)]
    key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentsInput {
    agent: String,
    on: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetInput {
    id: String,
    /// The new value; leave it out to go back to IT's default.
    #[serde(default)]
    value: Option<SettingValue>,
}

impl ActionProvider for SettingsProvider {
    fn actions(&self) -> Vec<ActionDef> {
        vec![
            ActionDef {
                name: LIST.parse().expect("static name"),
                title: "Read this computer's settings".into(),
                description:
                    "Every device setting for the person: its value, whether IT locked it, \
                              and whether it is IT's, the person's own choice or the default."
                        .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: SET.parse().expect("static name"),
                title: "Change a setting".into(),
                description: "Change one of the person's own settings, such as the 24-hour clock. \
                              Settings IT locked, and the computer's, such as the browser's, \
                              can't be changed here. Leave out value to go back to IT's default."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "The setting's id from settings.list, such as \"clock.hour24\"" },
                        "value": { "description": "true or false, a number, or text, as the setting takes" }
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: MODELS.parse().expect("static name"),
                title: "Read how agents reach AI models".into(),
                description: "Which AI models service the computer's owner connected, if any. \
                              Never the key."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: MODELS_CONNECT.parse().expect("static name"),
                title: "Connect AI models".into(),
                description: "The computer's owner connects their own OpenRouter or Anthropic \
                              account, with its key, in Settings. Never an agent."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "service": { "type": ["string", "null"], "enum": ["openrouter", "anthropic", null] },
                        "key": { "type": "string" }
                    },
                    "required": ["service"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: MODELS_AGENTS.parse().expect("static name"),
                title: "Choose which agents use your AI models".into(),
                description: "The computer's owner lets an agent ask the AI models they \
                              connected, or stops it, in Settings. Never an agent."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "agent": { "type": "string" },
                        "on": { "type": "boolean" }
                    },
                    "required": ["agent", "on"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
        ]
    }

    fn preview(
        &self,
        _ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        match action.as_str() {
            LIST => {
                let _: NoInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: "Read this computer's settings".into(),
                    targets: vec!["Settings".into()],
                    ..Preview::default()
                })
            }
            SET => {
                let input: SetInput = glass_actions::parse_input(input)?;
                let def = settings::def(&input.id)
                    .ok_or_else(|| ActionError::InvalidInput(format!("no setting {}", input.id)))?;
                Ok(Preview {
                    summary: match &input.value {
                        Some(v) => format!("Set {} to {}", def.title, settings::describe(def, v)),
                        None => format!("Put {} back to IT's default", def.title),
                    },
                    targets: vec![format!("Settings: {}", def.title)],
                    ..Preview::default()
                })
            }
            MODELS => {
                let _: NoInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: "Read how agents reach AI models".into(),
                    targets: vec!["Settings: AI models".into()],
                    ..Preview::default()
                })
            }
            MODELS_AGENTS => {
                let input: AgentsInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: if input.on {
                        format!("Let {} use your AI models", input.agent)
                    } else {
                        format!("Stop {} using your AI models", input.agent)
                    },
                    targets: vec!["Settings: AI models".into()],
                    ..Preview::default()
                })
            }
            MODELS_CONNECT => {
                // Never the key: the summary and targets are audited.
                let input: ConnectInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: match &input.service {
                        Some(service) => format!("Connect {service} for AI models"),
                        None => "Disconnect AI models".into(),
                    },
                    targets: vec!["Settings: AI models".into()],
                    ..Preview::default()
                })
            }
            other => Err(ActionError::InvalidInput(format!(
                "settings has no action {other}"
            ))),
        }
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        let who = &ctx.principal.on_behalf_of;
        match action.as_str() {
            LIST => {
                let _: NoInput = glass_actions::parse_input(input)?;
                let all = self.handle.person(&who.name, who.uid);
                let locked = all.iter().filter(|e| e.locked).count();
                Ok(ActionOutput {
                    text: format!("{} settings; IT locked {locked}.", all.len()),
                    data: json!({
                        "settings": all,
                        "device": self.handle.computer(Some(&who.name)),
                        "app_names": self.handle.app_names(who.uid, &all),
                    }),
                    changes: Vec::new(),
                })
            }
            SET => {
                let input: SetInput = glass_actions::parse_input(input)?;
                // The computer's own settings change only in Settings, by
                // its owner's hand: never an agent's call (ADR 0038).
                if let Some(def) = settings::def(&input.id)
                    && def.scope != Scope::Person
                    && ctx.principal.agent.as_str() != APP
                {
                    return Err(ActionError::Failed(format!(
                        "{} is this computer's own setting: its owner changes it in Settings, never an agent.",
                        def.title
                    )));
                }
                let now = self
                    .handle
                    .choose(&who.name, who.uid, &input.id, input.value)?;
                let text = match &now.value {
                    // Apps hidden from search (DK-1), by their names.
                    SettingValue::List(ids) if now.def.kind == settings::Kind::Apps => {
                        let names = self.handle.app_names(who.uid, std::slice::from_ref(&now));
                        if ids.is_empty() {
                            "Every app shows in search.".to_owned()
                        } else {
                            let named: Vec<&str> = ids
                                .iter()
                                .map(|id| names.get(id).unwrap_or(id).as_str())
                                .collect();
                            format!("Hidden from search: {}.", named.join(", "))
                        }
                    }
                    _ => format!(
                        "{} is {}.",
                        now.def.title,
                        settings::describe(now.def, &now.value)
                    ),
                };
                Ok(ActionOutput {
                    text,
                    data: json!({ "setting": now }),
                    changes: Vec::new(),
                })
            }
            MODELS => {
                let _: NoInput = glass_actions::parse_input(input)?;
                let view = self.handle.models_view(&who.name);
                Ok(ActionOutput {
                    text: match view["connected"].as_str() {
                        Some(service) => format!("AI models: {service}."),
                        None => "No AI models are connected.".into(),
                    },
                    data: view,
                    changes: Vec::new(),
                })
            }
            MODELS_AGENTS => {
                // As connecting: the owner's own, in Settings, never an agent.
                if ctx.principal.agent.as_str() != APP {
                    return Err(ActionError::Failed(
                        "Only the computer's owner chooses which agents use its AI models, in \
                         Settings; never an agent."
                            .into(),
                    ));
                }
                let input: AgentsInput = glass_actions::parse_input(input)?;
                let text = self.handle.allow_agent(&who.name, &input.agent, input.on)?;
                Ok(ActionOutput {
                    text,
                    data: self.handle.models_view(&who.name),
                    changes: Vec::new(),
                })
            }
            MODELS_CONNECT => {
                // The person's own, in Settings: policy makes it Never for
                // every agent, and anything but Settings is refused here too.
                if ctx.principal.agent.as_str() != APP {
                    return Err(ActionError::Failed(
                        "Only the computer's owner connects AI models, in Settings; never an agent."
                            .into(),
                    ));
                }
                let input: ConnectInput = glass_actions::parse_input(input)?;
                let choice = match (&input.service, &input.key) {
                    (Some(service), Some(key)) => Some((service.as_str(), key.as_str())),
                    (Some(_), None) => {
                        return Err(ActionError::InvalidInput("the key is missing".into()));
                    }
                    (None, _) => None,
                };
                let text = self.handle.connect_models(&who.name, choice)?;
                Ok(ActionOutput {
                    text,
                    data: self.handle.models_view(&who.name),
                    changes: Vec::new(),
                })
            }
            other => Err(ActionError::InvalidInput(format!(
                "settings has no action {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(extra: &str) -> Policy {
        Policy::from_toml(&format!("{}\n{extra}", glass_policy::DEFAULT_POLICY)).unwrap()
    }

    const AP: &str = r#"
[[group]]
name = "ap-ops"
members = ["dana"]
[group.settings]
"lock.after_minutes" = { value = 5, locked = true }
"clock.hour24" = { value = true }
"#;

    #[test]
    fn adr0045_only_it_turns_meeting_transcripts_on_and_people_may_turn_theirs_off() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        const ID: &str = "meetings.transcripts";
        // Nobody set it: off, and the person can't turn it on.
        handle.set_policy(&policy(""));
        assert!(!handle.on("dana", 1001, ID));
        let refused = handle
            .choose("dana", 1001, ID, Some(SettingValue::Bool(true)))
            .unwrap_err()
            .to_string();
        assert!(refused.contains("only IT turns them on"), "{refused}");
        assert!(!handle.on("dana", 1001, ID));

        // IT turns it on for Dana's group, unlocked: she may turn hers off,
        // and on again; Sam's group didn't, so his stays off.
        handle.set_policy(&policy(
            "[[group]]\nname = \"sales\"\nmembers = [\"dana\"]\n\
             [group.settings]\n\"meetings.transcripts\" = { value = true }\n",
        ));
        assert!(handle.on("dana", 1001, ID));
        assert!(!handle.on("sam", 1002, ID));
        handle
            .choose("dana", 1001, ID, Some(SettingValue::Bool(false)))
            .unwrap();
        assert!(!handle.on("dana", 1001, ID));
        handle
            .choose("dana", 1001, ID, Some(SettingValue::Bool(true)))
            .unwrap();
        assert!(handle.on("dana", 1001, ID));
        assert!(
            handle
                .choose("sam", 1002, ID, Some(SettingValue::Bool(true)))
                .is_err()
        );
    }

    #[test]
    fn ux1_locked_settings_win_then_the_persons_choice_then_its_default() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        let heard = Arc::new(Mutex::new(Vec::new()));
        let h = Arc::clone(&heard);
        handle.on_change(move |uid| h.lock().unwrap().push(uid));
        handle.set_policy(&policy(AP));
        let get = |id: &str| {
            handle
                .person("dana", 1001)
                .into_iter()
                .find(|e| e.def.id == id)
                .unwrap()
        };
        assert_eq!(get("clock.hour24").from, SettingFrom::It);
        let mine = handle
            .choose(
                "dana",
                1001,
                "clock.hour24",
                Some(SettingValue::Bool(false)),
            )
            .unwrap();
        assert_eq!(
            (mine.value, mine.from),
            (SettingValue::Bool(false), SettingFrom::You)
        );

        let refused = handle
            .choose(
                "dana",
                1001,
                "lock.after_minutes",
                Some(SettingValue::Number(60)),
            )
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("IT locked Lock after at 5 minutes"),
            "{refused}"
        );
        let computer = handle
            .choose(
                "dana",
                1001,
                "browser.camera",
                Some(SettingValue::Bool(false)),
            )
            .unwrap_err()
            .to_string();
        assert!(computer.contains("the computer's setting"), "{computer}");
        assert!(
            handle
                .choose(
                    "dana",
                    1001,
                    "lock.after_minutes",
                    Some(SettingValue::Bool(true))
                )
                .is_err()
        );

        // Someone else's choices are their own.
        assert_eq!(
            handle
                .person("sam", 1002)
                .into_iter()
                .find(|e| e.def.id == "clock.hour24")
                .unwrap()
                .from,
            SettingFrom::Default
        );
        // Back to IT's default.
        handle.choose("dana", 1001, "clock.hour24", None).unwrap();
        assert_eq!(get("clock.hour24").value, SettingValue::Bool(true));
        assert_eq!(*heard.lock().unwrap(), [None, Some(1001), Some(1001)]);

        // IT locks what Dana had chosen: IT's value applies.
        handle
            .choose(
                "dana",
                1001,
                "clock.hour24",
                Some(SettingValue::Bool(false)),
            )
            .unwrap();
        let locked = AP.replace(
            "\"clock.hour24\" = { value = true }",
            "\"clock.hour24\" = { value = true, locked = true }",
        );
        handle.set_policy(&policy(&locked));
        assert_eq!(get("clock.hour24").value, SettingValue::Bool(true));
        assert!(get("clock.hour24").locked);
        // The same policy again tells nobody.
        let before = heard.lock().unwrap().len();
        handle.set_policy(&policy(&locked));
        assert_eq!(heard.lock().unwrap().len(), before);
    }

    #[test]
    fn ux1_choices_are_kept_by_uid_readable_only_by_glassd() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path().join("settings"));
        handle
            .choose("sam", 1002, "clock.hour24", Some(SettingValue::Bool(true)))
            .unwrap();
        let path = dir.path().join("settings/1002.json");
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions());
        assert_eq!(mode & 0o077, 0);
        // A choice the table no longer allows is ignored, not applied.
        fs::write(&path, r#"{"clock.hour24": 7, "browser.camera": false}"#).unwrap();
        let all = handle.person("sam", 1002);
        assert!(all.iter().all(|e| e.from == SettingFrom::Default));
    }

    #[test]
    fn dk1_apps_hidden_from_search_come_with_their_names_and_no_others() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        let names = |uid: u32| -> BTreeMap<String, String> {
            assert_eq!(uid, 1001);
            [
                ("mail", "Mail"),
                ("my-mail-3", "My Mail"),
                ("notes", "Notes"),
            ]
            .into_iter()
            .map(|(id, name)| (id.to_owned(), name.to_owned()))
            .collect()
        };
        // Nothing hidden: no names asked for.
        assert!(
            handle
                .app_names(1001, &handle.person("dana", 1001))
                .is_empty()
        );
        handle.name_apps(names);
        assert!(
            handle
                .app_names(1001, &handle.person("dana", 1001))
                .is_empty()
        );
        handle
            .choose(
                "dana",
                1001,
                "search.hidden_apps",
                Some(SettingValue::List(vec!["my-mail-3".into(), "gone".into()])),
            )
            .unwrap();
        let named = handle.app_names(1001, &handle.person("dana", 1001));
        assert_eq!(
            named,
            BTreeMap::from([("my-mail-3".to_owned(), "My Mail".to_owned())])
        );
    }

    fn ctx(login: &str, uid: u32, app: &str) -> TaskContext {
        let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
        ctx.principal.on_behalf_of.name = login.into();
        ctx.principal.on_behalf_of.uid = uid;
        ctx.principal.agent = app.parse().unwrap();
        ctx
    }

    #[test]
    fn adr0038_the_owner_of_an_unmanaged_computer_sets_its_own_settings_for_everyone() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        handle.set_policy(&policy(""));
        let heard = Arc::new(Mutex::new(Vec::new()));
        let h = Arc::clone(&heard);
        handle.on_change(move |uid| h.lock().unwrap().push(uid));
        let get = |user: &str, uid, id: &str| {
            handle
                .person(user, uid)
                .into_iter()
                .find(|e| e.def.id == id)
                .unwrap()
        };
        let usb = |handle: &SettingsHandle, seat| {
            handle
                .computer(seat)
                .into_iter()
                .find(|r| r.id == "devices.usb_storage")
                .unwrap()
        };

        // Managed (no owner): the computer's settings are IT's alone.
        assert!(!get("dana", 1001, "devices.usb_storage").editable);
        let it = handle
            .choose(
                "dana",
                1001,
                "devices.usb_storage",
                Some(SettingValue::Bool(false)),
            )
            .unwrap_err()
            .to_string();
        assert!(it.contains("IT sets it"), "{it}");

        // Dana owns it: she turns USB drives off and the time zone to Lisbon.
        handle.set_owner(Some("dana".into()));
        assert!(get("dana", 1001, "devices.usb_storage").editable);
        assert!(!get("sam", 1002, "devices.usb_storage").editable);
        let off = handle
            .choose(
                "dana",
                1001,
                "devices.usb_storage",
                Some(SettingValue::Bool(false)),
            )
            .unwrap();
        assert_eq!(
            (off.value, off.from),
            (SettingValue::Bool(false), SettingFrom::You)
        );
        handle
            .choose(
                "dana",
                1001,
                "region.time_zone",
                Some(SettingValue::Text("Europe/Lisbon".into())),
            )
            .unwrap();
        assert_eq!(
            heard.lock().unwrap().last(),
            Some(&None),
            "everyone's changed"
        );
        // It applies whoever is at the screen, and Sam sees whose it is.
        for seat in [None, Some("dana"), Some("sam")] {
            let r = usb(&handle, seat);
            assert_eq!(
                (r.value, r.locked, r.source),
                (SettingValue::Bool(false), true, settings::Source::Owner)
            );
        }
        let sams = get("sam", 1002, "devices.usb_storage");
        assert_eq!(
            (sams.value, sams.from),
            (SettingValue::Bool(false), SettingFrom::Owner)
        );
        // person() and computer() agree on every one of the computer's own.
        for def in settings::table()
            .iter()
            .filter(|d| d.scope != Scope::Person)
        {
            let seen = get("sam", 1002, def.id).value;
            let applied = handle
                .computer(Some("sam"))
                .into_iter()
                .find(|r| r.id == def.id)
                .unwrap()
                .value;
            assert_eq!(seen, applied, "{}", def.id);
        }

        // Sam isn't the owner; nor is a person's own file a way in.
        let sam = handle
            .choose(
                "sam",
                1002,
                "devices.usb_storage",
                Some(SettingValue::Bool(true)),
            )
            .unwrap_err()
            .to_string();
        assert!(sam.contains("its owner, dana"), "{sam}");
        fs::write(
            dir.path().join("1002.json"),
            r#"{"devices.usb_storage": true}"#,
        )
        .unwrap();
        assert_eq!(usb(&handle, Some("sam")).value, SettingValue::Bool(false));

        // An agent working for Dana can't, even with settings.set; the
        // Settings app can. Her own settings stay hers to change anywhere.
        let provider = SettingsProvider::new(handle.clone());
        let set = SET.parse().unwrap();
        let input = json!({ "id": "devices.usb_storage", "value": true });
        let agent = provider
            .invoke(&ctx("dana", 1001, "claude-code"), &set, &input)
            .unwrap_err()
            .to_string();
        assert!(agent.contains("never an agent"), "{agent}");
        assert_eq!(usb(&handle, None).value, SettingValue::Bool(false));
        provider
            .invoke(&ctx("dana", 1001, APP), &set, &input)
            .unwrap();
        assert_eq!(usb(&handle, None).value, SettingValue::Bool(true));
        provider
            .invoke(
                &ctx("dana", 1001, "claude-code"),
                &set,
                &json!({ "id": "clock.hour24", "value": true }),
            )
            .unwrap();

        // Remixing follows the owner's choice; back to the default with None.
        assert!(handle.remixing());
        handle
            .choose("dana", 1001, "apps.remix", Some(SettingValue::Bool(false)))
            .unwrap();
        assert!(!handle.remixing());
        handle.choose("dana", 1001, "apps.remix", None).unwrap();
        assert!(handle.remixing());

        // Enrolled later: the owner's choices stop applying, and IT's rule.
        handle.set_owner(None);
        assert_eq!(usb(&handle, None).value, SettingValue::Bool(true));
        assert_eq!(usb(&handle, None).source, settings::Source::Default);
    }

    #[test]
    fn adr0038_a_setting_policy_locks_stays_locked_for_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        handle.set_policy(&policy(
            "[[group]]\nname = \"family\"\nmembers = [\"dana\"]\n[group.settings]\n\"devices.usb_storage\" = { value = false, locked = true }\n",
        ));
        handle.set_owner(Some("dana".into()));
        let locked = handle
            .choose(
                "dana",
                1001,
                "devices.usb_storage",
                Some(SettingValue::Bool(true)),
            )
            .unwrap_err()
            .to_string();
        assert!(locked.contains("locked"), "{locked}");
    }

    /// A key store as glassd's, in memory: it remembers the key so the
    /// test can check it went there and nowhere else.
    #[derive(Default)]
    struct Keys(Mutex<Option<(String, String)>>);

    impl ModelKeys for Keys {
        fn connected(&self) -> Option<String> {
            self.0.lock().unwrap().as_ref().map(|(s, _)| s.clone())
        }
        fn connect(&self, choice: Option<(&str, &str)>) -> Result<(), String> {
            *self.0.lock().unwrap() = choice.map(|(s, k)| (s.to_owned(), k.to_owned()));
            Ok(())
        }
        fn setup_choice(&self) -> Option<String> {
            Some("api-key".into())
        }
    }

    #[test]
    fn adr0039_the_owner_connects_their_own_models_in_settings_and_no_agent_can() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        let keys = Arc::new(Keys::default());
        handle.set_model_keys(keys.clone());
        let provider = SettingsProvider::new(handle.clone());
        let connect = MODELS_CONNECT.parse().unwrap();
        let openrouter = json!({ "service": "openrouter", "key": "sk-or-v1-own-key-not-real" });

        // A managed computer: IT decides.
        let it = provider
            .invoke(&ctx("dana", 1001, APP), &connect, &openrouter)
            .unwrap_err()
            .to_string();
        assert!(it.contains("Your organization"), "{it}");

        handle.set_owner(Some("dana".into()));
        // Sam isn't the owner; an agent never connects, even for Dana.
        let sam = provider
            .invoke(&ctx("sam", 1002, APP), &connect, &openrouter)
            .unwrap_err()
            .to_string();
        assert!(sam.contains("owner, dana"), "{sam}");
        let agent = provider
            .invoke(&ctx("dana", 1001, "claude-code"), &connect, &openrouter)
            .unwrap_err()
            .to_string();
        assert!(agent.contains("never an agent"), "{agent}");
        // A key that isn't the service's.
        let wrong = provider
            .invoke(
                &ctx("dana", 1001, APP),
                &connect,
                &json!({ "service": "anthropic", "key": "sk-or-v1-x" }),
            )
            .unwrap_err()
            .to_string();
        assert!(wrong.contains("start with sk-ant-"), "{wrong}");
        // ADR 0043: a Claude sign-in token is refused, and not kept.
        let token = provider
            .invoke(
                &ctx("dana", 1001, APP),
                &connect,
                &json!({ "service": "anthropic", "key": "sk-ant-oat01-not-real" }),
            )
            .unwrap_err()
            .to_string();
        assert!(token.contains("sign-in token, not an API key"), "{token}");
        assert_eq!(keys.connected(), None);

        // Dana connects hers: kept, and never said back.
        let done = provider
            .invoke(&ctx("dana", 1001, APP), &connect, &openrouter)
            .unwrap();
        assert!(done.text.contains("OpenRouter"), "{}", done.text);
        assert_eq!(
            keys.0.lock().unwrap().as_ref().unwrap().1,
            "sk-or-v1-own-key-not-real"
        );
        let read = provider
            .invoke(
                &ctx("sam", 1002, "claude-code"),
                &MODELS.parse().unwrap(),
                &json!({}),
            )
            .unwrap();
        assert_eq!(read.data["connected"], "openrouter");
        assert_eq!(read.data["editable"], false);
        assert_eq!(read.data["setup_choice"], "api-key");
        let preview = provider
            .preview(&ctx("dana", 1001, APP), &connect, &openrouter)
            .unwrap();
        for shown in [
            done.text,
            done.data.to_string(),
            read.data.to_string(),
            format!("{preview:?}"),
        ] {
            assert!(!shown.contains("own-key"), "{shown}");
        }

        // And disconnects.
        provider
            .invoke(
                &ctx("dana", 1001, APP),
                &connect,
                &json!({ "service": null }),
            )
            .unwrap();
        assert_eq!(keys.connected(), None);
    }

    /// The owner's agent choices, as glassd keeps them.
    struct Chooser(Mutex<Vec<ModelAgent>>);

    impl ModelKeys for Chooser {
        fn connected(&self) -> Option<String> {
            Some("openrouter".into())
        }
        fn connect(&self, _: Option<(&str, &str)>) -> Result<(), String> {
            Ok(())
        }
        fn setup_choice(&self) -> Option<String> {
            None
        }
        fn agents(&self) -> Vec<ModelAgent> {
            self.0.lock().unwrap().clone()
        }
        fn allow(&self, agent: &str, on: bool) -> Result<(), String> {
            for a in self.0.lock().unwrap().iter_mut().filter(|a| a.id == agent) {
                a.on = on;
            }
            Ok(())
        }
    }

    #[test]
    fn adr0039_the_owner_chooses_which_agents_use_their_models_and_no_agent_can() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        let agent = |id: &str, name: &str, on: bool, always: bool| ModelAgent {
            id: id.into(),
            name: name.into(),
            on,
            always,
        };
        let keys = Arc::new(Chooser(Mutex::new(vec![
            agent("it-agent", "IT agent", true, false),
            agent("skill-evals", "Skill evals", true, true),
        ])));
        handle.set_model_keys(keys.clone());
        let provider = SettingsProvider::new(handle.clone());
        let choose = MODELS_AGENTS.parse().unwrap();
        let off = json!({ "agent": "it-agent", "on": false });

        // A managed computer: IT decides, and Settings lists no agents.
        assert!(
            provider
                .invoke(&ctx("dana", 1001, APP), &choose, &off)
                .is_err()
        );
        assert_eq!(handle.models_view("dana")["agents"], json!([]));

        handle.set_owner(Some("dana".into()));
        assert_eq!(handle.models_view("dana")["agents"][0]["on"], true);
        // Not Sam, not an agent, not one policy always lets ask, not one
        // that isn't here.
        let sam = provider
            .invoke(&ctx("sam", 1002, APP), &choose, &off)
            .unwrap_err()
            .to_string();
        assert!(sam.contains("owner, dana"), "{sam}");
        let by_agent = provider
            .invoke(&ctx("dana", 1001, "it-agent"), &choose, &off)
            .unwrap_err()
            .to_string();
        assert!(by_agent.contains("never an agent"), "{by_agent}");
        let always = provider
            .invoke(
                &ctx("dana", 1001, APP),
                &choose,
                &json!({ "agent": "skill-evals", "on": false }),
            )
            .unwrap_err()
            .to_string();
        assert!(always.contains("always asks"), "{always}");
        assert!(
            provider
                .invoke(
                    &ctx("dana", 1001, APP),
                    &choose,
                    &json!({ "agent": "nobody", "on": true })
                )
                .is_err()
        );

        // Dana turns the IT agent off, and on again.
        let done = provider
            .invoke(&ctx("dana", 1001, APP), &choose, &off)
            .unwrap();
        assert_eq!(done.text, "IT agent no longer uses your AI models.");
        assert_eq!(done.data["agents"][0]["on"], false);
        let done = provider
            .invoke(
                &ctx("dana", 1001, APP),
                &choose,
                &json!({ "agent": "it-agent", "on": true }),
            )
            .unwrap();
        assert_eq!(done.text, "IT agent may use your AI models now.");
    }
}
