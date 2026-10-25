//! Kernel patches IT reviews (ADR 0057, slice 3).
//!
//! A managed computer that doesn't take patches nobody reviewed sends an
//! agent's patch here as source. The console has the build service build it
//! again, one at a time (as the service builds), and keeps the build beside
//! the catalog. IT reads the diff, the build's log, and who asked for it on
//! which computer. Approving it signs that build again with the org's key,
//! as reviewed: what the agent's computer built is never what other
//! computers run.

use std::sync::Arc;

use glass_managed::hwpatch::{Bundle, GLASSHOUSE, KERNEL, Manifest, Module, Review};
use glass_managed::kbuild::{BuildRequest, BuildResult};
use glass_managed::{DocKind, OrgKey, PublicKey, base64, sha256_hex};

/// What builds the patches IT reviews.
pub trait PatchBuilder: Send + Sync {
    /// Builds `request`, its bundle signed with [`Self::key`].
    fn build(&self, request: &BuildRequest) -> Result<BuildResult, String>;
    /// The key the builder signs with: a build is checked with it.
    fn key(&self) -> PublicKey;
}

/// The build service (`glass-kbuild`), with the console's own token, and
/// the key it signs with.
pub struct Service {
    client: glass_gateway_providers::BuildClient,
    key: PublicKey,
}

impl Service {
    pub fn new(client: glass_gateway_providers::BuildClient, key: PublicKey) -> Arc<Self> {
        Arc::new(Self { client, key })
    }
}

impl PatchBuilder for Service {
    fn build(&self, request: &BuildRequest) -> Result<BuildResult, String> {
        self.client.build(request)
    }
    fn key(&self) -> PublicKey {
        self.key
    }
}

/// For `--dev` and the console's tests: "builds" each module as a few
/// bytes naming it, and fails a diff that says `does not build`. Its key is
/// fixed, and no computer trusts it.
pub struct DevBuilder(OrgKey);

impl DevBuilder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self(OrgKey::from_seed([0x6b; 32])))
    }
}

impl PatchBuilder for DevBuilder {
    fn build(&self, request: &BuildRequest) -> Result<BuildResult, String> {
        request.check()?;
        if request.diff.contains("does not build") {
            return Ok(BuildResult {
                ok: false,
                log: "sys/dev/x.c:1:1: error: expected expression".into(),
                bundle: None,
            });
        }
        let mut files: Vec<(String, bool)> = request
            .modules
            .iter()
            .map(|m| (m.file.clone(), m.load_at_boot))
            .collect();
        if request.kernel {
            files.push((KERNEL.into(), false));
        }
        let built: Vec<(Module, Vec<u8>)> = files
            .into_iter()
            .map(|(file, load_at_boot)| {
                let bytes = format!("\x7fELF development build of {file}").into_bytes();
                let module = Module {
                    file,
                    sha256: sha256_hex(&bytes),
                    load_at_boot,
                };
                (module, bytes)
            })
            .collect();
        let manifest = Manifest {
            id: request.id.clone(),
            summary: request.summary.clone(),
            release: request.release.clone(),
            origin: request.origin.clone(),
            review: Review::Community,
            targets: request.targets.clone(),
            modules: built.iter().map(|(m, _)| m.clone()).collect(),
            patch_sha256: sha256_hex(request.diff.as_bytes()),
            requirements: Vec::new(),
            includes: Vec::new(),
            resumes: 0,
        };
        let payload = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
        Ok(BuildResult {
            ok: true,
            log: format!("built {} in development", request.id),
            bundle: Some(Bundle {
                signed: self.0.sign(DocKind::HardwarePatch, GLASSHOUSE, 1, payload),
                patch: request.diff.clone(),
                modules: built
                    .into_iter()
                    .map(|(m, bytes)| (m.file, base64::encode(&bytes)))
                    .collect(),
            }),
        })
    }
    fn key(&self) -> PublicKey {
        self.0.public()
    }
}

/// Whether `built` is what `request` asked for, signed by `key`, as a
/// patch nobody reviewed: what IT reads is what was built.
pub fn check_build(request: &BuildRequest, built: &Bundle, key: &PublicKey) -> Result<(), String> {
    let opened = built
        .open(GLASSHOUSE, std::slice::from_ref(key), None)
        .map_err(|e| format!("the build service's bundle doesn't check: {e}"))?;
    let m = &opened.manifest;
    if m.id != request.id
        || m.release != request.release
        || m.targets != request.targets
        || m.review != Review::Community
        || opened.patch != request.diff
    {
        return Err("the build service's bundle isn't the patch it was asked to build".into());
    }
    Ok(())
}

/// `built`, signed again by `key` as reviewed, at `revision`.
pub fn reviewed(built: &Bundle, key: &OrgKey, revision: u64) -> Result<Bundle, String> {
    let mut manifest: Manifest =
        serde_json::from_str(&built.signed.payload).map_err(|e| e.to_string())?;
    manifest.review = Review::Reviewed;
    let payload = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
    Ok(Bundle {
        signed: key.sign(DocKind::HardwarePatch, GLASSHOUSE, revision, payload),
        patch: built.patch.clone(),
        modules: built.modules.clone(),
    })
}
