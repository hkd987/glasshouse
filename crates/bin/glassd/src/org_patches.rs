//! Kernel patches IT reviewed (ADR 0057, slice 3), on this computer. The
//! signed catalog names each one's bundle by its digest. The Store offers a
//! patch only to a computer with the hardware it's for, on the release it
//! was built for. On the person's yes (`catalog.patch_install`), glassd
//! fetches the bundle from the console and keeps it only if the digest
//! matches. Then the runtime checks it as it checks any patch: the org's key
//! as a reviewer's, the release, one patch at a time. Nothing is fetched
//! before that yes, and a patch is never pushed.

use std::sync::{Arc, OnceLock, Weak};

use glass_api::{PatchTrial, TrialState};
use glass_app_catalog::OrgPatches;
use glass_managed::hwreview::{OrgPatch, PatchFetch, PatchLink, bundle_digest};
use glass_platform::hardware::Report;
use glass_runtime::Runtime;
use glass_types::AgentId;

/// This computer's side of the org's reviewed patches.
#[derive(Clone, Default)]
pub(crate) struct OrgPatchStore {
    link: Arc<OnceLock<Arc<dyn PatchLink>>>,
    runtime: Arc<OnceLock<Weak<Runtime>>>,
    /// What this computer is, read once: its parts don't change while
    /// glassd runs.
    report: Arc<OnceLock<Option<(Report, String)>>>,
}

impl std::fmt::Debug for OrgPatchStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgPatchStore")
            .field("connected", &self.link.get().is_some())
            .finish_non_exhaustive()
    }
}

impl OrgPatchStore {
    /// Bundles come from the org's console through `link`.
    pub(crate) fn connect(&self, link: Arc<dyn PatchLink>) {
        let _ = self.link.set(link);
    }

    pub(crate) fn set_runtime(&self, runtime: Weak<Runtime>) {
        let _ = self.runtime.set(runtime);
    }

    /// This computer's report and the release it runs, if they can be read.
    fn computer(&self) -> Option<&(Report, String)> {
        self.report
            .get_or_init(|| {
                let report = glass_platform::hardware::report().ok()?;
                let release = glass_platform::hardware::kernel_release().ok()?;
                Some((report, release))
            })
            .as_ref()
    }

    fn runtime(&self) -> Result<Arc<Runtime>, String> {
        self.runtime
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| "glassd is stopping".into())
    }
}

impl OrgPatches for OrgPatchStore {
    fn fits(&self, patch: &OrgPatch) -> bool {
        self.computer().is_some_and(|(report, release)| {
            report.fits(&patch.targets)
                && glass_platform::hardware::release_takes(release, &patch.release)
        })
    }

    fn state(&self, patch: &OrgPatch) -> Option<TrialState> {
        let status = self
            .runtime()
            .ok()?
            .hardware_patch_status(&crate::hardware::root())
            .ok()?;
        let same = |t: &PatchTrial| t.id == patch.patch && t.revision == patch.revision;
        status
            .trial
            .iter()
            .find(|t| same(t))
            .map(|t| t.state)
            .or_else(|| status.kept.iter().any(same).then_some(TrialState::Kept))
    }

    fn install(&self, item: &AgentId, patch: &OrgPatch) -> Result<PatchTrial, String> {
        let link = self
            .link
            .get()
            .ok_or("this computer isn't connected to its organization's console")?;
        let bundle = link.bundle(&PatchFetch {
            id: item.to_string(),
            revision: patch.revision,
        })?;
        // What the signed catalog names, or nothing.
        if bundle_digest(&bundle) != patch.bundle_sha256 {
            return Err("the console's bundle isn't the one the catalog names".into());
        }
        let bytes = serde_json::to_vec(&bundle).map_err(|e| e.to_string())?;
        self.runtime()?
            .hardware_patch_propose(&bytes)
            .map_err(|e| e.to_string())
    }
}
