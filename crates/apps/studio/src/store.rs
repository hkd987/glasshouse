//! Where the apps people make are kept (ADR 0032).
//!
//! Every version glassd checked and bundled is a folder of its own under
//! the state directory, root only, never changed once written:
//! `<root>/<login>/<app>/<n>/` with `record.json`, `manifest.json`, the
//! source (`src/`) and what runs (`dist/`). Which version is in use is the
//! person's choice, a file in their home ([`MINE`]) written as them, so undo
//! restores it like any file a call changed (GA-3). Writing it by hand only
//! picks among versions that passed the checks.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use glass_api::AppManifest;
use glass_platform::fs::ConfinedDir;
use glass_types::{ActionName, AgentId, Timestamp};
use serde::{Deserialize, Serialize};

/// Which version of each of their apps a person uses, under their home.
pub const MINE: &str = ".local/share/glasshouse/apps/mine.json";
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
