//! Signed OS updates installed as a boot environment of their own (ADR
//! 0061; EP-1, EP-11, EP-12, BS-4), step by step as `specs/tla/OsUpdate.tla`
//! models them.
//!
//! [`apply`] takes the newest release on the computer's channel from its
//! update source:
//! 1. the root's key list, kept if it's newer than the one the computer
//!    has, then the release: the channel's key and the OS key the list
//!    names, unrevoked, and a number above the last one taken; a kept
//!    kernel patch's fix must survive it (Check);
//! 2. the record says "staging" (BeginStage);
//! 3. the image, copied in and checked against the OS key's digest,
//!    received into a new boot environment (Import);
//! 4. the kept kernel patches' rebuilds written into it (CarryPatches);
//! 5. the release's number kept as the last one taken (Take);
//! 6. the next start set to it, once (ActivateOnce);
//! 7. the record says "staged" (MarkStaged).
//!
//! [`at_start`], as glassd starts on the new environment, checks the image
//! is the release's and makes it the permanent one, then the record says
//! kept; back on the one before, the update rolled back. [`undo`] goes back
//! to the version before; [`cancel`] takes back an update not yet started.
//!
//! The record lives on glassd's own dataset, outside the environments, as
//! the kernel patches' does (ADR 0057).

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use glass_api::{OsUpdate, PatchTrial, TrialState, UpdateAvailable, UpdateStatus};
use glass_audit::{AuditEvent, EventKind, Outcome};
use glass_managed::hwpatch::{Bundle, GLASSHOUSE, Opened};
use glass_managed::osupdate::{self, KeyList, Verified};
use glass_managed::{PublicKey, Signed};
use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

use crate::hwpatch::{self, Envs, HardwarePatches};

/// The largest image this computer takes.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// What an image says it is.
pub const OS_RELEASE: &str = "usr/local/share/glasshouse/os-release.json";

/// Which updates this computer takes (glassd.toml `[update]`).
#[derive(Debug, Clone, Default)]
pub struct OsUpdates {
    /// The root keys the image trusts. With none, it takes no update.
    pub roots: Vec<PublicKey>,
    /// `pilot`, `early` or `broad`.
    pub channel: String,
    /// A folder holding `keys.json` and `<channel>/release.json` with its
    /// image beside it: a share, a USB drive, or where the updates the
    /// console brings are put.
    pub source: Option<PathBuf>,
}

/// What an image says it is (`/usr/local/share/glasshouse/os-release.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRelease {
    pub version: String,
    #[serde(default)]
    pub base: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    update: OsUpdate,
    /// The kept patches' rebuilds written into its environment, kept there
    /// once the update is.
    #[serde(default)]
    carried: Vec<PatchTrial>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Taken {
    number: u64,
}

/// The update's record, the updates before, the key list and the last
/// release number taken, in `<state>/update`.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("update"),
        }
    }

    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        let text = std::fs::read(self.dir.join(name)).ok()?;
        serde_json::from_slice(&text).ok()
    }

    fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<(), String> {
        let text = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
        hwpatch::write_whole(&self.dir.join(name), &text, 0o600)
    }

    fn record(&self) -> Option<Record> {
        self.read("record.json")
    }

    fn history(&self) -> Vec<OsUpdate> {
        self.read("history.json").unwrap_or_default()
    }

    fn keep(&self, record: &Record) -> Result<(), String> {
        self.write("record.json", record)
    }

    /// The update over: into the history first, then its record.
    fn end(&self, record: &Record) -> Result<(), String> {
        let mut history = self.history();
        history.retain(|u| !(u.env == record.update.env && u.staged_at == record.update.staged_at));
        history.insert(0, record.update.clone());
        history.truncate(50);
        self.write("history.json", &history)?;
        self.keep(record)
    }

    fn taken(&self) -> Option<u64> {
        self.read::<Taken>("taken.json").map(|t| t.number)
    }

    /// The key list in force: the newest one a root signed.
    fn keys(&self) -> Option<Signed> {
        self.read("keys.json")
    }
}

fn event(update: &OsUpdate, summary: String, outcome: Option<Outcome>) -> AuditEvent {
    let mut event = AuditEvent::new(EventKind::OsUpdate, summary);
    event.outcome = outcome;
    event.detail = serde_json::json!({
        "number": update.number,
        "version": update.version,
        "channel": update.channel,
        "base": update.base,
        "env": update.env,
        "from_env": update.from_env,
        "state": update.state,
        "patches": update.patches,
    });
    event
}

/// The newest release on the channel, verified against the key list the
/// computer holds, or a newer one in the source, which it then keeps.
fn newest(store: &Store, config: &OsUpdates) -> Result<(Verified, PathBuf), String> {
    if config.roots.is_empty() {
        return Err("this computer takes no OS updates: it trusts no root key".into());
    }
    let source = config
        .source
        .as_ref()
        .ok_or("this computer has no update source")?;
    let read = |path: PathBuf| -> Result<Signed, String> {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    };
    // The key list: a newer one replaces the one in force, revoking keys
    // whatever happens to the release.
    let have = store.keys();
    let offered = read(source.join("keys.json")).ok();
    let mut keys: Option<KeyList> = have
        .as_ref()
        .and_then(|signed| osupdate::open_keys(signed, &config.roots, None).ok());
    if let Some(offered) = offered
        && have.as_ref().is_none_or(|h| offered.version > h.version)
    {
        let newer = osupdate::open_keys(&offered, &config.roots, have.map(|h| h.version))?;
        store.write("keys.json", &offered)?;
        keys = Some(newer);
    }
    let keys = keys.ok_or("no key list from the root yet")?;
    let channel = source.join(&config.channel);
    let release = read(channel.join("release.json"))?;
    let verified = osupdate::verify_release(&keys, &release, &config.channel, store.taken())?;
    Ok((verified, channel))
}

/// A kept kernel patch's rebuild for the new image: its record there, and
/// the bundle to write in.
type Rebuild = (PatchTrial, Opened);

/// What the update does with each kernel patch the computer keeps, or why
/// it can't be taken: the patch's fix would be gone after it (Check,
/// `CarriesFix`).
fn plan_patches(
    hw: &hwpatch::Store,
    patches: &HardwarePatches,
    verified: &Verified,
    channel: &Path,
    running: &str,
    drop_patches: bool,
) -> Result<(Vec<String>, Vec<Rebuild>), String> {
    let mut notes = Vec::new();
    let mut rebuilds = Vec::new();
    for kept in hwpatch::in_effect(&hw.history(), running) {
        let release = &verified.release;
        if release.fixes.contains(&kept.id) {
            notes.push(format!(
                "{}: fixed in {}, so the patch goes",
                kept.id, release.version
            ));
        } else if let Some(rebuild) = release.patches.iter().find(|p| p.id == kept.id) {
            let path = channel.join(&rebuild.bundle);
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let bundle: Bundle = serde_json::from_slice(&bytes)
                .map_err(|e| format!("{}: not a patch bundle: {e}", rebuild.bundle))?;
            let opened = bundle
                .open(GLASSHOUSE, &patches.keys, None)
                .map_err(|e| format!("{}'s rebuild: {e}", kept.id))?;
            if opened.manifest.id != kept.id {
                return Err(format!(
                    "{}'s rebuild is patch {}",
                    kept.id, opened.manifest.id
                ));
            }
            if !glass_platform::hardware::release_takes(&release.base, &opened.manifest.release) {
                return Err(format!(
                    "{}'s rebuild is for {}, and {} is built on {}",
                    kept.id, opened.manifest.release, release.version, release.base
                ));
            }
            notes.push(format!(
                "{}: rebuilt for {} (revision {})",
                kept.id, release.base, opened.revision
            ));
            let mut trial = kept.clone();
            trial.revision = opened.revision;
            trial.modules = opened
                .manifest
                .modules
                .iter()
                .map(|m| m.file.clone())
                .collect();
            rebuilds.push((trial, opened));
        } else if drop_patches {
            notes.push(format!(
                "{}: dropped; what it fixed ({}) is broken again after the update",
                kept.id, kept.summary
            ));
        } else {
            return Err(format!(
                "kernel patch {} keeps this working: {}. {} neither fixes that nor brings a \
                 rebuild of it: wait for one, or update without it",
                kept.id, kept.summary, release.version
            ));
        }
    }
    Ok((notes, rebuilds))
}

/// The version of an update being installed, if one is: no kernel patch
/// is set up meanwhile (ADR 0061).
pub fn open_update(store: &Store) -> Option<String> {
    store
        .record()
        .filter(|r| r.update.state.open())
        .map(|r| r.update.version)
}

/// Whether anything blocks an update now: one under way, or a kernel
/// patch being tried.
fn busy(store: &Store, hw: &hwpatch::Store) -> Result<(), String> {
    if let Some(r) = store.record().filter(|r| r.update.state.open()) {
        return Err(format!(
            "update {} is already {}",
            r.update.version,
            match r.update.state {
                TrialState::Staging => "being installed",
                _ => "installed: restart to use it, or take it back",
            }
        ));
    }
    if let Some(t) = hw.trial().filter(|t| t.trial.state.open()) {
        return Err(format!(
            "kernel patch {} is being tried: it's kept or taken back first",
            t.trial.id
        ));
    }
    Ok(())
}

/// The newest release, checked, and what taking it would do: nothing
/// changes.
pub fn check(
    envs: &dyn Envs,
    store: &Store,
    hw: &hwpatch::Store,
    config: &OsUpdates,
    patches: &HardwarePatches,
) -> UpdateStatus {
    let mut out = status(envs, store, config);
    match newest(store, config) {
        Ok((verified, channel)) => {
            let running = envs.running().unwrap_or_default();
            let (notes, blocked) =
                match plan_patches(hw, patches, &verified, &channel, &running, false) {
                    Ok((notes, _)) => (notes, busy(store, hw).err()),
                    Err(why) => (Vec::new(), Some(why)),
                };
            out.available = Some(UpdateAvailable {
                number: verified.number,
                version: verified.release.version,
                base: verified.release.base,
                notes: verified.release.notes,
                patches: notes,
                blocked,
            });
        }
        Err(why) => out.checked = Some(why),
    }
    out
}

/// Installs the newest release in a boot environment of its own, which the
/// next start uses once. A refusal changes nothing and is on the audit
/// record; a failure after the record says "staging" removes what was made.
pub fn apply(
    envs: &dyn Envs,
    store: &Store,
    hw: &hwpatch::Store,
    config: &OsUpdates,
    patches: &HardwarePatches,
    drop_patches: bool,
    audit: &mut dyn FnMut(AuditEvent),
) -> Result<OsUpdate, String> {
    let refuse = |why: String, audit: &mut dyn FnMut(AuditEvent)| {
        let mut event = AuditEvent::new(EventKind::OsUpdate, format!("OS update refused: {why}"));
        event.outcome = Some(Outcome::Blocked);
        audit(event);
        why
    };
    // 1. Check.
    let checked = busy(store, hw).and_then(|()| {
        let (verified, channel) = newest(store, config)?;
        let running = envs.running()?;
        let env = osupdate::env_name(&verified.release.version)
            .ok_or_else(|| format!("{:?} isn't a version", verified.release.version))?;
        if env == running {
            return Err(format!(
                "the computer already runs {}",
                verified.release.version
            ));
        }
        // A delta goes on top of the image the computer runs.
        if let Some(from) = &verified.image.delta_from {
            let runs = running_version(envs);
            if runs.as_deref() != Some(from.as_str()) {
                return Err(format!(
                    "{} is the changes since {from}, and this computer runs {}",
                    verified.release.version,
                    runs.as_deref().unwrap_or("a version it doesn't know")
                ));
            }
        }
        let (notes, rebuilds) =
            plan_patches(hw, patches, &verified, &channel, &running, drop_patches)?;
        Ok((verified, channel, running, env, notes, rebuilds))
    });
    let (verified, channel, running, env, notes, rebuilds) = match checked {
        Ok(checked) => checked,
        Err(why) => return Err(refuse(why, audit)),
    };
    // 2. BeginStage.
    let mut record = Record {
        update: OsUpdate {
            number: verified.number,
            version: verified.release.version.clone(),
            channel: verified.release.channel.clone(),
            base: verified.release.base.clone(),
            env: env.clone(),
            from_env: running,
            state: TrialState::Staging,
            staged_at: Timestamp::now(),
            ended_at: None,
            note: None,
            patches: notes,
        },
        carried: rebuilds.iter().map(|(t, _)| t.clone()).collect(),
    };
    store.keep(&record)?;
    audit(event(
        &record.update,
        format!(
            "Installing OS update {} (release {}) in boot environment {env}",
            record.update.version, record.update.number
        ),
        None,
    ));
    // 3 to 6. The start-once is last: until it's set, nothing starts the
    // new environment.
    let staged = (|| {
        if envs.exists(&env)? {
            // Left by an update that didn't stay.
            envs.destroy(&env)?;
        }
        let copy = copy_in(store, &channel, &verified)?;
        let imported = match &verified.image.delta_from {
            Some(_) => envs.import_onto(&env, &copy, &record.update.from_env),
            None => envs.import(&env, &copy),
        };
        let _ = std::fs::remove_file(&copy);
        imported?;
        for (_, opened) in &rebuilds {
            hwpatch::write_modules(envs, hw, &env, opened)?;
        }
        store.write(
            "taken.json",
            &Taken {
                number: verified.number,
            },
        )?;
        envs.activate_once(&env)
    })();
    if let Err(why) = staged {
        if envs.exists(&env).unwrap_or(false) {
            let _ = envs.destroy(&env);
        }
        record.update.state = TrialState::RolledBack;
        record.update.ended_at = Some(Timestamp::now());
        record.update.note = Some(format!("installing it failed: {why}"));
        store.end(&record)?;
        audit(event(
            &record.update,
            format!(
                "OS update {} couldn't be installed: {why}",
                record.update.version
            ),
            Some(Outcome::Failed { error: why.clone() }),
        ));
        return Err(format!("installing it failed: {why}"));
    }
    // 7. MarkStaged.
    record.update.state = TrialState::Staged;
    store.keep(&record)?;
    audit(event(
        &record.update,
        format!(
            "OS update {} ready: the next start uses it, once",
            record.update.version
        ),
        Some(Outcome::Completed),
    ));
    Ok(record.update)
}

/// The image copied beside glassd's state, hashed as it goes, and checked
/// against the OS key's digest: what's imported is what was checked, even
/// if the source changes meanwhile.
fn copy_in(store: &Store, channel: &Path, verified: &Verified) -> Result<PathBuf, String> {
    use sha2::Digest as _;
    let image = &verified.image;
    if image.bytes > MAX_IMAGE_BYTES {
        return Err(format!(
            "the image is {} bytes, more than this computer takes",
            image.bytes
        ));
    }
    let from = channel.join(&image.file);
    let dir = store.dir.join("incoming");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let to = dir.join(&image.file);
    let mut input = std::fs::File::open(&from).map_err(|e| format!("{}: {e}", from.display()))?;
    let mut output = std::fs::File::create(&to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total: u64 = 0;
    let copied = (|| {
        loop {
            let n = input.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > image.bytes {
                return Err("the image is longer than its signed size".to_owned());
            }
            hasher.update(&buf[..n]);
            output.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        }
        output.sync_all().map_err(|e| e.to_string())?;
        if total != image.bytes {
            return Err(format!(
                "the image is {total} bytes, and its signed size is {}",
                image.bytes
            ));
        }
        if hex::encode(hasher.finalize()) != image.sha256 {
            return Err("the image doesn't match the digest the OS key signed".into());
        }
        Ok(())
    })();
    if let Err(why) = copied {
        let _ = std::fs::remove_file(&to);
        return Err(why);
    }
    Ok(to)
}

/// As glassd starts: on the update, the image is the release's, and it's
/// made the permanent one, then the record says kept (with the rebuilt
/// kernel patches kept in it); back on the one before, the update rolled
/// back; an update that didn't stay is removed.
pub fn at_start(
    envs: &dyn Envs,
    store: &Store,
    hw: &hwpatch::Store,
    audit: &mut dyn FnMut(AuditEvent),
) {
    let Some(mut record) = store.record() else {
        return;
    };
    let Ok(running) = envs.running() else {
        return;
    };
    let env = record.update.env.clone();
    let from = record.update.from_env.clone();
    match record.update.state {
        TrialState::Staging | TrialState::Staged if running == env => {
            let says: Option<ImageRelease> = std::fs::read(envs.root().join(OS_RELEASE))
                .ok()
                .and_then(|text| serde_json::from_slice(&text).ok());
            if says.as_ref().map(|s| s.version.as_str()) != Some(record.update.version.as_str()) {
                // Not the release's image: back to the one before.
                if envs.permanent(&env).unwrap_or(false) {
                    let _ = envs.activate(&from);
                }
                let why = format!(
                    "the image says it's {}, not {}",
                    says.map_or("nothing".into(), |s| s.version),
                    record.update.version
                );
                record.update.state = TrialState::Failed;
                record.update.ended_at = Some(Timestamp::now());
                record.update.note = Some(why.clone());
                if store.end(&record).is_ok() {
                    audit(event(
                        &record.update,
                        format!(
                            "OS update {} failed ({why}); the next start goes back to {from}",
                            record.update.version
                        ),
                        Some(Outcome::Failed { error: why }),
                    ));
                }
                return;
            }
            // The permanent one first, then the record: a crash between is
            // finished at the next start (Confirm, Kept).
            if !envs.permanent(&env).unwrap_or(false)
                && let Err(why) = envs.activate(&env)
            {
                tracing::warn!("making {env} the boot environment: {why}");
                return;
            }
            if hw.carry(&env, &record.carried).is_err() {
                tracing::warn!("recording the kernel patches {env} carries");
            }
            record.update.state = TrialState::Kept;
            record.update.ended_at = Some(Timestamp::now());
            if store.end(&record).is_ok() {
                audit(event(
                    &record.update,
                    format!(
                        "OS update {} kept: the computer started on it; {env} is the boot \
                         environment from now on, and {from} is there to go back to",
                        record.update.version
                    ),
                    Some(Outcome::Completed),
                ));
            }
            remove_older(envs, store, &env, &from);
        }
        TrialState::Staging | TrialState::Staged if running == from => {
            match envs.starts_once(&env) {
                Ok(true) => {}
                Ok(false) => {
                    let why = if record.update.state == TrialState::Staged {
                        "the computer didn't start on it, and came back on the version before"
                    } else {
                        "installing it didn't finish"
                    };
                    record.update.state = TrialState::RolledBack;
                    record.update.ended_at = Some(Timestamp::now());
                    record.update.note = Some(why.into());
                    if store.end(&record).is_ok() {
                        audit(event(
                            &record.update,
                            format!("OS update {} rolled back: {why}", record.update.version),
                            Some(Outcome::Failed { error: why.into() }),
                        ));
                    }
                    remove(envs, &env, &running);
                }
                Err(_) => {}
            }
        }
        // Back on the version before a kept update that isn't the permanent
        // one any more: an undo a crash cut short.
        TrialState::Kept
            if running == from
                && envs.exists(&env).unwrap_or(false)
                && !envs.permanent(&env).unwrap_or(true) =>
        {
            record.update.state = TrialState::Undone;
            record.update.ended_at = Some(Timestamp::now());
            if store.end(&record).is_ok() {
                audit(event(
                    &record.update,
                    format!(
                        "OS update {} taken out: the computer is back on {from}",
                        record.update.version
                    ),
                    Some(Outcome::Completed),
                ));
            }
        }
        TrialState::Failed
        | TrialState::RolledBack
        | TrialState::Cancelled
        | TrialState::Undone => remove(envs, &env, &running),
        _ => {}
    }
}

/// Removes an update's environment once nothing runs from it or will start
/// it.
fn remove(envs: &dyn Envs, env: &str, running: &str) {
    if env != running
        && envs.exists(env).unwrap_or(false)
        && !envs.starts_once(env).unwrap_or(true)
        && !envs.permanent(env).unwrap_or(true)
    {
        let _ = envs.destroy(env);
    }
}

/// Once an update is kept: the version before stays, to go back to, and
/// the computer's first; the updates in between go.
fn remove_older(envs: &dyn Envs, store: &Store, kept: &str, before: &str) {
    for old in store.history() {
        if old.env != kept && old.env != before && old.state == TrialState::Kept {
            remove(envs, &old.env, kept);
        }
    }
}

/// Takes back an update installed but not yet started.
pub fn cancel(
    envs: &dyn Envs,
    store: &Store,
    audit: &mut dyn FnMut(AuditEvent),
) -> Result<OsUpdate, String> {
    let mut record = store
        .record()
        .filter(|r| r.update.state == TrialState::Staged)
        .ok_or("no OS update is waiting for a restart")?;
    let env = record.update.env.clone();
    if envs.starts_once(&env)? {
        envs.cancel_once()?;
    }
    envs.destroy(&env)?;
    record.update.state = TrialState::Cancelled;
    record.update.ended_at = Some(Timestamp::now());
    store.end(&record)?;
    audit(event(
        &record.update,
        format!(
            "OS update {} taken back before a restart",
            record.update.version
        ),
        Some(Outcome::Completed),
    ));
    Ok(record.update)
}

/// Back to the version before the update the computer runs: from the next
/// start.
pub fn undo(
    envs: &dyn Envs,
    store: &Store,
    audit: &mut dyn FnMut(AuditEvent),
) -> Result<OsUpdate, String> {
    let running = envs.running()?;
    let mut record = store
        .record()
        .filter(|r| r.update.state == TrialState::Kept && r.update.env == running)
        .ok_or_else(|| format!("{running} isn't an OS update this computer installed"))?;
    let from = record.update.from_env.clone();
    if !envs.exists(&from)? {
        return Err(format!(
            "{from}, the version before, is gone: nothing to go back to"
        ));
    }
    if !envs.permanent(&from)? {
        envs.activate(&from)?;
    }
    record.update.state = TrialState::Undone;
    record.update.ended_at = Some(Timestamp::now());
    record.update.note = Some("taken out".into());
    store.end(&record)?;
    audit(event(
        &record.update,
        format!(
            "OS update {} taken out: the next start is from {from}",
            record.update.version
        ),
        Some(Outcome::Completed),
    ));
    Ok(record.update)
}

/// What the running image says it is.
fn running_version(envs: &dyn Envs) -> Option<String> {
    std::fs::read(envs.root().join(OS_RELEASE))
        .ok()
        .and_then(|text| serde_json::from_slice::<ImageRelease>(&text).ok())
        .map(|r| r.version)
}

pub fn status(envs: &dyn Envs, store: &Store, config: &OsUpdates) -> UpdateStatus {
    let version = running_version(envs);
    UpdateStatus {
        running: envs.running().unwrap_or_default(),
        version,
        channel: config.channel.clone(),
        taken: store.taken().unwrap_or(0),
        update: store.record().map(|r| r.update),
        history: store.history(),
        available: None,
        checked: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use glass_managed::hwpatch::{Manifest, Module, Review};
    use glass_managed::osupdate::{ImageDigest, ListedKey, PatchRebuild, Release};
    use glass_managed::{DocKind, OrgKey, base64, sha256_hex};

    use super::*;
    use crate::testenvs::Fake;

    const ROOT: u8 = 1;
    const OS: u8 = 2;
    const PILOT: u8 = 3;
    const PATCHES: u8 = 4;

    fn key(n: u8) -> OrgKey {
        OrgKey::from_seed([n; 32])
    }

    struct Setup {
        fake: Fake,
        store: Store,
        hw: hwpatch::Store,
        source: tempfile::TempDir,
        audit: Vec<AuditEvent>,
    }

    fn setup() -> Setup {
        let fake = Fake::new();
        let state = fake.dir.path().join("state");
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("pilot")).unwrap();
        publish_keys(source.path(), 1, &[]);
        Setup {
            store: Store::new(&state),
            hw: hwpatch::Store::new(&state),
            fake,
            source,
            audit: Vec::new(),
        }
    }

    fn config(s: &Setup) -> OsUpdates {
        OsUpdates {
            roots: vec![key(ROOT).public()],
            channel: "pilot".into(),
            source: Some(s.source.path().to_owned()),
        }
    }

    fn patches() -> HardwarePatches {
        HardwarePatches {
            keys: vec![key(PATCHES).public()],
            ..HardwarePatches::default()
        }
    }

    /// The root's key list, revoking `revoked`.
    fn publish_keys(source: &Path, version: u64, revoked: &[u8]) {
        let list = KeyList {
            keys: vec![
                ListedKey {
                    purpose: "os".into(),
                    key: key(OS).public(),
                },
                ListedKey {
                    purpose: "channel:pilot".into(),
                    key: key(PILOT).public(),
                },
            ],
            revoked: revoked.iter().map(|n| key(*n).public().id()).collect(),
        };
        let signed = key(ROOT).sign(
            DocKind::OsKeys,
            GLASSHOUSE,
            version,
            serde_json::to_string(&list).unwrap(),
        );
        std::fs::write(
            source.join("keys.json"),
            serde_json::to_vec(&signed).unwrap(),
        )
        .unwrap();
    }

    /// Release `number`, version `version`: an image whose files are
    /// `files` (the fake's stream), signed by the OS key, and the release
    /// by the pilot key, with `edit` applied first.
    fn publish(
        s: &Setup,
        number: u64,
        version: &str,
        files: &[(&str, &str)],
        edit: impl FnOnce(&mut Release),
    ) {
        publish_with(s, number, version, files, None, edit);
    }

    /// [`publish`], the image a delta from `delta_from` when there's one.
    fn publish_with(
        s: &Setup,
        number: u64,
        version: &str,
        files: &[(&str, &str)],
        delta_from: Option<&str>,
        edit: impl FnOnce(&mut Release),
    ) {
        let mut files: BTreeMap<String, String> = files
            .iter()
            .map(|(p, c)| ((*p).to_owned(), (*c).to_owned()))
            .collect();
        files.entry(OS_RELEASE.into()).or_insert_with(|| {
            format!("{{\"version\": \"{version}\", \"base\": \"15.1-RELEASE-p2\"}}")
        });
        let stream = serde_json::to_vec(&files).unwrap();
        let file = format!("glasshouse-{version}.zfs");
        std::fs::write(s.source.path().join("pilot").join(&file), &stream).unwrap();
        let digest = ImageDigest {
            file,
            sha256: sha256_hex(&stream),
            bytes: stream.len() as u64,
            version: version.into(),
            delta_from: delta_from.map(str::to_owned),
        };
        let image = key(OS).sign(
            DocKind::OsImage,
            GLASSHOUSE,
            number,
            serde_json::to_string(&digest).unwrap(),
        );
        let mut release = Release {
            channel: "pilot".into(),
            version: version.into(),
            base: "15.1-RELEASE-p2".into(),
            image,
            fixes: vec![],
            patches: vec![],
            notes: String::new(),
        };
        edit(&mut release);
        let signed = key(PILOT).sign(
            DocKind::OsRelease,
            GLASSHOUSE,
            number,
            serde_json::to_string(&release).unwrap(),
        );
        std::fs::write(
            s.source.path().join("pilot/release.json"),
            serde_json::to_vec(&signed).unwrap(),
        )
        .unwrap();
    }

    fn apply_it(s: &mut Setup, drop_patches: bool) -> Result<OsUpdate, String> {
        let config = config(s);
        let audit = &mut s.audit;
        apply(
            &s.fake,
            &s.store,
            &s.hw,
            &config,
            &patches(),
            drop_patches,
            &mut |e| audit.push(e),
        )
    }

    fn start(s: &mut Setup) {
        s.fake.restart();
        let audit = &mut s.audit;
        at_start(&s.fake, &s.store, &s.hw, &mut |e| audit.push(e));
    }

    fn state(s: &Setup) -> TrialState {
        s.store.record().unwrap().update.state
    }

    #[test]
    fn ep1_an_update_is_tried_once_then_kept() {
        let mut s = setup();
        publish(&s, 7, "2026.11.1", &[], |_| {});
        let checked = check(&s.fake, &s.store, &s.hw, &config(&s), &patches());
        assert_eq!(checked.available.as_ref().unwrap().version, "2026.11.1");
        assert_eq!(checked.available.unwrap().blocked, None);

        let update = apply_it(&mut s, false).unwrap();
        assert_eq!(update.state, TrialState::Staged);
        assert_eq!(update.env, "glass-os-2026.11.1");
        assert!(s.fake.starts_once(&update.env).unwrap());
        assert_eq!(
            s.fake.s().permanent,
            "default",
            "not permanent before it starts"
        );
        assert_eq!(s.store.taken(), Some(7));
        assert!(
            std::fs::read_dir(s.store.dir.join("incoming"))
                .unwrap()
                .next()
                .is_none(),
            "the copy is gone"
        );

        start(&mut s);
        assert_eq!(state(&s), TrialState::Kept);
        assert_eq!(s.fake.s().permanent, update.env);
        let now = status(&s.fake, &s.store, &config(&s));
        assert_eq!(now.version.as_deref(), Some("2026.11.1"));
        assert!(
            s.fake.exists("default").unwrap(),
            "the version before stays"
        );

        // The same release again, or an older one: nothing newer to take.
        let again = apply_it(&mut s, false).unwrap_err();
        assert!(again.contains("not newer"), "{again}");
        publish(&s, 6, "2026.10.9", &[], |_| {});
        assert!(apply_it(&mut s, false).unwrap_err().contains("not newer"));
        assert!(
            s.audit
                .iter()
                .any(|e| e.summary.starts_with("OS update refused")),
            "refusals are on the record"
        );
    }

    /// OsUpdate.tla's "permanent": an image that doesn't start is never
    /// what every start uses.
    #[test]
    fn ep1_an_image_that_doesnt_start_rolls_back() {
        let mut s = setup();
        publish(
            &s,
            7,
            "2026.11.1",
            &[("boot/loader.conf", "glass_panic_test_load=\"YES\"\n")],
            |_| {},
        );
        s.fake.s().panics.push("glass_panic_test".into());
        let update = apply_it(&mut s, false).unwrap();
        start(&mut s);
        assert_eq!(s.fake.s().running, "default");
        let ended = s.store.record().unwrap().update;
        assert_eq!(ended.state, TrialState::RolledBack);
        assert!(ended.note.unwrap().contains("didn't start on it"));
        assert!(!s.fake.exists(&update.env).unwrap(), "removed");
    }

    #[test]
    fn ep1_an_image_that_isnt_the_release_fails() {
        let mut s = setup();
        publish(
            &s,
            7,
            "2026.11.1",
            &[(OS_RELEASE, "{\"version\": \"2026.10.9\"}")],
            |_| {},
        );
        apply_it(&mut s, false).unwrap();
        start(&mut s);
        assert_eq!(state(&s), TrialState::Failed);
        assert_eq!(s.fake.s().permanent, "default");
        start(&mut s);
        assert_eq!(s.fake.s().running, "default");
    }

    /// OsUpdate.tla's "trustAnyKey": what isn't signed as the root's list
    /// says is refused, and changes nothing.
    #[test]
    fn ep11_what_the_keys_dont_sign_is_refused() {
        let mut s = setup();
        publish(&s, 7, "2026.11.1", &[], |_| {});
        // The image changed after the OS key signed it.
        let image = s.source.path().join("pilot/glasshouse-2026.11.1.zfs");
        let mut bytes = std::fs::read(&image).unwrap();
        bytes[1] ^= 1;
        std::fs::write(&image, bytes).unwrap();
        let refused = apply_it(&mut s, false).unwrap_err();
        assert!(refused.contains("doesn't match the digest"), "{refused}");
        assert_eq!(state(&s), TrialState::RolledBack);
        assert!(!s.fake.exists("glass-os-2026.11.1").unwrap());
        assert_eq!(s.store.taken(), None, "a failed install takes no number");

        // The root revokes the pilot key: the release is refused, and stays
        // refused when the old list comes back.
        publish(&s, 7, "2026.11.1", &[], |_| {});
        publish_keys(s.source.path(), 2, &[PILOT]);
        assert!(
            apply_it(&mut s, false)
                .unwrap_err()
                .contains("no key is listed")
        );
        publish_keys(s.source.path(), 1, &[]);
        assert!(
            apply_it(&mut s, false)
                .unwrap_err()
                .contains("no key is listed")
        );

        // A computer that trusts no root takes nothing.
        let mut none = config(&s);
        none.roots.clear();
        assert!(
            apply(
                &s.fake,
                &s.store,
                &s.hw,
                &none,
                &patches(),
                false,
                &mut |_| {}
            )
            .unwrap_err()
            .contains("trusts no root key")
        );
    }

    /// A kept kernel patch's rebuild, with its own signature: what a
    /// computer that keeps it gets with the update.
    fn rebuild(s: &Setup, id: &str, release: &str) -> String {
        let module = format!("\x7fELF {id} rebuilt").into_bytes();
        let manifest = Manifest {
            id: id.into(),
            summary: "Wi-Fi works again after sleep".into(),
            release: release.into(),
            origin: "glasshouse".into(),
            review: Review::Reviewed,
            targets: vec![],
            modules: vec![Module {
                file: "if_iwlwifi.ko".into(),
                sha256: sha256_hex(&module),
                load_at_boot: false,
            }],
            patch_sha256: sha256_hex(b""),
            requirements: vec![],
            includes: vec![],
            resumes: 0,
        };
        let bundle = Bundle {
            signed: key(PATCHES).sign(
                DocKind::HardwarePatch,
                GLASSHOUSE,
                2,
                serde_json::to_string(&manifest).unwrap(),
            ),
            patch: String::new(),
            modules: BTreeMap::from([("if_iwlwifi.ko".into(), base64::encode(&module))]),
        };
        let file = format!("{id}.bundle.json");
        std::fs::write(
            s.source.path().join("pilot").join(&file),
            serde_json::to_vec(&bundle).unwrap(),
        )
        .unwrap();
        file
    }

    /// OsUpdate.tla's "patchesIgnored": a computer that keeps a kernel
    /// patch keeps its fix through an update, or the person chose not to.
    #[test]
    fn adr0061_a_kept_patch_s_fix_survives_the_update() {
        let mut s = setup();
        let kept = PatchTrial {
            id: "lkpi-wlan-suspend".into(),
            revision: 1,
            summary: "Wi-Fi works again after sleep".into(),
            review: "reviewed".into(),
            env: "default".into(),
            from_env: "default".into(),
            state: TrialState::Kept,
            modules: vec!["if_iwlwifi.ko".into()],
            staged_at: Timestamp::now(),
            ended_at: None,
            note: None,
            ran_minutes: 0,
            probation_minutes: 0,
            woke: 0,
            probation_resumes: 0,
        };
        s.hw.carry("default", &[kept]).unwrap();

        // Neither a fix nor a rebuild: refused, unless the person drops it.
        publish(&s, 7, "2026.11.1", &[], |_| {});
        let blocked = check(&s.fake, &s.store, &s.hw, &config(&s), &patches());
        assert!(
            blocked
                .available
                .unwrap()
                .blocked
                .unwrap()
                .contains("keeps this working"),
            "check says so"
        );
        let refused = apply_it(&mut s, false).unwrap_err();
        assert!(
            refused.contains("neither fixes that nor brings a rebuild"),
            "{refused}"
        );
        assert!(s.store.record().is_none(), "nothing set up");

        // A rebuild for a release the image isn't: refused.
        let wrong = rebuild(&s, "lkpi-wlan-suspend", "15.0-RELEASE");
        publish(&s, 7, "2026.11.1", &[], |r| {
            r.patches = vec![PatchRebuild {
                id: "lkpi-wlan-suspend".into(),
                bundle: wrong,
            }];
        });
        assert!(
            apply_it(&mut s, false)
                .unwrap_err()
                .contains("15.0-RELEASE")
        );

        // Its rebuild: written into the new image, kept there with it.
        let file = rebuild(&s, "lkpi-wlan-suspend", "15.1-RELEASE");
        publish(&s, 7, "2026.11.1", &[], |r| {
            r.patches = vec![PatchRebuild {
                id: "lkpi-wlan-suspend".into(),
                bundle: file,
            }];
        });
        let update = apply_it(&mut s, false).unwrap();
        assert!(
            update.patches[0].contains("rebuilt for 15.1-RELEASE-p2"),
            "{:?}",
            update.patches
        );
        let module = s
            .fake
            .env_dir(&update.env)
            .join("boot/kernel/if_iwlwifi.ko");
        assert!(std::fs::read(module).unwrap().ends_with(b"rebuilt"));
        start(&mut s);
        assert_eq!(state(&s), TrialState::Kept);
        let in_effect = hwpatch::in_effect(&s.hw.history(), &update.env);
        assert_eq!(in_effect.len(), 1);
        assert_eq!(in_effect[0].revision, 2);

        // The next release fixes it: the patch goes.
        publish(&s, 8, "2026.12.1", &[], |r| {
            r.fixes = vec!["lkpi-wlan-suspend".into()];
        });
        let next = apply_it(&mut s, false).unwrap();
        assert!(next.patches[0].contains("fixed in 2026.12.1"));
        start(&mut s);
        assert!(hwpatch::in_effect(&s.hw.history(), &next.env).is_empty());

        // Once a third is kept, the update two back is removed; the one
        // before stays to go back to, and the computer's first stays too.
        publish(&s, 9, "2027.1.1", &[], |_| {});
        apply_it(&mut s, false).unwrap();
        start(&mut s);
        assert!(!s.fake.exists(&update.env).unwrap(), "two back is removed");
        assert!(s.fake.exists(&next.env).unwrap());
        assert!(s.fake.exists("default").unwrap());
    }

    /// A delta: the changes since the image the computer runs, received as
    /// a clone of it.
    #[test]
    fn bs4_a_delta_goes_on_top_of_the_image_the_computer_runs() {
        let mut s = setup();
        let default = s.fake.env_dir("default");
        std::fs::create_dir_all(default.join("usr/local/share/glasshouse")).unwrap();
        std::fs::write(default.join(OS_RELEASE), "{\"version\": \"2026.10.1\"}").unwrap();
        std::fs::write(default.join("etc-motd"), "before").unwrap();

        publish_with(
            &s,
            7,
            "2026.11.1",
            &[("etc-motd", "after")],
            Some("2026.10.9"),
            |_| {},
        );
        let refused = apply_it(&mut s, false).unwrap_err();
        assert!(refused.contains("the changes since 2026.10.9"), "{refused}");

        publish_with(
            &s,
            7,
            "2026.11.1",
            &[("etc-motd", "after")],
            Some("2026.10.1"),
            |_| {},
        );
        let update = apply_it(&mut s, false).unwrap();
        let new = s.fake.env_dir(&update.env);
        assert_eq!(
            std::fs::read_to_string(new.join("etc-motd")).unwrap(),
            "after"
        );
        assert!(
            new.join("boot/kernel/if_iwlwifi.ko").exists(),
            "the rest from the base"
        );
        start(&mut s);
        assert_eq!(state(&s), TrialState::Kept);
        assert_eq!(
            status(&s.fake, &s.store, &config(&s)).version.as_deref(),
            Some("2026.11.1")
        );
    }

    #[test]
    fn adr0061_an_update_is_taken_back_or_taken_out() {
        let mut s = setup();
        publish(&s, 7, "2026.11.1", &[], |_| {});
        let update = apply_it(&mut s, false).unwrap();
        cancel(&s.fake, &s.store, &mut |_| {}).unwrap();
        assert!(!s.fake.exists(&update.env).unwrap());
        start(&mut s);
        assert_eq!(s.fake.s().running, "default");

        // Taken back, the release can't be taken again: its number is.
        assert!(apply_it(&mut s, false).unwrap_err().contains("not newer"));
        publish(&s, 8, "2026.11.2", &[], |_| {});
        let update = apply_it(&mut s, false).unwrap();
        start(&mut s);
        assert_eq!(state(&s), TrialState::Kept);
        let out = undo(&s.fake, &s.store, &mut |_| {}).unwrap();
        assert_eq!(out.state, TrialState::Undone);
        assert_eq!(s.fake.s().permanent, "default");
        start(&mut s);
        assert_eq!(s.fake.s().running, "default");
        assert!(!s.fake.exists(&update.env).unwrap(), "removed once off it");
    }
}
