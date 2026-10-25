//! Kernel patches IT reviews (ADR 0057, slice 3).
//!
//! On a managed computer that doesn't take patches nobody reviewed (IT's
//! `hardware.community_patches`, off by default), an agent's patch goes to
//! IT instead of being tried. The computer sends its source, the build
//! request, never the agent's binary. The console has the build service
//! build it, and IT reads the diff. Approving it signs that build again with
//! the org's key, as reviewed, and the signed catalog names the bundle's
//! digest ([`OrgPatch`]). A computer whose hardware it's for offers it in
//! the Store's Hardware category, and fetches it on the person's yes.
//!
//! A kernel patch is never pushed: the person at the computer says yes,
//! and its trial, probation and undo are the same as any patch's.

use serde::{Deserialize, Serialize};

use crate::hwpatch::Bundle;
use crate::kbuild::BuildRequest;

/// A computer sends IT a patch for review.
pub const SUBMIT_PATH: &str = "/device/v1/hardware/submit";
/// A computer fetches a reviewed patch's bundle.
pub const BUNDLE_PATH: &str = "/device/v1/hardware/bundle";
/// The largest reviewed bundle a computer reads: one that carries a kernel
/// is about 40 MB.
pub const MAX_BUNDLE_REPLY: usize = 160 * 1024 * 1024;
/// The most of what a person said that goes with a patch.
pub const MAX_ASKED: usize = 2000;

/// An agent's patch, sent to IT for review instead of being tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchSubmission {
    /// The patch's source, as the agent had it built: the console has it
    /// built again, never taking the computer's binary.
    pub request: BuildRequest,
    /// The login of the person who asked for the fix.
    pub person: String,
    /// What they said was wrong.
    pub asked: String,
    /// The computer's make and model, as its hardware report says.
    #[serde(default)]
    pub computer: String,
}

impl PatchSubmission {
    pub fn check(&self) -> Result<(), String> {
        self.request.check()?;
        if !self.request.origin.starts_with("agent:") {
            return Err("only an agent's patch is sent for review".into());
        }
        if self.person.is_empty() || self.person.len() > 64 {
            return Err("it names the person who asked".into());
        }
        if self.asked.len() > MAX_ASKED || self.computer.len() > 200 {
            return Err("what was asked, and the computer, are too long".into());
        }
        if [&self.person, &self.computer]
            .iter()
            .any(|s| s.chars().any(char::is_control))
        {
            return Err("the person and the computer are one line each".into());
        }
        Ok(())
    }
}

/// What the console says to a patch sent for review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchSubmitted {
    /// The catalog item it waits as.
    pub id: String,
    pub version: String,
    /// For the person.
    pub text: String,
}

/// A patch IT reviewed, in the org's catalog: what a computer fetches, by
/// the digest the signed catalog names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgPatch {
    /// The patch's own id, which a computer keeps it under.
    pub patch: String,
    /// The release it's built for.
    pub release: String,
    /// What it's for: `pci:8086:2725`. A computer with none of them isn't
    /// offered it.
    pub targets: Vec<String>,
    pub revision: u64,
    /// [`bundle_digest`] of the reviewed bundle.
    pub bundle_sha256: String,
    pub bytes: u64,
    /// It carries a whole kernel.
    #[serde(default)]
    pub kernel: bool,
    /// Who reviewed it, by name.
    pub reviewed_by: String,
}

/// `BUNDLE_PATH`: a reviewed patch, at the revision the catalog names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchFetch {
    /// The catalog item's id.
    pub id: String,
    pub revision: u64,
}

/// The digest a catalog names a bundle by: of its JSON, which is the same
/// for the same bundle wherever it's written.
pub fn bundle_digest(bundle: &Bundle) -> String {
    crate::sha256_hex(&serde_json::to_vec(bundle).unwrap_or_default())
}

/// The console's side of a computer's link, for kernel patches.
pub trait PatchLink: Send + Sync {
    fn submit(&self, submission: &PatchSubmission) -> Result<PatchSubmitted, String>;
    fn bundle(&self, fetch: &PatchFetch) -> Result<Bundle, String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kbuild::BuildModule;

    fn submission() -> PatchSubmission {
        PatchSubmission {
            request: BuildRequest {
                release: "15.1-RELEASE".into(),
                id: "iwlwifi-resume".into(),
                summary: "Wi-Fi works again after sleep".into(),
                diff: "--- a/sys/dev/x.c\n+++ b/sys/dev/x.c\n@@ -1 +1 @@\n-a\n+b\n".into(),
                modules: vec![BuildModule {
                    dir: "iwlwifi".into(),
                    file: "if_iwlwifi.ko".into(),
                    load_at_boot: false,
                }],
                kernel: false,
                targets: vec!["pci:8086:2725".into()],
                origin: "agent:9b1f".into(),
            },
            person: "dana".into(),
            asked: "Wi-Fi stops after sleep".into(),
            computer: "LENOVO 20W0".into(),
        }
    }

    #[test]
    fn adr0057_a_patch_sent_for_review_is_an_agent_s_source_and_one_line_of_who() {
        assert_eq!(submission().check(), Ok(()));
        let bad = |edit: fn(&mut PatchSubmission)| {
            let mut s = submission();
            edit(&mut s);
            s.check().unwrap_err()
        };
        assert!(bad(|s| s.request.origin = "glasshouse".into()).contains("agent's patch"));
        assert!(bad(|s| s.request.diff = "nothing".into()).contains("changes no file"));
        assert!(bad(|s| s.person.clear()).contains("person"));
        assert!(bad(|s| s.computer = "a\nb".into()).contains("one line"));
        assert!(bad(|s| s.asked = "x".repeat(MAX_ASKED + 1)).contains("too long"));
    }
}
