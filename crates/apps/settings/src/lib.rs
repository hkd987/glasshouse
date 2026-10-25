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

pub mod bluetooth;
pub mod computer;
pub mod wifi;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use glass_actions::{ActionDef, ActionError, ActionOutput, ActionProvider, Preview, TaskContext};
use glass_api::AskLabels;
use glass_api::settings::{Brightness, BrightnessChange, BrightnessStep, SettingFrom, SettingView};
use glass_platform::backlight::Backlight;
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
/// Wi-Fi as Settings shows it (CF-12): the networks nearby and known, the
/// one in use, inside IT's rules. The person's own, in Settings: never an
/// agent's (where the computer is, from the networks around it, is theirs).
pub const WIFI: &str = "settings.wifi";
/// Joins a Wi-Fi network, with its password the first time.
pub const WIFI_JOIN: &str = "settings.wifi_join";
/// Forgets a Wi-Fi network the person joined.
pub const WIFI_FORGET: &str = "settings.wifi_forget";
/// The screen's brightness (ADR 0060).
pub const BRIGHTNESS: &str = "settings.brightness";
/// Sets the screen's brightness, never so dim it's off
/// (`glass_platform::backlight::LEAST`).
pub const BRIGHTNESS_SET: &str = "settings.brightness_set";
/// `text` as a sentence: a capital first, a full stop last.
fn sentence(text: &str) -> String {
    let mut chars = text.trim().chars();
    let mut out: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// Bluetooth devices, paired and nearby (CF-13, ADR 0062). The person's,
/// in Settings.
pub const BLUETOOTH: &str = "settings.bluetooth";
/// Looks for devices nearby: about ten seconds.
pub const BLUETOOTH_SCAN: &str = "settings.bluetooth_scan";
/// Pairs one the last search found. The person's own: a paired keyboard
/// types.
pub const BLUETOOTH_PAIR: &str = "settings.bluetooth_pair";
pub const BLUETOOTH_REMOVE: &str = "settings.bluetooth_remove";
/// Turns Bluetooth on or off, when IT lets it be on.
pub const BLUETOOTH_POWER: &str = "settings.bluetooth_power";
/// This computer as Settings shows it: its version, OS updates and kernel
/// patches (ADR 0057, 0061).
pub const COMPUTER: &str = "settings.computer";
/// And the newest release on its channel, checked.
pub const COMPUTER_CHECK: &str = "settings.computer_check";
/// Installs the newest release: the computer's owner, on a computer nobody
/// manages. The person's own.
pub const UPDATE_INSTALL: &str = "settings.update_install";
/// Takes a kernel patch back before its restart, or out: anyone at the
/// screen, back to the computer as it was. The person's own.
pub const PATCH_REMOVE: &str = "settings.patch_remove";
/// Restarts into the OS update installed, now or tonight, or not tonight
/// after all: anyone at the screen. The person's own.
pub const UPDATE_RESTART: &str = "settings.update_restart";
/// Where the screen's brightness is kept for the next start.
const BRIGHTNESS_FILE: &str = "brightness";

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

/// What a setting holds: on or off, a number, text, or a list of text.
fn value_shape() -> Value {
    json!({ "type": ["boolean", "integer", "string", "array"], "items": { "type": "string" } })
}

/// The shape of an [`Effective`] setting (GA-1, ADR 0049).
fn effective_shape() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "description": "such as \"clock.hour24\"" },
            "section": { "type": "string", "description": "the Settings app's section it is in" },
            "title": { "type": "string" },
            "help": { "type": "string" },
            "kind": {
                "type": "object",
                "description": "what it holds; a number's min, max and unit",
                "properties": {
                    "type": { "enum": ["toggle", "number", "url", "sites", "time_zone", "apps", "folders", "networks"] },
                    "min": { "type": "integer" },
                    "max": { "type": "integer" },
                    "unit": { "type": "string" }
                },
                "required": ["type"],
                "additionalProperties": false
            },
            "scope": { "enum": ["person", "device", "seat"], "description": "each person's own, or the computer's" },
            "default": value_shape(),
            "value": value_shape(),
            "locked": { "type": "boolean", "description": "IT locked it: the person can't change it" },
            "from": { "enum": ["it", "you", "owner", "default"], "description": "who decided it" },
            "editable": { "type": "boolean", "description": "the person may change it" }
        },
        "required": ["id", "section", "title", "help", "kind", "scope", "default", "value", "locked", "from", "editable"],
        "additionalProperties": false
    })
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
    /// The computer's Wi-Fi, as glassd drives it (CF-12).
    wifi: RwLock<Option<Arc<dyn wifi::Radio>>>,
    /// The screen's backlight, if it's dimmed from here (ADR 0060).
    backlight: RwLock<Option<Backlight>>,
    /// The runtime's OS updates and kernel patches (ADR 0057, 0061).
    system: RwLock<Option<Arc<dyn computer::Computer>>>,
    /// The computer's Bluetooth controller (CF-13), and what the last
    /// search for devices found.
    bluetooth: RwLock<Option<bluetooth::Controller>>,
    nearby: Mutex<Vec<bluetooth::Device>>,
    /// Held from a change's check through its write: the desktop's
    /// `settings.set` and the Settings app's can come at once, and each
    /// rewrites the whole file (ADR 0052).
    writing: Mutex<()>,
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

/// What a person's settings are resolved from: each group's name, members
/// and settings, in order. A release that only moves someone into or out
/// of a group changes theirs too (ADR 0052).
#[allow(clippy::type_complexity)]
fn group_settings(
    policy: Option<&Policy>,
) -> Vec<(
    String,
    Vec<String>,
    BTreeMap<String, glass_policy::document::Managed>,
)> {
    policy
        .map(|p| {
            p.document()
                .groups
                .iter()
                .map(|g| (g.name.clone(), g.members.clone(), g.settings.clone()))
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
            wifi: RwLock::new(None),
            backlight: RwLock::new(None),
            system: RwLock::new(None),
            bluetooth: RwLock::new(None),
            nearby: Mutex::new(Vec::new()),
            writing: Mutex::new(()),
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

    /// The computer's Wi-Fi radio (glassd's, CF-12).
    pub fn set_wifi(&self, radio: Arc<dyn wifi::Radio>) {
        *self.0.wifi.write().unwrap_or_else(PoisonError::into_inner) = Some(radio);
    }

    fn wifi(&self) -> Option<Arc<dyn wifi::Radio>> {
        self.0
            .wifi
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The runtime's OS updates and kernel patches (glassd's).
    pub fn set_computer(&self, system: Arc<dyn computer::Computer>) {
        *self
            .0
            .system
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(system);
    }

    fn system(&self) -> Result<Arc<dyn computer::Computer>, ActionError> {
        self.0
            .system
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                ActionError::Failed("This computer's updates aren't set up here.".into())
            })
    }

    /// This computer as `user` sees it in Settings; checked for updates
    /// when `check`.
    fn computer_view(&self, user: &str, check: bool) -> Result<Value, ActionError> {
        let system = self.system()?;
        let owner = self.owner().as_deref() == Some(user);
        let updates = if check && owner {
            system.check()
        } else {
            system.updates()
        }
        .map_err(ActionError::Failed)?;
        let patches = system.patches().map_err(ActionError::Failed)?;
        Ok(computer::view(&updates, &patches, owner))
    }

    /// The screen's backlight (glassd's, ADR 0060), set to the brightness
    /// it had when the computer last ran.
    pub fn set_backlight(&self, backlight: Backlight) -> Result<(), String> {
        let kept = fs::read_to_string(self.0.dir.join(BRIGHTNESS_FILE))
            .ok()
            .and_then(|t| t.trim().parse::<u8>().ok());
        let restored = kept.map(|p| backlight.set(p).map_err(|e| e.to_string()));
        *self
            .0
            .backlight
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(backlight);
        restored.transpose().map(drop)
    }

    fn backlight(&self) -> Option<Backlight> {
        self.0
            .backlight
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The screen's brightness now.
    pub fn brightness(&self) -> Result<Brightness, String> {
        match self.backlight() {
            Some(b) => Ok(Brightness {
                present: true,
                percent: Some(b.percent().map_err(|e| e.to_string())?),
            }),
            None => Ok(Brightness {
                present: false,
                percent: None,
            }),
        }
    }

    /// A step up or down, or a percent, kept for the next start; neither
    /// says what it is.
    pub fn change_brightness(&self, change: &BrightnessChange) -> Result<Brightness, String> {
        let Some(backlight) = self.backlight() else {
            return self.brightness();
        };
        let now = match (change.step, change.percent) {
            (Some(step), _) => backlight.step(step == BrightnessStep::Up),
            (None, Some(percent)) => backlight.set(percent),
            (None, None) => return self.brightness(),
        }
        .map_err(|e| e.to_string())?;
        // Kept as it goes: a computer that loses power keeps the last one.
        let _ = write_private(
            &self.0.dir.join(BRIGHTNESS_FILE),
            format!("{now}\n").as_bytes(),
        );
        Ok(Brightness {
            present: true,
            percent: Some(now),
        })
    }

    /// The computer's Bluetooth controller (glassd's, CF-13).
    pub fn set_bluetooth(&self, controller: bluetooth::Controller) {
        *self
            .0
            .bluetooth
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(controller);
    }

    pub fn has_bluetooth(&self) -> bool {
        self.bluetooth().is_some()
    }

    fn bluetooth(&self) -> Option<bluetooth::Controller> {
        self.0
            .bluetooth
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// What IT, or the computer's owner, decided about Bluetooth.
    pub fn bluetooth_rules(&self) -> bluetooth::Rules {
        let mut rules = bluetooth::Rules::default();
        for r in self.computer(None) {
            match (r.id, r.value) {
                (settings::BLUETOOTH_ENABLED, SettingValue::Bool(b)) => rules.enabled = b,
                (settings::BLUETOOTH_INPUT_ONLY, SettingValue::Bool(b)) => rules.input_only = b,
                _ => {}
            }
        }
        rules
    }

    /// Brings Bluetooth down when the rules say it's off: as glassd starts
    /// and whenever they change. What it changed, in words.
    pub fn bluetooth_reconcile(&self) -> Result<Vec<String>, String> {
        match self.bluetooth() {
            Some(controller) => bluetooth::reconcile(&controller, &self.bluetooth_rules()),
            None => Ok(Vec::new()),
        }
    }

    fn bluetooth_view(&self) -> Result<bluetooth::View, ActionError> {
        let nearby = self
            .0
            .nearby
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        bluetooth::view(self.bluetooth().as_ref(), &self.bluetooth_rules(), &nearby)
            .map_err(ActionError::Failed)
    }

    /// What IT, or the computer's owner, decided about Wi-Fi.
    pub fn wifi_rules(&self) -> wifi::Rules {
        let mut rules = wifi::Rules::default();
        for r in self.computer(None) {
            match (r.id, r.value) {
                (settings::WIFI_NETWORKS, SettingValue::List(l)) => rules.organization = l,
                (settings::WIFI_BLOCKED, SettingValue::List(l)) => rules.blocked = l,
                (settings::WIFI_ONLY_ORGANIZATION, SettingValue::Bool(b)) => {
                    rules.only_organization = b;
                }
                _ => {}
            }
        }
        rules
    }

    /// Makes the radio's networks what the rules say: as glassd starts and
    /// whenever they change. What it changed, in words.
    pub fn wifi_reconcile(&self) -> Result<Vec<String>, String> {
        match self.wifi() {
            Some(radio) => wifi::reconcile(radio.as_ref(), &self.wifi_rules()),
            None => Ok(Vec::new()),
        }
    }

    fn wifi_view(&self) -> Result<wifi::View, ActionError> {
        let radio = self.wifi().ok_or_else(no_wifi)?;
        wifi::view(radio.as_ref(), &self.wifi_rules()).map_err(ActionError::Failed)
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

    /// `user`'s list `id` (folders, sites, apps), as [`Self::person`]
    /// resolves it; empty for one that isn't a list.
    pub fn list(&self, user: &str, uid: u32, id: &str) -> Vec<String> {
        self.person(user, uid)
            .into_iter()
            .find(|e| e.def.id == id)
            .and_then(|e| match e.value {
                SettingValue::List(list) => Some(list),
                _ => None,
            })
            .unwrap_or_default()
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
        let writing = self
            .0
            .writing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
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
        drop(writing);
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
        let writing = self
            .0
            .writing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
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
        drop(writing);
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

fn no_wifi() -> ActionError {
    ActionError::Failed("This computer has no Wi-Fi.".into())
}

fn no_bluetooth() -> ActionError {
    ActionError::Failed("This computer has no Bluetooth.".into())
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
struct RestartInput {
    when: glass_api::RestartWhen,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinInput {
    network: String,
    #[serde(default)]
    password: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgetInput {
    network: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceInput {
    address: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PowerInput {
    on: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrightnessInput {
    percent: u8,
}

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
            ActionDef {
                name: WIFI.parse().expect("static name"),
                title: "See Wi-Fi networks".into(),
                description: "The Wi-Fi networks nearby and known, the one in use, and which \
                              the computer may join. The person's own, in Settings: never an \
                              agent."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: WIFI_JOIN.parse().expect("static name"),
                title: "Join a Wi-Fi network".into(),
                description: "Joins a network, with its password the first time. The person's \
                              own, in Settings: never an agent."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "network": { "type": "string" },
                        "password": { "type": "string" }
                    },
                    "required": ["network"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: WIFI_FORGET.parse().expect("static name"),
                title: "Forget a Wi-Fi network".into(),
                description: "Forgets a network the person joined. The person's own, in \
                              Settings: never an agent."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "network": { "type": "string" } },
                    "required": ["network"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: BLUETOOTH.parse().expect("static name"),
                title: "See Bluetooth devices".into(),
                description: "The Bluetooth devices paired, and those the last search found. \
                              The person's own, in Settings: never an agent."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: BLUETOOTH_SCAN.parse().expect("static name"),
                title: "Look for Bluetooth devices".into(),
                description: "Looks for devices nearby, for about ten seconds. The person's \
                              own, in Settings: never an agent."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: BLUETOOTH_PAIR.parse().expect("static name"),
                title: "Pair a Bluetooth device".into(),
                description: "Pairs a device the last search found; the person types the PIN \
                              it gives on the device. The person's own, in Settings: never an \
                              agent."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "address": { "type": "string" } },
                    "required": ["address"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: BLUETOOTH_REMOVE.parse().expect("static name"),
                title: "Remove a Bluetooth device".into(),
                description: "Forgets a paired device. The person's own, in Settings: never an \
                              agent."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "address": { "type": "string" } },
                    "required": ["address"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: BLUETOOTH_POWER.parse().expect("static name"),
                title: "Turn Bluetooth on or off".into(),
                description: "Turns Bluetooth on or off, when IT lets it be on. The person's \
                              own, in Settings: never an agent."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "on": { "type": "boolean" } },
                    "required": ["on"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: COMPUTER.parse().expect("static name"),
                title: "See this computer's version and updates".into(),
                description: "The version this computer runs, its OS update, and the kernel \
                              patches it tries or keeps. The person's, in Settings."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: COMPUTER_CHECK.parse().expect("static name"),
                title: "Check for OS updates".into(),
                description: "The newest release on this computer's channel, checked. The \
                              computer's owner, in Settings."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: UPDATE_INSTALL.parse().expect("static name"),
                title: "Install an OS update".into(),
                description: "Installs the newest release; the next restart uses it, and goes \
                              back if it doesn't start. The computer's owner, in Settings; never \
                              an agent."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: UPDATE_RESTART.parse().expect("static name"),
                title: "Restart into the OS update".into(),
                description: "Restarts the computer into the OS update installed, now or at 3 \
                              in the morning, or takes tonight back. Anyone at the screen, in \
                              Settings; never an agent."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "when": { "enum": ["now", "tonight", "cancel"] } },
                    "required": ["when"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: PATCH_REMOVE.parse().expect("static name"),
                title: "Take a kernel patch out".into(),
                description: "Takes back the kernel patch waiting for a restart, or takes out \
                              the one the computer runs: the next restart is as before it. The \
                              person's, in Settings; never an agent."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: BRIGHTNESS.parse().expect("static name"),
                title: "See the screen's brightness".into(),
                description: "How bright the screen is, in percent, and whether it's dimmed \
                              from here."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: BRIGHTNESS_SET.parse().expect("static name"),
                title: "Set the screen's brightness".into(),
                description: "Sets the screen's brightness, in percent: never below 5, so the \
                              screen never goes dark. The person's, in Settings."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "percent": { "type": "integer", "minimum": 0, "maximum": 100 } },
                    "required": ["percent"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Never,
                ask: AskLabels::default(),
            },
        ]
    }

    fn returns(&self, action: &ActionName) -> Option<Value> {
        Some(match action.as_str() {
            LIST => json!({
                "type": "object",
                "properties": {
                    "settings": { "type": "array", "description": "every setting, as it applies to the person", "items": effective_shape() },
                    "device": {
                        "type": "array",
                        "description": "the computer's own settings, as they apply to it",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "value": value_shape(),
                                "locked": { "type": "boolean", "description": "the person can't change it" },
                                "source": {
                                    "type": "object",
                                    "description": "who set it: one of their groups (named in group), everyone's, the computer's owner, or nobody",
                                    "properties": {
                                        "from": { "enum": ["group", "everyone", "owner", "default"] },
                                        "group": { "type": "string" }
                                    },
                                    "required": ["from"],
                                    "additionalProperties": false
                                }
                            },
                            "required": ["id", "value", "locked", "source"],
                            "additionalProperties": false
                        }
                    },
                    "app_names": {
                        "type": "object",
                        "description": "by app id, the names of apps settings list, such as those hidden from search"
                    }
                },
                "required": ["settings", "device", "app_names"],
                "additionalProperties": false
            }),
            SET => json!({
                "type": "object",
                "properties": { "setting": effective_shape() },
                "required": ["setting"],
                "additionalProperties": false
            }),
            MODELS | MODELS_CONNECT | MODELS_AGENTS => json!({
                "type": "object",
                "properties": {
                    "connected": { "type": ["string", "null"], "description": "the service agents reach AI models through, \"openrouter\" or \"anthropic\"; null when none is connected" },
                    "setup_choice": { "type": ["string", "null"], "description": "how setup said agents should reach AI models; null when it wasn't asked" },
                    "managed": { "type": "boolean", "description": "an organization sets how agents reach AI models" },
                    "editable": { "type": "boolean", "description": "the person is this computer's owner and may connect a service" },
                    "services": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": { "id": { "type": "string" }, "name": { "type": "string" } },
                            "required": ["id", "name"],
                            "additionalProperties": false
                        }
                    },
                    "agents": {
                        "type": "array",
                        "description": "on a computer someone owns, the agents that may ask its owner's models",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "name": { "type": "string" },
                                "on": { "type": "boolean", "description": "it may ask the owner's models" },
                                "always": { "type": "boolean", "description": "policy lets it ask models whatever the owner chooses" }
                            },
                            "required": ["id", "name", "on", "always"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["connected", "setup_choice", "managed", "editable", "services", "agents"],
                "additionalProperties": false
            }),
            BLUETOOTH | BLUETOOTH_SCAN | BLUETOOTH_PAIR | BLUETOOTH_REMOVE | BLUETOOTH_POWER => {
                json!({
                    "type": "object",
                    "properties": {
                        "present": { "type": "boolean", "description": "the computer has Bluetooth" },
                        "on": { "type": "boolean", "description": "devices connect" },
                        "allowed": { "type": "boolean", "description": "IT (or the owner) lets it be on" },
                        "input_only": { "type": "boolean", "description": "only keyboards, mice and other input devices may be paired" },
                        "pin": { "type": "string", "description": "after pairing: what the person types on the device" },
                        "devices": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "address": { "type": "string" },
                                    "name": { "type": "string" },
                                    "kind": { "enum": ["keyboard", "mouse", "keyboard_and_mouse", "game_controller", "input", "audio", "phone", "computer", "other"] },
                                    "label": { "type": "string" },
                                    "paired": { "type": "boolean" },
                                    "connected": { "type": "boolean" },
                                    "blocked": { "type": "string", "description": "why it can't be paired here" }
                                },
                                "required": ["address", "name", "kind", "label", "paired", "connected"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["present", "on", "allowed", "input_only", "devices"],
                    "additionalProperties": false
                })
            }
            COMPUTER | COMPUTER_CHECK | UPDATE_INSTALL | UPDATE_RESTART | PATCH_REMOVE => json!({
                "type": "object",
                "properties": {
                    "restart_at": { "type": ["string", "null"], "description": "when the computer restarts into the update installed" },
                    "version": { "type": ["string", "null"], "description": "the version this computer runs" },
                    "channel": { "type": "string" },
                    "owner": { "type": "boolean", "description": "the person may install updates" },
                    "update": { "type": ["object", "null"], "description": "the update being installed" },
                    "last_update": { "type": ["object", "null"] },
                    "available": { "type": ["object", "null"], "description": "a newer release, from a check" },
                    "checked": { "type": ["string", "null"], "description": "why a check found none" },
                    "patch": { "type": ["object", "null"], "description": "the kernel patch being tried" },
                    "kept": { "type": "array", "items": { "type": "object" } }
                },
                "required": ["version", "channel", "owner", "kept"]
            }),
            BRIGHTNESS | BRIGHTNESS_SET => json!({
                "type": "object",
                "properties": {
                    "present": { "type": "boolean", "description": "the screen is dimmed from here" },
                    "percent": { "type": ["integer", "null"], "description": "how bright it is" }
                },
                "required": ["present", "percent"],
                "additionalProperties": false
            }),
            WIFI | WIFI_JOIN | WIFI_FORGET => json!({
                "type": "object",
                "properties": {
                    "present": { "type": "boolean", "description": "the computer has Wi-Fi" },
                    "state": { "type": "string", "description": "the radio's state: COMPLETED once joined, SCANNING, ASSOCIATING, DISCONNECTED" },
                    "joined": { "type": ["string", "null"], "description": "the network in use" },
                    "address": { "type": ["string", "null"], "description": "the address it gave" },
                    "only_organization": { "type": "boolean", "description": "only the organization's networks may be joined" },
                    "networks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "signal": { "type": "integer", "description": "dBm, when nearby: -30 next to it, -80 barely there" },
                                "security": { "enum": ["open", "personal", "enterprise"] },
                                "joined": { "type": "boolean" },
                                "known": { "type": "boolean", "description": "the computer joins it by itself" },
                                "failed": { "type": "boolean", "description": "joining it failed, as a wrong password does" },
                                "organization": { "type": "boolean" },
                                "blocked": { "type": "string", "description": "why it can't be joined here" }
                            },
                            "required": ["name", "joined", "known", "failed", "organization"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["present", "state", "joined", "address", "only_organization", "networks"],
                "additionalProperties": false
            }),
            _ => return None,
        })
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
            UPDATE_RESTART => {
                let input: RestartInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: match input.when {
                        glass_api::RestartWhen::Now => "Restart into the OS update now",
                        glass_api::RestartWhen::Tonight => "Restart into the OS update tonight",
                        glass_api::RestartWhen::Cancel => "Don't restart tonight",
                    }
                    .into(),
                    targets: vec!["Settings: This computer".into()],
                    ..Preview::default()
                })
            }
            COMPUTER | COMPUTER_CHECK | UPDATE_INSTALL | PATCH_REMOVE => {
                let _: NoInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: match action.as_str() {
                        COMPUTER => "See this computer's version and updates",
                        COMPUTER_CHECK => "Check for OS updates",
                        UPDATE_INSTALL => "Install the OS update",
                        _ => "Take the kernel patch out",
                    }
                    .into(),
                    targets: vec!["Settings: This computer".into()],
                    ..Preview::default()
                })
            }
            BRIGHTNESS => {
                let _: NoInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: "See the screen's brightness".into(),
                    targets: vec!["Settings: Display & sleep".into()],
                    ..Preview::default()
                })
            }
            BRIGHTNESS_SET => {
                let input: BrightnessInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: format!("Set the screen's brightness to {}%", input.percent),
                    targets: vec!["Settings: Display & sleep".into()],
                    ..Preview::default()
                })
            }
            BLUETOOTH | BLUETOOTH_SCAN => {
                let _: NoInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: if action.as_str() == BLUETOOTH {
                        "See Bluetooth devices"
                    } else {
                        "Look for Bluetooth devices"
                    }
                    .into(),
                    targets: vec!["Settings: Bluetooth".into()],
                    ..Preview::default()
                })
            }
            BLUETOOTH_PAIR | BLUETOOTH_REMOVE => {
                let input: DeviceInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: format!(
                        "{} the Bluetooth device {}",
                        if action.as_str() == BLUETOOTH_PAIR {
                            "Pair"
                        } else {
                            "Remove"
                        },
                        input.address
                    ),
                    targets: vec![format!("Bluetooth: {}", input.address)],
                    ..Preview::default()
                })
            }
            BLUETOOTH_POWER => {
                let input: PowerInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: format!("Turn Bluetooth {}", if input.on { "on" } else { "off" }),
                    targets: vec!["Settings: Bluetooth".into()],
                    ..Preview::default()
                })
            }
            WIFI => {
                let _: NoInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: "See Wi-Fi networks".into(),
                    targets: vec!["Settings: Wi-Fi".into()],
                    ..Preview::default()
                })
            }
            WIFI_JOIN => {
                // Never the password: the summary and targets are audited.
                let input: JoinInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: format!("Join the Wi-Fi network {}", input.network),
                    targets: vec![format!("Wi-Fi: {}", input.network)],
                    ..Preview::default()
                })
            }
            WIFI_FORGET => {
                let input: ForgetInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: format!("Forget the Wi-Fi network {}", input.network),
                    targets: vec![format!("Wi-Fi: {}", input.network)],
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
            COMPUTER | COMPUTER_CHECK | UPDATE_INSTALL | UPDATE_RESTART | PATCH_REMOVE => {
                // The person's own, in Settings: policy makes these Never for
                // every agent, and anything but Settings is refused here too.
                if ctx.principal.agent.as_str() != APP {
                    return Err(ActionError::Failed(
                        "This computer's updates and kernel patches are the person's, in \
                         Settings; never an agent's."
                            .into(),
                    ));
                }
                let restart = if action.as_str() == UPDATE_RESTART {
                    Some(glass_actions::parse_input::<RestartInput>(input)?.when)
                } else {
                    let _: NoInput = glass_actions::parse_input(input)?;
                    None
                };
                let system = self.handle.system()?;
                let text = match action.as_str() {
                    UPDATE_RESTART => {
                        let when = restart.unwrap_or(glass_api::RestartWhen::Cancel);
                        let status = system.restart(when).map_err(ActionError::Failed)?;
                        match (when, status.restart_at) {
                            (glass_api::RestartWhen::Now, _) => {
                                "Restarting into the update…".into()
                            }
                            (glass_api::RestartWhen::Tonight, Some(at)) => {
                                format!("The computer restarts into the update at {}.", at.clock())
                            }
                            _ => "It won't restart tonight: restart when you like.".into(),
                        }
                    }
                    UPDATE_INSTALL => {
                        if self.handle.owner().as_deref() != Some(who.name.as_str()) {
                            return Err(ActionError::Failed(
                                "Only the computer's owner installs its updates; on a computer \
                                 an organization manages, IT does."
                                    .into(),
                            ));
                        }
                        let update = system.install().map_err(ActionError::Failed)?;
                        format!("{} is installed: restart to use it.", update.version)
                    }
                    PATCH_REMOVE => {
                        let patches = system.patches().map_err(ActionError::Failed)?;
                        let waiting = patches
                            .trial
                            .as_ref()
                            .is_some_and(|t| t.state == glass_api::TrialState::Staged);
                        if waiting {
                            system.cancel_patch().map_err(ActionError::Failed)?;
                            "The kernel patch is taken back: it won't be tried.".into()
                        } else {
                            system.undo_patch().map_err(ActionError::Failed)?;
                            "The kernel patch is taken out: the next restart is as before it."
                                .into()
                        }
                    }
                    _ => String::new(),
                };
                let view = self
                    .handle
                    .computer_view(&who.name, action.as_str() == COMPUTER_CHECK)?;
                let text = if text.is_empty() {
                    match (
                        view["available"]["version"].as_str(),
                        view["checked"].as_str(),
                        view["version"].as_str(),
                    ) {
                        (Some(newer), _, _) => format!("{newer} is available."),
                        // A check that found nothing says why.
                        (None, Some(why), _) => sentence(why),
                        (None, None, Some(version)) => format!("This computer runs {version}."),
                        (None, None, None) => "This computer's version isn't known.".into(),
                    }
                } else {
                    text
                };
                Ok(ActionOutput {
                    text,
                    data: view,
                    changes: Vec::new(),
                })
            }
            BRIGHTNESS | BRIGHTNESS_SET => {
                let brightness = if action.as_str() == BRIGHTNESS_SET {
                    let input: BrightnessInput = glass_actions::parse_input(input)?;
                    self.handle.change_brightness(&BrightnessChange {
                        step: None,
                        percent: Some(input.percent),
                    })
                } else {
                    let _: NoInput = glass_actions::parse_input(input)?;
                    self.handle.brightness()
                }
                .map_err(ActionError::Failed)?;
                Ok(ActionOutput {
                    text: match brightness.percent {
                        Some(p) => format!("The screen is at {p}%."),
                        None => "This screen's brightness isn't set from here.".into(),
                    },
                    data: serde_json::to_value(brightness).unwrap_or_default(),
                    changes: Vec::new(),
                })
            }
            BLUETOOTH | BLUETOOTH_SCAN | BLUETOOTH_PAIR | BLUETOOTH_REMOVE | BLUETOOTH_POWER => {
                // The person's own, in Settings: policy makes these Never for
                // every agent, and anything but Settings is refused here too.
                if ctx.principal.agent.as_str() != APP {
                    return Err(ActionError::Failed(
                        "Bluetooth is the person's own, in Settings; never an agent's.".into(),
                    ));
                }
                let controller = self.handle.bluetooth().ok_or_else(no_bluetooth)?;
                let rules = self.handle.bluetooth_rules();
                let mut pin = None;
                let text = match action.as_str() {
                    BLUETOOTH_SCAN => {
                        let _: NoInput = glass_actions::parse_input(input)?;
                        if !controller
                            .on()
                            .map_err(|e| ActionError::Failed(e.to_string()))?
                        {
                            return Err(ActionError::Failed("Turn Bluetooth on first.".into()));
                        }
                        let found = controller
                            .inquiry()
                            .map_err(|e| ActionError::Failed(e.to_string()))?;
                        let n = found.len();
                        *self
                            .handle
                            .0
                            .nearby
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) = found;
                        match n {
                            0 => "Found nothing nearby: is the device in pairing mode?".into(),
                            1 => "Found 1 device nearby.".into(),
                            n => format!("Found {n} devices nearby."),
                        }
                    }
                    BLUETOOTH_PAIR => {
                        let input: DeviceInput = glass_actions::parse_input(input)?;
                        let nearby = self
                            .handle
                            .0
                            .nearby
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone();
                        let code = bluetooth::pair(&controller, &rules, &nearby, &input.address)
                            .map_err(ActionError::Failed)?;
                        // The PIN only in the data Settings shows, never in
                        // a line that may be kept.
                        pin = Some(code);
                        "Type the PIN Settings shows on the device, then press Enter.".into()
                    }
                    BLUETOOTH_REMOVE => {
                        let input: DeviceInput = glass_actions::parse_input(input)?;
                        controller
                            .remove(&input.address.to_ascii_lowercase())
                            .map_err(|e| ActionError::Failed(e.to_string()))?;
                        "Removed it: it won't connect again until it's paired.".into()
                    }
                    BLUETOOTH_POWER => {
                        let input: PowerInput = glass_actions::parse_input(input)?;
                        bluetooth::power(&controller, &rules, input.on)
                            .map_err(ActionError::Failed)?;
                        format!("Bluetooth is {}.", if input.on { "on" } else { "off" })
                    }
                    _ => {
                        let _: NoInput = glass_actions::parse_input(input)?;
                        String::new()
                    }
                };
                let view = self.handle.bluetooth_view()?;
                let text = if text.is_empty() {
                    let paired = view.devices.iter().filter(|d| d.paired).count();
                    match (view.present, view.on) {
                        (false, _) => "This computer has no Bluetooth.".into(),
                        (true, false) => format!("Bluetooth is off; {paired} devices paired."),
                        (true, true) => format!("Bluetooth is on; {paired} devices paired."),
                    }
                } else {
                    text
                };
                let mut data = serde_json::to_value(&view).unwrap_or_default();
                if let (Some(pin), Some(map)) = (pin, data.as_object_mut()) {
                    map.insert("pin".into(), json!(pin));
                }
                Ok(ActionOutput {
                    text,
                    data,
                    changes: Vec::new(),
                })
            }
            WIFI | WIFI_JOIN | WIFI_FORGET => {
                // The person's own, in Settings: policy makes these Never for
                // every agent, and anything but Settings is refused here too.
                if ctx.principal.agent.as_str() != APP {
                    return Err(ActionError::Failed(
                        "Wi-Fi is the person's own, in Settings; never an agent's.".into(),
                    ));
                }
                let radio = self.handle.wifi().ok_or_else(no_wifi)?;
                let rules = self.handle.wifi_rules();
                let text = match action.as_str() {
                    WIFI_JOIN => {
                        let input: JoinInput = glass_actions::parse_input(input)?;
                        wifi::join(
                            radio.as_ref(),
                            &rules,
                            &input.network,
                            input.password.as_deref().filter(|p| !p.is_empty()),
                        )
                        .map_err(ActionError::Failed)?;
                        format!("Joining {}.", input.network)
                    }
                    WIFI_FORGET => {
                        let input: ForgetInput = glass_actions::parse_input(input)?;
                        wifi::forget(radio.as_ref(), &rules, &input.network)
                            .map_err(ActionError::Failed)?;
                        format!("Forgot {}.", input.network)
                    }
                    _ => {
                        let _: NoInput = glass_actions::parse_input(input)?;
                        String::new()
                    }
                };
                let view = self.handle.wifi_view()?;
                let text = if text.is_empty() {
                    match &view.joined {
                        Some(n) => {
                            format!("On {n}; {} networks nearby or known.", view.networks.len())
                        }
                        None if view.present => {
                            format!("Not on a network; {} nearby or known.", view.networks.len())
                        }
                        None => "This computer has no Wi-Fi.".into(),
                    }
                } else {
                    text
                };
                Ok(ActionOutput {
                    text,
                    data: serde_json::to_value(&view).unwrap_or_default(),
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

    /// The SettingsLayers model (ADR 0052): the desktop and Settings change
    /// settings at once, each a read and a write of the person's choices;
    /// none of the changes is lost, and none fails.
    #[test]
    fn ux1_settings_changed_at_once_all_hold() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        handle.set_policy(&policy(""));
        let defs: Vec<&'static SettingDef> = settings::table()
            .iter()
            .filter(|d| d.scope == Scope::Person && !settings::narrow_only(d.id))
            .filter(|d| {
                matches!(d.kind, settings::Kind::Number { .. })
                    || matches!(d.default, SettingValue::Bool(_))
            })
            .collect();
        assert!(defs.len() >= 3, "{}", defs.len());
        let value = |def: &SettingDef, round: usize| match def.kind {
            settings::Kind::Number { min, max, .. } => {
                SettingValue::Number(if round.is_multiple_of(2) { min } else { max })
            }
            _ => SettingValue::Bool(!round.is_multiple_of(2)),
        };
        const ROUNDS: usize = 20;
        std::thread::scope(|s| {
            for def in &defs {
                let handle = handle.clone();
                s.spawn(move || {
                    for round in 0..ROUNDS {
                        handle
                            .choose("dana", 1001, def.id, Some(value(def, round)))
                            .unwrap();
                    }
                });
            }
        });
        let now = handle.person("dana", 1001);
        for def in &defs {
            let effective = now.iter().find(|e| e.def.id == def.id).unwrap();
            assert_eq!(effective.value, value(def, ROUNDS - 1), "{}", def.id);
            assert_eq!(effective.from, SettingFrom::You, "{}", def.id);
        }
    }

    /// The SettingsLayers model (ADR 0052): a release that only moves the
    /// person into a group, its settings the same, changes what's in force
    /// for them, so whoever listens hears: the desktop, and glassd applying
    /// the computer's settings for whoever is signed in.
    #[test]
    fn adr0031_a_release_that_only_moves_people_between_groups_is_heard() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        let heard = Arc::new(Mutex::new(Vec::new()));
        let h = Arc::clone(&heard);
        handle.on_change(move |uid| h.lock().unwrap().push(uid));
        let finance = |members: &str| {
            policy(&format!(
                "[[group]]\nname = \"finance\"\nmembers = [{members}]\n[group.settings]\n\
                 \"devices.usb_storage\" = {{ value = false, locked = true }}\n\
                 \"lock.after_minutes\" = {{ value = 2, locked = true }}\n"
            ))
        };
        handle.set_policy(&finance(""));
        heard.lock().unwrap().clear();
        let usb = |handle: &SettingsHandle| {
            handle
                .computer(Some("dana"))
                .into_iter()
                .find(|r| r.id == "devices.usb_storage")
                .unwrap()
                .value
        };
        assert_eq!(usb(&handle), SettingValue::Bool(true));

        handle.set_policy(&finance("\"dana\""));
        assert_eq!(*heard.lock().unwrap(), [None]);
        assert_eq!(usb(&handle), SettingValue::Bool(false));
        assert!(
            handle
                .person("dana", 1001)
                .iter()
                .any(|e| e.def.id == "lock.after_minutes" && e.locked)
        );

        // And out again.
        handle.set_policy(&finance(""));
        assert_eq!(*heard.lock().unwrap(), [None, None]);
        assert_eq!(usb(&handle), SettingValue::Bool(true));
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

    #[test]
    fn adr0060_brightness_never_goes_dark_and_is_kept_for_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let screen = dir.path().join("screen");
        let handle = SettingsHandle::new(dir.path().join("state"));
        handle.set_backlight(Backlight::stand_in(&screen)).unwrap();
        let change = |step, percent| {
            handle
                .change_brightness(&BrightnessChange { step, percent })
                .unwrap()
                .percent
        };
        assert_eq!(change(None, None), Some(100), "neither reads it");
        assert_eq!(change(Some(BrightnessStep::Down), None), Some(90));
        assert_eq!(change(None, Some(0)), Some(5), "never off");
        assert_eq!(change(Some(BrightnessStep::Up), None), Some(10));
        assert_eq!(change(None, Some(35)), Some(35));

        // The firmware starts the screen at full; the next start puts it
        // back where the person left it.
        std::fs::write(&screen, "100").unwrap();
        let next = SettingsHandle::new(dir.path().join("state"));
        next.set_backlight(Backlight::stand_in(&screen)).unwrap();
        assert_eq!(next.brightness().unwrap().percent, Some(35));
    }

    /// A computer with an update to take and a patch being tried.
    struct Machine(Mutex<(glass_api::UpdateStatus, glass_api::PatchStatus)>);

    fn machine() -> Arc<Machine> {
        let trial = glass_api::PatchTrial {
            id: "iwlwifi-resume".into(),
            revision: 1,
            summary: "Wi-Fi works again after sleep".into(),
            review: "community".into(),
            env: "default-patch-1".into(),
            from_env: "default".into(),
            state: glass_api::TrialState::Staged,
            modules: vec!["if_iwlwifi.ko".into()],
            staged_at: glass_types::Timestamp::now(),
            ended_at: None,
            note: None,
            ran_minutes: 0,
            probation_minutes: 1440,
            woke: 0,
            probation_resumes: 2,
        };
        Arc::new(Machine(Mutex::new((
            glass_api::UpdateStatus {
                running: "default".into(),
                version: Some("2026.10.1".into()),
                channel: "stable".into(),
                taken: 7,
                update: None,
                history: Vec::new(),
                available: None,
                checked: None,
                restart_at: None,
            },
            glass_api::PatchStatus {
                running: "default".into(),
                trial: Some(trial),
                history: Vec::new(),
                kept: Vec::new(),
            },
        ))))
    }

    impl computer::Computer for Machine {
        fn updates(&self) -> Result<glass_api::UpdateStatus, String> {
            Ok(self.0.lock().unwrap().0.clone())
        }
        fn check(&self) -> Result<glass_api::UpdateStatus, String> {
            let mut state = self.0.lock().unwrap();
            state.0.available = Some(glass_api::UpdateAvailable {
                number: 8,
                version: "2026.11.1".into(),
                base: "15.1".into(),
                notes: String::new(),
                patches: Vec::new(),
                blocked: None,
            });
            Ok(state.0.clone())
        }
        fn install(&self) -> Result<glass_api::OsUpdate, String> {
            let mut state = self.0.lock().unwrap();
            let update = glass_api::OsUpdate {
                number: 8,
                version: "2026.11.1".into(),
                channel: "stable".into(),
                base: "15.1".into(),
                env: "update-8".into(),
                from_env: "default".into(),
                state: glass_api::TrialState::Staged,
                staged_at: glass_types::Timestamp::now(),
                ended_at: None,
                note: None,
                patches: Vec::new(),
            };
            state.0.update = Some(update.clone());
            Ok(update)
        }
        fn restart(&self, when: glass_api::RestartWhen) -> Result<glass_api::UpdateStatus, String> {
            let mut state = self.0.lock().unwrap();
            if state.0.update.is_none() {
                return Err("no update is installed and waiting for a restart".into());
            }
            state.0.restart_at = match when {
                glass_api::RestartWhen::Cancel => None,
                _ => glass_types::Timestamp::from_unix_seconds(1_793_000_000),
            };
            Ok(state.0.clone())
        }
        fn patches(&self) -> Result<glass_api::PatchStatus, String> {
            Ok(self.0.lock().unwrap().1.clone())
        }
        fn cancel_patch(&self) -> Result<glass_api::PatchStatus, String> {
            let mut state = self.0.lock().unwrap();
            let trial = state.1.trial.as_mut().ok_or("no patch")?;
            if trial.state != glass_api::TrialState::Staged {
                return Err("it's been tried".into());
            }
            trial.state = glass_api::TrialState::Cancelled;
            Ok(state.1.clone())
        }
        fn undo_patch(&self) -> Result<glass_api::PatchStatus, String> {
            let mut state = self.0.lock().unwrap();
            let trial = state.1.trial.as_mut().ok_or("no patch")?;
            trial.state = glass_api::TrialState::Undone;
            Ok(state.1.clone())
        }
    }

    #[test]
    fn adr0061_the_owner_installs_updates_and_anyone_at_the_screen_takes_a_patch_out() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        handle.set_policy(&policy(""));
        let provider = SettingsProvider::new(handle.clone());
        let call = |ctx: &TaskContext, name: &str| {
            provider.invoke(ctx, &name.parse().unwrap(), &json!({}))
        };
        let dana = ctx("dana", 1001, APP);
        let sam = ctx("sam", 1002, APP);

        // Not set up here (the dev host's glassd): said, not a crash.
        let none = call(&dana, COMPUTER).unwrap_err().to_string();
        assert!(none.contains("aren't set up here"), "{none}");
        let system = machine();
        handle.set_computer(system.clone());

        // Managed (no owner): everyone sees the version and the patch, and
        // nobody here checks or installs: IT does.
        let seen = call(&sam, COMPUTER).unwrap();
        assert_eq!(seen.data["version"], "2026.10.1");
        assert_eq!(seen.data["owner"], false);
        assert_eq!(seen.data["patch"]["label"], "Restart to try it");
        assert!(call(&dana, COMPUTER_CHECK).unwrap().data["available"].is_null());
        let it = call(&dana, UPDATE_INSTALL).unwrap_err().to_string();
        assert!(it.contains("IT does"), "{it}");
        assert!(system.0.lock().unwrap().0.update.is_none());

        // Dana owns it: she checks, and installs; Sam still can't.
        handle.set_owner(Some("dana".into()));
        let checked = call(&dana, COMPUTER_CHECK).unwrap();
        assert_eq!(checked.text, "2026.11.1 is available.");
        assert_eq!(checked.data["owner"], true);
        assert!(call(&sam, UPDATE_INSTALL).is_err());
        let installed = call(&dana, UPDATE_INSTALL).unwrap();
        assert_eq!(installed.text, "2026.11.1 is installed: restart to use it.");
        assert_eq!(installed.data["update"]["state"], "staged");

        // Anyone at the screen chooses when it restarts into it: tonight,
        // then not tonight after all.
        let restart = |ctx: &TaskContext, when: &str| {
            provider.invoke(
                ctx,
                &UPDATE_RESTART.parse().unwrap(),
                &json!({ "when": when }),
            )
        };
        let tonight = restart(&sam, "tonight").unwrap();
        assert!(
            tonight
                .text
                .starts_with("The computer restarts into the update at"),
            "{}",
            tonight.text
        );
        assert!(tonight.data["restart_at"].is_string());
        let kept = restart(&sam, "cancel").unwrap();
        assert!(kept.text.contains("won't restart tonight"), "{}", kept.text);
        assert!(kept.data["restart_at"].is_null());
        assert!(restart(&sam, "whenever").is_err(), "now, tonight or cancel");

        // Anyone at the screen takes back a patch not yet tried, and takes
        // out one on probation: both only go back.
        let back = call(&sam, PATCH_REMOVE).unwrap();
        assert!(back.text.contains("taken back"), "{}", back.text);
        assert!(back.data["patch"].is_null(), "no longer open");
        system.0.lock().unwrap().1.trial.as_mut().unwrap().state = glass_api::TrialState::Probation;
        let out = call(&sam, PATCH_REMOVE).unwrap();
        assert!(out.text.contains("taken out"), "{}", out.text);

        // Never an agent, whatever policy said: the app refuses too.
        let agent = ctx("dana", 1001, "desktop-agent");
        let refused = provider
            .invoke(
                &agent,
                &UPDATE_RESTART.parse().unwrap(),
                &json!({ "when": "now" }),
            )
            .unwrap_err()
            .to_string();
        assert!(refused.contains("never an agent's"), "{refused}");
        for name in [COMPUTER, COMPUTER_CHECK, UPDATE_INSTALL, PATCH_REMOVE] {
            let refused = call(&agent, name).unwrap_err().to_string();
            assert!(refused.contains("never an agent's"), "{name}: {refused}");
        }
        for name in [UPDATE_INSTALL, UPDATE_RESTART, PATCH_REMOVE] {
            assert!(glass_policy::persons_own(&name.parse().unwrap()), "{name}");
        }
    }

    #[test]
    fn ga1_what_each_action_returns_is_what_it_declares() {
        let dir = tempfile::tempdir().unwrap();
        let handle = SettingsHandle::new(dir.path());
        // A group's setting, a locked one, and apps hidden by name.
        handle.set_policy(&policy(AP));
        handle.name_apps(|_| BTreeMap::from([("my-mail-3".to_owned(), "My Mail".to_owned())]));
        handle.set_model_keys(Arc::new(Chooser(Mutex::new(vec![ModelAgent {
            id: "it-agent".into(),
            name: "IT agent".into(),
            on: true,
            always: false,
        }]))));
        let provider = SettingsProvider::new(handle.clone());
        let dana = ctx("dana", 1001, APP);
        let run = |name: &str, input: Value| {
            let action: ActionName = name.parse().unwrap();
            provider.preview(&dana, &action, &input).unwrap();
            let mut out = provider
                .invoke(&dana, &action, &input)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            if let Some(data) = out.data.as_object_mut() {
                data.remove(glass_actions::AUDIT_DETAIL);
            }
            let shape = provider.returns(&action).unwrap();
            glass_actions::shape::conforms(&out.data, &shape)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            out.data
        };
        run(LIST, json!({}));
        run(SET, json!({ "id": "clock.hour24", "value": false }));
        run(
            SET,
            json!({ "id": "search.hidden_apps", "value": ["my-mail-3"] }),
        );
        run(SET, json!({ "id": "clock.hour24" }));
        let listed = run(LIST, json!({}));
        assert_eq!(listed["app_names"]["my-mail-3"], "My Mail");
        // Managed: IT decides how agents reach models, and lists no agents.
        run(MODELS, json!({}));

        // A computer Dana owns: her own computer settings, models and agents.
        handle.set_owner(Some("dana".into()));
        run(SET, json!({ "id": "devices.usb_storage", "value": false }));
        let listed = run(LIST, json!({}));
        assert!(
            listed["device"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["source"]["from"] == "owner"),
            "{listed}"
        );
        run(
            MODELS_CONNECT,
            json!({ "service": "openrouter", "key": "sk-or-v1-own-key-not-real" }),
        );
        run(MODELS_AGENTS, json!({ "agent": "it-agent", "on": false }));
        assert_eq!(run(MODELS, json!({}))["agents"][0]["on"], false);

        // A screen not dimmed from here, then one that is.
        assert_eq!(run(BRIGHTNESS, json!({}))["present"], false);
        handle
            .set_backlight(Backlight::stand_in(dir.path().join("screen")))
            .unwrap();
        assert_eq!(run(BRIGHTNESS_SET, json!({ "percent": 40 }))["percent"], 40);
        assert_eq!(run(BRIGHTNESS, json!({}))["percent"], 40);

        // This computer: its version, an update checked and installed, and
        // a patch taken back.
        handle.set_computer(machine());
        run(COMPUTER, json!({}));
        assert_eq!(
            run(COMPUTER_CHECK, json!({}))["available"]["version"],
            "2026.11.1"
        );
        run(UPDATE_INSTALL, json!({}));
        run(UPDATE_RESTART, json!({ "when": "tonight" }));
        run(PATCH_REMOVE, json!({}));

        // Bluetooth (CF-13): a keyboard found, paired, removed; off and on.
        let bt = dir.path().join("bluetooth.json");
        std::fs::write(
            &bt,
            json!({ "nearby": [{ "address": "00:1f:20:3a:4b:5c", "name": "Keyboard K380", "class": 0x002540 }] })
                .to_string(),
        )
        .unwrap();
        handle.set_bluetooth(bluetooth::Controller::stand_in(&bt));
        assert_eq!(run(BLUETOOTH, json!({}))["present"], true);
        assert_eq!(
            run(BLUETOOTH_SCAN, json!({}))["devices"][0]["label"],
            "Keyboard"
        );
        let paired = run(BLUETOOTH_PAIR, json!({ "address": "00:1f:20:3a:4b:5c" }));
        assert_eq!(paired["pin"].as_str().unwrap().len(), 6);
        assert_eq!(paired["devices"][0]["paired"], true);
        run(BLUETOOTH_REMOVE, json!({ "address": "00:1f:20:3a:4b:5c" }));
        assert_eq!(run(BLUETOOTH_POWER, json!({ "on": false }))["on"], false);
        run(BLUETOOTH_POWER, json!({ "on": true }));

        for def in provider.actions() {
            assert!(provider.returns(&def.name).is_some(), "{}", def.name);
        }
    }
}
