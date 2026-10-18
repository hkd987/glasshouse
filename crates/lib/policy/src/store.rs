//! Which policy is in force when glassd starts, and the last one that worked
//! (ADR 0021).
//!
//! Start-up reads only local disk, never the network: the policy file
//! (`policy.toml`), or the built-in default when there is none. If the file
//! is there but doesn't validate (a bad edit, a half-written copy), glassd
//! enforces the last policy that did and says why, instead of refusing to
//! start; only when there is no good copy either does it refuse (fail
//! closed). Every policy that goes into force is remembered in the store
//! once glassd is up, with the one before it kept for rollback:
//!
//! ```text
//! <state_dir>/policy/last-good.toml   the last policy in force (validated)
//! <state_dir>/policy/previous.toml    the one before it
//! ```
//!
//! New settings, such as managed policy fetched in the background (AD-15),
//! go through [`PolicyStore::apply`]: validated first, then swapped in with
//! one rename, the version they replace kept for [`PolicyStore::rollback`].

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{DEFAULT_POLICY, Policy};

const LAST_GOOD: &str = "last-good.toml";
const PREVIOUS: &str = "previous.toml";

/// Where the policy in force came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The policy file, as configured.
    File(PathBuf),
    /// No policy file: the built-in default, safe to ship unchanged.
    BuiltIn,
    /// The policy file didn't validate, so the last good policy is in force.
    LastGood { rejected: PathBuf, reason: String },
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(f, "{}", path.display()),
            Self::BuiltIn => f.write_str("built-in default"),
            Self::LastGood { rejected, .. } => {
                write!(f, "last good policy ({} was rejected)", rejected.display())
            }
        }
    }
}

/// A validated policy, its text, and where it came from.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub policy: Policy,
    pub text: String,
    pub origin: Origin,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {reason}; and there is no earlier policy that worked to fall back on")]
    NoGoodPolicy { path: PathBuf, reason: String },
    #[error("the new policy doesn't validate: {0}")]
    Invalid(String),
    #[error("there is no earlier policy to roll back to")]
    NothingToRollBack,
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
}

fn io_err(context: impl fmt::Display) -> impl FnOnce(io::Error) -> StoreError {
    let context = context.to_string();
    move |source| StoreError::Io { context, source }
}

/// The policy store under glassd's state directory.
#[derive(Debug, Clone)]
pub struct PolicyStore {
    dir: PathBuf,
}

impl PolicyStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn last_good_path(&self) -> PathBuf {
        self.dir.join(LAST_GOOD)
    }

    pub fn previous_path(&self) -> PathBuf {
        self.dir.join(PREVIOUS)
    }

    /// The policy to enforce now: `file` if it validates, the built-in
    /// default if there is no `file`, and the last good policy if `file`
    /// is there but doesn't validate. Reads local disk only.
    pub fn load(&self, file: &Path) -> Result<Loaded, StoreError> {
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let policy = Policy::from_toml(DEFAULT_POLICY)
                    .map_err(|e| StoreError::Invalid(format!("built-in default: {e}")))?;
                return Ok(Loaded {
                    policy,
                    text: DEFAULT_POLICY.to_owned(),
                    origin: Origin::BuiltIn,
                });
            }
            Err(e) => return self.fall_back(file, format!("can't be read: {e}")),
        };
        match Policy::from_toml(&text) {
            Ok(policy) => Ok(Loaded {
                policy,
                text,
                origin: Origin::File(file.to_owned()),
            }),
            Err(e) => self.fall_back(file, e.to_string()),
        }
    }

    fn fall_back(&self, file: &Path, reason: String) -> Result<Loaded, StoreError> {
        let no_good = || StoreError::NoGoodPolicy {
            path: file.to_owned(),
            reason: reason.clone(),
        };
        let text = fs::read_to_string(self.last_good_path()).map_err(|_| no_good())?;
        let policy = Policy::from_toml(&text).map_err(|_| no_good())?;
        Ok(Loaded {
            policy,
            text,
            origin: Origin::LastGood {
                rejected: file.to_owned(),
                reason,
            },
        })
    }

    /// Records the policy now in force as the last good one, keeping the
    /// one it replaces as the previous version. Returns whether anything
    /// changed. Call it once glassd is up: it writes and syncs files.
    pub fn remember(&self, loaded: &Loaded) -> Result<bool, StoreError> {
        if matches!(loaded.origin, Origin::LastGood { .. }) {
            return Ok(false);
        }
        self.replace(&self.last_good_path(), &loaded.text)
    }

    /// Puts new policy text into force at `file`: validates it first, then
    /// replaces `file` with one rename, keeping what it replaces as the
    /// previous version. Nothing changes if it doesn't validate.
    pub fn apply(&self, file: &Path, text: &str) -> Result<Policy, StoreError> {
        let policy = Policy::from_toml(text).map_err(|e| StoreError::Invalid(e.to_string()))?;
        self.replace(file, text)?;
        Ok(policy)
    }

    /// Puts the previous version back at `file`.
    pub fn rollback(&self, file: &Path) -> Result<Policy, StoreError> {
        let text =
            fs::read_to_string(self.previous_path()).map_err(|_| StoreError::NothingToRollBack)?;
        let policy = Policy::from_toml(&text).map_err(|e| StoreError::Invalid(e.to_string()))?;
        write_atomically(file, &text)?;
        Ok(policy)
    }

    /// Writes `text` to `file` unless it already holds it, first copying
    /// what was there to the previous version.
    fn replace(&self, file: &Path, text: &str) -> Result<bool, StoreError> {
        let current = fs::read_to_string(file).ok();
        if current.as_deref() == Some(text) {
            return Ok(false);
        }
        fs::create_dir_all(&self.dir)
            .map_err(io_err(format_args!("creating {}", self.dir.display())))?;
        if let Some(current) = current
            && Policy::from_toml(&current).is_ok()
        {
            write_atomically(&self.previous_path(), &current)?;
        }
        write_atomically(file, text)?;
        Ok(true)
    }
}

/// Writes a sibling temporary file, syncs it, renames it over `path` and
/// syncs the directory: a crash leaves the old file or the new one, never a
/// mix.
fn write_atomically(path: &Path, text: &str) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.new"));
    let mut file = File::create(&tmp).map_err(io_err(format_args!("writing {}", tmp.display())))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(io_err(format_args!("writing {}", tmp.display())))?;
    fs::rename(&tmp, path).map_err(io_err(format_args!("replacing {}", path.display())))?;
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_err(format_args!("syncing {}", dir.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "version = 1\n";
    const OTHER: &str = "version = 1\n[org]\nmax_label = \"public\"\n";
    const BAD: &str = "version = 1\nunknown_field = true\n";

    fn setup() -> (tempfile::TempDir, PolicyStore, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let store = PolicyStore::new(tmp.path().join("state/policy"));
        let file = tmp.path().join("policy.toml");
        (tmp, store, file)
    }

    #[test]
    fn no_file_means_the_built_in_default() {
        let (_tmp, store, file) = setup();
        let loaded = store.load(&file).unwrap();
        assert_eq!(loaded.origin, Origin::BuiltIn);
        assert_eq!(loaded.text, DEFAULT_POLICY);
    }

    #[test]
    fn a_valid_file_is_used_and_remembered() {
        let (_tmp, store, file) = setup();
        fs::write(&file, GOOD).unwrap();
        let loaded = store.load(&file).unwrap();
        assert_eq!(loaded.origin, Origin::File(file.clone()));
        assert!(store.remember(&loaded).unwrap());
        assert!(!store.remember(&loaded).unwrap(), "unchanged: no write");
        assert_eq!(fs::read_to_string(store.last_good_path()).unwrap(), GOOD);
    }

    #[test]
    fn a_bad_file_falls_back_to_the_last_good_policy() {
        let (_tmp, store, file) = setup();
        fs::write(&file, GOOD).unwrap();
        store.remember(&store.load(&file).unwrap()).unwrap();
        fs::write(&file, BAD).unwrap();
        let loaded = store.load(&file).unwrap();
        assert_eq!(loaded.text, GOOD);
        match &loaded.origin {
            Origin::LastGood { rejected, reason } => {
                assert_eq!(rejected, &file);
                assert!(reason.contains("unknown_field"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert!(loaded.origin.to_string().contains("was rejected"));
        assert!(!store.remember(&loaded).unwrap(), "a fallback isn't new");
    }

    #[test]
    fn a_bad_file_with_nothing_to_fall_back_on_fails_closed() {
        let (_tmp, store, file) = setup();
        fs::write(&file, BAD).unwrap();
        let err = store.load(&file).unwrap_err();
        assert!(matches!(err, StoreError::NoGoodPolicy { .. }), "{err}");
        assert!(err.to_string().contains("no earlier policy"), "{err}");
    }

    #[test]
    fn a_corrupt_last_good_copy_is_not_trusted() {
        let (_tmp, store, file) = setup();
        fs::create_dir_all(store.last_good_path().parent().unwrap()).unwrap();
        fs::write(store.last_good_path(), BAD).unwrap();
        fs::write(&file, BAD).unwrap();
        assert!(store.load(&file).is_err());
    }

    #[test]
    fn apply_validates_first_and_keeps_the_previous_version() {
        let (_tmp, store, file) = setup();
        fs::write(&file, GOOD).unwrap();
        assert!(matches!(
            store.apply(&file, BAD),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(fs::read_to_string(&file).unwrap(), GOOD, "untouched");
        store.apply(&file, OTHER).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), OTHER);
        assert_eq!(fs::read_to_string(store.previous_path()).unwrap(), GOOD);
        store.rollback(&file).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), GOOD);
    }

    #[test]
    fn rollback_needs_a_previous_version() {
        let (_tmp, store, file) = setup();
        assert!(matches!(
            store.rollback(&file),
            Err(StoreError::NothingToRollBack)
        ));
    }
}
