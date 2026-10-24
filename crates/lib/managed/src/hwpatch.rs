//! Hardware fixes as kernel patches (ADR 0057): what a bundle carries, and
//! the checks a computer makes before it tries one.
//!
//! A bundle is one JSON file:
//! - a signed manifest ([`DocKind::HardwarePatch`]) naming the patch's hash
//!   and each module's;
//! - the patch's text, so what runs can always be read;
//! - the modules, built off the computer (BS-4).
//!
//! [`Bundle::open`] checks all of it before anything is written anywhere.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{DocKind, PublicKey, Refusal, Signed, base64, sha256_hex};

/// The org name in the envelope of the patches we reviewed and signed.
pub const GLASSHOUSE: &str = "glasshouse";

/// What a patch is, as signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// `lkpi-wlan-suspend`: lowercase letters, digits and `-`.
    pub id: String,
    /// What it fixes, for a person: "Wi-Fi works again after sleep".
    pub summary: String,
    /// The base system release its modules were built for: `15.1-RELEASE`.
    pub release: String,
    /// Where it came from: an upstream commit's address, `glasshouse`, or
    /// `agent:<task id>`.
    pub origin: String,
    pub review: Review,
    /// What it's for: `pci:8086:2725`, `usb:8087:0032`, `model:<maker> <product>`.
    pub targets: Vec<String>,
    pub modules: Vec<Module>,
    pub patch_sha256: String,
    /// The requirements it serves (PRD ids).
    #[serde(default)]
    pub requirements: Vec<String>,
    /// The patches it was built on top of: its modules and kernel carry
    /// their changes too. A patch that rewrites a file a kept patch
    /// rewrote must name it (`specs/tla/HardwarePatchStack.tla`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
}

/// A module the patch rebuilt, or the kernel itself, replacing the base
/// system's of that name in `/boot/kernel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    /// `if_iwlwifi.ko`, or [`KERNEL`].
    pub file: String,
    pub sha256: String,
    /// The loader loads it at every start (`loader.conf`), rather than the
    /// hardware it drives calling for it.
    #[serde(default)]
    pub load_at_boot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    /// We read it, built it and signed it.
    Reviewed,
    /// Someone else's, or an agent's: nobody here has read it. It runs in
    /// the kernel, outside every sandbox (PA-3), so a person says yes
    /// knowing that, and a managed computer takes it only if IT allows.
    Community,
}

/// A patch as it travels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    /// The manifest, as JSON, signed.
    pub signed: Signed,
    /// The patch, as a unified diff.
    pub patch: String,
    /// Each module's bytes, base64, by file name.
    pub modules: BTreeMap<String, String>,
}

/// A bundle every check passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub manifest: Manifest,
    /// The envelope's version: this patch's revision.
    pub revision: u64,
    /// The key that signed it ([`PublicKey::id`]).
    pub signer: String,
    pub patch: String,
    /// Each module with its bytes, in the manifest's order.
    pub modules: Vec<(Module, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BundleError {
    #[error("{0}")]
    Refused(#[from] Refusal),
    #[error("the manifest doesn't parse: {0}")]
    Manifest(String),
    #[error("{0:?} isn't a name a patch or a module may have")]
    BadName(String),
    #[error("the patch doesn't match the hash it was signed with")]
    PatchHash,
    #[error("module {0} is named but not in the bundle")]
    MissingModule(String),
    #[error("module {0} isn't named in the signed manifest")]
    UnexpectedModule(String),
    #[error("module {0} doesn't match the hash it was signed with")]
    ModuleHash(String),
    #[error("module {0} isn't base64")]
    Encoding(String),
    #[error("a patch rebuilds at least one module")]
    NoModules,
}

impl Bundle {
    /// Checks the bundle was signed as a hardware patch from `org` by one of
    /// `keys`, is newer than revision `have` of the same patch, and that the
    /// patch and every module are the ones signed. Nothing in the bundle is
    /// trusted before that.
    pub fn open(
        &self,
        org: &str,
        keys: &[PublicKey],
        have: Option<u64>,
    ) -> Result<Opened, BundleError> {
        let payload = self
            .signed
            .verify(DocKind::HardwarePatch, org, keys, have)?;
        let manifest: Manifest =
            serde_json::from_str(payload).map_err(|e| BundleError::Manifest(e.to_string()))?;
        if let Some(bad) = std::iter::once(&manifest.id)
            .chain(&manifest.includes)
            .find(|id| !valid_id(id))
        {
            return Err(BundleError::BadName(bad.clone()));
        }
        if manifest.modules.is_empty() {
            return Err(BundleError::NoModules);
        }
        if sha256_hex(self.patch.as_bytes()) != manifest.patch_sha256 {
            return Err(BundleError::PatchHash);
        }
        if let Some(extra) = self
            .modules
            .keys()
            .find(|file| !manifest.modules.iter().any(|m| &m.file == *file))
        {
            return Err(BundleError::UnexpectedModule(extra.clone()));
        }
        let mut modules = Vec::with_capacity(manifest.modules.len());
        for module in &manifest.modules {
            if !valid_module_file(&module.file) {
                return Err(BundleError::BadName(module.file.clone()));
            }
            let text = self
                .modules
                .get(&module.file)
                .ok_or_else(|| BundleError::MissingModule(module.file.clone()))?;
            let bytes =
                base64::decode(text).ok_or_else(|| BundleError::Encoding(module.file.clone()))?;
            if sha256_hex(&bytes) != module.sha256 {
                return Err(BundleError::ModuleHash(module.file.clone()));
            }
            modules.push((module.clone(), bytes));
        }
        Ok(Opened {
            revision: self.signed.version,
            signer: self.signed.key.clone(),
            patch: self.patch.clone(),
            modules,
            manifest,
        })
    }
}

/// The file name of the kernel itself, for a patch that changes code the
/// kernel is built with (`/boot/kernel/kernel`).
pub const KERNEL: &str = "kernel";

/// A patch's id: 1 to 48 lowercase letters, digits and `-`, starting with
/// a letter.
pub fn valid_id(id: &str) -> bool {
    (1..=48).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A module's file name, as the base system names them (`if_iwlwifi.ko`),
/// or the kernel's ([`KERNEL`]). Nothing that could name another directory.
pub fn valid_module_file(file: &str) -> bool {
    file == KERNEL
        || file.strip_suffix(".ko").is_some_and(|stem| {
            (1..=64).contains(&stem.len())
                && stem
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OrgKey;

    fn key() -> OrgKey {
        OrgKey::from_seed([7; 32])
    }

    fn bundle(key: &OrgKey, revision: u64) -> Bundle {
        let patch = "--- a/sys/x.c\n+++ b/sys/x.c\n@@ -1 +1 @@\n-a\n+b\n".to_owned();
        let module = b"\x7fELF a module".to_vec();
        let manifest = Manifest {
            id: "lkpi-wlan-suspend".into(),
            summary: "Wi-Fi works again after sleep".into(),
            release: "15.1-RELEASE".into(),
            origin: "https://cgit.freebsd.org/src/commit/?id=0c0f6654".into(),
            review: Review::Reviewed,
            targets: vec!["pci:8086:2725".into()],
            modules: vec![Module {
                file: "if_iwlwifi.ko".into(),
                sha256: sha256_hex(&module),
                load_at_boot: false,
            }],
            patch_sha256: sha256_hex(patch.as_bytes()),
            requirements: vec!["PM-5".into()],
            includes: vec![],
        };
        let payload = serde_json::to_string(&manifest).unwrap();
        Bundle {
            signed: key.sign(DocKind::HardwarePatch, GLASSHOUSE, revision, payload),
            patch,
            modules: BTreeMap::from([("if_iwlwifi.ko".into(), base64::encode(&module))]),
        }
    }

    #[test]
    fn adr0057_a_signed_bundle_opens_with_its_modules() {
        let key = key();
        let opened = bundle(&key, 2)
            .open(GLASSHOUSE, &[key.public()], Some(1))
            .unwrap();
        assert_eq!(opened.manifest.id, "lkpi-wlan-suspend");
        assert_eq!(opened.revision, 2);
        assert_eq!(opened.signer, key.public().id());
        assert_eq!(opened.modules.len(), 1);
        assert_eq!(opened.modules[0].1, b"\x7fELF a module");
    }

    #[test]
    fn adr0057_nothing_unsigned_or_changed_opens() {
        let key = key();
        let keys = [key.public()];
        let good = bundle(&key, 2);

        let mut module = good.clone();
        module
            .modules
            .insert("if_iwlwifi.ko".into(), base64::encode(b"\x7fELF another"));
        assert_eq!(
            module.open(GLASSHOUSE, &keys, None),
            Err(BundleError::ModuleHash("if_iwlwifi.ko".into()))
        );

        let mut patch = good.clone();
        patch.patch.push_str("+more\n");
        assert_eq!(
            patch.open(GLASSHOUSE, &keys, None),
            Err(BundleError::PatchHash)
        );

        let mut extra = good.clone();
        extra
            .modules
            .insert("zfs.ko".into(), base64::encode(b"\x7fELF"));
        assert_eq!(
            extra.open(GLASSHOUSE, &keys, None),
            Err(BundleError::UnexpectedModule("zfs.ko".into()))
        );

        let mut missing = good.clone();
        missing.modules.clear();
        assert_eq!(
            missing.open(GLASSHOUSE, &keys, None),
            Err(BundleError::MissingModule("if_iwlwifi.ko".into()))
        );

        let stranger = OrgKey::from_seed([8; 32]);
        assert!(matches!(
            bundle(&stranger, 2).open(GLASSHOUSE, &keys, None),
            Err(BundleError::Refused(Refusal::UnknownKey(_)))
        ));
        // An older revision of a patch can't replace a newer one.
        assert!(matches!(
            good.open(GLASSHOUSE, &keys, Some(2)),
            Err(BundleError::Refused(Refusal::NotNewer { have: 2, got: 2 }))
        ));
        // Another document signed by the same key isn't a patch.
        let mut policy = good.clone();
        policy.signed = key.sign(DocKind::Policy, GLASSHOUSE, 2, good.signed.payload.clone());
        assert!(matches!(
            policy.open(GLASSHOUSE, &keys, None),
            Err(BundleError::Refused(Refusal::WrongKind { .. }))
        ));
    }

    #[test]
    fn adr0057_module_names_name_one_file_in_one_directory() {
        for good in [
            "if_iwlwifi.ko",
            "linuxkpi_wlan.ko",
            "glass_panic_test.ko",
            KERNEL,
        ] {
            assert!(valid_module_file(good), "{good}");
        }
        for bad in [
            "../kernel",
            "../../etc/rc.ko",
            "a/b.ko",
            ".ko",
            "IWL.ko",
            "if_iwlwifi",
            "x.ko.ko",
            "kernel.old",
        ] {
            assert!(!valid_module_file(bad), "{bad}");
        }
        let key = key();
        let mut named = bundle(&key, 1);
        let mut manifest: Manifest = serde_json::from_str(&named.signed.payload).unwrap();
        manifest.modules[0].file = "../loader.ko".into();
        named.modules =
            BTreeMap::from([("../loader.ko".into(), base64::encode(b"\x7fELF a module"))]);
        named.signed = key.sign(
            DocKind::HardwarePatch,
            GLASSHOUSE,
            1,
            serde_json::to_string(&manifest).unwrap(),
        );
        assert_eq!(
            named.open(GLASSHOUSE, &[key.public()], None),
            Err(BundleError::BadName("../loader.ko".into()))
        );
        assert!(valid_id("lkpi-wlan-suspend"));
        assert!(!valid_id("Lkpi") && !valid_id("-x") && !valid_id("a b") && !valid_id(""));
    }
}
