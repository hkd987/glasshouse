//! Signed OS updates (ADR 0061; EP-1, EP-11, EP-12, BS-4): what a release
//! is, and how a computer decides to take one.
//!
//! Three kinds of key, as EP-11 asks, each signing one thing:
//!
//! - **The root** (offline, a key ceremony) signs the [`KeyList`]: the keys
//!   in use, by purpose, and those it revoked. A computer holds only the
//!   root's public key, from its image. A newer list replaces the one it
//!   has, so a stolen channel or OS key is revoked, and a new one brought
//!   in, without re-imaging the fleet (EP-12).
//! - **The OS key** signs each image's digest ([`ImageDigest`]).
//! - **Each channel's key** (`pilot`, `early`, `broad`) signs that
//!   channel's [`Release`], which carries the OS key's signed digest.
//!
//! A computer takes a release only when the list it holds names the
//! channel's key and the OS key, unrevoked, both signatures verify, the
//! digest is for the same release number, and the number is above the
//! last it took ([`verify_release`], `specs/tla/OsUpdate.tla`).

use serde::{Deserialize, Serialize};

use crate::hwpatch::GLASSHOUSE;
use crate::{DocKind, PublicKey, Signed};

/// A computer reads a file of its ring's release from its org's console.
pub const FILE_PATH: &str = "/device/v1/os/file";
/// The most of a file the console sends at once: an image comes in parts.
pub const MAX_PART: u64 = 8 * 1024 * 1024;

/// A file of the release on the computer's channel, from its console: the
/// channel is its ring, which the console knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRequest {
    pub device: String,
    /// `keys.json`, or a file beside the channel's release: its
    /// `release.json`, its image, a kernel patch's rebuild.
    pub name: String,
    /// From this byte on.
    #[serde(default)]
    pub offset: u64,
}

/// The image a release installs: a ZFS stream of a whole boot environment's
/// [`IMAGE_SNAPSHOT`], or of the changes to it since another image's.
pub const IMAGE_SUFFIX: &str = ".zfs";

/// What the root lists: each key's purpose, `os` or `channel:<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyList {
    pub keys: Vec<ListedKey>,
    /// Ids ([`PublicKey::id`]) of keys no longer trusted for anything.
    #[serde(default)]
    pub revoked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListedKey {
    pub purpose: String,
    pub key: PublicKey,
}

impl KeyList {
    /// The keys listed for `purpose` and not revoked.
    pub fn for_purpose(&self, purpose: &str) -> Vec<PublicKey> {
        self.keys
            .iter()
            .filter(|k| k.purpose == purpose && !self.revoked.contains(&k.key.id()))
            .map(|k| k.key)
            .collect()
    }
}

/// An image, as the OS key signs it; the signature's version is the
/// release number it's for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageDigest {
    /// The image's file name beside the release: `glasshouse-<version>.zfs`.
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
    /// The version the image says it is
    /// (`/usr/local/share/glasshouse/os-release.json`).
    pub version: String,
    /// A delta: the changes from the image of this version, which the
    /// computer must be running. Without it, the whole image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_from: Option<String>,
}

/// The snapshot an image is sent from and received as: a delta is the
/// changes between two images' snapshots of this name.
pub const IMAGE_SNAPSHOT: &str = "glass-image";

/// A release on a channel, as its key signs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub channel: String,
    /// For people: `2026.11.1`.
    pub version: String,
    /// The base system release the image is built on: `15.1-RELEASE-p2`.
    pub base: String,
    /// The OS key's signed [`ImageDigest`].
    pub image: Signed,
    /// Kernel patches (ADR 0057) whose fix the image has: a computer that
    /// keeps one drops it with this update.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<String>,
    /// Kernel patches rebuilt for this image: a computer that keeps one
    /// gets the rebuild in the new image.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<PatchRebuild>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchRebuild {
    pub id: String,
    /// The bundle's file name beside the release.
    pub bundle: String,
}

/// A release a computer may take.
#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    /// The release number: only ever goes up.
    pub number: u64,
    pub release: Release,
    pub image: ImageDigest,
}

/// The newest key list: signed by a root key, and newer than `have`.
pub fn open_keys(list: &Signed, roots: &[PublicKey], have: Option<u64>) -> Result<KeyList, String> {
    let keys: KeyList = list
        .open(DocKind::OsKeys, GLASSHOUSE, roots, have)
        .map_err(|e| format!("the key list: {e}"))?;
    if keys.revoked.contains(&list.key) {
        return Err("the key list revokes the root that signed it".into());
    }
    Ok(keys)
}

/// Whether to take `release` on `channel`: its channel's key and the OS
/// key are listed and unrevoked, both signatures verify, the image is for
/// the same release, and the number is above `taken`.
pub fn verify_release(
    keys: &KeyList,
    release: &Signed,
    channel: &str,
    taken: Option<u64>,
) -> Result<Verified, String> {
    let channel_keys = keys.for_purpose(&format!("channel:{channel}"));
    if channel_keys.is_empty() {
        return Err(format!("no key is listed for the {channel} channel"));
    }
    let opened: Release = release
        .open(DocKind::OsRelease, GLASSHOUSE, &channel_keys, taken)
        .map_err(|e| format!("the release: {e}"))?;
    if opened.channel != channel {
        return Err(format!(
            "the release is for the {} channel, and this computer follows {channel}",
            opened.channel
        ));
    }
    let image: ImageDigest = opened
        .image
        .open(DocKind::OsImage, GLASSHOUSE, &keys.for_purpose("os"), None)
        .map_err(|e| format!("the image: {e}"))?;
    if opened.image.version != release.version {
        return Err(format!(
            "the image is signed for release {}, not {}",
            opened.image.version, release.version
        ));
    }
    if !valid_file(&image.file) || !image.file.ends_with(IMAGE_SUFFIX) {
        return Err(format!("{:?} isn't an image's file name", image.file));
    }
    for patch in &opened.patches {
        if !valid_file(&patch.bundle) {
            return Err(format!("{:?} isn't a bundle's file name", patch.bundle));
        }
    }
    Ok(Verified {
        number: release.version,
        release: opened,
        image,
    })
}

/// A file name beside the release, never a path out of its folder.
pub fn valid_file(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// A version as a boot environment's name: `glass-os-2026.11.1`.
pub fn env_name(version: &str) -> Option<String> {
    (!version.is_empty()
        && version.len() <= 40
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')))
    .then(|| format!("glass-os-{version}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OrgKey;

    fn key(n: u8) -> OrgKey {
        OrgKey::from_seed([n; 32])
    }

    const ROOT: u8 = 1;
    const OS: u8 = 2;
    const PILOT: u8 = 3;

    fn list(version: u64, revoked: &[u8]) -> Signed {
        let keys = KeyList {
            keys: vec![
                ListedKey {
                    purpose: "os".into(),
                    key: key(OS).public(),
                },
                ListedKey {
                    purpose: "channel:pilot".into(),
                    key: key(PILOT).public(),
                },
                ListedKey {
                    purpose: "channel:pilot".into(),
                    key: key(5).public(),
                },
            ],
            revoked: revoked.iter().map(|n| key(*n).public().id()).collect(),
        };
        key(ROOT).sign(
            DocKind::OsKeys,
            GLASSHOUSE,
            version,
            serde_json::to_string(&keys).unwrap(),
        )
    }

    fn release(number: u64, image_number: u64, signer: u8, os: u8) -> Signed {
        let digest = ImageDigest {
            file: "glasshouse-2026.11.1.zfs".into(),
            sha256: "ab".repeat(32),
            bytes: 1024,
            version: "2026.11.1".into(),
            delta_from: None,
        };
        let image = key(os).sign(
            DocKind::OsImage,
            GLASSHOUSE,
            image_number,
            serde_json::to_string(&digest).unwrap(),
        );
        let release = Release {
            channel: "pilot".into(),
            version: "2026.11.1".into(),
            base: "15.1-RELEASE-p2".into(),
            image,
            fixes: vec!["lkpi-wlan-suspend".into()],
            patches: vec![],
            notes: String::new(),
        };
        key(signer).sign(
            DocKind::OsRelease,
            GLASSHOUSE,
            number,
            serde_json::to_string(&release).unwrap(),
        )
    }

    #[test]
    fn ep11_a_release_needs_the_channel_key_and_the_os_key_the_root_lists() {
        let keys = open_keys(&list(1, &[]), &[key(ROOT).public()], None).unwrap();
        let ok = verify_release(&keys, &release(7, 7, PILOT, OS), "pilot", Some(6)).unwrap();
        assert_eq!(ok.number, 7);
        assert_eq!(ok.image.version, "2026.11.1");

        let refused = |r: Signed, channel: &str, taken: Option<u64>| {
            verify_release(&keys, &r, channel, taken).unwrap_err()
        };
        // A key the root doesn't list for the channel, or the OS key
        // signing the release.
        assert!(refused(release(7, 7, 9, OS), "pilot", None).contains("doesn't trust"));
        assert!(refused(release(7, 7, OS, OS), "pilot", None).contains("doesn't trust"));
        // The image signed by the channel key, not the OS key.
        assert!(refused(release(7, 7, PILOT, PILOT), "pilot", None).contains("the image"));
        // An image signed for another release.
        assert!(refused(release(7, 6, PILOT, OS), "pilot", None).contains("signed for release 6"));
        // Another channel, an old release, the same one again.
        assert!(refused(release(7, 7, PILOT, OS), "broad", None).contains("broad"));
        assert!(refused(release(6, 6, PILOT, OS), "pilot", Some(6)).contains("not newer"));
    }

    #[test]
    fn ep12_a_revoked_key_signs_nothing_and_needs_no_reimage() {
        let roots = [key(ROOT).public()];
        let first = open_keys(&list(1, &[]), &roots, None).unwrap();
        assert!(verify_release(&first, &release(7, 7, PILOT, OS), "pilot", None).is_ok());
        // The root revokes the pilot key in a newer list; the second pilot
        // key it lists still signs.
        let second = open_keys(&list(2, &[PILOT]), &roots, Some(1)).unwrap();
        let refused = verify_release(&second, &release(7, 7, PILOT, OS), "pilot", None);
        assert!(refused.unwrap_err().contains("doesn't trust"));
        assert!(verify_release(&second, &release(7, 7, 5, OS), "pilot", None).is_ok());
        // The old list can't be played back to bring the key back.
        assert!(
            open_keys(&list(1, &[]), &roots, Some(2))
                .unwrap_err()
                .contains("not newer")
        );
        // Nor can anyone but the root sign a list.
        let forged = key(PILOT).sign(
            DocKind::OsKeys,
            GLASSHOUSE,
            3,
            serde_json::to_string(&KeyList {
                keys: vec![],
                revoked: vec![],
            })
            .unwrap(),
        );
        assert!(open_keys(&forged, &roots, Some(2)).is_err());
    }

    #[test]
    fn adr0061_names_stay_in_their_folder() {
        assert!(valid_file("glasshouse-2026.11.1.zfs"));
        assert!(!valid_file("../etc/passwd"));
        assert!(!valid_file("a/b.zfs"));
        assert!(!valid_file(".hidden"));
        assert_eq!(env_name("2026.11.1").as_deref(), Some("glass-os-2026.11.1"));
        assert_eq!(env_name("2026 11"), None);
    }
}
