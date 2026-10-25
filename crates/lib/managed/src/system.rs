//! What an image's system is (ADR 0064, EP-1): every file of its
//! programs, libraries and kernel by digest, as the release side lists
//! them and the OS key signs the list. A computer checks its system
//! against the list at every start.
//!
//! The list goes in the image beside the root's key list as of the image,
//! so the first start has the OS key before any update brings a newer
//! list: whichever of the two is newer names the keys.

use std::collections::BTreeMap;

use glass_types::FileDigest;
use serde::{Deserialize, Serialize};

use crate::Signed;
use crate::hwpatch::GLASSHOUSE;
use crate::osupdate::KeyList;
use crate::signing::DocKind;

/// The signed list, from the image's root.
pub const MANIFEST_PATH: &str = "usr/local/share/glasshouse/system.json";
/// The root's key list as of the image, from its root.
pub const KEYS_PATH: &str = "usr/local/share/glasshouse/keys.json";

/// An image's system files, as the OS key signs them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemManifest {
    /// The image's version, as its os-release.json says.
    pub version: String,
    /// By path from the root: `usr/bin/ls`.
    pub files: BTreeMap<String, FileDigest>,
}

/// The list, if an OS key the root lists signed it.
pub fn open_manifest(signed: &Signed, keys: &KeyList) -> Result<SystemManifest, String> {
    let os = keys.for_purpose("os");
    if os.is_empty() {
        return Err("the key list names no OS key".into());
    }
    signed
        .open(DocKind::OsManifest, GLASSHOUSE, &os, None)
        .map_err(|e| format!("the list of the system's files: {e}"))
}

/// How the system found differs from its list.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Differences {
    /// Listed, and not the same.
    pub changed: Vec<String>,
    /// Listed, and not there.
    pub missing: Vec<String>,
    /// There, and not listed: a program nobody signed.
    pub added: Vec<String>,
}

impl Differences {
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    pub fn count(&self) -> usize {
        self.changed.len() + self.missing.len() + self.added.len()
    }

    /// Each, for people: `/usr/bin/yes (changed)`.
    pub fn described(&self) -> Vec<String> {
        let mut all = Vec::with_capacity(self.count());
        for (list, what) in [
            (&self.changed, "changed"),
            (&self.missing, "missing"),
            (&self.added, "not listed"),
        ] {
            all.extend(list.iter().map(|p| format!("/{p} ({what})")));
        }
        all
    }
}

/// `expected` (the list, with any kernel patch's modules over it) against
/// what's there.
pub fn compare(
    expected: &BTreeMap<String, FileDigest>,
    found: &BTreeMap<String, FileDigest>,
) -> Differences {
    let mut d = Differences::default();
    for (path, want) in expected {
        match found.get(path) {
            Some(got) if got == want => {}
            Some(_) => d.changed.push(path.clone()),
            None => d.missing.push(path.clone()),
        }
    }
    d.added = found
        .keys()
        .filter(|p| !expected.contains_key(*p))
        .cloned()
        .collect();
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OrgKey;
    use crate::osupdate::ListedKey;

    fn files(list: &[(&str, &str)]) -> BTreeMap<String, FileDigest> {
        list.iter()
            .map(|(p, sha)| ((*p).to_owned(), FileDigest::file((*sha).to_owned(), 1)))
            .collect()
    }

    /// ADR 0064: the list opens only with an OS key the root lists, and
    /// what differs is named: changed, missing, and not listed.
    #[test]
    fn ep1_the_system_is_checked_against_the_list_the_os_key_signed() {
        let os = OrgKey::from_seed([1; 32]);
        let other = OrgKey::from_seed([2; 32]);
        let manifest = SystemManifest {
            version: "2026.11.1".into(),
            files: files(&[("usr/bin/yes", "aa"), ("usr/bin/no", "bb")]),
        };
        let body = serde_json::to_string(&manifest).unwrap();
        let keys = KeyList {
            keys: vec![ListedKey {
                purpose: "os".into(),
                key: os.public(),
            }],
            revoked: vec![],
        };
        let signed = os.sign(DocKind::OsManifest, GLASSHOUSE, 7, body.clone());
        assert_eq!(open_manifest(&signed, &keys).unwrap(), manifest);
        // Another key, another kind of document, a revoked key: no.
        let forged = other.sign(DocKind::OsManifest, GLASSHOUSE, 7, body.clone());
        assert!(open_manifest(&forged, &keys).is_err());
        let image = os.sign(DocKind::OsImage, GLASSHOUSE, 7, body);
        assert!(open_manifest(&image, &keys).is_err());
        let revoked = KeyList {
            revoked: vec![os.public().id()],
            ..keys
        };
        assert!(open_manifest(&signed, &revoked).is_err());

        let found = files(&[
            ("usr/bin/yes", "aa"),
            ("usr/bin/no", "cc"),
            ("usr/bin/x", "dd"),
        ]);
        assert!(compare(&manifest.files, &manifest.files).is_empty());
        let d = compare(&manifest.files, &found);
        assert_eq!(d.changed, ["usr/bin/no"]);
        assert_eq!(d.added, ["usr/bin/x"]);
        let gone = files(&[("usr/bin/no", "bb")]);
        assert_eq!(compare(&manifest.files, &gone).missing, ["usr/bin/yes"]);
        assert_eq!(
            d.described(),
            ["/usr/bin/no (changed)", "/usr/bin/x (not listed)"]
        );
    }
}
