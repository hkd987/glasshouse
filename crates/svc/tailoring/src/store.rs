//! Where layers live: one JSON file per user and agent under the daemon's
//! state directory, which only root can read, so a layer changes only
//! through the checks in the control API. Versions are never edited or
//! deleted; switching versions is how a change is undone (TL-2).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use glass_api::LayerStatus;
use glass_types::{AgentId, Tier, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is damaged: {source}")]
    Damaged {
        path: PathBuf,
        source: serde_json::Error,
    },
}

/// One user's layer on one agent, with every version they saved.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub user: String,
    pub versions: Vec<Version>,
    /// The version tasks use; `None` runs the published agent.
    pub active: Option<u32>,
    /// The agent version the active version was last checked against
    /// (TL-13).
    pub checked_against: String,
    /// Why the layer is paused, if it is.
    pub paused: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub version: u32,
    pub at: Timestamp,
    pub note: String,
    pub settings: BTreeMap<String, Value>,
    pub restrict: Vec<(String, Tier)>,
}

/// What [`Layer::recheck`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recheck {
    Passed,
    Paused(String),
}

impl Layer {
    pub fn status(&self) -> LayerStatus {
        match (&self.active, &self.paused) {
            (None, _) => LayerStatus::Off,
            (Some(_), Some(reason)) => LayerStatus::Paused {
                reason: reason.clone(),
            },
            (Some(_), None) => LayerStatus::On,
        }
    }

    pub fn version(&self, version: u32) -> Option<&Version> {
        self.versions.iter().find(|v| v.version == version)
    }

    pub fn active_version(&self) -> Option<&Version> {
        self.active.and_then(|v| self.version(v))
    }

    /// What tasks run with: the active version, unless it's paused.
    pub fn in_use(&self) -> Option<&Version> {
        self.active_version().filter(|_| self.paused.is_none())
    }

    /// Saves a new version and turns it on. The caller has checked it
    /// against `agent_version`.
    pub fn add(
        &mut self,
        user: &str,
        note: String,
        settings: BTreeMap<String, Value>,
        restrict: Vec<(String, Tier)>,
        agent_version: &str,
    ) -> u32 {
        let version = self.versions.last().map_or(1, |v| v.version + 1);
        self.user = user.to_owned();
        self.versions.push(Version {
            version,
            at: Timestamp::now(),
            note,
            settings,
            restrict,
        });
        self.turn_on(version, agent_version);
        version
    }

    /// Turns on an earlier version (checked by the caller), or with `None`
    /// turns the layer off.
    pub fn switch(&mut self, to: Option<u32>, agent_version: &str) -> Result<(), String> {
        match to {
            None => {
                self.active = None;
                self.paused = None;
                Ok(())
            }
            Some(version) if self.version(version).is_some() => {
                self.turn_on(version, agent_version);
                Ok(())
            }
            Some(version) => Err(format!("there is no version {version} of this layer")),
        }
    }

    fn turn_on(&mut self, version: u32, agent_version: &str) {
        self.active = Some(version);
        self.paused = None;
        agent_version.clone_into(&mut self.checked_against);
    }

    /// When the agent's version changed since the active version was
    /// checked, checks it again (TL-13): it stays on if `check` passes and
    /// is paused, with the reason, if not. `None` when there was nothing to
    /// check.
    pub fn recheck(
        &mut self,
        agent_version: &str,
        check: impl FnOnce(&Version) -> Result<(), Vec<String>>,
    ) -> Option<Recheck> {
        let active = self.active_version()?;
        if self.checked_against == agent_version {
            return None;
        }
        let outcome = match check(active) {
            Ok(()) => {
                self.paused = None;
                Recheck::Passed
            }
            Err(problems) => {
                let reason = format!(
                    "doesn't fit version {agent_version}: {}",
                    problems.join("; ")
                );
                self.paused = Some(reason.clone());
                Recheck::Paused(reason)
            }
        };
        agent_version.clone_into(&mut self.checked_against);
        Some(outcome)
    }
}

#[derive(Debug)]
pub struct LayerStore {
    dir: PathBuf,
    /// Serializes read-modify-write cycles.
    writing: Mutex<()>,
}

impl LayerStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            writing: Mutex::new(()),
        }
    }

    fn path(&self, uid: u32, agent: &AgentId) -> PathBuf {
        // Agent ids are lowercase letters, digits and '-', so this stays in
        // the user's directory.
        self.dir
            .join(uid.to_string())
            .join(format!("{}.json", agent.as_str()))
    }

    /// The user's layer on the agent; an empty one if they have none.
    pub fn load(&self, uid: u32, agent: &AgentId) -> Result<Layer, StoreError> {
        read(&self.path(uid, agent))
    }

    /// Loads, changes and saves a layer as one step.
    pub fn update<T>(
        &self,
        uid: u32,
        agent: &AgentId,
        change: impl FnOnce(&mut Layer) -> T,
    ) -> Result<(Layer, T), StoreError> {
        let _writing = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.path(uid, agent);
        let mut layer = read(&path)?;
        let before = layer.clone();
        let result = change(&mut layer);
        if layer != before {
            write(&path, &layer)?;
        }
        Ok((layer, result))
    }

    /// Every stored layer, as (uid, agent).
    pub fn all(&self) -> Result<Vec<(u32, AgentId)>, StoreError> {
        let mut found = Vec::new();
        let users = match fs::read_dir(&self.dir) {
            Ok(users) => users,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
            Err(source) => {
                return Err(StoreError::Io {
                    path: self.dir.clone(),
                    source,
                });
            }
        };
        for user in users.flatten() {
            let Some(uid) = user.file_name().to_str().and_then(|s| s.parse().ok()) else {
                continue;
            };
            let Ok(files) = fs::read_dir(user.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name();
                let Some(agent) = name
                    .to_str()
                    .and_then(|n| n.strip_suffix(".json"))
                    .and_then(|n| n.parse::<AgentId>().ok())
                else {
                    continue;
                };
                found.push((uid, agent));
            }
        }
        found.sort();
        Ok(found)
    }
}

fn read(path: &Path) -> Result<Layer, StoreError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| StoreError::Damaged {
            path: path.to_path_buf(),
            source,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Layer::default()),
        Err(source) => Err(StoreError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Writes beside the file and renames over it, so a crash leaves the old
/// layer or the new one, never half of one.
fn write(path: &Path, layer: &Layer) -> Result<(), StoreError> {
    let io = |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(io)?;
    }
    let text = serde_json::to_vec_pretty(layer).expect("a layer serializes");
    let staging = path.with_extension("json.new");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&staging)
        .map_err(io)?;
    file.write_all(&text).map_err(io)?;
    file.sync_all().map_err(io)?;
    fs::rename(&staging, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn agent() -> AgentId {
        "invoice-demo".parse().unwrap()
    }

    fn one(key: &str, value: Value) -> BTreeMap<String, Value> {
        BTreeMap::from([(key.to_owned(), value)])
    }

    #[test]
    fn tl2_every_change_is_a_version_and_switching_back_undoes_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = LayerStore::new(dir.path());
        assert_eq!(
            store.load(1001, &agent()).unwrap().status(),
            LayerStatus::Off
        );

        let (_, v1) = store
            .update(1001, &agent(), |l| {
                l.add(
                    "demo",
                    "flag big ones".into(),
                    one("flag_over", json!(1000)),
                    Vec::new(),
                    "1.0",
                )
            })
            .unwrap();
        let (layer, v2) = store
            .update(1001, &agent(), |l| {
                l.add(
                    "demo",
                    "and no sends".into(),
                    one("flag_over", json!(500)),
                    vec![("mail.send".into(), Tier::Never)],
                    "1.0",
                )
            })
            .unwrap();
        assert_eq!((v1, v2), (1, 2));
        assert_eq!(layer.in_use().unwrap().settings["flag_over"], json!(500));

        let (layer, switched) = store
            .update(1001, &agent(), |l| l.switch(Some(1), "1.0"))
            .unwrap();
        switched.unwrap();
        assert_eq!(layer.in_use().unwrap().version, 1);
        assert_eq!(layer.versions.len(), 2, "history is kept");

        let reloaded = store.load(1001, &agent()).unwrap();
        assert_eq!(reloaded, layer);
        let (mut layer, _) = store
            .update(1001, &agent(), |l| l.switch(None, "1.0"))
            .unwrap();
        assert_eq!(layer.status(), LayerStatus::Off);
        assert!(layer.switch(Some(9), "1.0").is_err());
        assert_eq!(store.all().unwrap(), [(1001, agent())]);
    }

    #[test]
    fn tl13_a_new_agent_version_rechecks_and_pauses_a_layer_that_no_longer_fits() {
        let mut layer = Layer::default();
        layer.add(
            "demo",
            String::new(),
            one("flag_over", json!(1000)),
            Vec::new(),
            "1.0",
        );
        assert_eq!(layer.recheck("1.0", |_| unreachable!()), None);

        assert_eq!(layer.recheck("1.1", |_| Ok(())), Some(Recheck::Passed));
        assert_eq!(layer.status(), LayerStatus::On);

        let outcome = layer.recheck("2.0", |_| Err(vec!["flag_over can't be changed".into()]));
        assert!(matches!(outcome, Some(Recheck::Paused(ref r)) if r.contains("version 2.0")));
        assert!(matches!(layer.status(), LayerStatus::Paused { .. }));
        assert!(layer.in_use().is_none(), "tasks run the published agent");
        assert_eq!(
            layer.recheck("2.0", |_| Ok(())),
            None,
            "checked once per version"
        );

        layer.add("demo", "fixed".into(), BTreeMap::new(), Vec::new(), "2.0");
        assert_eq!(layer.status(), LayerStatus::On);
    }

    #[test]
    fn a_damaged_file_is_reported_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let store = LayerStore::new(dir.path());
        let path = dir.path().join("1001/invoice-demo.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{").unwrap();
        assert!(matches!(
            store.load(1001, &agent()),
            Err(StoreError::Damaged { .. })
        ));
        assert!(store.update(1001, &agent(), |_| ()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{");
    }
}
