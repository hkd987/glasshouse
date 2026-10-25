//! glassd's check of the system's files at every start (ADR 0064, EP-1):
//! the image's programs, libraries and kernel against the list the OS key
//! signed for it, with the modules a kept kernel patch wrote over it. A
//! system that differs runs no agent until it's put right: an update taken
//! out, or the computer installed again.
//!
//! Verified, the files are made read-only again where they weren't: the
//! system immutable flag, which not even root gets past without taking it
//! off first.
//!
//! What this doesn't stop: root taking the flag off, changing a file and
//! glassd with it, before the next start. That needs the start itself
//! verified (Secure Boot) and every program checked as it runs (veriexec):
//! ADR 0026's slices.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use glass_api::{SystemCheck, SystemCheckState};
use glass_managed::system::{self, KEYS_PATH, MANIFEST_PATH};
use glass_managed::{PublicKey, Signed};
use glass_types::{FileDigest, Timestamp};

/// How many differences a check names; the rest are counted.
const NAMED: usize = 20;
/// The most of the list or the key list read.
const MAX_LIST: u64 = 64 * 1024 * 1024;

/// What a check goes on.
#[derive(Debug)]
pub struct Inputs<'a> {
    /// The system's root: `/`.
    pub root: &'a Path,
    /// The root keys the image trusts (`[update] roots`).
    pub roots: &'a [PublicKey],
    /// The key list an update brought, newer than the image's, if any.
    pub held_keys: Option<Signed>,
    /// An image built for release says the check must pass: with no list,
    /// or none that verifies, no agent starts.
    pub required: bool,
    /// The modules kernel patches wrote over the image, by path.
    pub patched: BTreeMap<String, FileDigest>,
}

/// The check, and the files to make read-only again.
pub fn check(inputs: &Inputs<'_>) -> (SystemCheck, Vec<PathBuf>) {
    let started = Instant::now();
    let not_checked = |why: &str| {
        let mut check = SystemCheck::unchecked(why);
        if inputs.required {
            check.state = SystemCheckState::Changed;
        }
        (check, Vec::new())
    };
    let changed = |why: String| {
        let mut check = SystemCheck::unchecked(why);
        check.state = SystemCheckState::Changed;
        check.checked_at = Some(Timestamp::now());
        (check, Vec::new())
    };
    let Some(signed) = read::<Signed>(&inputs.root.join(MANIFEST_PATH)) else {
        return not_checked(
            "this image carries no signed list of its system files: a development build",
        );
    };
    if inputs.roots.is_empty() {
        return not_checked("this computer trusts no root key, so nothing vouches for its system");
    }
    // The newer of the image's key list and one an update brought: a key
    // revoked since the image was made is revoked here too.
    let lists = [
        inputs.held_keys.clone(),
        read::<Signed>(&inputs.root.join(KEYS_PATH)),
    ];
    let Some(keys) = lists
        .into_iter()
        .flatten()
        .filter_map(|list| {
            glass_managed::osupdate::open_keys(&list, inputs.roots, None)
                .ok()
                .map(|keys| (list.version, keys))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, keys)| keys)
    else {
        return changed("no key list the root signed is here to check the system with".into());
    };
    let manifest = match system::open_manifest(&signed, &keys) {
        Ok(manifest) => manifest,
        Err(e) => return changed(e),
    };
    let mut expected = manifest.files;
    expected.extend(inputs.patched.clone());
    let found = match glass_platform::integrity::system_files(inputs.root) {
        Ok(found) => found,
        Err(e) => return changed(format!("the system's files couldn't be read: {e}")),
    };
    let differences = system::compare(&expected, &found);
    let mut check = SystemCheck {
        state: SystemCheckState::Verified,
        version: Some(manifest.version),
        files: found.len() as u64,
        checked_at: Some(Timestamp::now()),
        took_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        note: None,
        differences: Vec::new(),
        different: 0,
        sealed: 0,
    };
    if !differences.is_empty() {
        let count = differences.count();
        check.state = SystemCheckState::Changed;
        check.note = Some(format!(
            "{count} of the system's files differ from the list the OS key signed for this image"
        ));
        check.differences = differences.described().into_iter().take(NAMED).collect();
        check.different = count as u64;
        return (check, Vec::new());
    }
    let unsealed = expected
        .iter()
        .filter(|(_, digest)| digest.link.is_none())
        .map(|(path, _)| inputs.root.join(path))
        .filter(|path| glass_platform::integrity::immutable(path) == Some(false))
        .collect();
    (check, unsealed)
}

/// Agents start on a system that is the image's, or one nothing can vouch
/// for (a development build); not while it's checked, nor on one that
/// differs.
pub fn may_start(check: &SystemCheck) -> Result<(), String> {
    match check.state {
        SystemCheckState::Unchecked | SystemCheckState::Verified => Ok(()),
        SystemCheckState::Checking => Err(
            "this computer is checking its system files, as it does at every start: start it \
             again in a moment"
                .into(),
        ),
        SystemCheckState::Changed => Err(format!(
            "this computer's system has changed since it was signed ({}), so no agent runs on \
             it: IT puts it right, by taking out the last update or installing it again",
            check.note.as_deref().unwrap_or("its files differ")
        )),
    }
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_LIST).read_to_end(&mut bytes).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glass_managed::hwpatch::GLASSHOUSE;
    use glass_managed::osupdate::{KeyList, ListedKey};
    use glass_managed::system::SystemManifest;
    use glass_managed::{DocKind, OrgKey};

    struct Image {
        dir: tempfile::TempDir,
        root: OrgKey,
        os: OrgKey,
    }

    impl Image {
        fn at(&self, path: &str) -> PathBuf {
            self.dir.path().join(path)
        }

        fn write(&self, path: &str, bytes: &[u8]) {
            std::fs::create_dir_all(self.at(path).parent().unwrap()).unwrap();
            std::fs::write(self.at(path), bytes).unwrap();
        }

        /// The key list, version `version`, naming `os` (revoked or not).
        fn keys(&self, version: u64, revoke_os: bool) -> Signed {
            let list = KeyList {
                keys: vec![ListedKey {
                    purpose: "os".into(),
                    key: self.os.public(),
                }],
                revoked: if revoke_os {
                    vec![self.os.public().id()]
                } else {
                    vec![]
                },
            };
            self.root.sign(
                DocKind::OsKeys,
                GLASSHOUSE,
                version,
                serde_json::to_string(&list).unwrap(),
            )
        }

        /// The release side: the list of the system as it is now, signed,
        /// and the key list beside it.
        fn sign(&self, by: &OrgKey) {
            let manifest = SystemManifest {
                version: "2026.11.1".into(),
                files: glass_platform::integrity::system_files(self.dir.path()).unwrap(),
            };
            let signed = by.sign(
                DocKind::OsManifest,
                GLASSHOUSE,
                1,
                serde_json::to_string(&manifest).unwrap(),
            );
            self.write(MANIFEST_PATH, &serde_json::to_vec(&signed).unwrap());
            self.write(
                KEYS_PATH,
                &serde_json::to_vec(&self.keys(1, false)).unwrap(),
            );
        }

        fn check(&self, required: bool, held: Option<Signed>) -> SystemCheck {
            let roots = [self.root.public()];
            check(&Inputs {
                root: self.dir.path(),
                roots: &roots,
                held_keys: held,
                required,
                patched: BTreeMap::new(),
            })
            .0
        }
    }

    fn image() -> Image {
        let image = Image {
            dir: tempfile::tempdir().unwrap(),
            root: OrgKey::from_seed([1; 32]),
            os: OrgKey::from_seed([2; 32]),
        };
        image.write("usr/bin/yes", b"y\n");
        image.write("boot/kernel/kernel", b"the kernel");
        image.write("etc/rc.conf", b"hostname=x\n");
        image
    }

    /// ADR 0064: verified when the system is the list the OS key signed;
    /// changed when a file differs, goes, or one nobody listed appears;
    /// agents start only on the first.
    #[test]
    fn ep1_the_system_is_checked_at_every_start_against_its_signed_list() {
        let image = image();
        image.sign(&image.os);
        let check = image.check(false, None);
        assert_eq!(check.state, SystemCheckState::Verified, "{check:?}");
        assert_eq!(check.files, 2);
        assert_eq!(check.version.as_deref(), Some("2026.11.1"));
        assert!(may_start(&check).is_ok());

        // Configuration isn't the system.
        image.write("etc/rc.conf", b"hostname=y\n");
        assert_eq!(image.check(false, None).state, SystemCheckState::Verified);

        image.write("usr/bin/yes", b"n\n");
        image.write("usr/local/bin/planted", b"#!/bin/sh\n");
        let check = image.check(false, None);
        assert_eq!(check.state, SystemCheckState::Changed);
        assert_eq!(check.different, 2);
        assert_eq!(
            check.differences,
            [
                "/usr/bin/yes (changed)",
                "/usr/local/bin/planted (not listed)"
            ]
        );
        let refused = may_start(&check).unwrap_err();
        assert!(
            refused.contains("has changed since it was signed"),
            "{refused}"
        );

        std::fs::remove_file(image.at("usr/local/bin/planted")).unwrap();
        image.write("usr/bin/yes", b"y\n");
        std::fs::remove_file(image.at("boot/kernel/kernel")).unwrap();
        assert_eq!(
            image.check(false, None).differences,
            ["/boot/kernel/kernel (missing)"]
        );
    }

    /// Only the OS key the root lists vouches for the list: one signed by
    /// another key, or by an OS key a newer key list revoked, is a changed
    /// system. Without a list, a development build is unchecked, unless
    /// the image says the check must pass.
    #[test]
    fn ep1_only_the_listed_os_key_vouches_for_the_system() {
        let image = image();
        let rogue = OrgKey::from_seed([9; 32]);
        image.sign(&rogue);
        let check = image.check(false, None);
        assert_eq!(check.state, SystemCheckState::Changed);
        assert!(
            check
                .note
                .unwrap()
                .contains("the list of the system's files")
        );

        image.sign(&image.os);
        let revoking = image.keys(2, true);
        let check = image.check(false, Some(revoking));
        assert_eq!(
            check.state,
            SystemCheckState::Changed,
            "a newer list revokes it"
        );
        // An older list held changes nothing: the image's is newer.
        assert_eq!(
            image.check(false, Some(image.keys(0, true))).state,
            SystemCheckState::Verified
        );

        let dev = super::tests::image();
        let check = dev.check(false, None);
        assert_eq!(check.state, SystemCheckState::Unchecked);
        assert!(check.note.unwrap().contains("development build"));
        assert!(may_start(&dev.check(false, None)).is_ok());
        assert_eq!(dev.check(true, None).state, SystemCheckState::Changed);
    }

    /// A kept kernel patch's modules count as the image's: the list and
    /// what the patch wrote over it.
    #[test]
    fn ep1_a_kernel_patchs_modules_are_part_of_the_system() {
        let image = image();
        image.sign(&image.os);
        image.write("boot/kernel/if_iwlwifi.ko", b"patched");
        let roots = [image.root.public()];
        let mut inputs = Inputs {
            root: image.dir.path(),
            roots: &roots,
            held_keys: None,
            required: false,
            patched: BTreeMap::new(),
        };
        assert_eq!(check(&inputs).0.state, SystemCheckState::Changed);
        inputs.patched.insert(
            "boot/kernel/if_iwlwifi.ko".into(),
            FileDigest::file(glass_managed::sha256_hex(b"patched"), 7),
        );
        assert_eq!(check(&inputs).0.state, SystemCheckState::Verified);
    }
}
