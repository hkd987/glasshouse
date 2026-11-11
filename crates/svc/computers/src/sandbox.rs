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
//!
//! One host at a time has a computer awake (ADR 0066): the host that makes
//! or wakes one takes its lease in the store ([`crate::store::Lease`]),
//! renews it while the computer is awake here
//! ([`ComputerSandbox::keep_leases`]) and gives it up once it's asleep.
//! What a computer is, asleep, is what the store's record says, not what a
//! host remembers of it: a wake reads the record, and a change to a
//! sleeping computer's record is made there. Only the host that holds the
//! lease writes a version or the record. A host that finds another has
//! taken a computer it still has awake (it was cut off from the store for
//! longer than a lease lasts) saves nothing over it: its copy is set aside
//! on this host, and the record says so.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use glass_api::{
    ComputerNew, ComputerState, ComputerView, DisplayInput, PERSONAS_MAX, PersonaNew, PersonaView,
    ScreenView, StoredView,
};
use glass_platform::sandbox::{
    AGENT_SOCKET_PATH, ProcessSpec, Sandbox, SandboxSpec, Swept, ToolOutput, ToolRun, ToolRunner,
    UnshareTools, valid_computer_id,
};
use glass_platform::{PlatformError, Result};
use glass_types::{AgentId, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hypervisor::{ComputerSpec, Frame, Hypervisor, TaskRun};
use crate::store::{Claim, LEASE_FOR, Lease, Store, TRUST_FOR};

/// Where a task works inside its computer: kept, like the rest of it.
pub const WORK: &str = "/work";

/// Versions of a computer kept in the store: the newest, and two before.
const VERSIONS_KEPT: usize = 3;

/// Where a copy of a computer goes that another host took over while
/// this one had it awake, under the computers' folder; and for how long.
const ASIDE: &str = ".aside";
const ASIDE_KEPT: Duration = Duration::from_secs(7 * 24 * 3600);

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
    /// This host's name in computers' leases (ADR 0066): its own, for as
    /// long as it lives.
    host: String,
    /// How long another host's lease holds after its last renewal, and for
    /// how long this host acts on its own without asking the store again.
    lease_for: Duration,
    trust_for: Duration,
    owners: OwnerKeys,
    state: Mutex<State>,
    /// Computers with a display booting in the background, by folder: one
    /// boot each (ADR 0072).
    booting: Arc<Mutex<HashSet<PathBuf>>>,
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
    /// The leases this host holds, on the computers awake here.
    held: HashMap<(u32, String), Held>,
    /// Computers awake here that another host has taken: no task starts
    /// in one, and it's set aside once its tasks have ended.
    lost: HashMap<(u32, String), Lease>,
}

/// A lease this host holds, and when the store last took word of it.
#[derive(Debug, Clone)]
struct Held {
    lease: Lease,
    confirmed: Instant,
}

/// What keeping this host's leases found, for the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseNews {
    /// Another host took a computer this one still has awake: nothing of
    /// it is saved from here, and its tasks here should stop.
    Lost {
        uid: u32,
        id: String,
        name: String,
        /// The host that has it now.
        host: String,
    },
    /// This host's copy of such a computer is now in `aside`, and the
    /// computer is listed here as the store has it.
    SetAside {
        uid: u32,
        id: String,
        name: String,
        host: String,
        aside: PathBuf,
    },
    /// A lease couldn't be renewed or asked for: the store didn't answer.
    Trouble {
        uid: u32,
        id: String,
        name: String,
        why: String,
    },
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
        // A computer runs as its owner, who must get through every folder
        // above it to reach its disk: one closed to them (glassd's own
        // state, 0700) would stop every computer but root's.
        let dir = dir
            .canonicalize()
            .map_err(|e| fail(format!("reading {}", dir.display()), e))?;
        for above in dir.ancestors().skip(1) {
            let mode = fs::metadata(above)
                .map_err(|e| fail(format!("reading {}", above.display()), e))?
                .permissions()
                .mode();
            if mode & 0o001 == 0 {
                return Err(refuse(format!(
                    "computers can't live in {}: {} is closed to their owners (mode {:o}); \
                     put them where every folder above lets others through",
                    dir.display(),
                    above.display(),
                    mode & 0o7777
                )));
            }
        }
        let host = own_host_name(&dir)?;
        Ok(Self {
            dir,
            hypervisor,
            memory_mib,
            vcpus,
            store: None,
            host,
            lease_for: LEASE_FOR,
            trust_for: TRUST_FOR,
            owners: OwnerKeys::by_uid(),
            state: Mutex::new(State::default()),
            booting: Arc::default(),
        })
    }

    /// Owners' keys in the store from `owners` instead of their uids.
    #[must_use]
    pub fn with_owner_keys(mut self, owners: OwnerKeys) -> Self {
        self.owners = owners;
        self
    }

    /// This host's name in computers' leases: its instance id on AWS. By
    /// default it's a name made once and kept under the computers' folder,
    /// so two hosts never share one.
    #[must_use]
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// A host that asks the store every time, for tests.
    #[cfg(test)]
    fn trusting_for(mut self, trust_for: Duration) -> Self {
        self.trust_for = trust_for;
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
            owner: uid,
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
        // Made on another host and awake there since: its record is in the
        // store, though nothing of it is, to bring here.
        if self
            .stored_records(uid)
            .is_ok_and(|records| records.iter().any(|record| record.id == new.id))
        {
            return Err(refuse(format!(
                "you already have a computer called {}: it's awake on another host",
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
        // Its lease and its record in the store from the start: another
        // host knows the id is taken and that it's awake here. A store
        // that doesn't answer now is asked again as leases are kept.
        if let Some((store, _)) = &self.store {
            let _ = self.announce(store, uid, &meta);
        }
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
        let persona = PersonaView {
            id: new.id.clone(),
            name,
            role: glass_api::display_line(new.role.trim()).into_owned(),
            agent: new.agent.clone(),
            created_at: Timestamp::now(),
        };
        let (computer, meta) = self.change_record(uid, id, |meta| {
            if meta.personas.iter().any(|p| p.id == persona.id) {
                return Err(refuse(format!(
                    "you already have an agent called {}",
                    persona.id
                )));
            }
            if meta
                .personas
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&persona.name))
            {
                return Err(refuse(format!(
                    "you already have an agent named {}",
                    persona.name
                )));
            }
            if meta.personas.len() >= PERSONAS_MAX {
                return Err(refuse(format!(
                    "you have {PERSONAS_MAX} agents, the most a computer keeps: remove one first"
                )));
            }
            meta.personas.push(persona.clone());
            Ok(())
        })?;
        Ok(self.view_of(uid, &computer, meta))
    }

    /// The agent goes from the person's computer; what it left there stays.
    pub fn remove_persona(&self, uid: u32, id: &str) -> Result<ComputerView> {
        let (computer, meta) = self.change_record(uid, glass_api::PERSON_COMPUTER, |meta| {
            let before = meta.personas.len();
            meta.personas.retain(|p| p.id != id);
            if meta.personas.len() == before {
                return Err(PlatformError::Unsupported(format!(
                    "an agent called {id} for you"
                )));
            }
            Ok(())
        })?;
        Ok(self.view_of(uid, &computer, meta))
    }

    /// Changes a computer's record where the record lives.
    ///
    /// Awake here, that's this host: the store's record follows as it
    /// next sleeps. Asleep, it's the store's record, which a wake on any
    /// host goes by: the change is made there, holding the computer's
    /// lease for as long as it takes, so no other host wakes it or changes
    /// it in the middle (ADR 0066).
    fn change_record(
        &self,
        uid: u32,
        id: &str,
        change: impl FnOnce(&mut ComputerMeta) -> Result<()>,
    ) -> Result<(ComputerSpec, ComputerMeta)> {
        let (computer, meta) = self.find(uid, id)?;
        let asleep_in_store = meta.stored.is_some() && !meta.key.is_empty();
        let Some((store, _)) = self.store.as_ref().filter(|_| asleep_in_store) else {
            // Its record changes only while nothing else rewrites it: a
            // sleep or a wake takes it first (`moving`), under this lock.
            let state = self.state();
            if state.moving.contains(&(uid, id.to_owned())) {
                return Err(refuse(
                    "your computer is going to sleep or waking; try again in a moment",
                ));
            }
            let mut meta = Self::read_meta(&computer).ok_or_else(|| {
                PlatformError::Unsupported(format!("a computer called {id} for you"))
            })?;
            change(&mut meta)?;
            Self::write_meta(&computer, &meta)?;
            drop(state);
            return Ok((computer, meta));
        };
        let moving = self.moving(uid, id)?;
        let key = Self::store_key(uid, &meta);
        self.take(store, uid, &meta)?;
        let changed = (|| {
            let mut meta = Self::stored_record(store, &key, id)?.unwrap_or(meta);
            change(&mut meta)?;
            let record = serde_json::to_vec(&meta).map_err(|e| fail("a computer's record", e))?;
            store
                .put_record(&key, &record)
                .map_err(|e| fail(format!("keeping {}'s record", meta.name), e))?;
            Self::write_meta(&computer, &meta)?;
            Ok(meta)
        })();
        self.release(store, uid, id, &key);
        drop(moving);
        Ok((computer, changed?))
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

    /// The owner's computers' records in the store: each names its own
    /// computer, an agent's (a task's own computer never sleeps).
    fn stored_records(&self, uid: u32) -> Result<Vec<ComputerMeta>> {
        let Some((store, _)) = &self.store else {
            return Ok(Vec::new());
        };
        let owner = self.owners.of(uid)?;
        let records = store
            .records(&format!("{owner}-"))
            .map_err(|e| fail("looking for your computers in the store", e))?;
        Ok(records
            .into_iter()
            .filter_map(|(key, record)| {
                let meta = serde_json::from_slice::<ComputerMeta>(&record).ok()?;
                (meta.key == key && valid_computer_id(&meta.id) && !meta.fresh).then_some(meta)
            })
            .collect())
    }

    /// `id`'s record in the store under `key`, if the store has one.
    fn stored_record(store: &Store, key: &str, id: &str) -> Result<Option<ComputerMeta>> {
        let Some(record) = store
            .record(key)
            .map_err(|e| fail(format!("reading {id}'s record in the store"), e))?
        else {
            return Ok(None);
        };
        let meta: ComputerMeta = serde_json::from_slice(&record)
            .map_err(|e| fail(format!("reading {id}'s record in the store"), e))?;
        if meta.key != key || meta.id != id || meta.fresh {
            return Err(refuse(format!(
                "{id}'s record in the store is another computer's"
            )));
        }
        Ok(Some(meta))
    }

    /// Brings the owner's computers this host doesn't have from the
    /// store, asleep: every one, or only `only`. A computer here by the
    /// same id stays as it is. Returns how many it brought.
    pub fn adopt(&self, uid: u32, only: Option<&str>) -> Result<usize> {
        if self.store.is_none() {
            return Ok(0);
        }
        let gid = glass_platform::users::by_uid(uid).map_or(uid, |u| u.gid);
        let mut adopted = 0;
        for meta in self.stored_records(uid)? {
            // Asleep: one that never slept is all on the host that made it.
            if meta.stored.is_none() || only.is_some_and(|id| id != meta.id) {
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
            display: self.hypervisor.display(),
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
            let Some(uid) = owner_uid(&owner) else {
                continue;
            };
            for entry in fs::read_dir(owner.path()).into_iter().flatten().flatten() {
                let computer = ComputerSpec {
                    id: entry.file_name().to_string_lossy().into_owned(),
                    owner: uid,
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

    /// Its person takes the keyboard: new tasks are refused, saying so,
    /// until they hand it back.
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

    /// What its display shows (ADR 0072): its owner's only. Opening it
    /// turns it on, as opening a laptop does: until it's up, it says so.
    /// `since` the version they have, the picture is left out.
    pub fn frame(&self, uid: u32, id: &str, since: Option<&str>) -> Result<(Frame, String, bool)> {
        let (computer, meta) = self.find(uid, id)?;
        if !self.hypervisor.display() {
            return Err(refuse(
                "this computer's screen is a terminal: it has no desktop to show",
            ));
        }
        if meta.stored.is_some() {
            return Err(refuse(format!(
                "{} is asleep: wake it to see it",
                meta.name
            )));
        }
        if !self.hypervisor.running(&computer) || self.is_booting(&computer) {
            self.power_on(&computer);
            return Err(refuse(
                "Your computer is starting. It shows here as soon as it's up.",
            ));
        }
        let frame = self.hypervisor.frame(&computer).map_err(refuse)?;
        let version = hex::encode(&Sha256::digest(&frame.png)[..8]);
        let unchanged = since == Some(version.as_str());
        Ok((frame, version, unchanged))
    }

    fn is_booting(&self, computer: &ComputerSpec) -> bool {
        self.booting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&computer.dir)
    }

    /// Starts a computer with a display in the background, once: a VM
    /// takes seconds to boot, minutes where there's no KVM.
    fn power_on(&self, computer: &ComputerSpec) {
        if self.leaving() || self.is_booting(computer) {
            return;
        }
        self.booting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(computer.dir.clone());
        let (hypervisor, booting, computer) = (
            Arc::clone(&self.hypervisor),
            Arc::clone(&self.booting),
            computer.clone(),
        );
        std::thread::spawn(move || {
            if let Err(why) = hypervisor.start(&computer)
                && let Ok(mut file) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(computer.screen())
            {
                let _ = writeln!(file, "\n── couldn't start: {why} ──");
            }
            booting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&computer.dir);
        });
    }

    /// Its person's pointer and keys on its display, while they have the
    /// keyboard: refused otherwise, as typed lines are.
    pub fn display_input(&self, uid: u32, id: &str, events: &[DisplayInput]) -> Result<()> {
        let (computer, meta) = self.find(uid, id)?;
        if !self.hypervisor.display() {
            return Err(refuse(
                "this computer's screen is a terminal: type lines on it",
            ));
        }
        if !self.taken_over(uid, id) {
            return Err(refuse(format!(
                "take over {} first: it's {}'s keyboard until then",
                meta.name, meta.name
            )));
        }
        if meta.stored.is_some() || !self.hypervisor.running(&computer) {
            return Err(refuse(format!(
                "{} isn't running: wake it first",
                meta.name
            )));
        }
        if events.is_empty() || events.len() > glass_api::DISPLAY_INPUT_MAX {
            return Err(refuse(format!(
                "1 to {} input events at a time",
                glass_api::DISPLAY_INPUT_MAX
            )));
        }
        self.hypervisor
            .send_input(&computer, events)
            .map_err(refuse)
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
        let _ = glass_platform::fs::remove_tree(&computer.dir.join("tasks").join(name));
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

    /// The rest of a task's start, its run counted: the computer started,
    /// the task's folder made.
    fn start_run(&self, spec: &SandboxSpec, computer: &ComputerSpec, name: &str) -> Result<()> {
        // Its record as it is now nothing moves it: it was looked at
        // before its run counted, and may have gone to sleep or been set
        // aside since. A wake just now brought the store's record, with
        // the agents it has there.
        let meta = Self::read_meta(computer)
            .filter(|meta| meta.stored.is_none())
            .ok_or_else(|| {
                refuse(format!(
                    "{name} went to sleep as its task started; try again in a moment"
                ))
            })?;
        self.hypervisor
            .start(computer)
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
        let task = Self::task_dir(computer, spec);
        fs::create_dir_all(&task).map_err(|e| fail(format!("creating {}", task.display()), e))?;
        Self::trim_screen(computer);
        let by = persona.map(|name| format!(" {name}")).unwrap_or_default();
        self.screen_line(computer, &format!("\n── task {} ──{by}\n", spec.name));
        Ok(())
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
    ///
    /// Only the host that has its lease saves it (ADR 0066): one that
    /// another host took the computer from saves nothing over that
    /// host's.
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
        self.sure(store, uid, &meta)?;
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
        // A save can take minutes, and the record is what a wake goes by:
        // it's written only if the computer is still this host's. If it
        // isn't, the version just saved is one no record names.
        self.sure(store, uid, &meta)?;
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
        // Asleep in the store: any host may wake it.
        self.release(store, uid, id, &key);
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
                glass_platform::fs::remove_tree(&path).map_err(std::io::Error::other)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
        Ok(())
    }

    /// Brings it back from the store as it was, its files its owner's.
    ///
    /// What wakes is what the store's record says, not what this host
    /// remembers: another host may have woken it and put it to sleep
    /// again since, as a newer version, with agents added. And it wakes
    /// on one host at a time (ADR 0066): this one takes its lease first,
    /// and is refused while another has it.
    pub fn wake(&self, uid: u32, gid: u32, id: &str) -> Result<ComputerView> {
        let (store, _) = self
            .store
            .as_ref()
            .ok_or_else(|| refuse("this host has nowhere computers sleep"))?;
        let (computer, meta) = self.find(uid, id)?;
        if meta.stored.is_none() {
            return Ok(self.view_of(uid, &computer, meta));
        }
        self.refuse_if_leaving()?;
        let moving = self.moving(uid, id)?;
        let meta = Self::read_meta(&computer).unwrap_or(meta);
        let key = Self::store_key(uid, &meta);
        self.take(store, uid, &meta)?;
        let woken = self
            .wake_taken(store, &computer, meta, &key, uid, gid)
            .and_then(|mut record| {
                // A long wake's lease is renewed as it goes (`keep_leases`).
                // One this host couldn't renew for the whole of a lease may
                // be another host's by now: then it isn't awake here.
                match self.still_ours(store, uid, &record)? {
                    Ok(_) => {
                        record.stored = None;
                        Self::write_meta(&computer, &record)?;
                        Ok(record)
                    }
                    Err(theirs) => Err(refuse(format!(
                        "{} was taken by another host ({}) while it was waking here: \
                         ask again in a moment",
                        record.name, theirs.host
                    ))),
                }
            });
        let meta = match woken {
            Ok(meta) => meta,
            Err(e) => {
                // Not awake here after all: what came of it goes, and
                // another host may try.
                let _ = Self::clear_but_record(&computer);
                self.release(store, uid, id, &key);
                return Err(e);
            }
        };
        drop(moving);
        self.state()
            .used
            .insert((uid, id.to_owned()), Instant::now());
        Ok(self.view_of(uid, &computer, meta))
    }

    /// The rest of a wake, with the computer's lease held: all of it back
    /// in its folder. Returns the store's record of it; this host's own
    /// still says asleep.
    fn wake_taken(
        &self,
        store: &Store,
        computer: &ComputerSpec,
        local: ComputerMeta,
        key: &str,
        uid: u32,
        gid: u32,
    ) -> Result<ComputerMeta> {
        // The store's record, now nobody else can be writing it. A
        // computer from before records were kept has only this host's.
        let meta = match Self::stored_record(store, key, &computer.id)? {
            Some(record) => record,
            None if local.key.is_empty() => local,
            None => {
                return Err(refuse(format!(
                    "{} has no record in the store any more: there's nothing to wake",
                    local.name
                )));
            }
        };
        let Some(stored) = meta.stored.clone() else {
            return Err(refuse(format!(
                "{} never slept: the host it was on went away with all of it",
                meta.name
            )));
        };
        // Asleep, the store holds all of it: anything here is what a wake
        // or a sleep that glassd didn't live through left, and goes.
        Self::clear_but_record(computer).map_err(|e| {
            fail(
                format!("clearing what an earlier wake left of {}", meta.name),
                e,
            )
        })?;
        let waking = computer.dir.join(".waking");
        store
            .restore_tree(key, &stored.version, &waking)
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
        Ok(meta)
    }

    /// Asks the store for `meta`'s lease for this host, which doesn't have
    /// the computer awake: refused, in words, while another host has.
    fn take(&self, store: &Store, uid: u32, meta: &ComputerMeta) -> Result<Lease> {
        let key = Self::store_key(uid, meta);
        let asked = Instant::now();
        let claim = store
            .claim(&key, &self.host, Timestamp::now(), self.lease_for)
            .map_err(|e| fail(format!("asking the store who has {}", meta.name), e))?;
        match claim {
            Claim::Held(lease) => {
                self.hold(uid, &meta.id, lease.clone(), asked);
                Ok(lease)
            }
            Claim::Elsewhere(lease) => Err(refuse(format!(
                "{} is awake on another host ({}). It can wake here once it sleeps there, \
                 or a couple of minutes after that host goes away: ask again then",
                meta.name, lease.host
            ))),
        }
    }

    /// Makes sure `meta`, awake here, is still this host's before anything
    /// of it is written to the store. Another host having had it since is
    /// the end of this host's copy ([`Self::lose`]).
    fn sure(&self, store: &Store, uid: u32, meta: &ComputerMeta) -> Result<Lease> {
        let lost = self.state().lost.get(&(uid, meta.id.clone())).cloned();
        if let Some(lease) = lost {
            return Err(refuse(taken_words(&meta.name, &lease)));
        }
        match self.still_ours(store, uid, meta)? {
            Ok(lease) => Ok(lease),
            Err(theirs) => {
                let why = taken_words(&meta.name, &theirs);
                self.lose(uid, &meta.id, theirs);
                Err(refuse(why))
            }
        }
    }

    /// Whether `meta`'s lease is still this host's: it's held and the
    /// store took word of it lately, or it's asked for again, which the
    /// store grants only if no other host has had the computer since.
    /// `Ok(Err(lease))` is that host's lease.
    fn still_ours(
        &self,
        store: &Store,
        uid: u32,
        meta: &ComputerMeta,
    ) -> Result<std::result::Result<Lease, Lease>> {
        let at = (uid, meta.id.clone());
        let held = self.state().held.get(&at).cloned();
        if let Some(held) = held
            && held.confirmed.elapsed() < self.trust_for
        {
            return Ok(Ok(held.lease));
        }
        let key = Self::store_key(uid, meta);
        let asked = Instant::now();
        let claim = store
            .reclaim(&key, &self.host, Timestamp::now())
            .map_err(|e| fail(format!("asking the store who has {}", meta.name), e))?;
        Ok(match claim {
            Claim::Held(lease) => {
                self.hold(uid, &meta.id, lease.clone(), asked);
                Ok(lease)
            }
            Claim::Elsewhere(lease) => {
                self.state().held.remove(&at);
                Err(lease)
            }
        })
    }

    fn hold(&self, uid: u32, id: &str, lease: Lease, confirmed: Instant) {
        self.state()
            .held
            .insert((uid, id.to_owned()), Held { lease, confirmed });
    }

    /// Another host has the computer: nothing of this host's copy is saved
    /// from now, no task starts in it, and its person's keyboard is theirs
    /// no longer. It's set aside once its tasks have ended
    /// ([`Self::keep_leases`]).
    fn lose(&self, uid: u32, id: &str, lease: Lease) {
        let at = (uid, id.to_owned());
        let mut state = self.state();
        state.held.remove(&at);
        state.taken_over.remove(&at);
        state.lost.insert(at, lease);
    }

    /// Gives up this host's lease on a computer it no longer has awake. A
    /// store that doesn't take it leaves the lease to run out: another
    /// host waits a couple of minutes longer to wake the computer.
    fn release(&self, store: &Store, uid: u32, id: &str, key: &str) {
        let held = self.state().held.remove(&(uid, id.to_owned()));
        if let Some(held) = held {
            let _ = store.release(key, &held.lease, Timestamp::now());
        }
    }

    /// Takes a computer's lease as it's made here, and puts its record in
    /// the store.
    fn announce(
        &self,
        store: &Store,
        uid: u32,
        meta: &ComputerMeta,
    ) -> std::result::Result<(), String> {
        self.sure(store, uid, meta).map_err(|e| e.to_string())?;
        if meta.key.is_empty() {
            return Ok(());
        }
        let key = Self::store_key(uid, meta);
        if store.record(&key)?.is_none() {
            let record = serde_json::to_vec(meta).map_err(|e| e.to_string())?;
            store.put_record(&key, &record)?;
        }
        Ok(())
    }

    /// Removes from the store the chunks no computer's version names
    /// (ADR 0066): what sleeps leave behind as versions are trimmed.
    /// glassd calls it once a day. `None` on a host with no store.
    pub fn collect(&self) -> Option<std::result::Result<crate::store::Collected, String>> {
        let (store, _) = self.store.as_ref()?;
        Some(store.gc())
    }

    /// Keeps this host's leases (ADR 0066): glassd calls it every
    /// [`crate::store::RENEW_EVERY`].
    ///
    /// - A lease the store took word of lately is renewed: a computer
    ///   awake here, or one on its way (a wake or a save can take minutes).
    /// - One it didn't (the store was out of reach for a while), or a
    ///   computer awake from before this glassd started, is asked for
    ///   again: it's still this host's only if no other host has had it.
    /// - A computer another host has taken is set aside once nothing runs
    ///   in it: its folder goes to `.aside/` under the computers' folder,
    ///   kept a week, and the computer is listed here as the store has
    ///   it.
    ///
    /// Returns what it found that belongs on the record.
    pub fn keep_leases(&self) -> Vec<LeaseNews> {
        let mut news = Vec::new();
        let Some((store, _)) = &self.store else {
            return news;
        };
        let mut all = self.awake_here();
        for at in self.state().held.keys() {
            if !all.contains(at) {
                all.push(at.clone());
            }
        }
        for (uid, id) in all {
            let computer = self.spec(uid, &id);
            let at = (uid, id.clone());
            let Some(meta) = Self::read_meta(&computer) else {
                self.state().held.remove(&at);
                continue;
            };
            let key = Self::store_key(uid, &meta);
            let trouble = |why: String| LeaseNews::Trouble {
                uid,
                id: id.clone(),
                name: meta.name.clone(),
                why,
            };
            let (held, lost, moving) = {
                let state = self.state();
                (
                    state.held.get(&at).cloned(),
                    state.lost.get(&at).cloned(),
                    state.moving.contains(&at),
                )
            };
            if meta.stored.is_some() {
                match held {
                    // On its way here, or its record being changed: the
                    // lease is kept while that takes. One the store says
                    // is taken is forgotten, and the wake finds out.
                    Some(held) if moving => {
                        if held.confirmed.elapsed() < self.trust_for {
                            let asked = Instant::now();
                            match store.renew(&key, &held.lease, Timestamp::now()) {
                                Ok(Some(lease)) => self.renewed(store, &at, &key, lease, asked),
                                Ok(None) => {
                                    self.state().held.remove(&at);
                                }
                                Err(why) => news.push(trouble(why)),
                            }
                        }
                    }
                    // Asleep, with nothing under way: a lease still in hand
                    // is given up.
                    _ => self.release(store, uid, &id, &key),
                }
                continue;
            }
            if let Some(lease) = lost {
                match self.set_aside(uid, &computer, &meta, &lease) {
                    Ok(Some(aside)) => news.push(aside),
                    Ok(None) => {}
                    Err(e) => news.push(trouble(e.to_string())),
                }
                continue;
            }
            let first = held.is_none();
            let kept = match held {
                Some(held) if held.confirmed.elapsed() < self.trust_for => {
                    let asked = Instant::now();
                    match store.renew(&key, &held.lease, Timestamp::now()) {
                        Ok(Some(lease)) => {
                            self.renewed(store, &at, &key, lease, asked);
                            Ok(())
                        }
                        // Taken, though this host renewed in time by its
                        // own clock: the next ask finds who has it.
                        Ok(None) => {
                            self.state().held.remove(&at);
                            self.sure(store, uid, &meta).map(|_| ())
                        }
                        Err(why) => {
                            news.push(trouble(why));
                            continue;
                        }
                    }
                }
                _ => self.sure(store, uid, &meta).map(|_| ()),
            };
            match kept {
                Ok(()) => {
                    // Went to sleep while this asked: it isn't held awake.
                    let asleep = Self::read_meta(&computer).is_none_or(|m| m.stored.is_some());
                    if asleep && !self.state().moving.contains(&at) {
                        self.release(store, uid, &id, &key);
                    } else if first && !meta.key.is_empty() {
                        // Made while the store didn't answer, or before
                        // records were kept from the start.
                        if let Ok(None) = store.record(&key)
                            && let Ok(record) = serde_json::to_vec(&meta)
                        {
                            let _ = store.put_record(&key, &record);
                        }
                    }
                }
                Err(e) => match self.state().lost.get(&at) {
                    Some(lease) => news.push(LeaseNews::Lost {
                        uid,
                        id: id.clone(),
                        name: meta.name.clone(),
                        host: lease.host.clone(),
                    }),
                    None => news.push(trouble(e.to_string())),
                },
            }
        }
        news
    }

    /// A renewal the store took: kept, unless the computer went to sleep
    /// (and its lease was given up) while the renewal was on its way, which
    /// wrote the lease as held again.
    fn renewed(&self, store: &Store, at: &(u32, String), key: &str, lease: Lease, asked: Instant) {
        let mut state = self.state();
        match state.held.get_mut(at) {
            Some(held) if held.lease.term == lease.term => {
                held.lease = lease;
                held.confirmed = asked;
            }
            _ => {
                drop(state);
                let _ = store.release(key, &lease, Timestamp::now());
            }
        }
    }

    /// Agents' computers awake on this host: every owner's.
    fn awake_here(&self) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        let Ok(owners) = fs::read_dir(&self.dir) else {
            return out;
        };
        for owner in owners.flatten() {
            let Some(uid) = owner_uid(&owner) else {
                continue;
            };
            let Ok(computers) = fs::read_dir(owner.path()) else {
                continue;
            };
            for entry in computers.flatten() {
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let awake = Self::read_meta(&self.spec(uid, &id))
                    .is_some_and(|meta| !meta.fresh && meta.stored.is_none());
                if awake {
                    out.push((uid, id));
                }
            }
        }
        out
    }

    /// Sets this host's copy of a computer another host has taken aside,
    /// once nothing runs in it, and lists the computer here as the store
    /// has it. `None` while something still runs in it.
    fn set_aside(
        &self,
        uid: u32,
        computer: &ComputerSpec,
        meta: &ComputerMeta,
        lease: &Lease,
    ) -> Result<Option<LeaseNews>> {
        let at = (uid, computer.id.clone());
        {
            let mut state = self.state();
            if state.runs.values().any(|run| *run == at) || !state.moving.insert(at.clone()) {
                return Ok(None);
            }
        }
        let moving = Moving {
            sandbox: self,
            key: at.clone(),
        };
        self.hypervisor
            .suspend(computer)
            .map_err(|e| fail(format!("stopping {}", meta.name), e))?;
        let aside = self.dir.join(ASIDE);
        if !aside.is_dir() {
            fs::create_dir(&aside).map_err(|e| fail(format!("creating {}", aside.display()), e))?;
            fs::set_permissions(&aside, fs::Permissions::from_mode(0o700))
                .map_err(|e| fail(format!("chmod {}", aside.display()), e))?;
        }
        let to = aside.join(format!(
            "{uid}-{}-{}",
            computer.id,
            Timestamp::now().unix_seconds()
        ));
        fs::rename(&computer.dir, &to)
            .map_err(|e| fail(format!("setting {} aside", meta.name), e))?;
        {
            let mut state = self.state();
            state.lost.remove(&at);
            state.used.remove(&at);
            // Its owner's computers are looked for in the store again.
            state.adopted.remove(&uid);
        }
        drop(moving);
        // Here it is what the store says again: asleep, wherever it's
        // awake. A store that doesn't answer now is asked as it's next
        // looked for.
        let _ = self.adopt(uid, Some(&computer.id));
        Ok(Some(LeaseNews::SetAside {
            uid,
            id: computer.id.clone(),
            name: meta.name.clone(),
            host: lease.host.clone(),
            aside: to,
        }))
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
                        || state.lost.contains_key(&key)
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
/// What a person is told of a computer awake here that another host took.
fn taken_words(name: &str, lease: &Lease) -> String {
    format!(
        "{name} was taken by another host ({}) while this one couldn't reach the store: \
         nothing of this host's copy is saved over it, and the copy is set aside. Ask again \
         in a moment",
        lease.host
    )
}

/// This host's own name for computers' leases: made once, kept in
/// `<dir>/.host`.
fn own_host_name(dir: &Path) -> Result<String> {
    let path = dir.join(".host");
    let fits = |name: &str| {
        (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    if let Ok(kept) = fs::read_to_string(&path)
        && fits(kept.trim())
    {
        return Ok(kept.trim().to_owned());
    }
    let name = format!("host-{}", random_hex()?);
    let making = dir.join(".host.new");
    fs::write(&making, &name)
        .and_then(|()| fs::set_permissions(&making, fs::Permissions::from_mode(0o600)))
        .and_then(|()| fs::rename(&making, &path))
        .map_err(|e| fail(format!("saving {}", path.display()), e))?;
    Ok(name)
}

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
        if let Some(lease) = self.state().lost.get(&key) {
            return Err(refuse(taken_words(&meta.name, lease)));
        }
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
        // From here its run counts, in one step with the checks: no sleep
        // begins under it, and its folder stays where it is (a copy
        // another host took is set aside only once nothing runs in it).
        {
            let mut state = self.state();
            if let Some(lease) = state.lost.get(&key) {
                return Err(refuse(taken_words(&meta.name, lease)));
            }
            if state.taken_over.contains(&key) {
                return Err(refuse(format!(
                    "{} is taken over: hand it back for its tasks to run",
                    meta.name
                )));
            }
            if state.moving.contains(&key) {
                return Err(refuse(format!(
                    "{} is going to sleep or waking; try again in a moment",
                    meta.name
                )));
            }
            state.runs.insert(spec.name.clone(), key);
        }
        let started = self.start_run(spec, &computer, &meta.name);
        if started.is_err() {
            self.state().runs.remove(&spec.name);
        }
        started
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
        // What a task's processes left is theirs to shape: removed however
        // deep it goes (`remove_tree`); nothing there is fine.
        glass_platform::fs::remove_tree(&task)?;
        if fresh {
            self.hypervisor
                .suspend(&computer)
                .map_err(|e| fail(format!("stopping {}", computer.id), e))?;
            glass_platform::fs::remove_tree(&computer.dir)?;
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

    fn cleanup_stale(&self) -> Result<Swept> {
        // Before anything: no process of an earlier glassd's runs is left.
        let mut cleaned = self.hypervisor.stop_leftovers(&self.dir);
        // Copies set aside (another host took their computer) are kept a
        // week.
        if let Ok(aside) = fs::read_dir(self.dir.join(ASIDE)) {
            let now = Timestamp::now().unix_seconds();
            for entry in aside.flatten() {
                // Its name ends with when it was set aside.
                let old = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.rsplit('-').next()?.parse::<i64>().ok())
                    .and_then(|at| u64::try_from(now.saturating_sub(at)).ok())
                    .is_some_and(|age| age >= ASIDE_KEPT.as_secs());
                if old && glass_platform::fs::remove_tree(&entry.path()).is_ok() {
                    cleaned.push(format!(
                        "{} (set aside)",
                        entry.file_name().to_string_lossy()
                    ));
                }
            }
        }
        let Ok(owners) = fs::read_dir(&self.dir) else {
            return Ok(Swept {
                removed: cleaned,
                left: Vec::new(),
            });
        };
        for owner in owners.flatten() {
            let Some(uid) = owner_uid(&owner) else {
                continue;
            };
            let Ok(computers) = fs::read_dir(owner.path()) else {
                continue;
            };
            for entry in computers.flatten() {
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let computer = ComputerSpec {
                    id: id.clone(),
                    owner: uid,
                    dir: entry.path(),
                    memory_mib: self.memory_mib,
                    vcpus: self.vcpus,
                };
                if let Ok(tasks) = fs::read_dir(computer.dir.join("tasks")) {
                    for task in tasks.flatten() {
                        if glass_platform::fs::remove_tree(&task.path()).is_ok() {
                            cleaned.push(task.file_name().to_string_lossy().into_owned());
                        }
                    }
                }
                match Self::read_meta(&computer) {
                    Some(meta) if meta.fresh => {
                        let _ = self.hypervisor.suspend(&computer);
                        if glass_platform::fs::remove_tree(&computer.dir).is_ok() {
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
        Ok(Swept {
            removed: cleaned,
            left: Vec::new(),
        })
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

/// Whose computers a folder under the computers' folder holds: it is
/// named by their uid (`<dir>/<uid>/<id>`). Anything else there isn't an
/// owner's folder.
fn owner_uid(folder: &fs::DirEntry) -> Option<u32> {
    folder.file_name().to_str()?.parse().ok()
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

    /// A computer with a display, standing in for a Glasshouse OS VM: its
    /// display a fixed picture, what its person does on it kept.
    #[derive(Debug, Default)]
    struct Display {
        inputs: Mutex<Vec<DisplayInput>>,
    }

    impl Hypervisor for Display {
        fn kind(&self) -> &'static str {
            "Glasshouse OS virtual machine"
        }
        fn contained(&self) -> bool {
            true
        }
        fn start(&self, computer: &ComputerSpec) -> std::result::Result<(), String> {
            fs::create_dir_all(&computer.dir).map_err(|e| e.to_string())
        }
        fn running(&self, computer: &ComputerSpec) -> bool {
            computer.dir.exists()
        }
        fn command(&self, _: &ComputerSpec, _: &TaskRun) -> std::result::Result<Command, String> {
            Err("not in this test".into())
        }
        fn host_path(&self, _: &ComputerSpec, _: &Path) -> Option<PathBuf> {
            None
        }
        fn suspend(&self, _: &ComputerSpec) -> std::result::Result<(), String> {
            Ok(())
        }
        fn display(&self) -> bool {
            true
        }
        fn frame(&self, _: &ComputerSpec) -> std::result::Result<Frame, String> {
            let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
            png.extend_from_slice(&1280u32.to_be_bytes());
            png.extend_from_slice(&800u32.to_be_bytes());
            Frame::from_png(png)
        }
        fn send_input(
            &self,
            _: &ComputerSpec,
            events: &[DisplayInput],
        ) -> std::result::Result<(), String> {
            self.inputs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(events);
            Ok(())
        }
    }

    #[test]
    fn adr0072_only_its_person_sees_its_display_and_drives_it_only_once_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let display = Arc::new(Display::default());
        let sandbox =
            ComputerSandbox::new(dir.path().join("computers"), display.clone(), 1024, 1).unwrap();
        let (uid, gid) = me();
        let view = sandbox.make(uid, gid, &reconciler()).unwrap();
        assert!(view.display, "a computer with a display says so");

        let (frame, version, unchanged) = sandbox.frame(uid, "reconciler", None).unwrap();
        assert_eq!((frame.width, frame.height), (1280, 800));
        assert!(!unchanged);
        let (_, again, unchanged) = sandbox.frame(uid, "reconciler", Some(&version)).unwrap();
        assert_eq!(again, version);
        assert!(unchanged, "the same picture isn't sent twice");

        // Someone else: no computer of that name for them, seen or driven.
        let other = uid + 1;
        assert!(sandbox.frame(other, "reconciler", None).is_err());
        let click = [
            DisplayInput::Move { x: 10, y: 10 },
            DisplayInput::Button {
                button: glass_api::PointerButton::Left,
                down: true,
            },
        ];
        assert!(sandbox.display_input(other, "reconciler", &click).is_err());

        // Its person, watching: refused until they take it over.
        let refused = sandbox
            .display_input(uid, "reconciler", &click)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("take over"), "{refused}");
        sandbox.take_over(uid, "reconciler").unwrap();
        assert!(sandbox.display_input(other, "reconciler", &click).is_err());
        sandbox.display_input(uid, "reconciler", &click).unwrap();
        assert!(sandbox.display_input(uid, "reconciler", &[]).is_err());
        let too_many = vec![DisplayInput::Wheel { dy: 1 }; glass_api::DISPLAY_INPUT_MAX + 1];
        assert!(sandbox.display_input(uid, "reconciler", &too_many).is_err());
        sandbox.hand_back(uid, "reconciler").unwrap();
        assert!(sandbox.display_input(uid, "reconciler", &click).is_err());

        assert_eq!(
            *display.inputs.lock().unwrap(),
            click.to_vec(),
            "only what its person did while they had it reached it"
        );
    }

    #[test]
    fn adr0072_a_terminal_computer_has_no_display_to_drive() {
        let (_dir, sandbox) = sandbox();
        let (uid, gid) = me();
        let view = sandbox.make(uid, gid, &reconciler()).unwrap();
        assert!(!view.display);
        assert!(sandbox.frame(uid, "reconciler", None).is_err());
        sandbox.take_over(uid, "reconciler").unwrap();
        assert!(
            sandbox
                .display_input(uid, "reconciler", &[DisplayInput::Wheel { dy: 1 }])
                .is_err()
        );
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
                .removed
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

    // One host at a time per computer (ADR 0066). The hosts here keep
    // computers that are folders, with nothing running in them: what's
    // tested is who may wake, save and change one.

    type Hosts = (
        tempfile::TempDir,
        Arc<Store>,
        Arc<crate::store::FolderBlobs>,
    );

    fn shared_store() -> Hosts {
        let dir = tempfile::Builder::new()
            .prefix("glass-computers-store")
            .tempdir_in("/tmp")
            .unwrap();
        let blobs = Arc::new(crate::store::FolderBlobs::new(dir.path().join("s3")).unwrap());
        (dir, Arc::new(Store::new(blobs.clone())), blobs)
    }

    fn host_in(dir: &Path, store: &Arc<Store>) -> ComputerSandbox {
        ComputerSandbox::new(dir.join("computers"), Arc::new(Display::default()), 1024, 1)
            .unwrap()
            .with_store(store.clone(), "S3")
    }

    fn host(store: &Arc<Store>) -> (tempfile::TempDir, ComputerSandbox) {
        let dir = tempfile::Builder::new()
            .prefix("glass-computers-host")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let sandbox = host_in(dir.path(), store);
        (dir, sandbox)
    }

    /// The file a test keeps in a computer awake on `host`.
    fn work(host: &ComputerSandbox, id: &str) -> PathBuf {
        host.computer(me().0, id).dir.join("work.txt")
    }

    /// The one computer's key in the store.
    fn key_in(store: &Store) -> String {
        let records = store.records(&format!("u{}-", me().0)).unwrap();
        assert_eq!(records.len(), 1);
        records[0].0.clone()
    }

    fn record_in(store: &Store) -> ComputerMeta {
        let records = store.records(&format!("u{}-", me().0)).unwrap();
        serde_json::from_slice(&records[0].1).unwrap()
    }

    /// The lease as it is ten minutes after its host last renewed it: the
    /// host went away, or can't reach the store.
    fn unrenewed(store: &Store, blobs: &crate::store::FolderBlobs, key: &str) {
        use crate::store::Blobs;
        let mut lease = store.lease(key).unwrap().unwrap();
        lease.renewed =
            Timestamp::from_unix_seconds(Timestamp::now().unix_seconds() - 600).unwrap();
        blobs
            .put(
                &format!("computers/{key}/lease/{:020}.json", lease.term),
                &serde_json::to_vec(&lease).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn adr0066_a_wake_goes_by_the_stores_record_not_what_the_host_remembers() {
        let (_s3, store, _) = shared_store();
        let (_a, host_a) = host(&store);
        let (_b, host_b) = host(&store);
        let (uid, gid) = me();

        host_a.make(uid, gid, &reconciler()).unwrap();
        fs::write(work(&host_a, "reconciler"), "one").unwrap();
        let first = host_a.sleep(uid, "reconciler").unwrap().stored.unwrap();

        // Another host wakes it, works on it and puts it to sleep, four
        // times: the version host A put there is no longer kept.
        for words in ["two", "three", "four", "five"] {
            host_b.wake(uid, gid, "reconciler").unwrap();
            fs::write(work(&host_b, "reconciler"), words).unwrap();
            host_b.sleep(uid, "reconciler").unwrap();
        }
        let key = key_in(&store);
        let versions = store.versions(&key).unwrap();
        assert_eq!(versions.len(), VERSIONS_KEPT);
        assert!(!versions.contains(&first.version), "{versions:?}");

        // Host A still remembers the version it saved. It wakes what the
        // store's record says: the newest, with the work done since.
        let remembered = host_a.get(uid, "reconciler").unwrap().stored.unwrap();
        assert_eq!(remembered.version, first.version);
        let woke = host_a.wake(uid, gid, "reconciler").unwrap();
        assert_eq!(woke.state, ComputerState::Ready);
        assert_eq!(
            fs::read_to_string(work(&host_a, "reconciler")).unwrap(),
            "five"
        );
    }

    #[test]
    fn adr0066_a_computer_is_awake_on_one_host_at_a_time() {
        let (_s3, store, _) = shared_store();
        let (_a, host_a) = host(&store);
        let (_b, host_b) = host(&store);
        let (uid, gid) = me();
        host_a.make(uid, gid, &reconciler()).unwrap();
        fs::write(work(&host_a, "reconciler"), "one").unwrap();
        host_a.sleep(uid, "reconciler").unwrap();

        host_b.wake(uid, gid, "reconciler").unwrap();
        // While host B has it, host A doesn't wake it: not for its person,
        // and not for a task. It's told which host has it.
        let refused = host_a.wake(uid, gid, "reconciler").unwrap_err().to_string();
        assert!(refused.contains("is awake on another host"), "{refused}");
        assert!(refused.contains(host_b.host()), "{refused}");
        let refused = host_a
            .create(&task("glass-0021", Some("reconciler")))
            .unwrap_err()
            .to_string();
        assert!(refused.contains("is awake on another host"), "{refused}");
        // Nothing of it came to host A: its record, and no more.
        let here: Vec<_> = fs::read_dir(host_a.computer(uid, "reconciler").dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(here, ["computer.json"]);
        assert_eq!(
            host_a.get(uid, "reconciler").unwrap().state,
            ComputerState::Asleep
        );

        // Asleep again, it's any host's to wake, and then that host's.
        fs::write(work(&host_b, "reconciler"), "two").unwrap();
        host_b.sleep(uid, "reconciler").unwrap();
        host_a.wake(uid, gid, "reconciler").unwrap();
        assert_eq!(
            fs::read_to_string(work(&host_a, "reconciler")).unwrap(),
            "two"
        );
        let refused = host_b.wake(uid, gid, "reconciler").unwrap_err().to_string();
        assert!(refused.contains(host_a.host()), "{refused}");
    }

    #[test]
    fn adr0066_a_host_that_went_away_keeps_its_computer_only_for_as_long_as_its_lease() {
        let (_s3, store, blobs) = shared_store();
        let (_a, host_a) = host(&store);
        let (_b, host_b) = host(&store);
        let (uid, gid) = me();
        host_a.make(uid, gid, &reconciler()).unwrap();
        fs::write(work(&host_a, "reconciler"), "as it last slept").unwrap();
        host_a.sleep(uid, "reconciler").unwrap();
        host_a.wake(uid, gid, "reconciler").unwrap();
        fs::write(work(&host_a, "reconciler"), "since, and never saved").unwrap();
        // Host A is gone, with the computer awake on it.
        drop(host_a);

        let refused = host_b.wake(uid, gid, "reconciler").unwrap_err().to_string();
        assert!(refused.contains("is awake on another host"), "{refused}");
        // Its lease runs out, unrenewed, and the computer wakes elsewhere as
        // it last slept.
        unrenewed(&store, &blobs, &key_in(&store));
        host_b.wake(uid, gid, "reconciler").unwrap();
        assert_eq!(
            fs::read_to_string(work(&host_b, "reconciler")).unwrap(),
            "as it last slept"
        );
    }

    #[test]
    fn adr0066_a_host_cut_off_saves_nothing_over_the_host_that_took_its_computer() {
        let (_s3, store, blobs) = shared_store();
        let (_a, host_a) = host(&store);
        // Host A asks the store each time, as a host does that hasn't
        // heard from it for a while.
        let host_a = host_a.trusting_for(Duration::ZERO);
        let (_b, host_b) = host(&store);
        let (uid, gid) = me();
        host_a.make(uid, gid, &reconciler()).unwrap();
        fs::write(work(&host_a, "reconciler"), "one").unwrap();
        host_a.sleep(uid, "reconciler").unwrap();
        host_a.wake(uid, gid, "reconciler").unwrap();
        fs::write(work(&host_a, "reconciler"), "host a's, while cut off").unwrap();

        // Host A can't reach the store for ten minutes. Host B takes the
        // computer, works on it, and puts it to sleep.
        let key = key_in(&store);
        unrenewed(&store, &blobs, &key);
        host_b.wake(uid, gid, "reconciler").unwrap();
        fs::write(work(&host_b, "reconciler"), "host b's").unwrap();
        let slept = host_b.sleep(uid, "reconciler").unwrap().stored.unwrap();
        assert!(store.lease(&key).unwrap().unwrap().released);

        // Host A is back, with its copy from before all that. Nobody has
        // the computer now, and still it saves nothing over host B's.
        let versions = store.versions(&key).unwrap();
        let refused = host_a.sleep(uid, "reconciler").unwrap_err().to_string();
        assert!(refused.contains("was taken by another host"), "{refused}");
        assert!(refused.contains(host_b.host()), "{refused}");
        assert_eq!(store.versions(&key).unwrap(), versions);
        assert_eq!(record_in(&store).stored.unwrap().version, slept.version);
        // No task starts in its copy.
        let refused = host_a
            .create(&task("glass-0031", Some("reconciler")))
            .unwrap_err()
            .to_string();
        assert!(refused.contains("was taken by another host"), "{refused}");
        assert!(host_a.idle(Duration::ZERO).is_empty());

        // The copy is set aside, whole, and said so for the record.
        let news = host_a.keep_leases();
        let [
            LeaseNews::SetAside {
                id,
                name,
                host: by,
                aside,
                ..
            },
        ] = news.as_slice()
        else {
            panic!("{news:?}");
        };
        assert_eq!(
            (id.as_str(), name.as_str(), by.as_str()),
            ("reconciler", "Reconciler", host_b.host())
        );
        assert_eq!(
            fs::read_to_string(aside.join("work.txt")).unwrap(),
            "host a's, while cut off"
        );
        let mode = fs::metadata(aside.parent().unwrap()).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700, "only glassd reads what's set aside");

        // Host A lists the computer as the store has it, and wakes that.
        let view = host_a.get(uid, "reconciler").unwrap();
        assert_eq!(view.state, ComputerState::Asleep);
        assert_eq!(view.stored.unwrap().version, slept.version);
        host_a.wake(uid, gid, "reconciler").unwrap();
        assert_eq!(
            fs::read_to_string(work(&host_a, "reconciler")).unwrap(),
            "host b's"
        );
        assert_eq!(host_a.keep_leases(), []);
        assert_eq!(store.lease(&key).unwrap().unwrap().host, host_a.host());
    }

    #[test]
    fn adr0066_a_host_keeps_its_leases_and_is_told_of_one_it_lost() {
        let (_s3, store, blobs) = shared_store();
        let (dir_a, host_a) = host(&store);
        let (_b, host_b) = host(&store);
        let (uid, gid) = me();

        // Made here: its lease is this host's and its record is in the
        // store from the start, though it never slept.
        host_a.make(uid, gid, &reconciler()).unwrap();
        let key = key_in(&store);
        let made = store.lease(&key).unwrap().unwrap();
        assert_eq!((made.term, made.host.as_str()), (1, host_a.host()));
        assert_eq!(record_in(&store).stored, None);
        host_a.sleep(uid, "reconciler").unwrap();
        host_a.wake(uid, gid, "reconciler").unwrap();
        let woken = store.lease(&key).unwrap().unwrap();
        assert_eq!((woken.term, woken.released), (2, false));

        // Kept: renewed, the same term.
        unrenewed(&store, &blobs, &key);
        assert_eq!(host_a.keep_leases(), []);
        let kept = store.lease(&key).unwrap().unwrap();
        assert_eq!(kept.term, 2);
        assert!(kept.holds(Timestamp::now(), LEASE_FOR));

        // glassd starts again on the same host, with the computer awake:
        // the host's name is the same, and it asks for its lease again.
        let name = host_a.host().to_owned();
        drop(host_a);
        let host_a = host_in(dir_a.path(), &store);
        assert_eq!(host_a.host(), name);
        assert_eq!(host_a.keep_leases(), []);
        let again = store.lease(&key).unwrap().unwrap();
        assert_eq!((again.term, again.host.as_str()), (3, name.as_str()));

        // A task is running in it when host A is cut off, and host B
        // takes the computer.
        let spec = task("glass-0041", Some("reconciler"));
        host_a.create(&spec).unwrap();
        fs::write(work(&host_a, "reconciler"), "cut off").unwrap();
        unrenewed(&store, &blobs, &key);
        host_b.wake(uid, gid, "reconciler").unwrap();

        // Host A's next renewal finds out: said once, for the record and
        // for its tasks to be stopped.
        let news = host_a.keep_leases();
        assert_eq!(
            news,
            [LeaseNews::Lost {
                uid,
                id: "reconciler".into(),
                name: "Reconciler".into(),
                host: host_b.host().to_owned(),
            }]
        );
        // Its copy stays where it is while the task ends.
        assert_eq!(host_a.keep_leases(), []);
        assert!(work(&host_a, "reconciler").exists());
        host_a.destroy(&spec).unwrap();
        let news = host_a.keep_leases();
        assert!(
            matches!(news.as_slice(), [LeaseNews::SetAside { aside, .. }]
                if fs::read_to_string(aside.join("work.txt")).unwrap() == "cut off"),
            "{news:?}"
        );
        // Host B's lease was never touched.
        let theirs = store.lease(&key).unwrap().unwrap();
        assert_eq!((theirs.term, theirs.host.as_str()), (4, host_b.host()));
        assert_eq!(host_a.keep_leases(), []);
    }

    #[test]
    fn adr0066_an_agent_added_while_its_computer_sleeps_goes_where_it_wakes() {
        let (_s3, store, _) = shared_store();
        let (_a, host_a) = host(&store);
        let (_b, host_b) = host(&store);
        let (uid, gid) = me();
        let id = glass_api::PERSON_COMPUTER;
        host_a
            .add_persona(uid, gid, "Dana's computer", &persona("scout", "Scout"))
            .unwrap();
        host_a.sleep(uid, id).unwrap();

        // Added while it sleeps: the change is made in the store's record,
        // and the computer is nobody's again after.
        let view = host_a
            .add_persona(uid, gid, "Dana's computer", &persona("writer", "Writer"))
            .unwrap();
        assert_eq!(view.state, ComputerState::Asleep);
        let names = |meta: &ComputerMeta| -> Vec<String> {
            meta.personas.iter().map(|p| p.id.clone()).collect()
        };
        assert_eq!(names(&record_in(&store)), ["scout", "writer"]);
        assert!(store.lease(&key_in(&store)).unwrap().unwrap().released);

        // It wakes on another host with both.
        let woke = host_b.wake(uid, gid, id).unwrap();
        assert_eq!(woke.personas.len(), 2);
        // Awake there, its record isn't host A's to change.
        for refused in [
            host_a.add_persona(uid, gid, "Dana's computer", &persona("third", "Third")),
            host_a.remove_persona(uid, "scout"),
        ] {
            let refused = refused.unwrap_err().to_string();
            assert!(refused.contains("is awake on another host"), "{refused}");
        }
        assert_eq!(names(&record_in(&store)), ["scout", "writer"]);

        // Removed where it's awake, then asleep: host A, which remembers
        // two agents, wakes it with the one.
        host_b.remove_persona(uid, "scout").unwrap();
        host_b.sleep(uid, id).unwrap();
        assert_eq!(host_a.get(uid, id).unwrap().personas.len(), 2);
        let woke = host_a.wake(uid, gid, id).unwrap();
        assert_eq!(woke.personas.len(), 1);
        assert_eq!(woke.personas[0].id, "writer");
        // One the same name is refused in the store's record as here.
        host_a.sleep(uid, id).unwrap();
        let refused = host_a
            .add_persona(uid, gid, "Dana's computer", &persona("writer", "Writer"))
            .unwrap_err()
            .to_string();
        assert!(refused.contains("already have an agent"), "{refused}");
        assert!(store.lease(&key_in(&store)).unwrap().unwrap().released);
    }

    #[test]
    fn adr0066_an_id_awake_on_another_host_is_taken() {
        let (_s3, store, _) = shared_store();
        let (_a, host_a) = host(&store);
        let (_b, host_b) = host(&store);
        let (uid, gid) = me();
        host_a.make(uid, gid, &reconciler()).unwrap();
        // It never slept: nothing of it is in the store to list or wake.
        assert!(host_b.list(uid).is_empty());
        assert!(host_b.wake(uid, gid, "reconciler").is_err());
        // But its id is its person's already.
        let refused = host_b
            .make(uid, gid, &reconciler())
            .unwrap_err()
            .to_string();
        assert!(refused.contains("awake on another host"), "{refused}");
        assert_eq!(store.records(&format!("u{uid}-")).unwrap().len(), 1);
    }

    /// A store in which something happens, once, as a version is read:
    /// what another host does while this one is in the middle of a wake.
    struct Meanwhile {
        inner: Arc<crate::store::FolderBlobs>,
        then: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl std::fmt::Debug for Meanwhile {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Meanwhile")
        }
    }

    impl crate::store::Blobs for Meanwhile {
        fn put(&self, key: &str, bytes: &[u8]) -> std::result::Result<(), String> {
            self.inner.put(key, bytes)
        }
        fn put_new(&self, key: &str, bytes: &[u8]) -> std::result::Result<bool, String> {
            self.inner.put_new(key, bytes)
        }
        fn get(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, String> {
            if key.contains("/versions/") {
                let then = self.then.lock().unwrap().take();
                if let Some(then) = then {
                    then();
                }
            }
            self.inner.get(key)
        }
        fn exists(&self, key: &str) -> std::result::Result<bool, String> {
            self.inner.exists(key)
        }
        fn delete(&self, key: &str) -> std::result::Result<(), String> {
            self.inner.delete(key)
        }
        fn list(&self, prefix: &str) -> std::result::Result<Vec<crate::store::Listed>, String> {
            self.inner.list(prefix)
        }
    }

    #[test]
    fn adr0066_a_computer_taken_while_it_wakes_isnt_awake_here() {
        let (_s3, plain, blobs) = shared_store();
        let meanwhile = Arc::new(Meanwhile {
            inner: blobs.clone(),
            then: Mutex::new(None),
        });
        let store = Arc::new(Store::new(meanwhile.clone()));
        let (_a, host_a) = host(&store);
        let host_a = host_a.trusting_for(Duration::ZERO);
        let (uid, gid) = me();
        host_a.make(uid, gid, &reconciler()).unwrap();
        fs::write(work(&host_a, "reconciler"), "one").unwrap();
        host_a.sleep(uid, "reconciler").unwrap();
        let key = key_in(&plain);

        // In the middle of its wake host A loses the store for longer than
        // a lease lasts, and another host takes the computer.
        *meanwhile.then.lock().unwrap() = Some(Box::new({
            let (plain, blobs, key) = (plain.clone(), blobs.clone(), key.clone());
            move || {
                unrenewed(&plain, &blobs, &key);
                let taken = plain
                    .claim(&key, "host-b", Timestamp::now(), LEASE_FOR)
                    .unwrap();
                assert!(matches!(taken, Claim::Held(_)), "{taken:?}");
            }
        }));
        let refused = host_a.wake(uid, gid, "reconciler").unwrap_err().to_string();
        assert!(
            refused.contains("was taken by another host (host-b) while it was waking here"),
            "{refused}"
        );
        // It isn't awake on host A: nothing of it is here, no task starts
        // in it, and host B's lease is as host B left it.
        let view = host_a.get(uid, "reconciler").unwrap();
        assert_eq!(view.state, ComputerState::Asleep);
        let here: Vec<_> = fs::read_dir(host_a.computer(uid, "reconciler").dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(here, ["computer.json"]);
        assert_eq!(host_a.keep_leases(), []);
        let lease = plain.lease(&key).unwrap().unwrap();
        assert_eq!((lease.host.as_str(), lease.released), ("host-b", false));
    }
}
