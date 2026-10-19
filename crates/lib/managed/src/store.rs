//! The releases a device last applied, kept on its own disk (ADR 0021,
//! ADR 0023).
//!
//! ```text
//! <state_dir>/managed/policy.json            the signed policy release in force
//! <state_dir>/managed/policy.previous.json   the one before it
//! <state_dir>/managed/catalog.json           the signed catalog release in force
//! <state_dir>/managed/catalog.previous.json
//! ```
//!
//! They are kept signed, as they arrived, and checked again when read: a
//! damaged or swapped copy is not trusted, and the device carries on with
//! what it had before (or nothing) instead.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{DocKind, PublicKey, Signed};

#[derive(Debug, Clone)]
pub struct ReleaseStore {
    dir: PathBuf,
}

impl ReleaseStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self, kind: DocKind) -> PathBuf {
        self.dir.join(format!("{kind}.json"))
    }

    pub fn previous_path(&self, kind: DocKind) -> PathBuf {
        self.dir.join(format!("{kind}.previous.json"))
    }

    /// The release of `kind` last applied, checked again against `org` and
    /// `keys`. `Ok(None)` when there is none; `Err` says why a copy that is
    /// there can't be trusted.
    pub fn load(
        &self,
        kind: DocKind,
        org: &str,
        keys: &[PublicKey],
    ) -> Result<Option<Signed>, String> {
        let path = self.path(kind);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let signed: Signed =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        signed
            .verify(kind, org, keys, None)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(signed))
    }

    /// Records `signed` as the release in force, keeping the one it
    /// replaces as the previous. Written to a temporary file, synced and
    /// renamed: a crash leaves one or the other, never a mix.
    pub fn save(&self, signed: &Signed) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(signed.kind);
        if let Ok(current) = fs::read_to_string(&path) {
            write_atomically(&self.previous_path(signed.kind), current.as_bytes())?;
        }
        let text = serde_json::to_vec_pretty(signed).map_err(io::Error::other)?;
        write_atomically(&path, &text)
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.new"));
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OrgKey;

    #[test]
    fn adr0021_the_last_applied_release_is_kept_and_checked_again() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ReleaseStore::new(tmp.path().join("managed"));
        let org = OrgKey::from_seed([3; 32]);
        let keys = [org.public()];
        assert_eq!(store.load(DocKind::Catalog, "acme", &keys), Ok(None));

        let first = org.sign(DocKind::Catalog, "acme", 1, "{\"items\":[]}".into());
        let second = org.sign(DocKind::Catalog, "acme", 2, "{\"items\":[]}".into());
        store.save(&first).unwrap();
        store.save(&second).unwrap();
        let loaded = store.load(DocKind::Catalog, "acme", &keys).unwrap();
        assert_eq!(loaded.map(|s| s.version), Some(2));
        let previous: Signed = serde_json::from_str(
            &fs::read_to_string(store.previous_path(DocKind::Catalog)).unwrap(),
        )
        .unwrap();
        assert_eq!(previous.version, 1);

        // A damaged copy is not trusted.
        let mut text = fs::read_to_string(store.path(DocKind::Catalog)).unwrap();
        text = text.replace("\"version\": 2", "\"version\": 9");
        fs::write(store.path(DocKind::Catalog), text).unwrap();
        let err = store.load(DocKind::Catalog, "acme", &keys).unwrap_err();
        assert!(err.contains("signature"), "{err}");
    }
}
