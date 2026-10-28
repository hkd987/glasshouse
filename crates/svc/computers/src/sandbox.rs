//! The runtime's sandbox backend for agent computers (ADR 0065, ADR 0066).
//!
//! Where the jail backend makes a sandbox per task and removes it after,
//! this one keeps a computer per agent: a task that names one
//! ([`SandboxSpec::computer`]) runs in it, with the task's own socket,
//! and leaves its files there; a task that names none gets a computer of
//! its own that goes with it. What a task was granted still ends with the
//! task (PA-2): its socket, its processes and its mounts. What the agent
//! copied into its computer stays there, by design (ADR 0065 §2).
//!
//! Computers live under one folder, a folder per owner:
//!
//! ```text
//! <dir>/<uid>/<id>/computer.json   its name, role, agent, sleep record
//! <dir>/<uid>/<id>/screen.log      what its tasks and its person printed
//! <dir>/<uid>/<id>/tasks/<task>/   each live task's socket
//! <dir>/<uid>/<id>/…               the hypervisor's: its disk, its state
//! ```
//!
//! An owner's computers are theirs on the host too, and a run inside one
//! is root of it but its owner outside: who may use a computer is decided
//! by the uid the kernel reports for the caller (the control socket), never
//! by a name in a request.
//!
//! In the store a computer goes by a key of its own, made with it: its
//! owner's key (stable across hosts) and a random part, so no two
//! computers share versions. Each sleep keeps its record beside its
//! versions, and a host that never had it lists it and wakes it from
//! there ([`ComputerSandbox::adopt`]): a host replaced, or another host
//! of the stack.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use glass_api::{
    ComputerNew, ComputerState, ComputerView, PERSONAS_MAX, PersonaNew, PersonaView, ScreenView,
    StoredView,
};
use glass_platform::sandbox::{
    AGENT_SOCKET_PATH, ProcessSpec, Sandbox, SandboxSpec, ToolOutput, ToolRun, ToolRunner,
    UnshareTools, valid_computer_id,
};
use glass_platform::{PlatformError, Result};
use glass_types::{AgentId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::hypervisor::{ComputerSpec, Hypervisor, TaskRun};
use crate::store::Store;

/// Where a task works inside its computer: kept, like the rest of it.
pub const WORK: &str = "/work";

/// Versions of a computer kept in the store: the newest, and two before.
const VERSIONS_KEPT: usize = 3;

/// What a screen read returns at most.
const SCREEN_READ_MAX: u64 = 64 * 1024;
/// A screen past this is cut to its last [`SCREEN_KEEP`] as a task starts.
const SCREEN_MAX: u64 = 1024 * 1024;
const SCREEN_KEEP: u64 = 256 * 1024;

/// What glassd keeps about a computer beside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputerMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: String,
    pub agent: AgentId,
    pub created_at: Timestamp,
    /// Made for one task that named no computer, removed with it.
    #[serde(default)]
    pub fresh: bool,
    /// Set as it goes to sleep, cleared as it wakes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored: Option<StoredView>,
    /// Its name in the store, made with it: its owner's key and a random
    /// part. Empty for a computer made before keys were (`u<uid>-<id>`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// Its person's agents, all working on it (ADR 0068): in its record,
    /// so they go where it goes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub personas: Vec<PersonaView>,
}

/// Each owner's key in the store, by uid: the same person's on every host
/// (ADR 0067), letters and digits only.
pub struct OwnerKeys(Box<dyn Fn(u32) -> String + Send + Sync>);

impl std::fmt::Debug for OwnerKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerKeys")
    }
}

impl OwnerKeys {
    pub fn new(keys: impl Fn(u32) -> String + Send + Sync + 'static) -> Self {
        Self(Box::new(keys))
    }

    /// On a computer people have accounts on, their uid is who they are.
    pub fn by_uid() -> Self {
        Self::new(|uid| format!("u{uid}"))
    }

    fn of(&self, uid: u32) -> Result<String> {
        let key = (self.0)(uid);
        if key.is_empty() || key.len() > 40 || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(refuse(format!("{key:?} isn't an owner's key")));
        }
        Ok(key)
    }
}

/// Computers, each task run in one.
#[derive(Debug)]
pub struct ComputerSandbox {
    dir: PathBuf,
    hypervisor: Arc<dyn Hypervisor>,
    memory_mib: u32,
    vcpus: u32,
    /// Where computers sleep (ADR 0065 §11), and its name for people.
    store: Option<(Arc<Store>, String)>,
    owners: OwnerKeys,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// Live runs, by sandbox name: whose computer, which.
    runs: HashMap<String, (u32, String)>,
    /// Computers whose person has the keyboard.
    taken_over: HashSet<(u32, String)>,
    /// Computers going to sleep or waking: nothing else may start them.
    moving: HashSet<(u32, String)>,
    /// When each computer's last run ended, since glassd started.
    used: HashMap<(u32, String), Instant>,
    /// Owners whose computers in the store this host has looked for: done,
    /// or when it last couldn't.
    adopted: HashMap<u32, Option<Instant>>,
    /// This host is being replaced: its computers go to sleep, and none
    /// starts or wakes here again.
    leaving: bool,
}

fn fail(context: impl Into<String>, why: impl std::fmt::Display) -> PlatformError {
    PlatformError::Io {
        context: context.into(),
        source: std::io::Error::other(why.to_string()),
    }
}

fn refuse(why: impl Into<String>) -> PlatformError {
    PlatformError::Unsafe(why.into())
}

impl ComputerSandbox {
    /// Computers under `dir`, each given `memory_mib` and `vcpus`.
    pub fn new(
        dir: PathBuf,
        hypervisor: Arc<dyn Hypervisor>,
        memory_mib: u32,
        vcpus: u32,
    ) -> Result<Self> {
        fs::create_dir_all(&dir).map_err(|e| fail(format!("creating {}", dir.display()), e))?;
        // Each owner's folder is theirs; this one only lets them through.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o711))
            .map_err(|e| fail(format!("chmod {}", dir.display()), e))?;
        Ok(Self {
            dir,
            hypervisor,
            memory_mib,
            vcpus,
            store: None,
            owners: OwnerKeys::by_uid(),
            state: Mutex::new(State::default()),
        })
    }

    /// Owners' keys in the store from `owners` instead of their uids.
    #[must_use]
    pub fn with_owner_keys(mut self, owners: OwnerKeys) -> Self {
        self.owners = owners;
        self
    }

    /// This host is being replaced: from now on no computer starts or
    /// wakes here, and each goes to sleep as its work ends.
    pub fn leave(&self) {
        self.state().leaving = true;
    }

    pub fn leaving(&self) -> bool {
        self.state().leaving
    }

    fn refuse_if_leaving(&self) -> Result<()> {
        if self.leaving() {
            return Err(refuse(
                "this host is being replaced: ask again in a minute, and another host takes it",
            ));
        }
        Ok(())
    }

    /// Computers sleep in `store`, which people know as `place` ("S3:
    /// acme-agents", a folder in development).
    #[must_use]
    pub fn with_store(mut self, store: Arc<Store>, place: impl Into<String>) -> Self {
        self.store = Some((store, place.into()));
        self
    }

    pub fn hypervisor(&self) -> &dyn Hypervisor {
        self.hypervisor.as_ref()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn spec(&self, uid: u32, id: &str) -> ComputerSpec {
        ComputerSpec {
            id: id.to_owned(),
            dir: self.dir.join(uid.to_string()).join(id),
            memory_mib: self.memory_mib,
            vcpus: self.vcpus,
        }
    }

    fn meta_path(computer: &ComputerSpec) -> PathBuf {
        computer.dir.join("computer.json")
    }

    fn read_meta(computer: &ComputerSpec) -> Option<ComputerMeta> {
        let text = fs::read(Self::meta_path(computer)).ok()?;
        serde_json::from_slice(&text).ok()
    }

    fn write_meta(computer: &ComputerSpec, meta: &ComputerMeta) -> Result<()> {
        let path = Self::meta_path(computer);
        let making = path.with_extension("json.new");
        let text = serde_json::to_vec_pretty(meta).map_err(|e| fail("saving a computer", e))?;
        fs::write(&making, text)
            .and_then(|()| fs::rename(&making, &path))
            .map_err(|e| fail(format!("saving {}", path.display()), e))
    }

    /// Makes a computer's folder, its owner's, if it isn't there.
    fn make_folder(&self, computer: &ComputerSpec, uid: u32, gid: u32) -> Result<()> {
        let owner = computer.dir.parent().unwrap_or(&self.dir);
        for dir in [owner, computer.dir.as_path()] {
            if dir.is_dir() {
                continue;
            }
            fs::create_dir(dir).map_err(|e| fail(format!("creating {}", dir.display()), e))?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .map_err(|e| fail(format!("chmod {}", dir.display()), e))?;
            std::os::unix::fs::chown(dir, Some(uid), Some(gid))
                .map_err(|e| fail(format!("chown {}", dir.display()), e))?;
        }
        Ok(())
    }

    /// A new agent with a computer of its own, for `uid`. Its disk is made
    /// as it first starts.
    pub fn make(&self, uid: u32, gid: u32, new: &ComputerNew) -> Result<ComputerView> {
        if !valid_computer_id(&new.id) {
            return Err(refuse(format!(
                "{:?} isn't a computer's id: lowercase letters, digits and -",
                new.id
            )));
        }
        if new.id.starts_with("task-") {
            return Err(refuse(
                "ids starting task- are kept for tasks' own computers",
            ));
        }
        let name = new.name.trim();
        if name.is_empty() || name.chars().count() > 40 {
            return Err(refuse("an agent's name is 1 to 40 characters"));
        }
        self.refuse_if_leaving()?;
        if self.find(uid, &new.id).is_ok() {
            return Err(refuse(format!(
                "you already have a computer called {}",
                new.id
            )));
        }
        let computer = self.spec(uid, &new.id);
        let key = format!("{}-{}", self.owners.of(uid)?, random_hex()?);
        self.make_folder(&computer, uid, gid)?;
        let meta = ComputerMeta {
            id: new.id.clone(),
            name: glass_api::display_line(name).into_owned(),
            role: glass_api::display_line(new.role.trim()).into_owned(),
            agent: new.agent.clone(),
            created_at: Timestamp::now(),
            fresh: false,
            stored: None,
            key,
            personas: Vec::new(),
        };
        Self::write_meta(&computer, &meta)?;
        Ok(self.view_of(uid, &computer, meta))
    }

    /// A new agent on the person's own computer (ADR 0068), which is made
    /// first, called `computer_name`, if they have none.
    pub fn add_persona(
        &self,
        uid: u32,
        gid: u32,
        computer_name: &str,
        new: &PersonaNew,
    ) -> Result<ComputerView> {
        self.refuse_if_leaving()?;
        if !valid_computer_id(&new.id) || new.id.starts_with("task-") {
            return Err(refuse(format!(
                "{:?} isn't an agent's id: lowercase letters, digits and -",
                new.id
            )));
        }
        let name = glass_api::display_line(new.name.trim()).into_owned();
        if name.is_empty() || name.chars().count() > 40 {
            return Err(refuse("an agent's name is 1 to 40 characters"));
        }
        let id = glass_api::PERSON_COMPUTER;
        if self.find(uid, id).is_err() {
            self.make(
                uid,
                gid,
                &ComputerNew {
                    id: id.to_owned(),
                    name: computer_name.to_owned(),
                    role: String::new(),
                    agent: new.agent.clone(),
                },
            )?;
        }
        let (computer, _) = self.find(uid, id)?;
        // Its record changes only while nothing else rewrites it: a sleep
        // or a wake takes it first (`moving`), under this same lock.
        let state = self.state();
        if state.moving.contains(&(uid, id.to_owned())) {
            return Err(refuse(
                "your computer is going to sleep or waking; try again in a moment",
            ));
        }
        let mut meta = Self::read_meta(&computer)
            .ok_or_else(|| PlatformError::Unsupported(format!("a computer called {id} for you")))?;
        if meta.personas.iter().any(|p| p.id == new.id) {
            return Err(refuse(format!(
                "you already have an agent called {}",
                new.id
            )));
        }
        if meta
            .personas
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(&name))
        {
            return Err(refuse(format!("you already have an agent named {name}")));
        }
        if meta.personas.len() >= PERSONAS_MAX {
            return Err(refuse(format!(
                "you have {PERSONAS_MAX} agents, the most a computer keeps: remove one first"
            )));
        }
        meta.personas.push(PersonaView {
            id: new.id.clone(),
            name,
            role: glass_api::display_line(new.role.trim()).into_owned(),
            agent: new.agent.clone(),
            created_at: Timestamp::now(),
        });
        Self::write_meta(&computer, &meta)?;
        drop(state);
        Ok(self.view_of(uid, &computer, meta))
    }

    /// The agent goes from the person's computer; what it left there stays.
    pub fn remove_persona(&self, uid: u32, id: &str) -> Result<ComputerView> {
        let computer_id = glass_api::PERSON_COMPUTER;
        let (computer, _) = self.find(uid, computer_id)?;
        let state = self.state();
        if state.moving.contains(&(uid, computer_id.to_owned())) {
            return Err(refuse(
                "your computer is going to sleep or waking; try again in a moment",
            ));
        }
        let mut meta = Self::read_meta(&computer).ok_or_else(|| {
            PlatformError::Unsupported(format!("a computer called {computer_id} for you"))
        })?;
        let before = meta.personas.len();
        meta.personas.retain(|p| p.id != id);
        if meta.personas.len() == before {
            return Err(PlatformError::Unsupported(format!(
                "an agent called {id} for you"
            )));
        }
        Self::write_meta(&computer, &meta)?;
        drop(state);
        Ok(self.view_of(uid, &computer, meta))
    }

    /// One of the person's agents on one of their computers.
    pub fn persona(&self, uid: u32, computer: &str, id: &str) -> Result<PersonaView> {
        let (_, meta) = self.find(uid, computer)?;
        meta.personas
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| PlatformError::Unsupported(format!("an agent called {id} for you")))
    }

    /// The caller's computers, oldest first; tasks' own computers aren't
    /// listed.
    pub fn list(&self, uid: u32) -> Vec<ComputerView> {
        // Once per owner while glassd runs, and a minute after a try that
        // couldn't: the store is a network away, and the front lists often.
        let due = match self.state().adopted.get(&uid) {
            None => true,
            Some(None) => false,
            Some(Some(failed)) => failed.elapsed() >= Duration::from_secs(60),
        };
        if due {
            let done = self.adopt(uid, None).is_ok();
            self.state().adopted.insert(uid, (!done).then(Instant::now));
        }
        let Ok(entries) = fs::read_dir(self.dir.join(uid.to_string())) else {
            return Vec::new();
        };
        let mut views: Vec<ComputerView> = entries
            .flatten()
            .filter_map(|entry| {
                let id = entry.file_name().to_str()?.to_owned();
                let computer = self.spec(uid, &id);
                let meta = Self::read_meta(&computer).filter(|m| !m.fresh)?;
                Some(self.view_of(uid, &computer, meta))
            })
            .collect();
        views.sort_by_key(|v| v.created_at.unix_seconds());
        views
    }

    /// One of the caller's computers.
    pub fn get(&self, uid: u32, id: &str) -> Result<ComputerView> {
        let (computer, meta) = self.find(uid, id)?;
        Ok(self.view_of(uid, &computer, meta))
    }

    /// Its meta, by owner and id; another person's computer is as absent
    /// as one that doesn't exist.
    pub fn find(&self, uid: u32, id: &str) -> Result<(ComputerSpec, ComputerMeta)> {
        if !valid_computer_id(id) {
            return Err(refuse(format!("{id:?} isn't a computer's id")));
        }
        let computer = self.spec(uid, id);
        if let Some(meta) = Self::read_meta(&computer) {
            return Ok((computer, meta));
        }
        // Made or last asleep on another host: its record is in the store.
        if self.store.is_some()
            && matches!(self.adopt(uid, Some(id)), Ok(n) if n > 0)
            && let Some(meta) = Self::read_meta(&computer)
        {
            return Ok((computer, meta));
        }
        Err(PlatformError::Unsupported(format!(
            "a computer called {id} for you"
        )))
    }

    /// Brings the owner's computers this host doesn't have from the
    /// store, asleep: every one, or only `only`. A computer here by the
    /// same id stays as it is. Returns how many it brought.
    pub fn adopt(&self, uid: u32, only: Option<&str>) -> Result<usize> {
        let Some((store, _)) = &self.store else {
            return Ok(0);
        };
        let owner = self.owners.of(uid)?;
        let records = store
            .records(&format!("{owner}-"))
            .map_err(|e| fail("looking for your computers in the store", e))?;
        let gid = glass_platform::users::by_uid(uid).map_or(uid, |u| u.gid);
        let mut adopted = 0;
        for (key, record) in records {
            let Ok(meta) = serde_json::from_slice::<ComputerMeta>(&record) else {
                continue;
            };
            // A record names only its own computer, asleep, its owner's.
            let fits = meta.key == key
                && valid_computer_id(&meta.id)
                && !meta.fresh
                && meta.stored.is_some()
                && only.is_none_or(|id| id == meta.id);
            if !fits {
                continue;
            }
            let computer = self.spec(uid, &meta.id);
            if Self::read_meta(&computer).is_some() {
                continue;
            }
            self.make_folder(&computer, uid, gid)?;
            Self::write_meta(&computer, &meta)?;
            adopted += 1;
        }
        Ok(adopted)
    }

    fn view_of(&self, uid: u32, computer: &ComputerSpec, meta: ComputerMeta) -> ComputerView {
        let key = (uid, meta.id.clone());
        let (live, taken_over) = {
            let state = self.state();
            (
                state.runs.values().any(|run| *run == key),
                state.taken_over.contains(&key),
            )
        };
        let moving = self.state().moving.contains(&key);
        let state = if live || moving {
            ComputerState::Running
        } else if meta.stored.is_some() {
            ComputerState::Asleep
        } else {
            ComputerState::Ready
        };
        ComputerView {
            id: meta.id,
            name: meta.name,
            role: meta.role,
            agent: meta.agent,
            state,
            kind: self.hypervisor.kind().to_owned(),
            contained: self.hypervisor.contained(),
            disk_bytes: if state == ComputerState::Asleep {
                0
            } else {
                disk_bytes(&computer.dir)
            },
            live_tasks: Vec::new(),
            taken_over,
            stored: meta.stored,
            created_at: meta.created_at,
            personas: meta.personas,
        }
    }

    /// Computers on this host awake now, every owner's: what's metered
    /// (ADR 0066).
    pub fn awake(&self) -> u32 {
        let Ok(owners) = fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut awake = 0;
        for owner in owners.flatten() {
            for entry in fs::read_dir(owner.path()).into_iter().flatten().flatten() {
                let computer = ComputerSpec {
                    id: entry.file_name().to_string_lossy().into_owned(),
                    dir: entry.path(),
                    memory_mib: self.memory_mib,
                    vcpus: self.vcpus,
                };
                if Self::read_meta(&computer).is_some_and(|m| m.stored.is_none()) {
                    awake += 1;
                }
            }
        }
        awake
    }

    /// Whether a run (a task, or its person's line) is using it.
    pub fn in_use(&self, uid: u32, id: &str) -> bool {
        let key = (uid, id.to_owned());
        self.state().runs.values().any(|run| *run == key)
    }

    /// Its person takes the keyboard: new tasks wait for it back.
    pub fn take_over(&self, uid: u32, id: &str) -> Result<ComputerView> {
        let (computer, meta) = self.find(uid, id)?;
        self.state().taken_over.insert((uid, id.to_owned()));
        self.screen_line(&computer, &format!("\n── {} took over ──\n", "You"));
        Ok(self.view_of(uid, &computer, meta))
    }

    pub fn hand_back(&self, uid: u32, id: &str) -> Result<ComputerView> {
        let (computer, meta) = self.find(uid, id)?;
        if self.state().taken_over.remove(&(uid, id.to_owned())) {
            self.screen_line(&computer, "\n── handed back ──\n");
        }
        Ok(self.view_of(uid, &computer, meta))
    }

    pub fn taken_over(&self, uid: u32, id: &str) -> bool {
        self.state().taken_over.contains(&(uid, id.to_owned()))
    }

    /// What its screen showed from `offset`, at most 64 KB of it.
    pub fn screen(&self, uid: u32, id: &str, offset: u64) -> Result<ScreenView> {
        let (computer, meta) = self.find(uid, id)?;
        let view = self.view_of(uid, &computer, meta);
        let mut text = String::new();
        let mut next = offset;
        if let Ok(mut file) = fs::File::open(computer.screen()) {
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            // A screen cut shorter since: read it from the start.
            let from = if offset > len { 0 } else { offset };
            let from = from.max(len.saturating_sub(SCREEN_READ_MAX));
            let mut bytes = Vec::new();
            if file.seek(SeekFrom::Start(from)).is_ok() {
                let _ = file.take(SCREEN_READ_MAX).read_to_end(&mut bytes);
            }
            next = from + bytes.len() as u64;
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        Ok(ScreenView {
            text,
            offset: next,
            taken_over: view.taken_over,
            state: view.state,
        })
    }

    fn screen_line(&self, computer: &ComputerSpec, text: &str) {
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(computer.screen())
        {
            let _ = file.write_all(text.as_bytes());
        }
    }

    /// Cuts a long screen to its end, as a task starts.
    fn trim_screen(computer: &ComputerSpec) {
        let path = computer.screen();
        let Ok(len) = fs::metadata(&path).map(|m| m.len()) else {
            return;
        };
        if len <= SCREEN_MAX {
            return;
        }
        let mut tail = Vec::new();
        if let Ok(mut file) = fs::File::open(&path)
            && file.seek(SeekFrom::Start(len - SCREEN_KEEP)).is_ok()
            && file.read_to_end(&mut tail).is_ok()
        {
            let making = path.with_extension("log.new");
            if fs::write(&making, &tail).is_ok() {
                let _ = fs::rename(&making, &path);
            }
        }
    }

    /// A line its person typed while they have the keyboard: the command
    /// that runs it as root on the computer, its output appended to the
    /// screen, and the run's name to [`Self::input_done`] when it ends.
    pub fn input(&self, uid: u32, id: &str, line: &str) -> Result<(Command, String)> {
        let (computer, meta) = self.find(uid, id)?;
        if !self.taken_over(uid, id) {
            return Err(refuse(format!(
                "take over {} first: it's {}'s keyboard until then",
                meta.name, meta.name
            )));
        }
        if meta.stored.is_some() {
            return Err(refuse(format!("{} is asleep: wake it first", meta.name)));
        }
        let line = line.trim();
        if line.is_empty() || line.len() > glass_api::INPUT_MAX_BYTES || line.contains('\0') {
            return Err(refuse("a line is 1 to 4096 bytes"));
        }
        self.hypervisor
            .start(&computer)
            .map_err(|e| fail(format!("starting {}", meta.name), e))?;
        let name = format!("console-{}", Timestamp::now().unix_seconds());
        let console = computer.dir.join("tasks").join(&name);
        fs::create_dir_all(&console).map_err(|e| fail("the console", e))?;
        let run = TaskRun {
            // No socket: its person is root here, not an agent.
            socket: console.join("agent.sock"),
            workdir: "/root".into(),
            process: ProcessSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), line.to_owned()],
                env: vec![
                    ("HOME".into(), "/root".into()),
                    ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
                    ("TERM".into(), "dumb".into()),
                    ("LANG".into(), "C.UTF-8".into()),
                ],
            },
        };
        let mut command = self
            .hypervisor
            .command(&computer, &run)
            .map_err(|e| fail(format!("running on {}", meta.name), e))?;
        self.screen_line(&computer, &format!("root@{id}:~# {line}\n"));
        let screen = OpenOptions::new()
            .create(true)
            .append(true)
            .open(computer.screen())
            .map_err(|e| fail("the screen", e))?;
        let err = screen.try_clone().map_err(|e| fail("the screen", e))?;
        command.stdin(Stdio::null()).stdout(screen).stderr(err);
        self.state().runs.insert(name.clone(), (uid, id.to_owned()));
        Ok((command, name))
    }

    /// A line has finished.
    pub fn input_done(&self, uid: u32, id: &str, name: &str) {
        {
            let mut state = self.state();
            state.runs.remove(name);
            state.used.insert((uid, id.to_owned()), Instant::now());
        }
        let computer = self.spec(uid, id);
        let _ = fs::remove_dir_all(computer.dir.join("tasks").join(name));
    }

    /// Marks it asleep with where it was kept, once its disk is in the
    /// store and gone from this host.
    pub fn set_stored(&self, uid: u32, id: &str, stored: Option<StoredView>) -> Result<()> {
        let (computer, mut meta) = self.find(uid, id)?;
        meta.stored = stored;
        Self::write_meta(&computer, &meta)
    }

    /// The hypervisor's view of one, for sleep and wake.
    pub fn computer(&self, uid: u32, id: &str) -> ComputerSpec {
        self.spec(uid, id)
    }

    /// Its name in the store: its own key, or, made before keys were, its
    /// owner's uid and its id.
    fn store_key(uid: u32, meta: &ComputerMeta) -> String {
        if meta.key.is_empty() {
            format!("u{uid}-{}", meta.id)
        } else {
            meta.key.clone()
        }
    }

    /// Marks it moving for as long as the guard lives.
    fn moving(&self, uid: u32, id: &str) -> Result<Moving<'_>> {
        let key = (uid, id.to_owned());
        let mut state = self.state();
        if state.runs.values().any(|run| *run == key) {
            return Err(refuse("it's working: it sleeps once its tasks are done"));
        }
        if state.taken_over.contains(&key) {
            return Err(refuse("you have its keyboard: hand it back first"));
        }
        if !state.moving.insert(key.clone()) {
            return Err(refuse("it's already going to sleep or waking"));
        }
        Ok(Moving { sandbox: self, key })
    }

    /// Puts it to sleep (ADR 0065 §11): stops it, saves all of its folder
    /// to the store as a new version, and removes it from this host, all
    /// but its record. Nothing of it stays here to keep warm.
    pub fn sleep(&self, uid: u32, id: &str) -> Result<ComputerView> {
        let (store, place) = self
            .store
            .as_ref()
            .ok_or_else(|| refuse("this host has nowhere to keep computers asleep"))?;
        let (computer, meta) = self.find(uid, id)?;
        if meta.stored.is_some() {
            return Ok(self.view_of(uid, &computer, meta));
        }
        let moving = self.moving(uid, id)?;
        // Read again now it's ours: an agent added since is kept.
        let mut meta = Self::read_meta(&computer).unwrap_or(meta);
        self.hypervisor
            .suspend(&computer)
            .map_err(|e| fail(format!("stopping {}", meta.name), e))?;
        let key = Self::store_key(uid, &meta);
        let saved = store
            .save_tree(&key, &computer.dir)
            .map_err(|e| fail(format!("saving {}", meta.name), e))?;
        meta.stored = Some(StoredView {
            version: saved.version.clone(),
            bytes: saved.bytes,
            at: Timestamp::now(),
            place: place.clone(),
        });
        // Its record in the store, naming a version that's there, so any
        // host can wake it; then here: from now it wakes from the store.
        if !meta.key.is_empty() {
            let record = serde_json::to_vec(&meta).map_err(|e| fail("a computer's record", e))?;
            store
                .put_record(&key, &record)
                .map_err(|e| fail(format!("keeping {}'s record", meta.name), e))?;
        }
        Self::write_meta(&computer, &meta)?;
        Self::clear_but_record(&computer)
            .map_err(|e| fail(format!("clearing {} from this host", meta.name), e))?;
        if let Ok(versions) = store.versions(&key) {
            let old = versions.len().saturating_sub(VERSIONS_KEPT);
            for version in &versions[..old] {
                let _ = store.remove_version(&key, version);
            }
        }
        drop(moving);
        Ok(self.view_of(uid, &computer, meta))
    }

    /// Removes all of a computer's folder but its record.
    fn clear_but_record(computer: &ComputerSpec) -> std::io::Result<()> {
        for entry in fs::read_dir(&computer.dir)?.flatten() {
            if entry.file_name() == "computer.json" {
                continue;
            }
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
        Ok(())
    }

    /// Brings it back from the store as it was, its files its owner's.
    pub fn wake(&self, uid: u32, gid: u32, id: &str) -> Result<ComputerView> {
        let (store, _) = self
            .store
            .as_ref()
            .ok_or_else(|| refuse("this host has nowhere computers sleep"))?;
        let (computer, meta) = self.find(uid, id)?;
        let Some(stored) = meta.stored.clone() else {
            return Ok(self.view_of(uid, &computer, meta));
        };
        self.refuse_if_leaving()?;
        let moving = self.moving(uid, id)?;
        let mut meta = Self::read_meta(&computer).unwrap_or(meta);
        // Asleep, the store holds all of it: anything here is what a wake
        // or a sleep that glassd didn't live through left, and goes.
        Self::clear_but_record(&computer).map_err(|e| {
            fail(
                format!("clearing what an earlier wake left of {}", meta.name),
                e,
            )
        })?;
        let waking = computer.dir.join(".waking");
        store
            .restore_tree(&Self::store_key(uid, &meta), &stored.version, &waking)
            .map_err(|e| fail(format!("waking {}", meta.name), e))?;
        let entries = fs::read_dir(&waking).map_err(|e| fail("reading what woke", e))?;
        for entry in entries.flatten() {
            if entry.file_name() == "computer.json" {
                continue;
            }
            let to = computer.dir.join(entry.file_name());
            fs::rename(entry.path(), &to)
                .map_err(|e| fail(format!("putting back {}", to.display()), e))?;
        }
        fs::remove_dir_all(&waking).map_err(|e| fail("clearing what woke", e))?;
        // The store keeps modes, not owners: all of it is its owner's.
        crate::namespaces::chown_all(&computer.dir, uid, gid)
            .map_err(|e| fail(format!("giving {} back to its owner", meta.name), e))?;
        meta.stored = None;
        Self::write_meta(&computer, &meta)?;
        drop(moving);
        self.state()
            .used
            .insert((uid, id.to_owned()), Instant::now());
        Ok(self.view_of(uid, &computer, meta))
    }

    /// Computers awake and unused for `idle`: nothing running, nobody at
    /// the keyboard. Their last use is the later of their last run and
    /// their screen's last line.
    pub fn idle(&self, idle: Duration) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        let Ok(owners) = fs::read_dir(&self.dir) else {
            return out;
        };
        let now = SystemTime::now();
        for owner in owners.flatten() {
            let Some(uid) = owner
                .file_name()
                .to_str()
                .and_then(|u| u.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(computers) = fs::read_dir(owner.path()) else {
                continue;
            };
            for entry in computers.flatten() {
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let computer = self.spec(uid, &id);
                let Some(meta) = Self::read_meta(&computer) else {
                    continue;
                };
                let key = (uid, id.clone());
                let busy = {
                    let state = self.state();
                    state.runs.values().any(|run| *run == key)
                        || state.taken_over.contains(&key)
                        || state.moving.contains(&key)
                        || state.used.get(&key).is_some_and(|at| at.elapsed() < idle)
                };
                if meta.fresh || meta.stored.is_some() || busy {
                    continue;
                }
                let last = [computer.screen(), Self::meta_path(&computer)]
                    .iter()
                    .filter_map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
                    .max();
                if last.is_none_or(|at| now.duration_since(at).unwrap_or_default() >= idle) {
                    out.push(key);
                }
            }
        }
        out
    }

    fn run_of(&self, spec: &SandboxSpec) -> (ComputerSpec, bool) {
        match &spec.computer {
            Some(id) => (self.spec(spec.uid, id), false),
            None => (self.spec(spec.uid, &format!("task-{}", spec.name)), true),
        }
    }

    fn task_dir(computer: &ComputerSpec, spec: &SandboxSpec) -> PathBuf {
        computer.dir.join("tasks").join(&spec.name)
    }
}

/// A computer going to sleep or waking, until dropped.
struct Moving<'a> {
    sandbox: &'a ComputerSandbox,
    key: (u32, String),
}

impl Drop for Moving<'_> {
    fn drop(&mut self) {
        self.sandbox.state().moving.remove(&self.key);
    }
}

/// 64 random bits, in hex: a computer's own part of its key.
fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 8];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| fail("reading /dev/urandom", e))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Bytes in a computer's folder, its disk among them.
fn disk_bytes(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return 0;
    };
    if !meta.is_dir() {
        // Blocks used, so a sparse image counts what it holds.
        return meta.blocks() * 512;
    }
    fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| disk_bytes(&e.path())).sum())
        .unwrap_or(0)
}

impl Sandbox for ComputerSandbox {
    fn kind(&self) -> &'static str {
        self.hypervisor.kind()
    }

    fn is_security_boundary(&self) -> bool {
        self.hypervisor.contained()
    }

    fn keeps_computers(&self) -> bool {
        true
    }

    fn running_note(&self) -> &'static str {
        "Agent working on its own computer"
    }

    fn create(&self, spec: &SandboxSpec) -> Result<()> {
        if spec.name.is_empty()
            || spec.name.len() > 40
            || !spec
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(refuse(format!("bad sandbox name {:?}", spec.name)));
        }
        self.refuse_if_leaving()?;
        let (computer, fresh) = self.run_of(spec);
        let key = (spec.uid, computer.id.clone());
        let meta = if fresh {
            self.make_folder(&computer, spec.uid, spec.gid)?;
            let meta = ComputerMeta {
                id: computer.id.clone(),
                name: "Task".into(),
                role: String::new(),
                agent: AgentId::try_from("task".to_owned()).map_err(|e| fail("a task", e))?,
                created_at: Timestamp::now(),
                fresh: true,
                stored: None,
                key: String::new(),
                personas: Vec::new(),
            };
            Self::write_meta(&computer, &meta)?;
            meta
        } else {
            let id = spec.computer.as_deref().unwrap_or_default();
            self.find(spec.uid, id)?.1
        };
        if self.state().taken_over.contains(&key) {
            return Err(refuse(format!(
                "{} is taken over: hand it back for its tasks to run",
                meta.name
            )));
        }
        if self.state().moving.contains(&key) {
            return Err(refuse(format!(
                "{} is going to sleep or waking; try again in a moment",
                meta.name
            )));
        }
        // A task wakes it (ADR 0065 §11).
        if meta.stored.is_some() {
            self.wake(spec.uid, spec.gid, &computer.id)?;
        }
        self.hypervisor
            .start(&computer)
            .map_err(|e| fail(format!("starting {}", meta.name), e))?;
        // A persona's task, its person's agent on this computer.
        let persona = match &spec.persona {
            Some(id) => Some(
                meta.personas
                    .iter()
                    .find(|p| &p.id == id)
                    .ok_or_else(|| refuse(format!("there's no agent {id} on {}", meta.name)))?
                    .name
                    .clone(),
            ),
            None => None,
        };
        let task = Self::task_dir(&computer, spec);
        fs::create_dir_all(&task).map_err(|e| fail(format!("creating {}", task.display()), e))?;
        Self::trim_screen(&computer);
        let by = persona.map(|name| format!(" {name}")).unwrap_or_default();
        self.screen_line(&computer, &format!("\n── task {} ──{by}\n", spec.name));
        self.state().runs.insert(spec.name.clone(), key);
        Ok(())
    }

    fn command(&self, spec: &SandboxSpec, process: &ProcessSpec) -> Command {
        let (computer, _) = self.run_of(spec);
        let run = TaskRun {
            socket: self.host_path(spec, Path::new(AGENT_SOCKET_PATH)),
            // Each persona starts in a folder of its own; all of the
            // computer is theirs to share (ADR 0068).
            workdir: match &spec.persona {
                Some(persona) if valid_computer_id(persona) => Path::new(WORK).join(persona),
                _ => WORK.into(),
            },
            process: process.clone(),
        };
        match self.hypervisor.command(&computer, &run) {
            Ok(command) => command,
            Err(why) => {
                // The task fails with this as its last line.
                let mut command = Command::new("/bin/sh");
                command.args(["-c", "printf '%s\\n' \"$1\" >&2; exit 125", "sh", &why]);
                command
            }
        }
    }

    fn destroy(&self, spec: &SandboxSpec) -> Result<()> {
        let (computer, fresh) = self.run_of(spec);
        {
            let mut state = self.state();
            state.runs.remove(&spec.name);
            state
                .used
                .insert((spec.uid, computer.id.clone()), Instant::now());
        }
        let task = Self::task_dir(&computer, spec);
        match fs::remove_dir_all(&task) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(fail(format!("removing {}", task.display()), e));
            }
            _ => {}
        }
        if fresh {
            self.hypervisor
                .suspend(&computer)
                .map_err(|e| fail(format!("stopping {}", computer.id), e))?;
            match fs::remove_dir_all(&computer.dir) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(fail(format!("removing {}", computer.dir.display()), e));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn agent_path(&self, _spec: &SandboxSpec, inside: &Path) -> PathBuf {
        inside.to_owned()
    }

    fn host_path(&self, spec: &SandboxSpec, inside: &Path) -> PathBuf {
        let (computer, _) = self.run_of(spec);
        let task = Self::task_dir(&computer, spec);
        if inside == Path::new(AGENT_SOCKET_PATH) {
            return task.join("agent.sock");
        }
        self.hypervisor
            .host_path(&computer, inside)
            .unwrap_or_else(|| task.join(inside.strip_prefix("/").unwrap_or(inside)))
    }

    fn screen(&self, spec: &SandboxSpec) -> Option<PathBuf> {
        Some(self.run_of(spec).0.screen())
    }

    fn cleanup_stale(&self) -> Result<Vec<String>> {
        // Before anything: no process of an earlier glassd's runs is left.
        let mut cleaned = self.hypervisor.stop_leftovers(&self.dir);
        let Ok(owners) = fs::read_dir(&self.dir) else {
            return Ok(cleaned);
        };
        for owner in owners.flatten() {
            let Ok(computers) = fs::read_dir(owner.path()) else {
                continue;
            };
            for entry in computers.flatten() {
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let computer = ComputerSpec {
                    id: id.clone(),
                    dir: entry.path(),
                    memory_mib: self.memory_mib,
                    vcpus: self.vcpus,
                };
                if let Ok(tasks) = fs::read_dir(computer.dir.join("tasks")) {
                    for task in tasks.flatten() {
                        if fs::remove_dir_all(task.path()).is_ok() {
                            cleaned.push(task.file_name().to_string_lossy().into_owned());
                        }
                    }
                }
                match Self::read_meta(&computer) {
                    Some(meta) if meta.fresh => {
                        let _ = self.hypervisor.suspend(&computer);
                        if fs::remove_dir_all(&computer.dir).is_ok() {
                            cleaned.push(id);
                        }
                    }
                    // Asleep, it keeps nothing here: what a sleep glassd
                    // didn't live through left goes (specs/tla/AgentComputers.tla).
                    Some(meta) if meta.stored.is_some() => {
                        let left = fs::read_dir(&computer.dir)
                            .map(|d| d.count() > 1)
                            .unwrap_or(false);
                        if left && Self::clear_but_record(&computer).is_ok() {
                            cleaned.push(format!("{id}'s leftovers"));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(cleaned)
    }
}

impl ToolRunner for ComputerSandbox {
    fn run_tool(&self, run: &ToolRun) -> Result<ToolOutput> {
        UnshareTools.run_tool(run)
    }

    fn runs_as_user(&self) -> bool {
        UnshareTools.runs_as_user()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixListener;

    use super::*;
    use crate::namespaces::NamespaceHypervisor;

    fn sandbox() -> (tempfile::TempDir, ComputerSandbox) {
        let dir = tempfile::Builder::new()
            .prefix("glass-computers-test")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let sandbox = ComputerSandbox::new(
            dir.path().join("computers"),
            Arc::new(NamespaceHypervisor::new(Vec::new())),
            1024,
            1,
        )
        .unwrap();
        (dir, sandbox)
    }

    fn me() -> (u32, u32) {
        let meta = fs::metadata("/proc/self").unwrap();
        (meta.uid(), meta.gid())
    }

    fn task(name: &str, computer: Option<&str>) -> SandboxSpec {
        let (uid, gid) = me();
        SandboxSpec {
            name: name.into(),
            root: "/nonexistent".into(),
            mounts: Vec::new(),
            user: "me".into(),
            uid,
            gid,
            computer: computer.map(str::to_owned),
            persona: None,
        }
    }

    fn shell(script: &str) -> ProcessSpec {
        ProcessSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
        }
    }

    fn reconciler() -> ComputerNew {
        ComputerNew {
            id: "reconciler".into(),
            name: "Reconciler".into(),
            role: "Matches invoices to payments".into(),
            agent: "desktop-agent".parse().unwrap(),
        }
    }

    #[test]
    fn adr0066_an_agent_keeps_its_computer_between_tasks() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let (_dir, sandbox) = sandbox();
        let (uid, gid) = me();
        let made = sandbox.make(uid, gid, &reconciler()).unwrap();
        assert_eq!(made.state, ComputerState::Ready);
        assert!(sandbox.make(uid, gid, &reconciler()).is_err());
        assert!(sandbox.keeps_computers());

        // The first task leaves a file; the socket is the task's own.
        let first = task("glass-0001", Some("reconciler"));
        sandbox.create(&first).unwrap();
        let socket = sandbox.host_path(&first, Path::new(AGENT_SOCKET_PATH));
        let listener = UnixListener::bind(&socket).unwrap();
        assert_eq!(
            sandbox.get(uid, "reconciler").unwrap().state,
            ComputerState::Running
        );
        let out = sandbox
            .command(
                &first,
                &shell("test -S /run/glasshouse/agent.sock && echo 214 > /work/matched && pwd"),
            )
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "/work\n");
        drop(listener);
        sandbox.destroy(&first).unwrap();
        assert!(!socket.exists());
        assert_eq!(
            sandbox.get(uid, "reconciler").unwrap().state,
            ComputerState::Ready
        );

        // The next task finds it; its socket is gone with its task.
        let second = task("glass-0002", Some("reconciler"));
        sandbox.create(&second).unwrap();
        let out = sandbox
            .command(
                &second,
                &shell("cat /work/matched; ls /run/glasshouse | wc -l"),
            )
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "214\n0\n");
        sandbox.destroy(&second).unwrap();
        // Destroy again: nothing to do.
        sandbox.destroy(&second).unwrap();

        let screen = sandbox.screen(uid, "reconciler", 0).unwrap();
        assert!(
            screen.text.contains("── task glass-0001 ──"),
            "{}",
            screen.text
        );
        assert!(screen.text.contains("── task glass-0002 ──"));
        let later = sandbox.screen(uid, "reconciler", screen.offset).unwrap();
        assert!(later.text.is_empty());

        let listed = sandbox.list(uid);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Reconciler");
        assert!(listed[0].disk_bytes > 0);
    }

    #[test]
    fn adr0066_a_task_without_a_computer_gets_one_that_goes_with_it() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let (dir, sandbox) = sandbox();
        let (uid, _) = me();
        let spec = task("glass-0003", None);
        sandbox.create(&spec).unwrap();
        let out = sandbox
            .command(&spec, &shell("echo scratch > /work/x; cat /work/x"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "scratch\n");
        assert!(sandbox.list(uid).is_empty());
        sandbox.destroy(&spec).unwrap();
        let left = dir
            .path()
            .join("computers")
            .join(uid.to_string())
            .join("task-glass-0003");
        assert!(!left.exists());
    }

    #[test]
    fn adr0066_its_person_takes_the_keyboard_and_tasks_wait() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let (_dir, sandbox) = sandbox();
        let (uid, gid) = me();
        sandbox.make(uid, gid, &reconciler()).unwrap();
        assert!(sandbox.input(uid, "reconciler", "ls").is_err());

        sandbox.take_over(uid, "reconciler").unwrap();
        let refused = sandbox.create(&task("glass-0004", Some("reconciler")));
        assert!(refused.unwrap_err().to_string().contains("hand it back"));

        let (mut command, name) = sandbox
            .input(uid, "reconciler", "echo typed > /root/note; cat /root/note")
            .unwrap();
        assert!(sandbox.in_use(uid, "reconciler"));
        assert!(command.status().unwrap().success());
        sandbox.input_done(uid, "reconciler", &name);
        assert!(!sandbox.in_use(uid, "reconciler"));
        let screen = sandbox.screen(uid, "reconciler", 0).unwrap();
        assert!(
            screen.text.contains("root@reconciler:~# echo typed"),
            "{}",
            screen.text
        );
        assert!(screen.text.contains("\ntyped\n"));
        assert!(screen.taken_over);

        sandbox.hand_back(uid, "reconciler").unwrap();
        let spec = task("glass-0005", Some("reconciler"));
        sandbox.create(&spec).unwrap();
        sandbox.destroy(&spec).unwrap();
    }

    fn persona(id: &str, name: &str) -> PersonaNew {
        PersonaNew {
            id: id.into(),
            name: name.into(),
            role: String::new(),
            agent: "desktop-agent".parse().unwrap(),
        }
    }

    fn persona_task(name: &str, persona: &str) -> SandboxSpec {
        SandboxSpec {
            persona: Some(persona.into()),
            ..task(name, Some(glass_api::PERSON_COMPUTER))
        }
    }

    #[test]
    fn adr0068_a_persons_agents_share_their_one_computer() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let (dir, sandbox) = sandbox();
        let blobs = crate::store::FolderBlobs::new(dir.path().join("s3")).unwrap();
        let store = Arc::new(Store::new(Arc::new(blobs)));
        let sandbox = sandbox.with_store(store.clone(), "S3");
        let (uid, gid) = me();

        // The first agent makes their computer; the second joins it.
        let view = sandbox
            .add_persona(
                uid,
                gid,
                "Dana's computer",
                &persona("reconciler", "Reconciler"),
            )
            .unwrap();
        assert_eq!(view.id, glass_api::PERSON_COMPUTER);
        assert_eq!(view.name, "Dana's computer");
        let view = sandbox
            .add_persona(uid, gid, "ignored", &persona("researcher", "Researcher"))
            .unwrap();
        assert_eq!(view.name, "Dana's computer");
        assert_eq!(
            view.personas
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["reconciler", "researcher"]
        );
        assert_eq!(sandbox.list(uid).len(), 1, "one computer, two agents");

        // Both at once, each in a folder of its own, on one disk.
        let a = persona_task("glass-0021", "reconciler");
        let b = persona_task("glass-0022", "researcher");
        sandbox.create(&a).unwrap();
        sandbox.create(&b).unwrap();
        let out_a = sandbox
            .command(&a, &shell("pwd; echo matched > notes.txt"))
            .output()
            .unwrap();
        let out_b = sandbox
            .command(&b, &shell("pwd; cat ../reconciler/notes.txt"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out_a.stdout), "/work/reconciler\n");
        assert_eq!(
            String::from_utf8_lossy(&out_b.stdout),
            "/work/researcher\nmatched\n",
            "one person's agents share their computer"
        );
        sandbox.destroy(&a).unwrap();
        sandbox.destroy(&b).unwrap();
        let screen = sandbox.screen(uid, glass_api::PERSON_COMPUTER, 0).unwrap();
        assert!(
            screen.text.contains("── task glass-0021 ── Reconciler"),
            "{}",
            screen.text
        );
        assert!(screen.text.contains("── task glass-0022 ── Researcher"));

        // A task for an agent the computer doesn't have is refused.
        let stranger = persona_task("glass-0023", "nobody");
        assert!(
            sandbox
                .create(&stranger)
                .unwrap_err()
                .to_string()
                .contains("no agent nobody")
        );

        // Ids and names are each the person's once; a computer keeps 20.
        for (id, name) in [
            ("reconciler", "Other"),
            ("other", "reconciler"),
            ("Bad Id", "X"),
            ("task-x", "X"),
            ("x", ""),
        ] {
            assert!(
                sandbox
                    .add_persona(uid, gid, "", &persona(id, name))
                    .is_err(),
                "{id} {name}"
            );
        }
        for n in 2..glass_api::PERSONAS_MAX {
            sandbox
                .add_persona(
                    uid,
                    gid,
                    "",
                    &persona(&format!("helper-{n}"), &format!("Helper {n}")),
                )
                .unwrap();
        }
        let full = sandbox.add_persona(uid, gid, "", &persona("one-more", "One more"));
        assert!(
            full.unwrap_err()
                .to_string()
                .contains("the most a computer keeps")
        );
        // Another person has none of them.
        let other = uid.wrapping_add(1);
        assert!(
            sandbox
                .persona(other, glass_api::PERSON_COMPUTER, "reconciler")
                .is_err()
        );
        assert!(sandbox.remove_persona(other, "reconciler").is_err());

        // Removed, an agent goes; what it left on the computer stays.
        let view = sandbox.remove_persona(uid, "researcher").unwrap();
        assert!(view.personas.iter().all(|p| p.id != "researcher"));
        assert!(sandbox.remove_persona(uid, "researcher").is_err());

        // Asleep, its agents go with it: another host has them all.
        sandbox.sleep(uid, glass_api::PERSON_COMPUTER).unwrap();
        let (_dir_b, host_b) = self::sandbox();
        let host_b = host_b.with_store(store, "S3");
        let listed = host_b.list(uid);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].personas.len(), glass_api::PERSONAS_MAX - 1);
        assert!(
            host_b
                .persona(uid, glass_api::PERSON_COMPUTER, "reconciler")
                .is_ok()
        );
    }

    #[test]
    fn adr0065_another_host_wakes_a_computer_from_its_record_in_the_store() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let (dir, host_a) = sandbox();
        let blobs = crate::store::FolderBlobs::new(dir.path().join("s3")).unwrap();
        let store = Arc::new(Store::new(Arc::new(blobs)));
        let host_a = host_a.with_store(store.clone(), "S3");
        let (_dir_b, host_b) = sandbox();
        let host_b = host_b.with_store(store.clone(), "S3");
        let (uid, gid) = me();

        // Made on host A, worked on, and asleep: host A is replaced.
        let made = host_a.make(uid, gid, &reconciler()).unwrap();
        let spec = task("glass-0011", Some("reconciler"));
        host_a.create(&spec).unwrap();
        let out = host_a
            .command(&spec, &shell("seq 1 300 > /work/ledger.csv"))
            .output()
            .unwrap();
        assert!(out.status.success());
        host_a.destroy(&spec).unwrap();
        host_a.leave();
        let refused = host_a.create(&task("glass-0012", Some("reconciler")));
        assert!(refused.unwrap_err().to_string().contains("being replaced"));
        assert_eq!(
            host_a.sleep(uid, "reconciler").unwrap().state,
            ComputerState::Asleep
        );
        assert!(
            host_a.wake(uid, gid, "reconciler").is_err(),
            "leaving, it won't wake"
        );

        // Host B never had it: its person sees it, asleep, and a task wakes
        // it with its files.
        let listed = host_b.list(uid);
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (
                listed[0].id.as_str(),
                listed[0].name.as_str(),
                listed[0].state
            ),
            ("reconciler", "Reconciler", ComputerState::Asleep)
        );
        assert_eq!(listed[0].created_at, made.created_at);
        let spec = task("glass-0013", Some("reconciler"));
        host_b.create(&spec).unwrap();
        let out = host_b
            .command(&spec, &shell("wc -l < /work/ledger.csv"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "300\n");
        host_b.destroy(&spec).unwrap();

        // Two computers by one id never share versions: each has a key.
        let (_dir_c, host_c) = sandbox();
        let host_c = host_c.with_store(store.clone(), "S3");
        let other = ComputerNew {
            id: "scratch".into(),
            ..reconciler()
        };
        host_c.make(uid, gid, &other).unwrap();
        host_c.sleep(uid, "scratch").unwrap();
        let owner = format!("u{uid}-");
        let keys: Vec<String> = store
            .records(&owner)
            .unwrap()
            .into_iter()
            .map(|r| r.0)
            .collect();
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert!(keys.iter().all(|k| k.len() == owner.len() + 16), "{keys:?}");
        for key in &keys {
            assert_eq!(store.versions(key).unwrap().len(), 1);
        }
    }

    #[test]
    fn adr0065_a_computer_sleeps_to_the_store_and_a_task_wakes_it() {
        if let Err(why) = NamespaceHypervisor::available() {
            eprintln!("skipped: {why}");
            return;
        }
        let (dir, sandbox) = sandbox();
        let blobs = crate::store::FolderBlobs::new(dir.path().join("s3")).unwrap();
        let store = Arc::new(Store::new(Arc::new(blobs)));
        let sandbox = sandbox.with_store(store.clone(), "a folder");
        let (uid, gid) = me();
        sandbox.make(uid, gid, &reconciler()).unwrap();
        let first = task("glass-0007", Some("reconciler"));
        sandbox.create(&first).unwrap();
        let out = sandbox
            .command(
                &first,
                &shell("seq 1 5000 > /work/ledger.csv; mkdir -p /opt/venv"),
            )
            .output()
            .unwrap();
        assert!(out.status.success());
        // Working, it doesn't sleep.
        assert!(sandbox.sleep(uid, "reconciler").is_err());
        sandbox.destroy(&first).unwrap();
        assert_eq!(
            sandbox.idle(Duration::ZERO),
            [(uid, "reconciler".to_owned())]
        );
        assert!(sandbox.idle(Duration::from_secs(3600)).is_empty());

        let asleep = sandbox.sleep(uid, "reconciler").unwrap();
        assert_eq!(asleep.state, ComputerState::Asleep);
        let stored = asleep.stored.clone().unwrap();
        assert_eq!(stored.place, "a folder");
        assert!(stored.bytes > 20_000, "{stored:?}");
        // Nothing of it left on this host but its record.
        let folder = dir
            .path()
            .join("computers")
            .join(uid.to_string())
            .join("reconciler");
        let left: Vec<String> = fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["computer.json"]);
        assert_eq!(sandbox.get(uid, "reconciler").unwrap().disk_bytes, 0);
        assert!(sandbox.idle(Duration::ZERO).is_empty());

        // A task wakes it, its files as they were.
        let second = task("glass-0008", Some("reconciler"));
        sandbox.create(&second).unwrap();
        let out = sandbox
            .command(
                &second,
                &shell("wc -l < /work/ledger.csv; ls -d /opt/venv; id -u"),
            )
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "5000\n/opt/venv\n0\n");
        sandbox.destroy(&second).unwrap();
        assert_eq!(
            sandbox.get(uid, "reconciler").unwrap().state,
            ComputerState::Ready
        );
        let screen = sandbox.screen(uid, "reconciler", 0).unwrap();
        assert!(
            screen.text.contains("── task glass-0007 ──"),
            "the screen sleeps too"
        );

        // glassd died halfway through a wake: some of it back, the record
        // still asleep. The next wake starts over from the store.
        sandbox.sleep(uid, "reconciler").unwrap();
        fs::create_dir_all(folder.join("disk/work")).unwrap();
        fs::write(folder.join("disk/work/ledger.csv"), "half\n").unwrap();
        fs::create_dir_all(folder.join(".waking/disk")).unwrap();
        sandbox.wake(uid, gid, "reconciler").unwrap();
        let ledger = fs::read_to_string(folder.join("disk/work/ledger.csv")).unwrap();
        assert_eq!(ledger.lines().count(), 5000);

        // glassd died halfway through clearing a sleep: its next start
        // clears it.
        sandbox.sleep(uid, "reconciler").unwrap();
        fs::create_dir_all(folder.join("disk/work")).unwrap();
        assert!(
            sandbox
                .cleanup_stale()
                .unwrap()
                .iter()
                .any(|c| c.contains("leftovers"))
        );
        assert!(!folder.join("disk").exists());
        sandbox.wake(uid, gid, "reconciler").unwrap();

        // Three sleeps later, only the last three versions are kept.
        for _ in 0..3 {
            sandbox.sleep(uid, "reconciler").unwrap();
            sandbox.wake(uid, gid, "reconciler").unwrap();
        }
        let records = store.records(&format!("u{uid}-")).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(store.versions(&records[0].0).unwrap().len(), 3);
    }

    #[test]
    fn adr0066_one_persons_computer_is_absent_to_another() {
        let (_dir, sandbox) = sandbox();
        let (uid, gid) = me();
        sandbox.make(uid, gid, &reconciler()).unwrap();
        let other = uid.wrapping_add(1);
        assert!(sandbox.get(other, "reconciler").is_err());
        assert!(sandbox.list(other).is_empty());
        let mut spec = task("glass-0006", Some("reconciler"));
        spec.uid = other;
        assert!(sandbox.create(&spec).is_err());
        for bad in ["../x", "Reconciler", "task-x"] {
            let new = ComputerNew {
                id: bad.into(),
                ..reconciler()
            };
            assert!(sandbox.make(uid, gid, &new).is_err(), "{bad}");
        }
    }
}
