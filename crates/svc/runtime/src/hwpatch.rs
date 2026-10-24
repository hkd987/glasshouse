//! Kernel patches tried in a boot environment of their own (ADR 0057), step
//! by step as `specs/tla/HardwarePatch.tla` models them.
//!
//! [`apply`] checks a bundle, then sets it up:
//! 1. the trial's record says "staging" (BeginStage);
//! 2. a boot environment copied from the running one (CreateEnv);
//! 3. the modules written into it (WriteModules);
//! 4. the next start set to it, once (ActivateOnce);
//! 5. the record says "staged" (MarkStaged).
//!
//! [`at_start`], as glassd starts, keeps it, says it failed, or rolls it
//! back (GlassdStart), and removes the environment of a trial that wasn't
//! kept (Remove). The record and the audit log are on glassd's own
//! dataset, outside the boot environments, so the environment the computer
//! starts from reads what the one before it wrote (post-install.sh).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use glass_api::{PatchStatus, PatchTrial, TrialState};
use glass_audit::{AuditEvent, EventKind, Outcome};
use glass_managed::PublicKey;
use glass_managed::hwpatch::{Bundle, GLASSHOUSE, Opened, Review};
use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

/// The largest bundle this computer reads.
pub const MAX_BUNDLE_BYTES: u64 = 128 * 1024 * 1024;

/// Which patches this computer takes (glassd.toml `[hardware]`).
#[derive(Debug, Clone, Default)]
pub struct HardwarePatches {
    /// Keys one of which must have signed a patch. With none, it takes no
    /// patch.
    pub keys: Vec<PublicKey>,
    /// Whether it takes patches nobody here reviewed. On a managed computer,
    /// only if IT allows it (ADR 0057).
    pub community: bool,
}

/// The base system's boot environments and modules, as [`apply`] and
/// [`at_start`] use them.
pub trait Envs: Send + Sync {
    /// The boot environment the computer is running from.
    fn running(&self) -> Result<String, String>;
    fn exists(&self, env: &str) -> Result<bool, String>;
    /// Whether the next start is from `env`, once.
    fn starts_once(&self, env: &str) -> Result<bool, String>;
    fn create(&self, env: &str) -> Result<(), String>;
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
    /// The kernel's last messages about `name` or loading modules: why a
    /// module didn't load. The trial's environment, its logs with it, may
    /// be removed, so a failed check keeps them.
    fn kernel_messages(&self, name: &str) -> String;
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
    fn create(&self, env: &str) -> Result<(), String> {
        glass_platform::bootenv::create(env).map_err(|e| e.to_string())
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
    fn load_module(&self, name: &str) -> Result<(), String> {
        glass_platform::hardware::load_module(name).map_err(|e| e.to_string())
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
struct Record {
    trial: PatchTrial,
    modules: Vec<ModuleRecord>,
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

    fn trial(&self) -> Option<Record> {
        let text = std::fs::read(self.dir.join("trial.json")).ok()?;
        serde_json::from_slice(&text).ok()
    }

    fn history(&self) -> Vec<PatchTrial> {
        std::fs::read(self.dir.join("history.json"))
            .ok()
            .and_then(|text| serde_json::from_slice(&text).ok())
            .unwrap_or_default()
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

    /// The revision of patch `id` this computer last kept.
    fn kept_revision(&self, id: &str) -> Option<u64> {
        self.history()
            .iter()
            .filter(|t| t.id == id && t.state == TrialState::Kept)
            .map(|t| t.revision)
            .max()
    }
}

/// Writes a sibling file, syncs it, renames it over `path` and syncs the
/// directory: a crash leaves the old file or the new one.
fn write_whole(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let new = dir.join(format!(".{name}.new"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&new)
        .map_err(|e| format!("writing {}: {e}", new.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("writing {}: {e}", new.display()))?;
    std::fs::rename(&new, path).map_err(|e| format!("replacing {}: {e}", path.display()))?;
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("syncing {}: {e}", dir.display()))
}

/// Checks a bundle and sets it up in a boot environment of its own, which
/// the next start tries once. On a failure before the start-once is set,
/// that environment is removed and the record says why.
pub fn apply(
    envs: &dyn Envs,
    store: &Store,
    config: &HardwarePatches,
    bytes: &[u8],
    audit: &mut dyn FnMut(AuditEvent),
) -> Result<PatchTrial, String> {
    if config.keys.is_empty() {
        return Err("this computer takes no kernel patches: it trusts no key to sign them".into());
    }
    let bundle: Bundle =
        serde_json::from_slice(bytes).map_err(|e| format!("not a patch bundle: {e}"))?;
    // Which patch it says it is, to find the revision kept, before anything
    // in it is trusted: `open` checks the signature over it.
    let claimed = serde_json::from_str::<serde_json::Value>(&bundle.signed.payload)
        .ok()
        .and_then(|m| m["id"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let opened = bundle
        .open(GLASSHOUSE, &config.keys, store.kept_revision(&claimed))
        .map_err(|e| format!("refused: {e}"))?;
    let manifest = &opened.manifest;
    if manifest.review == Review::Community && !config.community {
        return Err(format!(
            "refused: {} is a community patch nobody here reviewed, and this computer takes \
             only reviewed ones",
            manifest.id
        ));
    }
    let release = envs.kernel_release()?;
    if !glass_platform::hardware::release_takes(&release, &manifest.release) {
        return Err(format!(
            "refused: built for {}, and this computer runs {release}",
            manifest.release
        ));
    }
    if let Some(open) = store.trial().filter(|r| r.trial.state.open()) {
        return Err(format!(
            "patch {} is already set up: restart to try it, or take it back first",
            open.trial.id
        ));
    }
    let from = envs.running()?;
    let env = format!("glass-hw-{}-r{}", manifest.id, opened.revision);
    if env == from {
        return Err(format!("the computer is already running {env}"));
    }
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
    let set_up = envs
        .create(&env)
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

/// The modules into the new environment's `/boot/kernel`, and those the
/// loader loads at every start into its `loader.conf`.
fn write_modules(envs: &dyn Envs, store: &Store, env: &str, opened: &Opened) -> Result<(), String> {
    let at = store.dir.join("mnt").join(env);
    std::fs::create_dir_all(&at).map_err(|e| format!("making {}: {e}", at.display()))?;
    envs.mount(env, &at)?;
    let written = (|| {
        let kernel = at.join("boot/kernel");
        for (module, bytes) in &opened.modules {
            write_whole(&kernel.join(&module.file), bytes, 0o555)?;
        }
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

/// As glassd starts: keeps a patch the computer started on and that passes
/// the check, says it failed if it doesn't, rolls it back if the computer
/// came back without it, and removes the environment of a trial that
/// wasn't kept.
pub fn at_start(envs: &dyn Envs, store: &Store, audit: &mut dyn FnMut(AuditEvent)) {
    let Some(mut record) = store.trial() else {
        return;
    };
    let Ok(running) = envs.running() else {
        return;
    };
    let env = record.trial.env.clone();
    match record.trial.state {
        TrialState::Staging | TrialState::Staged if running == env => {
            let checked = check(envs, &record).and_then(|()| envs.activate(&env));
            record.trial.ended_at = Some(Timestamp::now());
            match checked {
                Ok(()) => {
                    record.trial.state = TrialState::Kept;
                    if store.end(&record).is_ok() {
                        audit(event(
                            &record.trial,
                            format!(
                                "Kernel patch {} kept: the computer started on it and it \
                                 passed its check; {env} is the boot environment from now on",
                                record.trial.id
                            ),
                            Some(Outcome::Completed),
                            None,
                        ));
                    }
                }
                Err(why) => {
                    record.trial.state = TrialState::Failed;
                    record.trial.note = Some(why.clone());
                    if store.end(&record).is_ok() {
                        audit(event(
                            &record.trial,
                            format!(
                                "Kernel patch {} failed its check ({why}); the next start \
                                 goes back to {}",
                                record.trial.id, record.trial.from_env
                            ),
                            Some(Outcome::Failed { error: why }),
                            None,
                        ));
                    }
                }
            }
        }
        TrialState::Staging | TrialState::Staged if running == record.trial.from_env => {
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
                    let why = if record.trial.state == TrialState::Staged {
                        "the computer didn't start on it, and came back on the boot \
                         environment before"
                    } else {
                        "setting it up didn't finish"
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
        TrialState::Failed | TrialState::RolledBack | TrialState::Cancelled => {
            remove(envs, &env, &running)
        }
        // Started from another environment by hand, or kept: nothing to do.
        _ => {}
    }
}

/// Removes a trial's environment once nothing runs from it or will start it.
fn remove(envs: &dyn Envs, env: &str, running: &str) {
    if env != running && envs.exists(env).unwrap_or(false) && !envs.starts_once(env).unwrap_or(true)
    {
        let _ = envs.destroy(env);
    }
}

/// The check as the patch's environment runs: its kernel and modules are
/// the ones signed (the computer started on that kernel), those loaded at
/// start are loaded, and the rest load.
fn check(envs: &dyn Envs, record: &Record) -> Result<(), String> {
    let kernel = envs.root().join("boot/kernel");
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
        if module.load_at_boot {
            if !envs.module_loaded(name) {
                return Err(said(format!(
                    "{name} wasn't loaded as the computer started"
                )));
            }
        } else {
            envs.load_module(name)
                .map_err(|e| said(format!("{name} doesn't load: {e}")))?;
        }
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

pub fn status(envs: &dyn Envs, store: &Store) -> PatchStatus {
    PatchStatus {
        running: envs.running().unwrap_or_default(),
        trial: store.trial().map(|r| r.trial),
        history: store.history(),
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
    });
    event
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use glass_managed::hwpatch::{Manifest, Module};
    use glass_managed::{DocKind, OrgKey, base64, sha256_hex};

    use super::*;

    /// Boot environments in a temporary directory, as the loader and `bectl`
    /// treat them: a start takes the start-once environment, clearing it,
    /// and a module that panics as it loads sends the computer back round.
    struct Fake {
        dir: tempfile::TempDir,
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        envs: Vec<String>,
        running: String,
        permanent: String,
        once: Option<String>,
        /// Modules that panic as they load.
        panics: Vec<String>,
        /// Modules that don't link against this kernel.
        broken: Vec<String>,
        loaded: Vec<String>,
        mounts: BTreeMap<String, PathBuf>,
    }

    impl Fake {
        fn new() -> Self {
            let fake = Self {
                dir: tempfile::tempdir().unwrap(),
                state: Mutex::new(FakeState {
                    envs: vec!["default".into()],
                    running: "default".into(),
                    permanent: "default".into(),
                    ..FakeState::default()
                }),
            };
            std::fs::create_dir_all(fake.env_dir("default").join("boot/kernel")).unwrap();
            std::fs::write(
                fake.env_dir("default").join("boot/kernel/if_iwlwifi.ko"),
                b"stock",
            )
            .unwrap();
            fake
        }

        fn env_dir(&self, env: &str) -> PathBuf {
            self.dir.path().join("envs").join(env)
        }

        fn s(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.state.lock().unwrap()
        }

        /// A restart: the loader's choice, then the modules loaded at start;
        /// one that panics restarts the computer at once.
        fn restart(&self) {
            loop {
                let mut s = self.s();
                let env = s.once.take().unwrap_or_else(|| s.permanent.clone());
                s.running = env.clone();
                s.loaded.clear();
                drop(s);
                let conf = std::fs::read_to_string(self.env_dir(&env).join("boot/loader.conf"))
                    .unwrap_or_default();
                let at_start: Vec<String> = conf
                    .lines()
                    .filter_map(|l| l.strip_suffix("_load=\"YES\""))
                    .map(str::to_owned)
                    .collect();
                let mut s = self.s();
                if at_start.iter().any(|m| s.panics.contains(m)) {
                    continue;
                }
                s.loaded = at_start;
                return;
            }
        }
    }

    impl Envs for Fake {
        fn running(&self) -> Result<String, String> {
            Ok(self.s().running.clone())
        }
        fn exists(&self, env: &str) -> Result<bool, String> {
            Ok(self.s().envs.iter().any(|e| e == env))
        }
        fn starts_once(&self, env: &str) -> Result<bool, String> {
            Ok(self.s().once.as_deref() == Some(env))
        }
        fn create(&self, env: &str) -> Result<(), String> {
            let from = self.env_dir(&self.s().running);
            let to = self.env_dir(env);
            std::fs::create_dir_all(to.join("boot/kernel")).unwrap();
            for entry in std::fs::read_dir(from.join("boot/kernel")).unwrap() {
                let entry = entry.unwrap();
                std::fs::copy(entry.path(), to.join("boot/kernel").join(entry.file_name()))
                    .unwrap();
            }
            if let Ok(conf) = std::fs::read(from.join("boot/loader.conf")) {
                std::fs::write(to.join("boot/loader.conf"), conf).unwrap();
            }
            self.s().envs.push(env.into());
            Ok(())
        }
        fn mount(&self, env: &str, at: &Path) -> Result<(), String> {
            std::fs::remove_dir(at).map_err(|e| e.to_string())?;
            std::os::unix::fs::symlink(self.env_dir(env), at).map_err(|e| e.to_string())?;
            self.s().mounts.insert(env.into(), at.to_owned());
            Ok(())
        }
        fn unmount(&self, env: &str) -> Result<(), String> {
            let at = self.s().mounts.remove(env).ok_or("not mounted")?;
            std::fs::remove_file(&at).map_err(|e| e.to_string())?;
            std::fs::create_dir(&at).map_err(|e| e.to_string())
        }
        fn activate_once(&self, env: &str) -> Result<(), String> {
            self.s().once = Some(env.into());
            Ok(())
        }
        fn cancel_once(&self) -> Result<(), String> {
            self.s().once = None;
            Ok(())
        }
        fn activate(&self, env: &str) -> Result<(), String> {
            self.s().permanent = env.into();
            Ok(())
        }
        fn destroy(&self, env: &str) -> Result<(), String> {
            let mut s = self.s();
            if s.running == env || s.permanent == env {
                return Err(format!("{env} is in use"));
            }
            s.envs.retain(|e| e != env);
            drop(s);
            let _ = std::fs::remove_dir_all(self.env_dir(env));
            Ok(())
        }
        fn kernel_release(&self) -> Result<String, String> {
            Ok("15.1-RELEASE-p1".into())
        }
        fn root(&self) -> PathBuf {
            self.env_dir(&self.s().running)
        }
        fn module_loaded(&self, name: &str) -> bool {
            self.s().loaded.iter().any(|m| m == name)
        }
        fn load_module(&self, name: &str) -> Result<(), String> {
            let mut s = self.s();
            if s.broken.iter().any(|m| m == name) {
                return Err("kldload: an error occurred while loading module".into());
            }
            s.loaded.push(name.into());
            Ok(())
        }
        fn kernel_messages(&self, name: &str) -> String {
            if self.s().broken.iter().any(|m| m == name) {
                "link_elf_obj: symbol lkpi_80211_pm_driver undefined".into()
            } else {
                String::new()
            }
        }
    }

    fn key() -> OrgKey {
        OrgKey::from_seed([3; 32])
    }

    fn config() -> HardwarePatches {
        HardwarePatches {
            keys: vec![key().public()],
            community: false,
        }
    }

    /// A bundle of one module, signed with [`key`].
    fn bundle(id: &str, file: &str, load_at_boot: bool, review: Review) -> Vec<u8> {
        let module = format!("\x7fELF {file} patched").into_bytes();
        let patch = "--- a/sys/x.c\n+++ b/sys/x.c\n".to_owned();
        let manifest = Manifest {
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
        };
        serde_json::to_vec(&Bundle {
            signed: key().sign(
                DocKind::HardwarePatch,
                GLASSHOUSE,
                1,
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
        at_start(&fake, &store, &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Staged);

        fake.restart();
        at_start(&fake, &store, &mut |e| audit.push(e));
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
        at_start(&fake, &store, &mut |e| audit.push(e));
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
        at_start(&fake, &store, &mut |e| audit.push(e));
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
        at_start(&fake, &store, &mut |e| audit.push(e));
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
        at_start(&fake, &store, &mut |e| audit.push(e));
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
        at_start(&fake, &store, &mut |e| audit.push(e));
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
        at_start(&fake, &store, &mut |e| audit.push(e));
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
        // Nothing of those reached a boot environment.
        assert_eq!(fake.s().envs, ["default"]);
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
        at_start(&fake, &store, &mut |e| audit.push(e));
        assert_eq!(state(&store), TrialState::Kept);
        let again = apply(&fake, &store, &config(), &bytes, &mut |e| audit.push(e));
        assert!(again.unwrap_err().contains("not newer"));
    }
}
