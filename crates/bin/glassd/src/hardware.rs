//! What the hardware app reaches through glassd (ADR 0057, slice 2): the
//! runtime's kernel patches, and the build service, with its token from a
//! root-only file as Wi-Fi passwords and model secrets are kept.

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::{Arc, Weak};

use glass_first_party::{BuildService, Patches};
use glass_gateway_providers::BuildClient;
use glass_managed::kbuild::{
    BuildRequest, BuildResult, SourceFound, SourceRead, SourceSearch, SourceText,
};
use glass_runtime::Runtime;

/// The runtime's kernel patches.
pub struct RuntimePatches(pub Weak<Runtime>);

impl Patches for RuntimePatches {
    fn release(&self) -> Result<String, String> {
        glass_platform::hardware::kernel_release().map_err(|e| e.to_string())
    }

    fn apply(&self, bundle: &[u8]) -> Result<glass_api::PatchTrial, String> {
        let runtime = self.0.upgrade().ok_or("glassd is stopping")?;
        runtime
            .hardware_patch_propose(bundle)
            .map_err(|e| e.to_string())
    }

    fn takes_community(&self) -> bool {
        self.0
            .upgrade()
            .is_some_and(|runtime| runtime.hardware_takes_community())
    }
}

/// The build service, as `[hardware] build_service` names it.
pub struct Service(BuildClient);

impl BuildService for Service {
    fn search(&self, request: &SourceSearch) -> Result<SourceFound, String> {
        self.0.search(request)
    }
    fn read(&self, request: &SourceRead) -> Result<SourceText, String> {
        self.0.read(request)
    }
    fn build(&self, request: &BuildRequest) -> Result<BuildResult, String> {
        self.0.build(request)
    }
}

/// The build service's client, or why there's none.
pub fn service(config: &crate::config::HardwareConfig) -> Result<Option<Arc<Service>>, String> {
    let Some(url) = &config.build_service else {
        return Ok(None);
    };
    let token = token(&config.build_token)?;
    BuildClient::new(url, &token, config.build_allow_insecure_http)
        .map(|client| Some(Arc::new(Service(client))))
}

/// The token in `path`: glassd's own file, mode 600, or refused.
fn token(path: &Path) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let owner = glass_platform::users::effective_uid();
    if !meta.file_type().is_file() || meta.uid() != owner || meta.mode() & 0o077 != 0 {
        return Err(format!(
            "{} must be a file of uid {owner}'s, mode 600: the build service's token is ignored",
            path.display()
        ));
    }
    std::fs::read_to_string(path)
        .map(|t| t.trim().to_owned())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// This computer's OS updates and kernel patches, as Settings shows them
/// (ADR 0061): the settings app decides who may do what (the owner installs
/// updates; anyone at the screen takes a patch out), and asks the runtime
/// as root, as `glassctl` run by root does.
pub struct RuntimeComputer(pub Weak<Runtime>);

impl RuntimeComputer {
    fn runtime(&self) -> Result<Arc<Runtime>, String> {
        self.0.upgrade().ok_or_else(|| "glassd is stopping".into())
    }
}

/// Root, as glassd itself asks the runtime for what only root may.
pub(crate) fn root() -> glass_runtime::Caller {
    glass_runtime::Caller {
        name: "root".into(),
        uid: 0,
        gid: 0,
        home: "/root".into(),
    }
}

impl glass_first_party::Computer for RuntimeComputer {
    fn updates(&self) -> Result<glass_api::UpdateStatus, String> {
        self.runtime()?
            .update_status(&root())
            .map_err(|e| e.to_string())
    }
    fn check(&self) -> Result<glass_api::UpdateStatus, String> {
        self.runtime()?
            .update_check(&root())
            .map_err(|e| e.to_string())
    }
    fn install(&self) -> Result<glass_api::OsUpdate, String> {
        self.runtime()?
            .update_apply(&root(), glass_api::UpdateApply::default())
            .map_err(|e| e.to_string())
    }
    fn patches(&self) -> Result<glass_api::PatchStatus, String> {
        self.runtime()?
            .hardware_patch_status(&root())
            .map_err(|e| e.to_string())
    }
    fn cancel_patch(&self) -> Result<glass_api::PatchStatus, String> {
        self.runtime()?
            .hardware_patch_cancel(&root())
            .map_err(|e| e.to_string())
    }
    fn undo_patch(&self) -> Result<glass_api::PatchStatus, String> {
        self.runtime()?
            .hardware_patch_undo(&root())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn adr0057_the_build_service_s_token_is_root_s_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kbuild-token");
        std::fs::write(&path, "dev-token\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(token(&path).unwrap_err().contains("mode 600"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(token(&path).unwrap(), "dev-token");
    }
}
