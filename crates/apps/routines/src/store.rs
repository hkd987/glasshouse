//! Where routines are kept (DA-4, ADR 0044).
//!
//! Every version saved is a file of its own under glassd's state, root
//! only, never changed once written: `<root>/<login>/<id>/<n>.json`. Which
//! version a person uses, whether they still have the routine and whether
//! its schedule is paused, is their choice, a file in their home ([`MINE`])
//! written as them, so task undo restores it like any file a call changed.
//! Writing it by hand only picks among versions that were saved through
//! the approval. Its last runs are glassd's record, beside its versions
//! (`runs.json`, ADR 0047), with the runs going now (`.running.json`) so a
//! run a restart cut off is kept too.
//!
//! Every change to the choices file reads and writes it under one lock,
//! and a scheduled start checks it under the same lock, so no call's
//! change is lost to another's and a routine paused is never started by
//! its schedule after (ADR 0051).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use glass_platform::fs::{ConfinedDir, Owner};
use glass_types::{AgentId, TaskId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::host::Host;
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
    /// Routines whose schedule is paused: they run only when started.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    paused: BTreeSet<String>,
}

/// The runs of a routine glassd keeps, newest first.
pub const KEPT_RUNS: usize = 20;
const MAX_RUNS_BYTES: u64 = 256 * 1024;
const RUNS: &str = "runs.json";
/// The runs of a routine going now: kept at their end, or as cut off when
/// glassd starts and finds them here.
const RUNNING: &str = ".running.json";
/// The last local minute the schedule started routines in, under the root.
const SCHEDULED: &str = ".scheduled";
/// What a run a restart cut off says.
pub const CUT_OFF: &str = "This computer restarted during the run.";

/// One change to a person's choices file at a time, in this process.
static CHOICES: Mutex<()> = Mutex::new(());

/// Runs `f` while no choices file changes: a scheduled start checks what
/// the person chose and starts in one step (ADR 0051).
pub fn locked<T>(f: impl FnOnce() -> T) -> T {
    let _one = CHOICES.lock().unwrap_or_else(PoisonError::into_inner);
    f()
}

/// One run of a routine, as it ended (ADR 0047).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub task: TaskId,
    pub version: u32,
    /// Its schedule started it, rather than the person.
    pub scheduled: bool,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub outcome: Outcome,
    /// A model was asked: the script failed and the agent did the request
    /// itself.
    pub model: bool,
    /// What it said when it was done, the first line, short.
    pub said: String,
    /// How many changes it made.
    pub changes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Failed,
    Stopped,
}

/// glassd's routines, by person: what the provider saves and the runtime
/// starts.
#[derive(Debug, Clone)]
pub struct Routines {
    root: PathBuf,
    /// Saves one at a time, so two never take the same version number.
    saving: Arc<Mutex<()>>,
    /// The runtime, for what the Routines app asks of it (ADR 0047).
    host: Arc<RwLock<Option<Arc<dyn Host>>>>,
}

impl Routines {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            saving: Arc::default(),
            host: Arc::default(),
        }
    }

    /// The runtime, set at startup, for every clone.
    pub fn set_host(&self, host: Arc<dyn Host>) {
        *self.host.write().unwrap_or_else(PoisonError::into_inner) = Some(host);
    }

    pub fn host(&self) -> Option<Arc<dyn Host>> {
        self.host
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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

    /// The runs of `login`'s routine `id` glassd kept, newest first.
    pub fn runs(&self, login: &str, id: &str) -> Vec<Run> {
        self.dir(login, id)
            .map(|dir| dir.join(RUNS))
            .filter(|path| std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_RUNS_BYTES))
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Keeps `run` first among `login`'s routine `id`'s runs, at most
    /// [`KEPT_RUNS`] of them, and no longer as going. A routine never
    /// saved has nowhere to keep it.
    pub fn record_run(&self, login: &str, id: &str, run: Run) -> io::Result<()> {
        let _one_at_a_time = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = self
            .dir(login, id)
            .filter(|dir| dir.is_dir())
            .ok_or_else(|| io::Error::other("not a routine kept here"))?;
        let mut runs = self.runs(login, id);
        runs.retain(|r| r.task != run.task);
        let task = run.task;
        runs.insert(0, run);
        runs.truncate(KEPT_RUNS);
        write_json(&dir, RUNS, &runs)?;
        let mut going = read_json::<Vec<Run>>(&dir.join(RUNNING));
        if going.iter().any(|r| r.task == task) {
            going.retain(|r| r.task != task);
            write_json(&dir, RUNNING, &going)?;
        }
        Ok(())
    }

    /// Notes `run` as going, so if glassd stops before its end the run is
    /// still kept ([`Routines::recover`]).
    pub fn run_started(&self, login: &str, id: &str, run: Run) -> io::Result<()> {
        let _one_at_a_time = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = self
            .dir(login, id)
            .filter(|dir| dir.is_dir())
            .ok_or_else(|| io::Error::other("not a routine kept here"))?;
        let mut going = read_json::<Vec<Run>>(&dir.join(RUNNING));
        going.retain(|r| r.task != run.task);
        going.push(run);
        write_json(&dir, RUNNING, &going)
    }

    /// At glassd's start: the runs that were going when it stopped are
    /// kept as cut off, failed. How many.
    pub fn recover(&self) -> usize {
        let mut kept = 0;
        for login in self.logins() {
            for id in self.ids(&login) {
                let Some(dir) = self.dir(&login, &id) else {
                    continue;
                };
                let going = read_json::<Vec<Run>>(&dir.join(RUNNING));
                for mut run in going {
                    run.ended_at = Timestamp::now();
                    run.outcome = Outcome::Failed;
                    run.said = CUT_OFF.to_owned();
                    if self.record_run(&login, &id, run).is_ok() {
                        kept += 1;
                    }
                }
            }
        }
        kept
    }

    /// Claims the local minute `minute` (`local_minute`'s number) for the
    /// schedule: false when it, or a later one, was claimed already, here
    /// or before a restart. A clock set back, or a minute that comes again
    /// when daylight saving ends, starts nothing twice (ADR 0051).
    pub fn claim_minute(&self, minute: i64) -> io::Result<bool> {
        let _one_at_a_time = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        let last: Option<i64> = std::fs::read_to_string(self.root.join(SCHEDULED))
            .ok()
            .and_then(|text| text.trim().parse().ok());
        if last.is_some_and(|last| minute <= last) {
            return Ok(false);
        }
        std::fs::create_dir_all(&self.root)?;
        write_json(&self.root, SCHEDULED, &minute)?;
        Ok(true)
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

/// The person's choices, to show: none where there is no file, or it
/// can't be read.
fn mine(home: &Path) -> Mine {
    read_mine(home).unwrap_or_default()
}

/// The person's choices, to change. The file is one they may edit: one
/// that no longer reads as choices (a typo) is set aside beside itself
/// and they start again, where the next save used to write over it with
/// only the routine being saved. One that can't be read at all is an
/// error, and nothing is written over it.
fn read_mine(home: &Path) -> io::Result<Mine> {
    Ok(ConfinedDir::open(home)?
        .read_json_or_set_aside(Path::new(MINE), MAX_MINE_BYTES)?
        .unwrap_or_default())
}

/// The versions `home`'s person uses, by routine. Missing or unreadable:
/// none.
pub fn chosen(home: &Path) -> BTreeMap<String, u32> {
    mine(home).routines
}

/// `f`'s change to the versions `home`'s person uses, read and written
/// under the choices lock. The routines they no longer have aren't paused
/// either.
pub fn change<T>(
    home: &Path,
    owner: Owner,
    f: impl FnOnce(&mut BTreeMap<String, u32>) -> io::Result<T>,
) -> io::Result<T> {
    let _one = CHOICES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut mine = read_mine(home)?;
    let done = f(&mut mine.routines)?;
    let routines = &mine.routines;
    mine.paused.retain(|id| routines.contains_key(id));
    write_mine(home, owner, &mine)?;
    Ok(done)
}

/// Those of `home`'s person's routines whose schedule they paused.
pub fn paused(home: &Path) -> BTreeSet<String> {
    let mine = mine(home);
    mine.paused
        .into_iter()
        .filter(|id| mine.routines.contains_key(id))
        .collect()
}

/// Writes the person's choices through their confined home, as them. The
/// routines they no longer have aren't paused either.
pub fn choose(home: &Path, owner: Owner, routines: BTreeMap<String, u32>) -> io::Result<()> {
    change(home, owner, |chosen| {
        *chosen = routines;
        Ok(())
    })
}

/// Pauses the schedule of `home`'s person's routine `id`, or resumes it.
pub fn pause(home: &Path, owner: Owner, id: &str, on: bool) -> io::Result<()> {
    let _one = CHOICES.lock().unwrap_or_else(PoisonError::into_inner);
    let mut mine = read_mine(home)?;
    if !mine.routines.contains_key(id) {
        return Err(io::Error::new(io::ErrorKind::NotFound, "not one of theirs"));
    }
    if on {
        mine.paused.insert(id.to_owned());
    } else {
        mine.paused.remove(id);
    }
    write_mine(home, owner, &mine)
}

/// glassd's own JSON, read whole; missing or unreadable: the default. A
/// file that isn't what it should be is set aside beside itself, not
/// written over.
fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::metadata(path)
        .ok()
        .filter(|m| m.len() <= MAX_RUNS_BYTES)
        .and_then(|_| glass_state::read_json_or_set_aside(path).ok().flatten())
        .unwrap_or_default()
}

/// Writes glassd's own JSON in `dir`: beside it, synced, renamed into
/// place, the folder synced (glass-state).
fn write_json(dir: &Path, name: &str, value: &impl Serialize) -> io::Result<()> {
    glass_state::write_json(&dir.join(name), value, glass_state::READABLE)
}

fn write_mine(home: &Path, owner: Owner, mine: &Mine) -> io::Result<()> {
    let dir = ConfinedDir::open(home)?;
    if let Some(folder) = Path::new(MINE).parent()
        && !dir.exists(folder)
    {
        dir.create_dir_all_as(folder, owner)?;
    }
    let bytes = serde_json::to_vec_pretty(mine).map_err(io::Error::other)?;
    dir.write(Path::new(MINE), &bytes, Some(owner)).map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::os::unix::fs::MetadataExt;

    use super::*;

    // Audit 4.3: the file of a person's choices is theirs to edit. One
    // with a typo read as "no routines", and the next save wrote a file
    // with only the routine being saved.
    #[test]
    fn da4_a_typo_in_a_persons_choices_loses_nothing_they_had() {
        let home = tempfile::tempdir().unwrap();
        let meta = std::fs::metadata(home.path()).unwrap();
        let me = Owner {
            uid: meta.uid(),
            gid: meta.gid(),
        };
        let both: BTreeMap<String, u32> =
            [("standup".to_owned(), 3), ("digest".to_owned(), 1)].into();
        choose(home.path(), me, both.clone()).unwrap();
        assert_eq!(chosen(home.path()), both);

        // Edited by hand, with a comma too many.
        let file = home.path().join(MINE);
        let typo = br#"{"routines":{"standup":3,"digest":1,}}"#;
        std::fs::write(&file, typo).unwrap();
        change(home.path(), me, |chosen| {
            chosen.insert("review".to_owned(), 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            chosen(home.path()),
            [("review".to_owned(), 1)].into(),
            "they start again"
        );
        // What they had is beside it, as they left it, to put right.
        let aside: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".damaged-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read(aside[0].path()).unwrap(), typo);

        // A file that can't be read at all is never written over.
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(chosen(home.path()).is_empty());
        assert!(
            change(home.path(), me, |chosen| {
                chosen.insert("review".to_owned(), 2);
                Ok(())
            })
            .is_err()
        );
        assert!(pause(home.path(), me, "review", true).is_err());
        assert!(file.is_dir());
    }
}
