//! Where the apps people make are kept (ADR 0032).
//!
//! Every version glassd checked and bundled is a folder of its own under
//! the state directory, root only, never changed once written:
//! `<root>/<login>/<app>/<n>/` with `record.json`, `manifest.json`, the
//! source (`src/`) and what runs (`dist/`). Which version is in use is the
//! person's choice, a file in their home ([`MINE`]) written as them, so undo
//! restores it like any file a call changed (GA-3). Writing it by hand only
//! picks among versions that passed the checks.
//!
//! Not every version is kept for good (audit 5.3; each is the app's whole
//! source and build): the newest [`VERSIONS_KEPT`] of an app are, the one
//! in use is, and any that was in use in the last three months, which is
//! what undoing a change of version can name ([`glass_state::kept`]).
//! [`Store::moved`] notes each change of the version in use and removes
//! the rest.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use glass_api::AppManifest;
use glass_platform::fs::ConfinedDir;
use glass_types::{ActionName, AgentId, Timestamp};
use serde::{Deserialize, Serialize};

/// Which version of each of their apps a person uses, under their home.
pub const MINE: &str = ".local/share/glasshouse/apps/mine.json";
/// The newest versions of an app that are kept whatever their age.
pub const VERSIONS_KEPT: usize = 20;
/// In a version's folder: when it was last known to be the one in use.
pub(crate) const USED: &str = ".used";
const MAX_MINE_BYTES: u64 = 64 * 1024;

/// The app a remix starts from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Base {
    pub id: AgentId,
    pub name: String,
    pub version: String,
}

/// One saved version of an app someone made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub app: AgentId,
    pub name: String,
    pub version: u32,
    /// Whose it is: the login it was made for.
    pub owner: String,
    /// What it remixes; none for an app built from the SDK's template.
    #[serde(default)]
    pub based_on: Option<Base>,
    /// What changed, in the words of whoever saved it.
    pub note: String,
    pub saved_at: Timestamp,
    /// The agent (or app) that saved it.
    pub saved_by: AgentId,
    /// SHA-256 of the source, for the inventory and later reviews.
    pub source_sha256: String,
    /// What the checks noted without refusing.
    #[serde(default)]
    pub notes: Vec<String>,
    /// The need a new app meets (GA-12): what the store was searched for.
    #[serde(default)]
    pub need: Option<String>,
    /// What the store or the bases offered for that need that the person
    /// turned down.
    #[serde(default)]
    pub declined: Vec<AgentId>,
    /// Actions it declares that its base doesn't (the privilege delta).
    #[serde(default)]
    pub beyond_base: Vec<ActionName>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Mine {
    apps: BTreeMap<AgentId, u32>,
}

/// The versions `home`'s person uses, by app. Missing or unreadable: none.
pub fn chosen(home: &Path) -> BTreeMap<AgentId, u32> {
    read_mine(home).unwrap_or_default()
}

/// The person's choices as their file has them. The file is one they may
/// edit: one that no longer reads as choices (a typo) is set aside beside
/// itself and they start again, where the next save used to write over it
/// with only the app being saved. One that can't be read at all is an
/// error.
fn read_mine(home: &Path) -> io::Result<BTreeMap<AgentId, u32>> {
    Ok(ConfinedDir::open(home)?
        .read_json_or_set_aside::<Mine>(Path::new(MINE), MAX_MINE_BYTES)?
        .map(|mine| mine.apps)
        .unwrap_or_default())
}

/// Writes the person's choices through their confined home, as them.
/// Never over a file of choices that can't be read.
pub fn choose(
    home: &Path,
    owner: glass_platform::fs::Owner,
    apps: BTreeMap<AgentId, u32>,
) -> io::Result<()> {
    read_mine(home)?;
    let dir = ConfinedDir::open(home)?;
    if let Some(folder) = Path::new(MINE).parent()
        && !dir.exists(folder)
    {
        dir.create_dir_all_as(folder, owner)?;
    }
    let text = serde_json::to_vec_pretty(&Mine { apps }).map_err(io::Error::other)?;
    dir.write(Path::new(MINE), &text, Some(owner)).map(|_| ())
}

/// Reads every file under `dir/sub` as text, by its path from `dir`
/// (`src/main.ts`), in at most [`crate::checks::MAX_FILES`] files.
pub fn read_tree(dir: &Path, sub: &str) -> io::Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![PathBuf::from(sub)];
    while let Some(rel) = pending.pop() {
        for entry in std::fs::read_dir(dir.join(&rel))? {
            let entry = entry?;
            let name = rel.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(name);
            } else if kind.is_file() {
                if files.len() >= crate::checks::MAX_FILES * 2 {
                    return Err(io::Error::other("too many files"));
                }
                let text = std::fs::read_to_string(dir.join(&name))?;
                files.insert(name.to_string_lossy().into_owned(), text);
            }
        }
    }
    Ok(files)
}

/// The apps people make, under glassd's state directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn app_dir(&self, login: &str, app: &AgentId) -> PathBuf {
        self.root.join(login).join(app.as_str())
    }

    fn version_dir(&self, login: &str, app: &AgentId, version: u32) -> PathBuf {
        self.app_dir(login, app).join(version.to_string())
    }

    /// The versions of `login`'s `app`, oldest first.
    pub fn versions(&self, login: &str, app: &AgentId) -> Vec<u32> {
        let mut versions: Vec<u32> = std::fs::read_dir(self.app_dir(login, app))
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        versions.sort_unstable();
        versions
    }

    /// The apps `login` has made, by id.
    pub fn apps(&self, login: &str) -> Vec<AgentId> {
        let mut apps: Vec<AgentId> = std::fs::read_dir(self.root.join(login))
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        apps.sort();
        apps
    }

    /// Everyone who has made an app here.
    pub fn owners(&self) -> Vec<String> {
        let mut owners: Vec<String> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let e = e.ok()?;
                let name = e.file_name().to_str()?.to_owned();
                (e.file_type().ok()?.is_dir() && glass_platform::users::valid_login(&name))
                    .then_some(name)
            })
            .collect();
        owners.sort();
        owners
    }

    pub fn record(&self, login: &str, app: &AgentId, version: u32) -> Option<Record> {
        let text = std::fs::read(self.version_dir(login, app, version).join("record.json")).ok()?;
        serde_json::from_slice(&text).ok()
    }

    pub fn manifest(&self, login: &str, app: &AgentId, version: u32) -> Option<AppManifest> {
        let text =
            std::fs::read(self.version_dir(login, app, version).join("manifest.json")).ok()?;
        serde_json::from_slice(&text).ok()
    }

    /// A version's source, by path (`src/main.ts`).
    pub fn source(
        &self,
        login: &str,
        app: &AgentId,
        version: u32,
    ) -> io::Result<BTreeMap<String, String>> {
        read_tree(&self.version_dir(login, app, version), "src")
    }

    /// What a version serves, by file name (`app.js`).
    pub fn dist(
        &self,
        login: &str,
        app: &AgentId,
        version: u32,
    ) -> io::Result<BTreeMap<String, String>> {
        let dir = self.version_dir(login, app, version).join("dist");
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                files.insert(name.to_owned(), std::fs::read_to_string(entry.path())?);
            }
        }
        Ok(files)
    }

    /// Notes that `login` went from version `from` of `app` to `to` as
    /// the one in use, and removes the versions that are past keeping
    /// ([`glass_state::kept`]). Which went, lowest first.
    ///
    /// Call it after the person's choice is written. Nothing is removed
    /// if a note can't be kept: a version without one would look unused.
    pub fn moved(&self, login: &str, app: &AgentId, from: Option<u32>, to: u32) -> Vec<u32> {
        self.moved_at(login, app, from, to, std::time::SystemTime::now())
    }

    pub(crate) fn moved_at(
        &self,
        login: &str,
        app: &AgentId,
        from: Option<u32>,
        to: u32,
        now: std::time::SystemTime,
    ) -> Vec<u32> {
        use glass_state::kept;

        let note = |version: u32| self.version_dir(login, app, version).join(USED);
        let noted = from
            .into_iter()
            .chain([to])
            .filter(|v| self.version_dir(login, app, *v).is_dir())
            .try_for_each(|v| kept::note_used(&note(v), now));
        let Ok(since) = noted.and_then(|()| kept::since(&self.root, now)) else {
            return Vec::new();
        };
        let versions: Vec<kept::Version> = self
            .versions(login, app)
            .into_iter()
            .filter_map(|number| {
                let used = kept::last_used(&note(number)).or_else(|| {
                    // When it was saved.
                    std::fs::metadata(self.version_dir(login, app, number).join("record.json"))
                        .and_then(|m| m.modified())
                        .ok()
                })?;
                Some(kept::Version { number, used })
            })
            .collect();
        let app_dir = self.app_dir(login, app);
        // What a removal that glassd stopped in the middle of left.
        for entry in std::fs::read_dir(&app_dir).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with(".gone-") {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        kept::past_keeping(&versions, Some(to), VERSIONS_KEPT, since, now)
            .into_iter()
            .filter(|version| {
                // Out of the way whole, then removed: a version either
                // exists complete or not at all.
                let gone = app_dir.join(format!(".gone-{version}"));
                let _ = std::fs::remove_dir_all(&gone);
                std::fs::rename(self.version_dir(login, app, *version), &gone).is_ok_and(|()| {
                    let _ = std::fs::remove_dir_all(&gone);
                    true
                })
            })
            .collect()
    }

    /// Writes a new version whole, then puts it in place: a version either
    /// exists complete or not at all.
    pub fn save(
        &self,
        record: &Record,
        manifest: &AppManifest,
        source: &BTreeMap<String, String>,
        dist: &BTreeMap<String, String>,
    ) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let app_dir = self.app_dir(&record.owner, &record.app);
        std::fs::create_dir_all(&app_dir)?;
        for dir in [self.root.as_path(), &self.root.join(&record.owner)] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let staging = app_dir.join(format!(".new-{}", record.version));
        let _ = std::fs::remove_dir_all(&staging);
        let write = |rel: &str, text: &[u8]| -> io::Result<()> {
            let path = staging.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, text)
        };
        write("record.json", &json(record)?)?;
        write("manifest.json", &json(manifest)?)?;
        // Paths were checked (`checks::check_source`): under src/, no `..`.
        for (path, text) in source {
            write(path, text.as_bytes())?;
        }
        for (name, text) in dist {
            write(&format!("dist/{name}"), text.as_bytes())?;
        }
        std::fs::rename(
            &staging,
            self.version_dir(&record.owner, &record.app, record.version),
        )
    }
}

fn json<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::time::{Duration, SystemTime};

    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 3600);
    /// No version removed.
    const NONE: [u32; 0] = [];

    /// A store with `demo`'s app saved up to version `upto`, each as a
    /// save leaves it.
    fn saved(store: &Store, app: &AgentId, from: u32, upto: u32) {
        for version in from..=upto {
            let dir = store.version_dir("demo", app, version);
            std::fs::create_dir_all(dir.join("dist")).unwrap();
            std::fs::write(dir.join("record.json"), b"{}").unwrap();
            std::fs::write(dir.join("dist/app.js"), b"// the app").unwrap();
        }
    }

    // Audit 5.3: every version of every app a person made stayed for good,
    // its whole source and build.
    #[test]
    fn ga3_an_apps_old_versions_go_once_nothing_can_name_them() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("apps"));
        let app: AgentId = "my-mail".parse().unwrap();
        let day = |n: u32| SystemTime::now() + DAY * n;
        let versions = |store: &Store| store.versions("demo", &app);

        // Twenty-five saves in a day: nothing goes. Any of them may be
        // what an undo puts back.
        saved(&store, &app, 1, 25);
        assert_eq!(store.moved_at("demo", &app, Some(24), 25, day(0)), NONE);
        assert_eq!(versions(&store).len(), 25);

        // Still nothing two months on.
        saved(&store, &app, 26, 26);
        assert_eq!(store.moved_at("demo", &app, Some(25), 26, day(60)), NONE);

        // Past three months, the ones before the newest twenty go, whole.
        saved(&store, &app, 27, 27);
        assert_eq!(
            store.moved_at("demo", &app, Some(26), 27, day(100)),
            [1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(versions(&store), (8..=27).collect::<Vec<u32>>());
        let left: Vec<String> = std::fs::read_dir(store.app_dir("demo", &app))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.parse::<u32>().is_err())
            .collect();
        assert_eq!(left, Vec::<String>::new(), "nothing half removed");

        // The person goes back to version 8, and uses it for a while.
        assert_eq!(store.moved_at("demo", &app, Some(27), 8, day(100)), NONE);
        // A save moves away from it: 8 was in use until this moment, so
        // undoing the save names it. It stays; 9, unused all along, goes.
        saved(&store, &app, 28, 29);
        assert_eq!(store.moved_at("demo", &app, Some(8), 29, day(150)), [9]);
        assert!(versions(&store).contains(&8));
        // Fifty days after it was last in use it's still there, as the
        // next unused one goes; three months after, it goes too.
        saved(&store, &app, 30, 30);
        assert_eq!(store.moved_at("demo", &app, Some(29), 30, day(200)), [10]);
        assert!(versions(&store).contains(&8));
        saved(&store, &app, 31, 31);
        assert_eq!(
            store.moved_at("demo", &app, Some(30), 31, day(250)),
            [8, 11]
        );

        // The one in use is never one of them, however old.
        assert_eq!(store.moved_at("demo", &app, Some(31), 12, day(250)), NONE);
        saved(&store, &app, 32, 33);
        // (Noted as in use on day 250, and still the one in use.)
        assert_eq!(store.moved_at("demo", &app, None, 12, day(400)), [13]);
        assert!(versions(&store).contains(&12));

        // Another app of theirs, and another person's, are their own.
        let other: AgentId = "my-notes".parse().unwrap();
        saved(&store, &other, 1, 3);
        assert_eq!(store.moved_at("demo", &other, Some(2), 3, day(400)), NONE);
        assert_eq!(store.versions("demo", &other), [1, 2, 3]);
    }

    // What glassd left when it stopped in the middle of removing one.
    #[test]
    fn ga3_a_removal_cut_short_is_finished_at_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("apps"));
        let app: AgentId = "my-mail".parse().unwrap();
        saved(&store, &app, 1, 2);
        let half = store.app_dir("demo", &app).join(".gone-0");
        std::fs::create_dir_all(half.join("dist")).unwrap();
        assert_eq!(store.versions("demo", &app), [1, 2]);
        store.moved_at("demo", &app, Some(1), 2, SystemTime::now());
        assert!(!half.exists());
        assert_eq!(store.versions("demo", &app), [1, 2]);
    }
}
