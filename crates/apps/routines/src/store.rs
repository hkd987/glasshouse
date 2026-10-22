//! Where routines are kept (DA-4, ADR 0044).
//!
//! Every version saved is a file of its own under glassd's state, root
//! only, never changed once written: `<root>/<login>/<id>/<n>.json`. Which
//! version a person uses, and whether they still have the routine, is
//! their choice, a file in their home ([`MINE`]) written as them, so task
//! undo restores it like any file a call changed. Writing it by hand only
//! picks among versions that were saved through the approval.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use glass_platform::fs::{ConfinedDir, Owner};
use glass_types::{AgentId, TaskId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::schedule::Schedule;

/// Which version of each of their routines a person uses, under their home.
pub const MINE: &str = ".local/share/glasshouse/routines.json";
const MAX_MINE_BYTES: u64 = 64 * 1024;
const MAX_ROUTINE_BYTES: u64 = 256 * 1024;

/// One saved version of a routine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Routine {
    /// Its id among the person's routines: lowercase words and digits.
    pub id: String,
    pub name: String,
    pub version: u32,
    /// Whose it is: the login it was saved for.
    pub owner: String,
    /// The agent that runs it: the one that saved it.
    pub agent: AgentId,
    /// What it's for, in the person's words: the request each run starts
    /// with, and what the agent does if the script breaks.
    pub request: String,
    /// What runs, with no model: JavaScript for the agent's script runner.
    pub script: String,
    /// The tools the script calls, by name, as it's written.
    pub calls: Vec<String>,
    /// When it runs by itself; never, without one.
    #[serde(default)]
    pub schedule: Option<Schedule>,
    /// The scope of the task that saved it: what each run may read and
    /// change (DA-4).
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    /// What changed, in the words of whoever saved it.
    pub note: String,
    pub saved_at: Timestamp,
    /// The task that saved it.
    pub saved_in: TaskId,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Mine {
    routines: BTreeMap<String, u32>,
}

/// glassd's routines, by person: what the provider saves and the runtime
/// starts.
#[derive(Debug, Clone)]
pub struct Routines {
    root: PathBuf,
    /// Saves one at a time, so two never take the same version number.
    saving: Arc<Mutex<()>>,
}

impl Routines {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            saving: Arc::default(),
        }
    }

    /// Nowhere: for listing actions.
    pub fn offline() -> Self {
        Self::new("/nonexistent")
    }

    /// Where `login`'s routine `id` is kept; none for an id or login that
    /// isn't one (a name of path parts), whatever a choices file says.
    fn dir(&self, login: &str, id: &str) -> Option<PathBuf> {
        (valid_id(id) && one_part(login)).then(|| self.root.join(login).join(id))
    }

    /// The people with routines kept here.
    pub fn logins(&self) -> Vec<String> {
        names(&self.root)
    }

    /// A person's routines' ids, chosen or not.
    pub fn ids(&self, login: &str) -> Vec<String> {
        names(&self.root.join(login))
    }

    /// A routine's versions, oldest first.
    pub fn versions(&self, login: &str, id: &str) -> Vec<u32> {
        let Some(dir) = self.dir(login, id) else {
            return Vec::new();
        };
        let mut versions: Vec<u32> = names(&dir)
            .iter()
            .filter_map(|n| n.strip_suffix(".json")?.parse().ok())
            .collect();
        versions.sort_unstable();
        versions
    }

    pub fn record(&self, login: &str, id: &str, version: u32) -> Option<Routine> {
        let path = self.dir(login, id)?.join(format!("{version}.json"));
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() as u64 > MAX_ROUTINE_BYTES {
            return None;
        }
        serde_json::from_slice::<Routine>(&bytes)
            .ok()
            .filter(|r| r.owner == login && r.id == id && r.version == version)
    }

    /// The routines `home`'s person has, at the versions they chose.
    pub fn current(&self, login: &str, home: &Path) -> Vec<Routine> {
        chosen(home)
            .into_iter()
            .filter_map(|(id, version)| self.record(login, &id, version))
            .collect()
    }

    /// Keeps `routine` as the next version of its id, numbered here, and
    /// returns that number.
    pub(crate) fn save(&self, mut routine: Routine) -> io::Result<Routine> {
        let _one_at_a_time = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        routine.version = self
            .versions(&routine.owner, &routine.id)
            .last()
            .map_or(1, |latest| latest + 1);
        let dir = self
            .dir(&routine.owner, &routine.id)
            .ok_or_else(|| io::Error::other("not a routine's id"))?;
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", routine.version));
        let staged = dir.join(format!(".{}.json.new", routine.version));
        std::fs::write(&staged, serde_json::to_vec_pretty(&routine)?)?;
        std::fs::rename(staged, path)?;
        Ok(routine)
    }
}

/// A login as one path part: never `..`, a separator or empty.
fn one_part(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 64
        && !matches!(login, "." | "..")
        && !login.contains(['/', '\0'])
}

/// A routine's id: lowercase letters, digits and `-`, as saves make them.
pub fn valid_id(id: &str) -> bool {
    (1..=48).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The versions `home`'s person uses, by routine. Missing or unreadable:
/// none.
pub fn chosen(home: &Path) -> BTreeMap<String, u32> {
    ConfinedDir::open(home)
        .and_then(|dir| dir.read(Path::new(MINE), MAX_MINE_BYTES))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Mine>(&bytes).ok())
        .map(|m| m.routines)
        .unwrap_or_default()
}

/// Writes the person's choices through their confined home, as them.
pub fn choose(home: &Path, owner: Owner, routines: BTreeMap<String, u32>) -> io::Result<()> {
    let dir = ConfinedDir::open(home)?;
    if let Some(folder) = Path::new(MINE).parent()
        && !dir.exists(folder)
    {
        dir.create_dir_all_as(folder, owner)?;
    }
    let bytes = serde_json::to_vec_pretty(&Mine { routines }).map_err(io::Error::other)?;
    dir.write(Path::new(MINE), &bytes, Some(owner)).map(|_| ())
}
