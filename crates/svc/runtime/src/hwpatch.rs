//! Kernel patches tried in a boot environment of their own (ADR 0057), step
//! by step as `specs/tla/HardwarePatch.tla` and `HardwarePatchStack.tla`
//! model them.
//!
//! [`apply`] checks a bundle, then sets it up:
//! 1. the trial's record says "staging" (BeginStage);
//! 2. a boot environment copied from the running one (CreateEnv);
//! 3. the modules written into it (WriteModules);
//! 4. the next start set to it, once (ActivateOnce);
//! 5. the record says "staged" (MarkStaged).
//!
//! [`at_start`], as glassd starts, checks a patch the computer started on
//! and puts it on probation, says it failed, or rolls it back if the
//! computer came back without it (GlassdStart), and removes the
//! environment of a trial that wasn't kept (Remove).
//!
//! On probation the patch's environment isn't the permanent one: a crash, a
//! panic or a power cut starts the one before. [`at_shutdown`], as the
//! computer shuts down cleanly, sets the next start to the patch's, once
//! (Shutdown); [`tick`] counts the minutes of use and [`resumed`] the wakes
//! from sleep and, once the probation is served, makes it permanent
//! (Promote, Confirm). While the computer runs a patch not yet kept, the
//! hardware watchdog is armed (`watchdogd`): a hang restarts it, on the
//! environment before (Hang, WatchdogReset). A patch the computer's
//! keys or community setting no longer allow fails instead
//! (FailOnProbation). [`undo`] takes a patch out (TakeBack, UndoActivate,
//! UndoEnd).
//!
//! The record and the audit log are on glassd's own dataset, outside the
//! boot environments, so the environment the computer starts from reads
//! what the one before it wrote (post-install.sh).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glass_api::{PatchStatus, PatchTrial, TrialState};
use glass_audit::{AuditEvent, EventKind, Outcome};
use glass_managed::PublicKey;
use glass_managed::hwpatch::{Bundle, GLASSHOUSE, Opened, Review};
use glass_types::{FileDigest, Timestamp};
use serde::{Deserialize, Serialize};

/// Why a patch failed when the watchdog restarted the computer on it.
const HUNG: &str = "the computer hung on it, and the hardware watchdog restarted it";

/// The largest bundle this computer reads.
pub const MAX_BUNDLE_BYTES: u64 = 128 * 1024 * 1024;

/// Which patches this computer takes (glassd.toml `[hardware]`).
#[derive(Debug, Clone, Default)]
pub struct HardwarePatches {
    /// Reviewers' keys: a patch one of them signed is what it says it is,
    /// reviewed or not (ours; on a managed computer, IT's too).
    pub keys: Vec<PublicKey>,
    /// Keys that sign only patches nobody reviewed: the build service's,
    /// which signs agents' patches for integrity alone (ADR 0057). A patch
    /// one of them signed that says it was reviewed is refused. With no
    /// key of either kind, the computer takes no patch.
    pub community_keys: Vec<PublicKey>,
    /// Whether it takes patches nobody here reviewed. On a managed computer,
    /// only if IT allows it (ADR 0057).
    pub community: bool,
    /// Minutes of use on a patch before it's kept. With none, a patch that
    /// passes its check at the first start is kept at once.
    pub probation_minutes: u64,
    /// Times the computer wakes from sleep on a patch before it's kept, as
    /// well; a patch may ask for more.
    pub probation_resumes: u64,
    /// Whether this computer sleeps (S3): a computer that doesn't counts
    /// no wakes, and refuses a patch that asks for them.
    pub sleeps: bool,
}

/// The base system's boot environments and modules, as [`apply`] and
/// [`at_start`] use them.
pub trait Envs: Send + Sync {
    /// The boot environment the computer is running from.
    fn running(&self) -> Result<String, String>;
    fn exists(&self, env: &str) -> Result<bool, String>;
    /// Whether the next start is from `env`, once.
    fn starts_once(&self, env: &str) -> Result<bool, String>;
    /// Whether `env` is the one every start uses otherwise.
    fn permanent(&self, env: &str) -> Result<bool, String>;
    fn create(&self, env: &str) -> Result<(), String>;
    /// A new environment received whole from an OS update's image (ADR
    /// 0061).
    fn import(&self, env: &str, image: &Path) -> Result<(), String>;
    /// A new environment received from a delta: the changes since `base`'s
    /// image (ADR 0061).
    fn import_onto(&self, env: &str, image: &Path, base: &str) -> Result<(), String>;
    fn mount(&self, env: &str, at: &Path) -> Result<(), String>;
    fn unmount(&self, env: &str) -> Result<(), String>;
    fn activate_once(&self, env: &str) -> Result<(), String>;
    fn cancel_once(&self) -> Result<(), String>;
    fn activate(&self, env: &str) -> Result<(), String>;
    fn destroy(&self, env: &str) -> Result<(), String>;
    /// The running kernel's release: `15.1-RELEASE`.
    fn kernel_release(&self) -> Result<String, String>;
    /// Where the running environment's files are: `/`.
    fn root(&self) -> PathBuf;
    fn module_loaded(&self, name: &str) -> bool;
    fn load_module(&self, name: &str) -> Result<(), String>;
    /// The running kernel has driver `name` built in, so a module of that
    /// name never loads beside it.
    fn built_in(&self, name: &str) -> bool;
    /// The kernel's last messages about `name` or loading modules: why a
    /// module didn't load. The trial's environment, its logs with it, may
    /// be removed, so a failed check keeps them.
    fn kernel_messages(&self, name: &str) -> String;
    /// Arms the hardware watchdog (`watchdogd`), or disarms it: a hang then
    /// restarts the computer. Nothing on a computer without one.
    fn watchdog(&self, on: bool) -> Result<(), String>;
    /// As the computer shuts down, keeps an armed watchdog armed after
    /// `watchdogd` stops with every other process, which turns it off.
    fn watchdog_through_shutdown(&self) -> Result<(), String>;
    /// This start came from the watchdog's restart: the computer hung.
    fn watchdog_restarted(&self) -> bool;
}

/// The computer's own, through `bectl` and `kld*` (glass_platform).
pub struct System;

impl Envs for System {
    fn running(&self) -> Result<String, String> {
        glass_platform::bootenv::running().map_err(|e| e.to_string())
    }
    fn exists(&self, env: &str) -> Result<bool, String> {
        Ok(listed(env)?.is_some())
    }
    fn starts_once(&self, env: &str) -> Result<bool, String> {
        Ok(listed(env)?.is_some_and(|b| b.next_start_only))
    }
    fn permanent(&self, env: &str) -> Result<bool, String> {
        Ok(listed(env)?.is_some_and(|b| b.on_restart))
    }
    fn create(&self, env: &str) -> Result<(), String> {
        glass_platform::bootenv::create(env).map_err(|e| e.to_string())
    }
    fn import(&self, env: &str, image: &Path) -> Result<(), String> {
        glass_platform::bootenv::import(env, image).map_err(|e| e.to_string())
    }
    fn import_onto(&self, env: &str, image: &Path, base: &str) -> Result<(), String> {
        glass_platform::bootenv::import_onto(
            env,
            image,
            base,
            glass_managed::osupdate::IMAGE_SNAPSHOT,
        )
        .map_err(|e| e.to_string())
    }
    fn mount(&self, env: &str, at: &Path) -> Result<(), String> {
        glass_platform::bootenv::mount(env, at).map_err(|e| e.to_string())
    }
    fn unmount(&self, env: &str) -> Result<(), String> {
        glass_platform::bootenv::unmount(env).map_err(|e| e.to_string())
    }
    fn activate_once(&self, env: &str) -> Result<(), String> {
        glass_platform::bootenv::activate_once(env).map_err(|e| e.to_string())
    }
    fn cancel_once(&self) -> Result<(), String> {
        glass_platform::bootenv::cancel_once().map_err(|e| e.to_string())
    }
    fn activate(&self, env: &str) -> Result<(), String> {
        glass_platform::bootenv::activate(env).map_err(|e| e.to_string())
    }
    fn destroy(&self, env: &str) -> Result<(), String> {
        glass_platform::bootenv::destroy(env).map_err(|e| e.to_string())
    }
    fn kernel_release(&self) -> Result<String, String> {
        glass_platform::hardware::kernel_release().map_err(|e| e.to_string())
    }
    fn root(&self) -> PathBuf {
        PathBuf::from("/")
    }
    fn module_loaded(&self, name: &str) -> bool {
        glass_platform::hardware::module_loaded(name)
    }
    fn watchdog(&self, on: bool) -> Result<(), String> {
        glass_platform::hardware::watchdog(on)
            .map(drop)
            .map_err(|e| e.to_string())
    }
    fn watchdog_through_shutdown(&self) -> Result<(), String> {
        glass_platform::hardware::watchdog_through_shutdown()
            .map(drop)
            .map_err(|e| e.to_string())
    }
    fn watchdog_restarted(&self) -> bool {
        glass_platform::hardware::watchdog_restarted()
    }
    fn load_module(&self, name: &str) -> Result<(), String> {
        glass_platform::hardware::load_module(name).map_err(|e| e.to_string())
    }
    fn built_in(&self, name: &str) -> bool {
        glass_platform::hardware::built_in(name)
    }
    fn kernel_messages(&self, name: &str) -> String {
        glass_platform::hardware::kernel_messages(name, 5).join("; ")
    }
}

fn listed(env: &str) -> Result<Option<glass_platform::bootenv::BootEnv>, String> {
    Ok(glass_platform::bootenv::list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|b| b.name == env))
}

/// The trial's record: what [`at_start`] checks against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub(crate) trial: PatchTrial,
    modules: Vec<ModuleRecord>,
    /// The key that signed it ([`PublicKey::id`]): still trusted when it's
    /// kept?
    #[serde(default)]
    signer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ModuleRecord {
    file: String,
    sha256: String,
    load_at_boot: bool,
}

/// The trial's record and the patches tried before, in `<state>/hardware`.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("hardware"),
        }
    }

    pub(crate) fn trial(&self) -> Option<Record> {
        let text = std::fs::read(self.dir.join("trial.json")).ok()?;
        serde_json::from_slice(&text).ok()
    }

    pub(crate) fn history(&self) -> Vec<PatchTrial> {
        std::fs::read(self.dir.join("history.json"))
            .ok()
            .and_then(|text| serde_json::from_slice(&text).ok())
            .unwrap_or_default()
    }

    /// Tests only: a patch on trial, as if set up just now.
    #[cfg(test)]
    pub(crate) fn begin_for_tests(&self, trial: PatchTrial) -> Result<(), String> {
        self.keep(&Record {
            trial,
            modules: Vec::new(),
            signer: String::new(),
        })
    }

    /// The trial's record, written whole.
    fn keep(&self, record: &Record) -> Result<(), String> {
        let text = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
        write_whole(&self.dir.join("trial.json"), &text, 0o600)
    }

    /// The trial over: into the history first, then its record, so a crash
    /// in between leaves it once in each, never in neither.
    fn end(&self, record: &Record) -> Result<(), String> {
        let mut history = self.history();
        history.retain(|t| !(t.env == record.trial.env && t.staged_at == record.trial.staged_at));
        history.insert(0, record.trial.clone());
        history.truncate(50);
        let text = serde_json::to_vec_pretty(&history).map_err(|e| e.to_string())?;
        write_whole(&self.dir.join("history.json"), &text, 0o600)?;
        self.keep(record)
    }

    /// Patches an OS update carried, rebuilt, into its environment `env`
    /// (ADR 0061): kept there, on top of nothing before.
    pub(crate) fn carry(&self, env: &str, carried: &[PatchTrial]) -> Result<(), String> {
        let mut history = self.history();
        for trial in carried {
            let mut kept = trial.clone();
            kept.env = env.to_owned();
            kept.from_env = env.to_owned();
            kept.state = TrialState::Kept;
            kept.ended_at = Some(Timestamp::now());
            history.insert(0, kept);
        }
        history.truncate(50);
        let text = serde_json::to_vec_pretty(&history).map_err(|e| e.to_string())?;
        write_whole(&self.dir.join("history.json"), &text, 0o600)
    }

    /// The files kernel patches wrote over the image in `env`, by path
    /// from its root, for the start's check of the system (ADR 0064).
    pub(crate) fn patched(&self, env: &str) -> BTreeMap<String, FileDigest> {
        std::fs::read(self.dir.join("files").join(format!("{env}.json")))
            .ok()
            .and_then(|text| serde_json::from_slice(&text).ok())
            .unwrap_or_default()
    }

    pub(crate) fn set_patched(
        &self,
        env: &str,
        files: &BTreeMap<String, FileDigest>,
    ) -> Result<(), String> {
        let text = serde_json::to_vec_pretty(files).map_err(|e| e.to_string())?;
        write_whole(
            &self.dir.join("files").join(format!("{env}.json")),
            &text,
            0o600,
        )
    }

    /// The revision of patch `id` this computer last kept.
    fn kept_revision(&self, id: &str) -> Option<u64> {
        self.history()
            .iter()
            .filter(|t| t.id == id && t.state == TrialState::Kept)
            .map(|t| t.revision)
            .max()
    }
}

/// The patches kept in `env`: its own and those of the environments it was
/// made from, newest first (`InEffect` in HardwarePatchStack.tla). An OS
/// update's environment keeps the rebuilds it carried, and nothing from
/// before it (ADR 0061).
pub(crate) fn in_effect(history: &[PatchTrial], env: &str) -> Vec<PatchTrial> {
    let mut kept: Vec<PatchTrial> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut at = env.to_owned();
    while !seen.contains(&at) {
        let here: Vec<&PatchTrial> = history
            .iter()
            .filter(|t| t.state == TrialState::Kept && t.env == at)
            .collect();
        let Some(first) = here.first() else {
            break;
        };
        seen.push(at.clone());
        for t in &here {
            if !kept.iter().any(|k| k.id == t.id) {
                kept.push((*t).clone());
            }
        }
        at = first.from_env.clone();
    }
    kept
}

/// Writes a sibling file, syncs it, renames it over `path` and syncs the
/// directory: a crash leaves the old file or the new one.
pub(crate) fn write_whole(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    glass_state::write(path, bytes, mode).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Checks a bundle and sets it up in a boot environment of its own, which
/// the next start tries once. On a failure before the start-once is set,
/// that environment is removed and the record says why. A refusal is on the
/// audit record too.
pub fn apply(
    envs: &dyn Envs,
    store: &Store,
    config: &HardwarePatches,
    bytes: &[u8],
    audit: &mut dyn FnMut(AuditEvent),
) -> Result<PatchTrial, String> {
    let (opened, from) = match admit(envs, store, config, bytes) {
        Ok(admitted) => admitted,
        Err((id, why)) => {
            let mut event = AuditEvent::new(
                EventKind::HardwarePatch,
                format!(
                    "Kernel patch {} refused: {why}",
                    if id.is_empty() { "(unnamed)" } else { &id }
                ),
            );
            event.outcome = Some(Outcome::Blocked);
            event.detail = serde_json::json!({ "id": id, "refused": why });
            audit(event);
            return Err(why);
        }
    };
    let manifest = &opened.manifest;
    let env = format!("glass-hw-{}-r{}", manifest.id, opened.revision);
    if envs.exists(&env)? {
        // Left by a trial that wasn't kept.
        envs.destroy(&env)?;
    }

    let mut record = Record {
        trial: PatchTrial {
            id: manifest.id.clone(),
            revision: opened.revision,
            summary: manifest.summary.clone(),
            review: match manifest.review {
                Review::Reviewed => "reviewed",
                Review::Community => "community",
            }
            .into(),
            env: env.clone(),
            from_env: from,
            state: TrialState::Staging,
            modules: manifest.modules.iter().map(|m| m.file.clone()).collect(),
            staged_at: Timestamp::now(),
            ended_at: None,
            note: None,
            ran_minutes: 0,
            probation_minutes: config.probation_minutes,
            woke: 0,
            probation_resumes: if config.sleeps {
                config.probation_resumes.max(u64::from(manifest.resumes))
            } else {
                0
            },
        },
        modules: manifest
            .modules
            .iter()
            .map(|m| ModuleRecord {
                file: m.file.clone(),
                sha256: m.sha256.clone(),
                load_at_boot: m.load_at_boot,
            })
            .collect(),
        signer: opened.signer.clone(),
    };
    // 1. On the record before anything changes.
    store.keep(&record)?;
    audit(event(
        &record.trial,
        format!(
            "Setting up kernel patch {} ({}) in boot environment {}",
            record.trial.id, record.trial.summary, env
        ),
        None,
        Some(&opened.signer),
    ));
    // 2 to 4. The start-once is last: until it's set, nothing starts the new
    // environment.
    // The new environment has what its own was patched with, then this.
    let set_up = envs
        .create(&env)
        .and_then(|()| store.set_patched(&env, &store.patched(&record.trial.from_env)))
        .and_then(|()| write_modules(envs, store, &env, &opened))
        .and_then(|()| envs.activate_once(&env));
    if let Err(why) = set_up {
        let _ = envs.destroy(&env);
        record.trial.state = TrialState::RolledBack;
        record.trial.ended_at = Some(Timestamp::now());
        record.trial.note = Some(format!("setting it up failed: {why}"));
        store.end(&record)?;
        audit(event(
            &record.trial,
            format!("Kernel patch {} couldn't be set up: {why}", record.trial.id),
            Some(Outcome::Failed { error: why.clone() }),
            None,
        ));
        return Err(format!("setting it up failed: {why}"));
    }
    // 5.
    record.trial.state = TrialState::Staged;
    store.keep(&record)?;
    audit(event(
        &record.trial,
        format!(
            "Kernel patch {} ready: the next start tries it, once",
            record.trial.id
        ),
        Some(Outcome::Completed),
        None,
    ));
    Ok(record.trial)
}

/// Everything [`apply`] checks before it changes anything: the bundle, and
/// whether this computer takes it on top of what it keeps. The bundle and
/// the environment it would be made from, or the id it claims and why not.
fn admit(
    envs: &dyn Envs,
    store: &Store,
    config: &HardwarePatches,
    bytes: &[u8],
) -> Result<(Opened, String), (String, String)> {
    if config.keys.is_empty() && config.community_keys.is_empty() {
        return Err((
            String::new(),
            "this computer takes no kernel patches: it trusts no key to sign them".into(),
        ));
    }
    let bundle: Bundle = serde_json::from_slice(bytes)
        .map_err(|e| (String::new(), format!("not a patch bundle: {e}")))?;
    // Which patch it says it is, to find the revision kept, before anything
    // in it is trusted: `open` checks the signature over it.
    let claimed = serde_json::from_str::<serde_json::Value>(&bundle.signed.payload)
        .ok()
        .and_then(|m| m["id"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let refuse = |why: String| (claimed.clone(), why);
    let keys: Vec<PublicKey> = config
        .keys
        .iter()
        .chain(&config.community_keys)
        .cloned()
        .collect();
    let opened = bundle
        .open(GLASSHOUSE, &keys, store.kept_revision(&claimed))
        .map_err(|e| refuse(format!("refused: {e}")))?;
    let manifest = &opened.manifest;
    // Reviewed is a reviewer's word, never the patch's own.
    if manifest.review == Review::Reviewed && !reviewer(config, &opened.signer) {
        return Err(refuse(format!(
            "refused: {} says it was reviewed, but the key that signed it signs only patches \
             nobody reviewed",
            manifest.id
        )));
    }
    if manifest.review == Review::Community && !config.community {
        return Err(refuse(format!(
            "refused: {} is a community patch nobody here reviewed, and this computer takes \
             only reviewed ones",
            manifest.id
        )));
    }
    if manifest.resumes > 0 && !config.sleeps {
        return Err(refuse(format!(
            "refused: {} must come through waking from sleep before it's kept, and this \
             computer doesn't sleep",
            manifest.id
        )));
    }
    fits_here(envs, manifest).map_err(&refuse)?;
    if let Some(open) = store.trial().filter(|r| r.trial.state.open()) {
        return Err(refuse(format!(
            "patch {} is {}: one patch is tried at a time",
            open.trial.id,
            match open.trial.state {
                TrialState::Probation => "on probation",
                _ => "already set up: restart to try it, or take it back first",
            }
        )));
    }
    let from = envs.running().map_err(&refuse)?;
    if from == format!("glass-hw-{}-r{}", manifest.id, opened.revision) {
        return Err(refuse(format!("the computer is already running {from}")));
    }
    // On top of what this computer keeps (HardwarePatchStack.tla): a file a
    // kept patch rewrote is rewritten only by a build that carries that
    // patch, and a build that carries a patch only where it's kept.
    let kept = in_effect(&store.history(), &from);
    for q in kept.iter().filter(|q| q.id != manifest.id) {
        let shared: Vec<&str> = manifest
            .modules
            .iter()
            .map(|m| m.file.as_str())
            .filter(|f| q.modules.iter().any(|k| k == f))
            .collect();
        if !shared.is_empty() && !manifest.includes.contains(&q.id) {
            return Err(refuse(format!(
                "refused: {} replaces {}, as kept patch {} did, without its change: it must \
                 be built on top of {}",
                manifest.id,
                shared.join(", "),
                q.id,
                q.id
            )));
        }
    }
    if let Some(missing) = manifest
        .includes
        .iter()
        .find(|i| !kept.iter().any(|q| &&q.id == i))
    {
        return Err(refuse(format!(
            "refused: {} was built on top of {missing}, which this computer doesn't keep",
            manifest.id
        )));
    }
    Ok((opened, from))
}

/// Whether a patch could run on this computer at all, whoever reviews it:
/// built for the release it runs, and bringing the kernel for a driver
/// the kernel has built in, which a module of its own never replaces.
fn fits_here(envs: &dyn Envs, manifest: &glass_managed::hwpatch::Manifest) -> Result<(), String> {
    if !manifest
        .modules
        .iter()
        .any(|m| m.file == glass_managed::hwpatch::KERNEL)
        && let Some(file) = manifest
            .modules
            .iter()
            .map(|m| m.file.as_str())
            .find(|f| envs.built_in(f.strip_suffix(".ko").unwrap_or(f)))
    {
        return Err(format!(
            "refused: this computer's kernel has {} built in, so the patch's {file} would never \
             run: a patch to it rebuilds the kernel",
            file.strip_suffix(".ko").unwrap_or(file)
        ));
    }
    let release = envs.kernel_release()?;
    if !glass_platform::hardware::release_takes(&release, &manifest.release) {
        return Err(format!(
            "refused: built for {}, and this computer runs {release}",
            manifest.release
        ));
    }
    Ok(())
}

/// [`fits_here`] for a bundle not checked further: one this computer sends
/// for IT to review rather than sets up, so IT never reviews a patch it
/// couldn't run.
pub fn fits(envs: &dyn Envs, bytes: &[u8]) -> Result<(), String> {
    let bundle: Bundle =
        serde_json::from_slice(bytes).map_err(|e| format!("not a patch bundle: {e}"))?;
    let manifest: glass_managed::hwpatch::Manifest =
        serde_json::from_str(&bundle.signed.payload)
            .map_err(|e| format!("not a patch bundle: {e}"))?;
    fits_here(envs, &manifest)
}

/// Whether [`apply`] would take `bytes`, changing nothing: what the
/// person is asked about is a patch this computer can set up.
pub fn would_take(
    envs: &dyn Envs,
    store: &Store,
    config: &HardwarePatches,
    bytes: &[u8],
) -> Result<(), String> {
    admit(envs, store, config, bytes)
        .map(drop)
        .map_err(|(_, why)| why)
}

/// The modules into the new environment's `/boot/kernel`, and those the
/// loader loads at every start into its `loader.conf`.
pub(crate) fn write_modules(
    envs: &dyn Envs,
    store: &Store,
    env: &str,
    opened: &Opened,
) -> Result<(), String> {
    let at = store.dir.join("mnt").join(env);
    std::fs::create_dir_all(&at).map_err(|e| format!("making {}: {e}", at.display()))?;
    envs.mount(env, &at)?;
    let written = (|| {
        let kernel = at.join("boot/kernel");
        let mut patched = store.patched(env);
        for (module, bytes) in &opened.modules {
            let path = kernel.join(&module.file);
            // The image's files are read-only (ADR 0064): not to root
            // either, until the flag is off.
            if path.exists() {
                let _ =
                    glass_platform::integrity::set_immutable(std::slice::from_ref(&path), false);
            }
            write_whole(&path, bytes, 0o555)?;
            patched.insert(
                format!("boot/kernel/{}", module.file),
                FileDigest::file(glass_managed::sha256_hex(bytes), bytes.len() as u64),
            );
        }
        store.set_patched(env, &patched)?;
        let loads: Vec<String> = opened
            .modules
            .iter()
            .filter(|(m, _)| m.load_at_boot)
            .filter_map(|(m, _)| m.file.strip_suffix(".ko"))
            .map(|name| format!("{name}_load=\"YES\""))
            .collect();
        if !loads.is_empty() {
            let conf = at.join("boot/loader.conf");
            let mut text = std::fs::read_to_string(&conf).unwrap_or_default();
            for line in loads {
                if !text.lines().any(|l| l.trim() == line) {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&line);
                    text.push('\n');
                }
            }
            write_whole(&conf, text.as_bytes(), 0o644)?;
        }
        Ok(())
    })();
    let unmounted = envs.unmount(env);
    let _ = std::fs::remove_dir(&at);
    written.and(unmounted)
}

/// As glassd starts: checks a patch the computer started on and puts it on
/// probation (or keeps it, with none), says it failed if it doesn't pass,
/// rolls it back if the computer came back without it or stopped on it
/// while on probation, and removes the environment of a trial that ended
/// without being kept.
pub fn at_start(
    envs: &dyn Envs,
    store: &Store,
    config: &HardwarePatches,
    audit: &mut dyn FnMut(AuditEvent),
) {
    let Some(mut record) = store.trial() else {
        return;
    };
    let Ok(running) = envs.running() else {
        return;
    };
    let env = record.trial.env.clone();
    let from = record.trial.from_env.clone();
    match record.trial.state {
        TrialState::Staging | TrialState::Staged if running == env => {
            // The loader cleared the start-once as it started this one.
            disarm(envs, &env);
            match check(envs, &record).and_then(|()| trusted(config, &record)) {
                Ok(()) if record.trial.served() => keep(envs, store, record, audit),
                Ok(()) => {
                    record.trial.state = TrialState::Probation;
                    if store.keep(&record).is_ok() {
                        audit(event(
                            &record.trial,
                            format!(
                                "Kernel patch {} passed its check and is on probation: kept \
                                 after {}; a crash or a hang before then goes back to {from}",
                                record.trial.id,
                                probation_words(&record.trial)
                            ),
                            Some(Outcome::Completed),
                            None,
                        ));
                    }
                }
                Err(why) => fail(envs, store, record, why, audit),
            }
        }
        TrialState::Probation if running == env => {
            if envs.permanent(&env).unwrap_or(false) {
                // Made permanent, and a crash before the record said so.
                keep(envs, store, record, audit);
                return;
            }
            // Started again by a clean restart, or glassd's own: a crash
            // from here goes back.
            disarm(envs, &env);
            if envs.watchdog_restarted() {
                // It hung, a shutdown say, which had set this start to it.
                fail(envs, store, record, HUNG.into(), audit);
            } else if let Err(why) = trusted(config, &record) {
                fail(envs, store, record, why, audit);
            } else if record.trial.served() {
                keep(envs, store, record, audit);
            }
        }
        TrialState::Staging | TrialState::Staged | TrialState::Probation if running == from => {
            match envs.starts_once(&env) {
                // Still to come: a crash after the start-once was set may
                // have left the record saying "staging".
                Ok(true) => {
                    if record.trial.state == TrialState::Staging {
                        record.trial.state = TrialState::Staged;
                        if store.keep(&record).is_ok() {
                            audit(event(
                                &record.trial,
                                format!(
                                    "Kernel patch {} ready: the next start tries it, once",
                                    record.trial.id
                                ),
                                Some(Outcome::Completed),
                                None,
                            ));
                        }
                    }
                }
                Ok(false) => {
                    let why = match record.trial.state {
                        TrialState::Staged => {
                            "the computer didn't start on it, and came back on the boot \
                             environment before"
                        }
                        TrialState::Probation if envs.watchdog_restarted() => {
                            "the computer hung on it, before its probation ended, and the \
                             hardware watchdog restarted it on the boot environment before"
                        }
                        TrialState::Probation => {
                            "the computer stopped while on it, before its probation ended (a \
                             crash, a panic, a hang or a power cut), and came back on the boot \
                             environment before"
                        }
                        _ => "setting it up didn't finish",
                    };
                    record.trial.state = TrialState::RolledBack;
                    record.trial.ended_at = Some(Timestamp::now());
                    record.trial.note = Some(why.into());
                    if store.end(&record).is_ok() {
                        audit(event(
                            &record.trial,
                            format!("Kernel patch {} rolled back: {why}", record.trial.id),
                            Some(Outcome::Failed { error: why.into() }),
                            None,
                        ));
                    }
                    remove(envs, &env, &running);
                }
                Err(_) => {}
            }
        }
        // Back on the environment before a kept patch, which isn't the
        // permanent one any more: an undo a crash cut short, or someone
        // made the one before permanent by hand.
        TrialState::Kept
            if running == from
                && envs.exists(&env).unwrap_or(false)
                && !envs.permanent(&env).unwrap_or(true) =>
        {
            record.trial.state = TrialState::Undone;
            record.trial.ended_at = Some(Timestamp::now());
            if store.end(&record).is_ok() {
                audit(event(
                    &record.trial,
                    format!(
                        "Kernel patch {} taken out: the computer is back on {from}",
                        record.trial.id
                    ),
                    Some(Outcome::Completed),
                    None,
                ));
            }
            remove(envs, &env, &running);
        }
        TrialState::Failed
        | TrialState::RolledBack
        | TrialState::Cancelled
        | TrialState::Undone => remove(envs, &env, &running),
        // Started from another environment by hand, or kept: nothing to do.
        _ => {}
    }
    // Running a patch not yet kept: the watchdog, until it's kept.
    if let Some(record) = store.trial()
        && record.trial.env == running
        && record.trial.state != TrialState::Kept
        && let Err(why) = envs.watchdog(true)
    {
        tracing::warn!("arming the watchdog: {why}");
    }
}

/// As the computer shuts down cleanly (rc.d, `glassctl system shutdown`):
/// a patch on probation is started again by the next start, once. A crash
/// never gets here, so the next start after one is the environment before.
pub fn at_shutdown(envs: &dyn Envs, store: &Store) -> Result<(), String> {
    // The watchdog stays armed through the shutdown: a patch can hang that
    // too (HardwarePatch.tla's "watchdogOffAtShutdown"). `watchdogd`, which
    // stops with every process, would turn it off.
    if let Err(why) = envs.watchdog_through_shutdown() {
        tracing::warn!("keeping the watchdog through the shutdown: {why}");
    }
    let Some(record) = store.trial() else {
        return Ok(());
    };
    let env = &record.trial.env;
    if record.trial.state == TrialState::Probation
        && envs.running()? == *env
        && !envs.permanent(env)?
    {
        envs.activate_once(env)?;
    }
    Ok(())
}

/// Every few minutes of use: counts them for a patch on probation, and
/// keeps it once its probation is served, if this computer still takes it.
pub fn tick(
    envs: &dyn Envs,
    store: &Store,
    config: &HardwarePatches,
    minutes: u64,
    audit: &mut dyn FnMut(AuditEvent),
) {
    let Some(mut record) = store.trial() else {
        return;
    };
    if record.trial.state != TrialState::Probation
        || envs.running().ok().as_deref() != Some(record.trial.env.as_str())
    {
        return;
    }
    if let Err(why) = trusted(config, &record) {
        fail(envs, store, record, why, audit);
        return;
    }
    record.trial.ran_minutes = record.trial.ran_minutes.saturating_add(minutes);
    if record.trial.served() {
        keep(envs, store, record, audit);
    } else {
        let _ = store.keep(&record);
    }
}

/// The computer woke from sleep (rc.resume, `glassctl system resume`): one
/// more wake for a patch on probation, which keeps it once its probation
/// is served, if this computer still takes it. A patch that breaks waking
/// never gets here: the computer hangs, and the person's power button
/// starts the environment before.
pub fn resumed(
    envs: &dyn Envs,
    store: &Store,
    config: &HardwarePatches,
    audit: &mut dyn FnMut(AuditEvent),
) {
    let Some(mut record) = store.trial() else {
        return;
    };
    if record.trial.state != TrialState::Probation
        || envs.running().ok().as_deref() != Some(record.trial.env.as_str())
    {
        return;
    }
    if let Err(why) = trusted(config, &record) {
        fail(envs, store, record, why, audit);
        return;
    }
    record.trial.woke = record.trial.woke.saturating_add(1);
    if record.trial.served() {
        keep(envs, store, record, audit);
    } else {
        let _ = store.keep(&record);
    }
}

/// What a probation asks, in words: "480 minutes of use and 2 wakes from
/// sleep".
fn probation_words(trial: &PatchTrial) -> String {
    let minutes = format!("{} minutes of use", trial.probation_minutes);
    match trial.probation_resumes {
        0 => minutes,
        1 => format!("{minutes} and a wake from sleep"),
        n => format!("{minutes} and {n} wakes from sleep"),
    }
}

/// Probation served: the patch's environment is the permanent one, then
/// the record says so (Promote, Confirm).
fn keep(envs: &dyn Envs, store: &Store, mut record: Record, audit: &mut dyn FnMut(AuditEvent)) {
    let env = record.trial.env.clone();
    if let Err(why) = envs.activate(&env) {
        // Still on probation; the next tick tries again.
        tracing::warn!("making {env} the boot environment: {why}");
        return;
    }
    disarm(envs, &env);
    record.trial.state = TrialState::Kept;
    record.trial.ended_at = Some(Timestamp::now());
    if store.end(&record).is_ok() {
        audit(event(
            &record.trial,
            format!(
                "Kernel patch {} kept: the computer started on it, it passed its check{}{}; \
                 {env} is the boot environment from now on",
                record.trial.id,
                if record.trial.probation_minutes > 0 {
                    format!(
                        " and ran {} minutes on it without a crash",
                        record.trial.ran_minutes
                    )
                } else {
                    String::new()
                },
                match record.trial.woke {
                    0 => String::new(),
                    1 => ", waking from sleep once".into(),
                    n => format!(", waking from sleep {n} times"),
                }
            ),
            Some(Outcome::Completed),
            None,
        ));
    }
    // Kept: the watchdog goes.
    let _ = envs.watchdog(false);
}

/// The patch failed: the record says why, and the next start is the
/// environment before.
fn fail(
    envs: &dyn Envs,
    store: &Store,
    mut record: Record,
    why: String,
    audit: &mut dyn FnMut(AuditEvent),
) {
    let env = record.trial.env.clone();
    disarm(envs, &env);
    if envs.permanent(&env).unwrap_or(false) {
        let _ = envs.activate(&record.trial.from_env);
    }
    record.trial.state = TrialState::Failed;
    record.trial.ended_at = Some(Timestamp::now());
    record.trial.note = Some(why.clone());
    if store.end(&record).is_ok() {
        audit(event(
            &record.trial,
            format!(
                "Kernel patch {} failed ({why}); the next start goes back to {}",
                record.trial.id, record.trial.from_env
            ),
            Some(Outcome::Failed { error: why }),
            None,
        ));
    }
}

/// The next start isn't `env` once any more.
fn disarm(envs: &dyn Envs, env: &str) {
    if envs.starts_once(env).unwrap_or(false) {
        let _ = envs.cancel_once();
    }
}

/// Removes a trial's environment once nothing runs from it or will start it.
fn remove(envs: &dyn Envs, env: &str, running: &str) {
    if env != running
        && envs.exists(env).unwrap_or(false)
        && !envs.starts_once(env).unwrap_or(true)
        && !envs.permanent(env).unwrap_or(true)
    {
        let _ = envs.destroy(env);
    }
}

/// The check as the patch's environment runs: its kernel and modules are
/// the ones signed (the computer started on that kernel), those loaded at
/// start are loaded, and the rest load.
fn check(envs: &dyn Envs, record: &Record) -> Result<(), String> {
    let kernel = envs.root().join("boot/kernel");
    let with_kernel = record
        .modules
        .iter()
        .any(|m| m.file == glass_managed::hwpatch::KERNEL);
    for module in &record.modules {
        let path = kernel.join(&module.file);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if glass_managed::sha256_hex(&bytes) != module.sha256 {
            return Err(format!("{} isn't the one the patch carries", module.file));
        }
        if module.file == glass_managed::hwpatch::KERNEL {
            continue;
        }
        let name = module.file.strip_suffix(".ko").unwrap_or(&module.file);
        let said = |why: String| match envs.kernel_messages(name) {
            messages if messages.is_empty() => why,
            messages => format!("{why} (the kernel: {messages})"),
        };
        if !module.load_at_boot {
            envs.load_module(name)
                .map_err(|e| said(format!("{name} doesn't load: {e}")))?;
        }
        // Its own file, not a driver of that name the kernel has built in,
        // which `kldload -n` takes for it, unless the kernel is the patch's
        // own: then the change is in it.
        if !envs.module_loaded(name) {
            if with_kernel && envs.built_in(name) {
                continue;
            }
            return Err(said(if envs.built_in(name) {
                format!(
                    "the kernel has {name} built in, so the patch's {} never ran",
                    module.file
                )
            } else {
                format!("{name} wasn't loaded as the computer started")
            }));
        }
    }
    Ok(())
}

/// Whether `signer` is a reviewer's key, whose word on a patch's review
/// this computer takes.
fn reviewer(config: &HardwarePatches, signer: &str) -> bool {
    config.keys.iter().any(|k| k.id() == signer)
}

/// The keys and the community setting in force now still take the patch:
/// IT may have turned either off since it was set up.
fn trusted(config: &HardwarePatches, record: &Record) -> Result<(), String> {
    let community_key = config
        .community_keys
        .iter()
        .any(|k| k.id() == record.signer);
    if !reviewer(config, &record.signer) && !(community_key && record.trial.review == "community") {
        return Err("this computer no longer trusts the key that signed it".into());
    }
    if record.trial.review == "community" && !config.community {
        return Err("this computer no longer takes community patches".into());
    }
    Ok(())
}

/// Takes back a patch set up but not yet tried.
pub fn cancel(
    envs: &dyn Envs,
    store: &Store,
    audit: &mut dyn FnMut(AuditEvent),
) -> Result<PatchTrial, String> {
    let mut record = store
        .trial()
        .filter(|r| r.trial.state == TrialState::Staged)
        .ok_or("no kernel patch is waiting for a restart")?;
    let env = record.trial.env.clone();
    if envs.starts_once(&env)? {
        envs.cancel_once()?;
    }
    envs.destroy(&env)?;
    record.trial.state = TrialState::Cancelled;
    record.trial.ended_at = Some(Timestamp::now());
    store.end(&record)?;
    audit(event(
        &record.trial,
        format!(
            "Kernel patch {} taken back before a restart",
            record.trial.id
        ),
        Some(Outcome::Completed),
        None,
    ));
    Ok(record.trial)
}

/// Takes out the patch the computer runs: one on probation (TakeBack), or
/// the last one kept (UndoActivate, UndoEnd). The environment it was made
/// from is the one the next start uses; that start removes the patch's.
pub fn undo(
    envs: &dyn Envs,
    store: &Store,
    audit: &mut dyn FnMut(AuditEvent),
) -> Result<PatchTrial, String> {
    let running = envs.running()?;
    let (mut record, ended, was) = match store.trial() {
        Some(r) if r.trial.state == TrialState::Probation && r.trial.env == running => {
            (r, TrialState::Cancelled, "on probation")
        }
        _ => {
            let trial = store
                .history()
                .into_iter()
                .find(|t| t.state == TrialState::Kept && t.env == running)
                .ok_or_else(|| {
                    format!(
                        "the computer isn't running a kernel patch: {running} is no patch's boot \
                         environment"
                    )
                })?;
            (
                Record {
                    trial,
                    modules: Vec::new(),
                    signer: String::new(),
                },
                TrialState::Undone,
                "after it was kept",
            )
        }
    };
    let from = record.trial.from_env.clone();
    if !envs.exists(&from)? {
        return Err(format!(
            "{from}, the boot environment it was made from, is gone: nothing to go back to"
        ));
    }
    disarm(envs, &running);
    if !envs.permanent(&from)? {
        envs.activate(&from)?;
    }
    // Taken out, but what runs until the next start: the watchdog again.
    let _ = envs.watchdog(true);
    record.trial.state = ended;
    record.trial.ended_at = Some(Timestamp::now());
    record.trial.note = Some(format!("taken out {was}"));
    store.end(&record)?;
    audit(event(
        &record.trial,
        format!(
            "Kernel patch {} taken out ({was}): the next start is from {from}",
            record.trial.id
        ),
        Some(Outcome::Completed),
        None,
    ));
    Ok(record.trial)
}

pub fn status(envs: &dyn Envs, store: &Store) -> PatchStatus {
    let running = envs.running().unwrap_or_default();
    let history = store.history();
    PatchStatus {
        kept: in_effect(&history, &running),
        running,
        trial: store.trial().map(|r| r.trial),
        history,
    }
}

fn event(
    trial: &PatchTrial,
    summary: String,
    outcome: Option<Outcome>,
    signer: Option<&str>,
) -> AuditEvent {
    let mut event = AuditEvent::new(EventKind::HardwarePatch, summary);
    event.outcome = outcome;
    event.detail = serde_json::json!({
        "id": trial.id,
        "revision": trial.revision,
        "review": trial.review,
        "env": trial.env,
        "from_env": trial.from_env,
        "state": trial.state,
        "modules": trial.modules,
        "signer": signer,
        "ran_minutes": trial.ran_minutes,
    });
    event
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use glass_managed::hwpatch::{Manifest, Module};
    use glass_managed::{DocKind, OrgKey, base64, sha256_hex};

    use super::*;

    use crate::testenvs::Fake;

    fn key() -> OrgKey {
        OrgKey::from_seed([3; 32])
    }

    /// Kept at the first start that passes the check.
    fn config() -> HardwarePatches {
        HardwarePatches {
            keys: vec![key().public()],
            community_keys: Vec::new(),
            community: false,
            probation_minutes: 0,
            probation_resumes: 0,
            sleeps: true,
        }
    }

    /// Kept after an hour of use.
    fn probation() -> HardwarePatches {
        HardwarePatches {
            probation_minutes: 60,
            ..config()
        }
    }

    /// A bundle of one module, signed with [`key`].
    fn bundle(id: &str, file: &str, load_at_boot: bool, review: Review) -> Vec<u8> {
        bundle_on(id, 1, file, load_at_boot, review, &[])
    }

    /// Revision `revision` of patch `id`, built on top of `includes`.
    fn bundle_on(
        id: &str,
        revision: u64,
        file: &str,
        load_at_boot: bool,
        review: Review,
        includes: &[&str],
    ) -> Vec<u8> {
        bundle_edited(id, revision, file, load_at_boot, review, includes, |_| {})
    }

    /// [`bundle_on`], its manifest edited first.
    fn bundle_edited(
        id: &str,
        revision: u64,
        file: &str,
        load_at_boot: bool,
        review: Review,
        includes: &[&str],
        edit: impl FnOnce(&mut Manifest),
    ) -> Vec<u8> {
        let module = format!("\x7fELF {file} patched").into_bytes();
        let patch = "--- a/sys/x.c\n+++ b/sys/x.c\n".to_owned();
        let mut manifest = Manifest {
            id: id.into(),
            summary: "Wi-Fi works again after sleep".into(),
            release: "15.1-RELEASE".into(),
            origin: "https://cgit.freebsd.org/src/commit/?id=0c0f6654".into(),
            review,
            targets: vec!["pci:8086:2725".into()],
            modules: vec![Module {
                file: file.into(),
                sha256: sha256_hex(&module),
                load_at_boot,
            }],
            patch_sha256: sha256_hex(patch.as_bytes()),
            requirements: vec![],
            includes: includes.iter().map(|i| (*i).to_owned()).collect(),
            resumes: 0,
        };
        edit(&mut manifest);
        serde_json::to_vec(&Bundle {
            signed: key().sign(
                DocKind::HardwarePatch,
                GLASSHOUSE,
                revision,
                serde_json::to_string(&manifest).unwrap(),
            ),
            patch,
            modules: BTreeMap::from([(file.into(), base64::encode(&module))]),
        })
        .unwrap()
    }

    fn setup() -> (Fake, Store, Vec<AuditEvent>) {
        let fake = Fake::new();
        let store = Store::new(&fake.dir.path().join("state"));
        (fake, store, Vec::new())
    }

    fn state(store: &Store) -> TrialState {
        store.trial().unwrap().trial.state
    }

    #[test]
    fn adr0057_a_good_patch_is_tried_once_then_kept() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e)).unwrap();
        assert_eq!(trial.state, TrialState::Staged);
        assert_eq!(trial.env, "glass-hw-lkpi-wlan-suspend-r1");
        // The running environment is untouched; the new one has the module.
        assert_eq!(
            std::fs::read(fake.env_dir("default").join("boot/kernel/if_iwlwifi.ko")).unwrap(),
            b"stock"
        );
        assert_eq!(
            std::fs::read(fake.env_dir(&trial.env).join("boot/kernel/if_iwlwifi.ko")).unwrap(),
            b"\x7fELF if_iwlwifi.ko patched"
        );
        assert_eq!(
            fake.s().permanent,
            "default",
            "nothing permanent before it's tried"
        );

        // A glassd restart before the computer's: still waiting.
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Staged);

        fake.restart();
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Kept);
        assert_eq!(fake.s().permanent, trial.env);
        assert_eq!(store.history()[0].state, TrialState::Kept);
        let said: Vec<&str> = audit.iter().map(|e| e.summary.as_str()).collect();
        assert!(
            said[0].starts_with("Setting up kernel patch lkpi-wlan-suspend"),
            "{said:?}"
        );
        assert!(said.last().unwrap().contains("kept"), "{said:?}");
        assert!(audit.iter().all(|e| e.kind == EventKind::HardwarePatch));
    }

    #[test]
    fn adr0057_a_patch_that_panics_costs_one_restart() {
        let (fake, store, mut audit) = setup();
        fake.s().panics.push("glass_panic_test".into());
        let bytes = bundle("panic-test", "glass_panic_test.ko", true, Review::Reviewed);
        apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        // It panicked as it started, and the computer came back on default.
        assert_eq!(fake.s().running, "default");
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        let record = store.trial().unwrap().trial;
        assert_eq!(record.state, TrialState::RolledBack);
        assert!(record.note.unwrap().contains("didn't start on it"));
        assert!(!fake.exists("glass-hw-panic-test-r1").unwrap(), "removed");
        assert!(matches!(
            audit.last().unwrap().outcome,
            Some(Outcome::Failed { .. })
        ));
        // And every later start is default's.
        fake.restart();
        assert_eq!(fake.s().running, "default");
    }

    #[test]
    fn adr0057_a_patch_that_fails_its_check_goes_back_at_the_next_start() {
        let (fake, store, mut audit) = setup();
        fake.s().broken.push("if_iwlwifi".into());
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Failed);
        // Why, from the kernel itself: the environment and its logs go.
        let note = store.trial().unwrap().trial.note.unwrap();
        assert!(
            note.contains("symbol lkpi_80211_pm_driver undefined"),
            "{note}"
        );
        assert!(note.contains("doesn't load"));
        assert_eq!(fake.s().permanent, "default");
        fake.restart();
        assert_eq!(fake.s().running, "default");
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        assert!(!fake.exists(&trial.env).unwrap(), "removed once off it");
    }

    #[test]
    fn adr0057_a_crash_while_setting_up_leaves_nothing_to_start() {
        // The record said "staging" and the environment was made, but the
        // start-once wasn't set: glassd stopped in between.
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.cancel_once().unwrap();
        let mut record = store.trial().unwrap();
        record.trial.state = TrialState::Staging;
        store.keep(&record).unwrap();

        fake.restart();
        assert_eq!(fake.s().running, "default");
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        let trial = store.trial().unwrap().trial;
        assert_eq!(trial.state, TrialState::RolledBack);
        assert_eq!(trial.note.as_deref(), Some("setting it up didn't finish"));
        assert!(!fake.exists(&trial.env).unwrap());

        // The other side: the start-once was set and then glassd stopped
        // before saying "staged". The next glassd says it.
        let (fake, store, mut audit) = setup();
        apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e)).unwrap();
        let mut record = store.trial().unwrap();
        record.trial.state = TrialState::Staging;
        store.keep(&record).unwrap();
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Staged);
    }

    /// A patch to code the kernel is built with carries the kernel: it's
    /// tried the same way, and checked by its hash (it's what runs).
    #[test]
    fn adr0057_a_patched_kernel_is_tried_and_kept_like_a_module() {
        let (fake, store, mut audit) = setup();
        let stock = fake.env_dir("default").join("boot/kernel/kernel");
        std::fs::write(&stock, b"stock kernel").unwrap();
        let bytes = bundle("lkpi-wlan-suspend", "kernel", false, Review::Reviewed);
        let trial = apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e)).unwrap();
        let tried = fake.env_dir(&trial.env).join("boot/kernel/kernel");
        assert_eq!(std::fs::read(&tried).unwrap(), b"\x7fELF kernel patched");
        assert_eq!(std::fs::read(&stock).unwrap(), b"stock kernel");
        fake.restart();
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Kept);
        assert!(
            !fake.s().loaded.iter().any(|m| m == "kernel"),
            "the kernel isn't loaded as a module"
        );
    }

    #[test]
    fn adr0057_taken_back_before_a_restart() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e)).unwrap();
        cancel(&fake, &store, &mut |e| audit.push(e)).unwrap();
        assert_eq!(state(&store), TrialState::Cancelled);
        assert!(!fake.starts_once(&trial.env).unwrap());
        assert!(!fake.exists(&trial.env).unwrap());
        fake.restart();
        assert_eq!(fake.s().running, "default");
        assert!(cancel(&fake, &store, &mut |e| audit.push(e)).is_err());
    }

    #[test]
    fn adr0057_what_this_computer_refuses() {
        let (fake, store, mut audit) = setup();
        let good = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let mut push = |e| audit.push(e);

        // No key trusted: no patch.
        let none = HardwarePatches::default();
        assert!(
            apply(&fake, &store, &none, &good, &mut push)
                .unwrap_err()
                .contains("no kernel patches")
        );

        // A community patch on a computer that takes only reviewed ones.
        let community = bundle("someone-wifi", "if_iwlwifi.ko", false, Review::Community);
        assert!(
            apply(&fake, &store, &config(), &community, &mut push)
                .unwrap_err()
                .contains("community patch")
        );
        let mut allowed = config();
        allowed.community = true;
        assert!(apply(&fake, &store, &allowed, &community, &mut push).is_ok());

        // One trial at a time.
        assert!(
            apply(&fake, &store, &config(), &good, &mut push)
                .unwrap_err()
                .contains("already set up")
        );
        cancel(&fake, &store, &mut push).unwrap();

        // Changed after signing.
        let mut tampered: Bundle = serde_json::from_slice(&good).unwrap();
        tampered
            .modules
            .insert("if_iwlwifi.ko".into(), base64::encode(b"\x7fELF other"));
        let tampered = serde_json::to_vec(&tampered).unwrap();
        assert!(
            apply(&fake, &store, &config(), &tampered, &mut push)
                .unwrap_err()
                .contains("refused")
        );
        assert!(
            apply(&fake, &store, &config(), b"{}", &mut push)
                .unwrap_err()
                .contains("not a patch bundle")
        );
        // Nothing of those reached a boot environment, and each refusal is
        // on the audit record.
        assert_eq!(fake.s().envs, ["default"]);
        let refused = audit
            .iter()
            .filter(|e| e.outcome == Some(Outcome::Blocked))
            .count();
        assert_eq!(refused, 5, "{audit:#?}");
    }

    #[test]
    fn adr0057_a_kept_revision_isnt_replaced_by_itself_or_older() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Kept);
        let again = apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e));
        assert!(again.unwrap_err().contains("not newer"));
    }

    fn kept(fake: &Fake, store: &Store, bytes: &[u8]) -> PatchTrial {
        let mut audit = Vec::new();
        let trial = apply(fake, store, &config(), bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(fake, store, &config(), &mut |e| audit.push(e));
        assert_eq!(state(store), TrialState::Kept);
        trial
    }

    /// HardwarePatch.tla's "noProbation": a patch that panics once the
    /// computer is in use (at the first sleep) was kept at the first start
    /// and then panicked after every one. On probation, the crash goes back.
    #[test]
    fn adr0057_a_crash_on_probation_goes_back() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &probation(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &probation(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Probation);
        assert_eq!(fake.s().permanent, "default", "not permanent on probation");
        assert!(!fake.starts_once(&trial.env).unwrap(), "a crash goes back");
        tick(&fake, &store, &probation(), 10, &mut |e| audit.push(e));
        assert_eq!(store.trial().unwrap().trial.ran_minutes, 10);

        // It panics: no shutdown, straight to the loader.
        fake.restart();
        assert_eq!(fake.s().running, "default");
        at_start(&fake, &store, &probation(), &mut |e| audit.push(e));
        let ended = store.trial().unwrap().trial;
        assert_eq!(ended.state, TrialState::RolledBack);
        assert!(ended.note.unwrap().contains("before its probation ended"));
        assert!(!fake.exists(&trial.env).unwrap(), "removed");
        fake.restart();
        assert_eq!(fake.s().running, "default");
    }

    /// A clean restart on probation comes back to the patch; its minutes
    /// of use add up, and then it's kept.
    #[test]
    fn adr0057_probation_survives_a_clean_restart_then_keeps_it() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &probation(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &probation(), &mut |e| audit.push(e));
        tick(&fake, &store, &probation(), 30, &mut |e| audit.push(e));

        at_shutdown(&fake, &store).unwrap();
        assert!(fake.starts_once(&trial.env).unwrap());
        fake.restart();
        assert_eq!(fake.s().running, trial.env);
        at_start(&fake, &store, &probation(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Probation);
        assert!(!fake.starts_once(&trial.env).unwrap(), "disarmed again");

        tick(&fake, &store, &probation(), 30, &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Kept);
        assert_eq!(fake.s().permanent, trial.env);
        assert!(
            audit
                .last()
                .unwrap()
                .summary
                .contains("ran 60 minutes on it without a crash"),
            "{:?}",
            audit.last()
        );
        // A crash now restarts on the patch: it's kept.
        fake.restart();
        assert_eq!(fake.s().running, trial.env);
    }

    /// Made permanent, then a crash before the record said so: the start
    /// on it finishes keeping it (Promote, then Confirm at GlassdStart).
    #[test]
    fn adr0057_kept_by_the_start_after_a_crash_between_activate_and_record() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &probation(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &probation(), &mut |e| audit.push(e));
        fake.activate(&trial.env).unwrap();
        fake.restart();
        at_start(&fake, &store, &probation(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Kept);
    }

    /// HardwarePatch.tla's "trustOnceAtApply": IT turning off the key, or
    /// community patches, after a patch was set up didn't stop it being
    /// kept. Now it fails, and the next start goes back.
    #[test]
    fn adr0057_a_patch_this_computer_no_longer_takes_is_not_kept() {
        // The key taken back before the restart.
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e)).unwrap();
        let other = HardwarePatches {
            keys: vec![OrgKey::from_seed([4; 32]).public()],
            ..config()
        };
        fake.restart();
        at_start(&fake, &store, &other, &mut |e| audit.push(e));
        let ended = store.trial().unwrap().trial;
        assert_eq!(ended.state, TrialState::Failed);
        assert!(ended.note.unwrap().contains("no longer trusts the key"));
        assert_eq!(fake.s().permanent, "default");
        fake.restart();
        assert_eq!(fake.s().running, "default");
        at_start(&fake, &store, &other, &mut |e| audit.push(e));
        assert!(!fake.exists(&trial.env).unwrap());

        // Community patches turned off on probation.
        let (fake, store, mut audit) = setup();
        let community = bundle("someone-wifi", "if_iwlwifi.ko", false, Review::Community);
        let open = HardwarePatches {
            community: true,
            ..probation()
        };
        apply(&fake, &store, &open, &community, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &open, &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Probation);
        tick(&fake, &store, &probation(), 60, &mut |e| audit.push(e));
        let ended = store.trial().unwrap().trial;
        assert_eq!(ended.state, TrialState::Failed);
        assert!(ended.note.unwrap().contains("no longer takes community"));
        assert_eq!(fake.s().permanent, "default");
        fake.restart();
        assert_eq!(fake.s().running, "default");
    }

    #[test]
    fn adr0057_a_kept_patch_is_taken_out() {
        let (fake, store, mut audit) = setup();
        assert!(
            undo(&fake, &store, &mut |e| audit.push(e))
                .unwrap_err()
                .contains("isn't running a kernel patch")
        );
        let trial = kept(
            &fake,
            &store,
            &bundle(
                "lkpi-wlan-suspend",
                "if_iwlwifi.ko",
                false,
                Review::Reviewed,
            ),
        );
        let out = undo(&fake, &store, &mut |e| audit.push(e)).unwrap();
        assert_eq!(out.state, TrialState::Undone);
        assert_eq!(fake.s().permanent, "default");
        assert!(status(&fake, &store).kept.is_empty());
        fake.restart();
        assert_eq!(fake.s().running, "default");
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        assert!(!fake.exists(&trial.env).unwrap(), "removed once off it");
        assert!(status(&fake, &store).kept.is_empty());
        assert_eq!(store.history()[0].state, TrialState::Undone);
    }

    /// The other half of an undo, cut short by a crash (or someone made
    /// the environment before permanent by hand): the start on it says so.
    #[test]
    fn adr0057_an_undo_cut_short_finishes_at_the_next_start() {
        let (fake, store, mut audit) = setup();
        let trial = kept(
            &fake,
            &store,
            &bundle(
                "lkpi-wlan-suspend",
                "if_iwlwifi.ko",
                false,
                Review::Reviewed,
            ),
        );
        fake.activate("default").unwrap();
        fake.restart();
        at_start(&fake, &store, &config(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Undone);
        assert!(!fake.exists(&trial.env).unwrap());
    }

    #[test]
    fn adr0057_a_patch_on_probation_is_taken_back() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &probation(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &probation(), &mut |e| audit.push(e));
        let out = undo(&fake, &store, &mut |e| audit.push(e)).unwrap();
        assert_eq!(out.state, TrialState::Cancelled);
        // A clean restart doesn't bring it back.
        at_shutdown(&fake, &store).unwrap();
        fake.restart();
        assert_eq!(fake.s().running, "default");
        at_start(&fake, &store, &probation(), &mut |e| audit.push(e));
        assert!(!fake.exists(&trial.env).unwrap());
    }

    /// Kept after an hour of use and a wake from sleep.
    fn waking() -> HardwarePatches {
        HardwarePatches {
            probation_resumes: 1,
            ..probation()
        }
    }

    /// HardwarePatch.tla's "minutesOnly": a patch kept on minutes of use
    /// alone, by a person who didn't close the lid that day, even if it
    /// breaks waking.
    #[test]
    fn adr0057_a_patch_is_kept_only_once_the_computer_has_woken_on_it() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &waking(), &bytes, &mut |e| audit.push(e)).unwrap();
        assert_eq!(trial.probation_resumes, 1);
        fake.restart();
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        assert!(
            audit
                .last()
                .unwrap()
                .summary
                .contains("kept after 60 minutes of use and a wake from sleep"),
            "{:?}",
            audit.last()
        );
        tick(&fake, &store, &waking(), 60, &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Probation, "no wake yet");
        assert_eq!(fake.s().permanent, "default");

        resumed(&fake, &store, &waking(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Kept);
        assert_eq!(fake.s().permanent, trial.env);
        assert!(
            audit
                .last()
                .unwrap()
                .summary
                .contains("ran 60 minutes on it without a crash, waking from sleep once"),
            "{:?}",
            audit.last()
        );
    }

    /// A patch that hangs the computer as it wakes: the person holds the
    /// power button, and the next start is the environment before.
    #[test]
    fn adr0057_a_patch_that_breaks_waking_goes_back() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &waking(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        tick(&fake, &store, &waking(), 90, &mut |e| audit.push(e));
        // Asleep, then hung waking: no resume, no shutdown, off.
        fake.restart();
        assert_eq!(fake.s().running, "default");
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::RolledBack);
        assert!(!fake.exists(&trial.env).unwrap());
    }

    /// A patch that asks for wakes, on a computer that doesn't sleep, could
    /// never be kept.
    #[test]
    fn adr0057_a_patch_for_waking_is_refused_where_the_computer_doesnt_sleep() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle_edited(
            "lkpi-wlan-suspend",
            1,
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
            &[],
            |m| m.resumes = 2,
        );
        let awake = HardwarePatches {
            sleeps: false,
            ..waking()
        };
        let refused = apply(&fake, &store, &awake, &bytes, &mut |e| audit.push(e)).unwrap_err();
        assert!(refused.contains("this computer doesn't sleep"), "{refused}");
        // Where it sleeps, it asks for its own two wakes, more than the
        // computer's one; a computer that doesn't sleep counts none.
        let trial = apply(&fake, &store, &waking(), &bytes, &mut |e| audit.push(e)).unwrap();
        assert_eq!(trial.probation_resumes, 2);
        cancel(&fake, &store, &mut |e| audit.push(e)).unwrap();
        let plain = bundle("other", "if_iwlwifi.ko", false, Review::Reviewed);
        let trial = apply(&fake, &store, &awake, &plain, &mut |e| audit.push(e)).unwrap();
        assert_eq!(trial.probation_resumes, 0);
    }

    /// The dev VM (2026-10-25): an agent's snd_hda.ko, for a kernel with
    /// snd_hda built in, would have passed the start check (`kldload -n`
    /// says it's loaded) and been kept without ever running. It's refused
    /// before it's set up, and the start check fails one that got there.
    #[test]
    fn adr0057_a_module_the_kernel_has_built_in_is_refused() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        fake.s().built_in.push("if_iwlwifi".into());
        let why = would_take(&fake, &store, &waking(), &bytes).unwrap_err();
        assert!(
            why.contains("built in") && why.contains("rebuilds the kernel"),
            "{why}"
        ); // And never sent to IT to review.
        assert!(fits(&fake, &bytes).unwrap_err().contains("built in"));

        assert!(apply(&fake, &store, &waking(), &bytes, &mut |e| audit.push(e)).is_err());
        assert_eq!(fake.s().envs.len(), 1, "nothing set up");

        // Set up while the kernel didn't have it, started on one that does.
        fake.s().built_in.clear();
        would_take(&fake, &store, &waking(), &bytes).unwrap();
        apply(&fake, &store, &waking(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.s().built_in.push("if_iwlwifi".into());
        fake.restart();
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        let ended = store.trial().unwrap().trial;
        assert_eq!(ended.state, TrialState::Failed);
        assert!(ended.note.unwrap().contains("never ran"));
        fake.restart();

        // Built into the kernel the patch brings, the driver is the
        // patch's: that's how it must be built.
        let bytes = bundle_edited(
            "lkpi-wlan-suspend",
            2,
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
            &[],
            |m| {
                m.modules.push(Module {
                    file: glass_managed::hwpatch::KERNEL.into(),
                    sha256: sha256_hex(b"\x7fELF kernel patched"),
                    load_at_boot: false,
                })
            },
        );
        let mut bundle: Bundle = serde_json::from_slice(&bytes).unwrap();
        bundle.modules.insert(
            glass_managed::hwpatch::KERNEL.into(),
            base64::encode(b"\x7fELF kernel patched"),
        );
        let bytes = serde_json::to_vec(&bundle).unwrap();
        would_take(&fake, &store, &waking(), &bytes).unwrap();
        apply(&fake, &store, &waking(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Probation);
    }

    /// HardwarePatch.tla's "noWatchdog": a patch that hangs the computer in
    /// use leaves it hung until someone holds the power button. The
    /// watchdog is armed from the start on a patch until it's kept, again
    /// while a kept one is taken out, and never through a shutdown.
    #[test]
    fn adr0057_the_watchdog_is_armed_while_a_patch_is_not_yet_kept() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        apply(&fake, &store, &waking(), &bytes, &mut |e| audit.push(e)).unwrap();
        assert!(!fake.s().watchdog, "not on the environment before");
        fake.restart();
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        assert!(fake.s().watchdog, "on probation");
        at_shutdown(&fake, &store).unwrap();
        fake.processes_stopped();
        assert!(fake.s().watchdog, "armed through the shutdown");
        fake.restart();
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        assert!(fake.s().watchdog, "armed again");
        tick(&fake, &store, &waking(), 60, &mut |e| audit.push(e));
        resumed(&fake, &store, &waking(), &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Kept);
        assert!(!fake.s().watchdog, "kept");

        undo(&fake, &store, &mut |e| audit.push(e)).unwrap();
        assert!(fake.s().watchdog, "taken out, still running");
    }

    /// The watchdog restarts a computer the patch hung in use: the next
    /// start is the environment before, and the record says why.
    #[test]
    fn adr0057_a_patch_that_hangs_goes_back_by_itself() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        apply(&fake, &store, &waking(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        // Hung, then the watchdog's restart.
        fake.restart();
        fake.s().restarted = true;
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        let ended = store.trial().unwrap().trial;
        assert_eq!(ended.state, TrialState::RolledBack);
        assert!(ended.note.unwrap().contains("hardware watchdog"));
    }

    /// HardwarePatch.tla's "resetUnheard": a patch that hangs every
    /// shutdown has its next start already set to it; the watchdog's restart
    /// is what tells.
    #[test]
    fn adr0057_a_patch_that_hangs_the_shutdown_fails() {
        let (fake, store, mut audit) = setup();
        let bytes = bundle(
            "lkpi-wlan-suspend",
            "if_iwlwifi.ko",
            false,
            Review::Reviewed,
        );
        let trial = apply(&fake, &store, &waking(), &bytes, &mut |e| audit.push(e)).unwrap();
        fake.restart();
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        at_shutdown(&fake, &store).unwrap();
        fake.processes_stopped();
        // The shutdown hangs; the watchdog, still armed, restarts it, on
        // the patch.
        assert!(fake.s().watchdog, "armed as the shutdown hangs");
        fake.restart();
        fake.s().restarted = true;
        assert_eq!(fake.s().running, trial.env);
        at_start(&fake, &store, &waking(), &mut |e| audit.push(e));
        let ended = store.trial().unwrap().trial;
        assert_eq!(ended.state, TrialState::Failed);
        assert!(ended.note.unwrap().contains("hardware watchdog"));
        fake.restart();
        assert_eq!(fake.s().running, "default");
    }

    /// HardwarePatchStack.tla's "stackBlind": a second patch that rebuilt
    /// the kernel replaced the first one's along with its fix, while the
    /// record said both were kept.
    #[test]
    fn adr0057_a_second_patch_must_be_built_on_the_first_it_overlaps() {
        let (fake, store, mut audit) = setup();
        let a = kept(
            &fake,
            &store,
            &bundle_on(
                "lkpi-wlan-suspend",
                1,
                "kernel",
                false,
                Review::Reviewed,
                &[],
            ),
        );
        let mut push = |e| audit.push(e);

        let blind = bundle_on("usb-fix", 1, "kernel", false, Review::Reviewed, &[]);
        let why = apply(&fake, &store, &config(), &blind, &mut push).unwrap_err();
        assert!(
            why.contains("must be built on top of lkpi-wlan-suspend"),
            "{why}"
        );

        let elsewhere = bundle_on(
            "usb-fix",
            1,
            "kernel",
            false,
            Review::Reviewed,
            &["lkpi-wlan-suspend", "sd-fix"],
        );
        let why = apply(&fake, &store, &config(), &elsewhere, &mut push).unwrap_err();
        assert!(why.contains("doesn't keep"), "{why}");

        // Built on top of it: taken, kept, and both are kept here.
        let on_top = bundle_on(
            "usb-fix",
            1,
            "kernel",
            false,
            Review::Reviewed,
            &["lkpi-wlan-suspend"],
        );
        let b = apply(&fake, &store, &config(), &on_top, &mut push).unwrap();
        assert_eq!(b.from_env, a.env);
        fake.restart();
        at_start(&fake, &store, &config(), &mut push);
        let kept_here: Vec<String> = status(&fake, &store)
            .kept
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(kept_here, ["usb-fix", "lkpi-wlan-suspend"]);

        // A newer lkpi-wlan-suspend now would replace usb-fix's kernel.
        let newer = bundle_on(
            "lkpi-wlan-suspend",
            2,
            "kernel",
            false,
            Review::Reviewed,
            &[],
        );
        let why = apply(&fake, &store, &config(), &newer, &mut push).unwrap_err();
        assert!(why.contains("as kept patch usb-fix did"), "{why}");

        // Patches to different modules stack freely.
        let module = bundle("sd-fix", "sdhci.ko", false, Review::Reviewed);
        assert!(apply(&fake, &store, &config(), &module, &mut push).is_ok());
    }

    /// Signs `bytes`' manifest again, with `by`.
    fn signed_by(bytes: &[u8], by: &OrgKey) -> Vec<u8> {
        let mut bundle: Bundle = serde_json::from_slice(bytes).unwrap();
        bundle.signed = by.sign(
            DocKind::HardwarePatch,
            GLASSHOUSE,
            bundle.signed.version,
            bundle.signed.payload.clone(),
        );
        serde_json::to_vec(&bundle).unwrap()
    }

    /// The build service signs agents' patches for integrity alone: its key
    /// never makes a patch reviewed, whatever the patch says (ADR 0057).
    #[test]
    fn adr0057_a_community_key_signs_only_patches_nobody_reviewed() {
        let service = OrgKey::from_seed([9; 32]);
        let rules = HardwarePatches {
            community_keys: vec![service.public()],
            community: true,
            ..config()
        };
        let mut push = |_| {};

        // It says it was reviewed: refused, here and with community off.
        let (fake, store, _) = setup();
        let claims = signed_by(
            &bundle("agent-wifi", "if_iwlwifi.ko", false, Review::Reviewed),
            &service,
        );
        let refused = apply(&fake, &store, &rules, &claims, &mut push).unwrap_err();
        assert!(
            refused.contains("signs only patches nobody reviewed"),
            "{refused}"
        );
        let closed = HardwarePatches {
            community: false,
            ..rules.clone()
        };
        assert!(apply(&fake, &store, &closed, &claims, &mut push).is_err());

        // It says nobody did: taken where community patches are, and
        // still trusted at the next start (kept at once: no probation).
        let honest = signed_by(
            &bundle("agent-wifi", "if_iwlwifi.ko", false, Review::Community),
            &service,
        );
        let refused = apply(&fake, &store, &closed, &honest, &mut push).unwrap_err();
        assert!(refused.contains("community patch"), "{refused}");
        let trial = apply(&fake, &store, &rules, &honest, &mut push).unwrap();
        assert_eq!(trial.review, "community");
        fake.restart();
        at_start(&fake, &store, &rules, &mut push);
        assert_eq!(state(&store), TrialState::Kept);

        // Only a community key alone: a computer that trusts nothing else.
        let only = HardwarePatches {
            keys: Vec::new(),
            ..rules.clone()
        };
        let (fake, store, _) = setup();
        assert!(apply(&fake, &store, &only, &honest, &mut push).is_ok());
        let ours = bundle("lkpi-wlan-suspend", "kernel", false, Review::Reviewed);
        let (fake2, store2, _) = setup();
        assert!(apply(&fake2, &store2, &only, &ours, &mut push).is_err());
        drop((fake, store));
    }
}
