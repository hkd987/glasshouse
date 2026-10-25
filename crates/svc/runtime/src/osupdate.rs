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
//!
//! Releases come from a [`Source`]: a folder ([`Folder`]: a share, a USB
//! drive), Glasshouse's update server over HTTPS, or the org's console,
//! which serves each computer its ring's channel. Whatever brings them,
//! what's taken is what the keys signed: an image is downloaded into
//! glassd's state, hashed as it arrives, and a download that stopped goes
//! on from where it was.

use std::io::Read as _;
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
/// What an update leaves free on the disk, beyond its image downloaded and
/// received.
const ROOM_TO_SPARE: u64 = 1024 * 1024 * 1024;
/// What an image says it is.
pub const OS_RELEASE: &str = "usr/local/share/glasshouse/os-release.json";

/// The most of `keys.json` or a `release.json` read.
const MAX_SIGNED_BYTES: u64 = 1024 * 1024;

/// Which updates this computer takes (glassd.toml `[update]`).
#[derive(Debug, Clone, Default)]
pub struct OsUpdates {
    /// The root keys the image trusts. With none, it takes no update.
    pub roots: Vec<PublicKey>,
    /// `pilot`, `early` or `broad`, unless the source says which (the
    /// org's console: the computer's ring).
    pub channel: String,
    /// Where releases come from.
    pub source: Option<std::sync::Arc<dyn Source>>,
}

impl OsUpdates {
    /// The channel this computer follows now.
    pub fn channel(&self) -> String {
        self.source
            .as_ref()
            .and_then(|s| s.channel())
            .unwrap_or_else(|| self.channel.clone())
    }
}

/// Where releases come from: the root's key list, and each channel's
/// `release.json` with its image and kernel patch rebuilds beside it.
/// Nothing a source brings is trusted for coming from it.
pub trait Source: Send + Sync + std::fmt::Debug {
    /// Where, for people: a folder, an address, "the org's console".
    fn describe(&self) -> String;
    /// The channel it serves this computer, when it decides: the org's
    /// console, by the computer's ring.
    fn channel(&self) -> Option<String> {
        None
    }
    /// The root's key list, `keys.json`: at most `max` bytes.
    fn keys(&self, max: u64) -> Result<Vec<u8>, String>;
    /// `name`, a file of `channel`'s release, from byte `offset` on, onto
    /// `to`: at most `max` bytes, the number written. What it wrote before
    /// an error stays written.
    fn file(
        &self,
        channel: &str,
        name: &str,
        offset: u64,
        to: &mut dyn std::io::Write,
        max: u64,
    ) -> Result<u64, String>;
}

/// A small file of `channel`'s release, whole.
fn small(source: &dyn Source, channel: &str, name: &str, max: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    source.file(channel, name, 0, &mut bytes, max)?;
    Ok(bytes)
}

/// Releases in a folder: `keys.json`, and `<channel>/release.json` with
/// its image beside it (a share, a USB drive).
#[derive(Debug, Clone)]
pub struct Folder(PathBuf);

impl Folder {
    pub fn new(dir: &Path) -> Self {
        Self(dir.to_owned())
    }

    fn read(
        path: &Path,
        offset: u64,
        to: &mut dyn std::io::Write,
        max: u64,
    ) -> Result<u64, String> {
        use std::io::Seek as _;
        let shown = |e: std::io::Error| format!("{}: {e}", path.display());
        let mut file = std::fs::File::open(path).map_err(shown)?;
        file.seek(std::io::SeekFrom::Start(offset)).map_err(shown)?;
        // One byte past the most, to tell a file that's too long.
        let copied = std::io::copy(&mut file.take(max.saturating_add(1)), to).map_err(shown)?;
        if copied > max {
            return Err(format!("{} is longer than {max} bytes", path.display()));
        }
        Ok(copied)
    }
}

impl Source for Folder {
    fn describe(&self) -> String {
        self.0.display().to_string()
    }

    fn keys(&self, max: u64) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        Self::read(&self.0.join("keys.json"), 0, &mut bytes, max)?;
        Ok(bytes)
    }

    fn file(
        &self,
        channel: &str,
        name: &str,
        offset: u64,
        to: &mut dyn std::io::Write,
        max: u64,
    ) -> Result<u64, String> {
        if !osupdate::valid_file(channel) || !osupdate::valid_file(name) {
            return Err(format!("{channel}/{name} isn't a release's file"));
        }
        Self::read(&self.0.join(channel).join(name), offset, to, max)
    }
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

    /// The last release number taken.
    pub fn taken(&self) -> Option<u64> {
        self.read::<Taken>("taken.json").map(|t| t.number)
    }

    /// The key list in force: the newest one a root signed.
    fn keys(&self) -> Option<Signed> {
        self.read("keys.json")
    }

    /// When the computer restarts into the update installed, if someone
    /// chose a time and the update still waits for it.
    fn restart_at(&self) -> Option<Timestamp> {
        let staged = self
            .record()
            .is_some_and(|r| r.update.state == TrialState::Staged);
        staged
            .then(|| self.read::<Restart>("restart.json").map(|r| r.at))
            .flatten()
    }
}

/// When the person chose to restart into the update installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Restart {
    at: Timestamp,
}

/// The restart into an update, at the hour of the night it happens at, the
/// computer's time: nobody's at work, and it's tonight for whoever chose it.
const RESTART_HOUR: i64 = 3;

/// The next 3 in the morning after `now`, `offset_secs` east of UTC.
pub fn tonight(now: Timestamp, offset_secs: i32) -> Timestamp {
    let local = now.unix_seconds() + i64::from(offset_secs);
    let mut at = local.div_euclid(86_400) * 86_400 + RESTART_HOUR * 3600;
    if at <= local {
        at += 86_400;
    }
    Timestamp::from_unix_seconds(at - i64::from(offset_secs)).unwrap_or(now)
}

/// Sets when the computer restarts into the update installed, or, with
/// `None`, takes that back. There must be one installed.
pub fn schedule_restart(store: &Store, at: Option<Timestamp>) -> Result<(), String> {
    let staged = store
        .record()
        .is_some_and(|r| r.update.state == TrialState::Staged);
    match at {
        Some(_) if !staged => Err("no update is installed and waiting for a restart".into()),
        Some(at) => store.write("restart.json", &Restart { at }),
        None => {
            let _ = std::fs::remove_file(store.dir.join("restart.json"));
            Ok(())
        }
    }
}

/// How late a restart someone chose still happens. A computer that was
/// off or asleep at 3 doesn't restart under someone at work at 9: its
/// restart moves to the next night.
const RESTART_LATE_SECS: i64 = 3 * 3600;

/// Whether the restart someone chose is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDue {
    /// None is set, or its time hasn't come.
    No,
    /// Its time has come and the update still waits for it.
    Now,
    /// Its time went by while the computer was off or asleep.
    Missed,
}

pub fn restart_due(store: &Store, now: Timestamp) -> RestartDue {
    match store.restart_at() {
        Some(at) if at > now => RestartDue::No,
        Some(at) if now.unix_seconds() - at.unix_seconds() < RESTART_LATE_SECS => RestartDue::Now,
        Some(_) => RestartDue::Missed,
        None => RestartDue::No,
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

/// Where releases come from, and the channel they're read on.
type Channel<'a> = (&'a dyn Source, String);

/// The newest release on the channel, verified against the key list the
/// computer holds, or a newer one in the source, which it then keeps.
fn newest<'a>(store: &Store, config: &'a OsUpdates) -> Result<(Verified, Channel<'a>), String> {
    if config.roots.is_empty() {
        return Err("this computer takes no OS updates: it trusts no root key".into());
    }
    let source = config
        .source
        .as_deref()
        .ok_or("this computer has no update source")?;
    let channel = config.channel();
    let parse = |what: &str, bytes: Vec<u8>| -> Result<Signed, String> {
        serde_json::from_slice(&bytes).map_err(|e| format!("{what}: {e}"))
    };
    // The key list: a newer one replaces the one in force, revoking keys
    // whatever happens to the release.
    let have = store.keys();
    let offered = source
        .keys(MAX_SIGNED_BYTES)
        .and_then(|bytes| parse("keys.json", bytes))
        .ok();
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
    let release = parse(
        "release.json",
        small(source, &channel, "release.json", MAX_SIGNED_BYTES)?,
    )?;
    let verified = osupdate::verify_release(&keys, &release, &channel, store.taken())?;
    Ok((verified, (source, channel)))
}

/// A kept kernel patch's rebuild for the new image: its record there, and
/// the bundle to write in.
type Rebuild = (PatchTrial, Opened);

/// What the update does with each kernel patch the computer keeps, or why
/// it can't be taken: the patch's fix would be gone after it (Check,
/// `CarriesFix`).
///
/// A check (`fetch` false) says a rebuild comes with the update without
/// downloading it: it's read and checked when the update is installed.
fn plan_patches(
    hw: &hwpatch::Store,
    patches: &HardwarePatches,
    verified: &Verified,
    (source, channel): &Channel<'_>,
    running: &str,
    drop_patches: bool,
    fetch: bool,
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
            if !fetch {
                notes.push(format!(
                    "{}: a rebuild for {} comes with it",
                    kept.id, release.base
                ));
                continue;
            }
            let bytes = small(
                *source,
                channel,
                &rebuild.bundle,
                glass_managed::hwreview::MAX_BUNDLE_REPLY as u64,
            )?;
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
                match plan_patches(hw, patches, &verified, &channel, &running, false, false) {
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

/// Downloads the newest release's image ahead of installing it, when it's
/// newer than the last taken and nothing blocks it: the install then reads
/// it from glassd's state. What it says is the check's.
pub fn fetch(
    envs: &dyn Envs,
    store: &Store,
    hw: &hwpatch::Store,
    config: &OsUpdates,
    patches: &HardwarePatches,
) -> UpdateStatus {
    let mut checked = check(envs, store, hw, config, patches);
    if checked
        .available
        .as_ref()
        .is_some_and(|a| a.blocked.is_none())
        && let Ok((verified, channel)) = newest(store, config)
        && let Err(why) = download(store, &channel, &verified)
    {
        checked.checked = Some(why);
    }
    let _ = store.write("checked.json", &checked);
    checked
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
        let (notes, rebuilds) = plan_patches(
            hw,
            patches,
            &verified,
            &channel,
            &running,
            drop_patches,
            true,
        )?;
        Ok((verified, channel, running, env, notes, rebuilds))
    });
    let (verified, channel, running, env, notes, rebuilds) = match checked {
        Ok(checked) => checked,
        Err(why) => return Err(refuse(why, audit)),
    };
    // The image, before anything is staged: a download that stops changes
    // nothing but what it downloaded, which the next try goes on from.
    let copy = match download(store, &channel, &verified) {
        Ok(copy) => copy,
        Err(why) => {
            let mut event = AuditEvent::new(
                EventKind::OsUpdate,
                format!(
                    "OS update {} not downloaded: {why}",
                    verified.release.version
                ),
            );
            event.outcome = Some(Outcome::Failed { error: why.clone() });
            audit(event);
            return Err(why);
        }
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

/// The image downloaded beside glassd's state, hashed as it arrives, and
/// checked against the OS key's digest: what's imported is what was
/// checked, even if the source changes meanwhile. A download that stopped
/// goes on from where it was, its file named by the digest it's for.
fn download(
    store: &Store,
    (source, channel): &Channel<'_>,
    verified: &Verified,
) -> Result<PathBuf, String> {
    use sha2::Digest as _;
    let image = &verified.image;
    if image.bytes > MAX_IMAGE_BYTES {
        return Err(format!(
            "the image is {} bytes, more than this computer takes",
            image.bytes
        ));
    }
    if image.sha256.len() != 64 || !image.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the image's digest isn't a SHA-256".into());
    }
    let dir = store.dir.join("incoming");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Downloads for other images are moot.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(&image.sha256)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let to = dir.join(format!("{}{}", image.sha256, osupdate::IMAGE_SUFFIX));
    // What arrived before, hashed again.
    let mut hasher = sha2::Sha256::new();
    let mut have: u64 = 0;
    if let Ok(mut partial) = std::fs::File::open(&to) {
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = partial.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            have += n as u64;
        }
    }
    if have > image.bytes {
        let _ = std::fs::remove_file(&to);
        return Err(
            "the downloaded image was longer than its signed size; download it again".into(),
        );
    }
    // Room for the rest of it, for receiving it as a boot environment, and
    // to spare: an update never fills the disk.
    let needed = (image.bytes - have) + image.bytes + ROOM_TO_SPARE;
    if let Ok(usage) = glass_platform::system::filesystem_usage(&dir)
        && usage.available_kb.saturating_mul(1024) < needed
    {
        return Err(format!(
            "not enough room for it: the update needs {} MB free and the disk has {} MB",
            needed.div_ceil(1024 * 1024),
            usage.available_kb / 1024
        ));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&to)
        .map_err(|e| format!("{}: {e}", to.display()))?;
    let mut sink = Hashing {
        file,
        hasher: &mut hasher,
        written: 0,
    };
    let fetched = if have < image.bytes {
        source.file(channel, &image.file, have, &mut sink, image.bytes - have)
    } else {
        Ok(0)
    };
    let written = sink.written;
    sink.file.sync_all().map_err(|e| e.to_string())?;
    drop(sink);
    let total = have + written;
    if let Err(why) = fetched {
        return Err(format!(
            "the download stopped at {} of {} MB ({why}); apply again to go on",
            total / (1024 * 1024),
            image.bytes.div_ceil(1024 * 1024)
        ));
    }
    if total != image.bytes {
        return Err(format!(
            "the download stopped at {total} of {} bytes; apply again to go on",
            image.bytes
        ));
    }
    if hex::encode(hasher.finalize()) != image.sha256 {
        let _ = std::fs::remove_file(&to);
        return Err("the image doesn't match the digest the OS key signed".into());
    }
    Ok(to)
}

/// A file that hashes what's written to it, and counts it.
struct Hashing<'a> {
    file: std::fs::File,
    hasher: &'a mut sha2::Sha256,
    written: u64,
}

impl std::io::Write for Hashing<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest as _;
        let n = self.file.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
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
    let kept = |u: &OsUpdate| u.state == TrialState::Kept && u.env == running;
    // The record is the last update tried: one tried since, and rolled
    // back, leaves the kept one in the history.
    let mut record = store
        .record()
        .filter(|r| kept(&r.update))
        .or_else(|| {
            store.history().into_iter().find(kept).map(|update| Record {
                update,
                carried: Vec::new(),
            })
        })
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
    let taken = store.taken().unwrap_or(0);
    // What the last check in the background found, while it's still news.
    let last = store
        .read::<UpdateStatus>("checked.json")
        .and_then(|c| c.available)
        .filter(|a| a.number > taken);
    UpdateStatus {
        running: envs.running().unwrap_or_default(),
        version,
        channel: config.channel(),
        taken,
        update: store.record().map(|r| r.update),
        history: store.history(),
        available: last,
        checked: None,
        restart_at: store.restart_at(),
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
            source: Some(std::sync::Arc::new(Folder::new(s.source.path()))),
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
        assert!(s.store.record().is_none(), "nothing was staged");
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

    fn at(text: &str) -> Timestamp {
        serde_json::from_str(&format!("\"{text}\"")).unwrap()
    }

    /// The person chooses when the computer restarts into an update: a
    /// time only while one is installed and waits, 3 in the morning the
    /// computer's time, and never hours late.
    #[test]
    fn adr0061_the_person_restarts_into_an_update_now_or_tonight() {
        // Tonight is the next 3 o'clock, local.
        assert_eq!(
            tonight(at("2026-10-25T22:15:00Z"), 7200),
            at("2026-10-26T01:00:00Z")
        );
        assert_eq!(
            tonight(at("2026-10-25T08:00:00Z"), -18000),
            at("2026-10-26T08:00:00Z")
        );
        assert_eq!(
            tonight(at("2026-10-25T07:59:00Z"), -18000),
            at("2026-10-25T08:00:00Z")
        );

        let mut s = setup();
        let soon = at("2026-10-26T01:00:00Z");
        assert!(
            schedule_restart(&s.store, Some(soon)).is_err(),
            "nothing installed"
        );
        publish(&s, 7, "2026.11.1", &[], |_| {});
        apply_it(&mut s, false).unwrap();
        schedule_restart(&s.store, Some(soon)).unwrap();
        assert_eq!(
            status(&s.fake, &s.store, &config(&s)).restart_at,
            Some(soon)
        );
        assert_eq!(
            restart_due(&s.store, at("2026-10-26T00:59:00Z")),
            RestartDue::No
        );
        assert_eq!(restart_due(&s.store, soon), RestartDue::Now);
        assert_eq!(
            restart_due(&s.store, at("2026-10-26T03:59:00Z")),
            RestartDue::Now
        );
        // Asleep at 3, it doesn't restart under someone at work at 9.
        assert_eq!(
            restart_due(&s.store, at("2026-10-26T07:00:00Z")),
            RestartDue::Missed
        );
        // Not tonight after all.
        schedule_restart(&s.store, None).unwrap();
        assert_eq!(restart_due(&s.store, soon), RestartDue::No);
        // Set again, then the update is taken back: nothing to restart for.
        schedule_restart(&s.store, Some(soon)).unwrap();
        let audit = &mut s.audit;
        cancel(&s.fake, &s.store, &mut |e| audit.push(e)).unwrap();
        assert_eq!(restart_due(&s.store, soon), RestartDue::No);
        assert_eq!(status(&s.fake, &s.store, &config(&s)).restart_at, None);
    }

    /// A [`Folder`] that stops after `stop` bytes of an image, as a
    /// download does when the network goes, and keeps what it was asked.
    #[derive(Debug)]
    struct Flaky {
        folder: Folder,
        stop: std::sync::Mutex<Option<u64>>,
        channel: Option<String>,
        asked: std::sync::Mutex<Vec<(String, u64)>>,
    }

    impl Source for Flaky {
        fn describe(&self) -> String {
            "a flaky network".into()
        }
        fn channel(&self) -> Option<String> {
            self.channel.clone()
        }
        fn keys(&self, max: u64) -> Result<Vec<u8>, String> {
            self.folder.keys(max)
        }
        fn file(
            &self,
            channel: &str,
            name: &str,
            offset: u64,
            to: &mut dyn std::io::Write,
            max: u64,
        ) -> Result<u64, String> {
            self.asked.lock().unwrap().push((name.to_owned(), offset));
            let stop = if name.ends_with(osupdate::IMAGE_SUFFIX) {
                self.stop.lock().unwrap().take()
            } else {
                None
            };
            match stop {
                Some(stop) => {
                    let mut all = Vec::new();
                    self.folder.file(channel, name, offset, &mut all, max)?;
                    to.write_all(&all[..stop as usize]).unwrap();
                    Err("the connection was reset".into())
                }
                _ => self.folder.file(channel, name, offset, to, max),
            }
        }
    }

    fn flaky(s: &Setup, stop: Option<u64>, channel: Option<&str>) -> std::sync::Arc<Flaky> {
        std::sync::Arc::new(Flaky {
            folder: Folder::new(s.source.path()),
            stop: std::sync::Mutex::new(stop),
            channel: channel.map(str::to_owned),
            asked: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// An image comes over the network: a download that stops stages
    /// nothing, and the next try goes on from where it stopped.
    #[test]
    fn bs4_a_download_that_stopped_goes_on_from_where_it_was() {
        let mut s = setup();
        publish(
            &s,
            7,
            "2026.11.1",
            &[("etc/motd", &"x".repeat(5000))],
            |_| {},
        );
        let source = flaky(&s, Some(1200), None);
        let mut config = config(&s);
        config.source = Some(source.clone());
        let audit = &mut s.audit;
        let mut run = || {
            apply(
                &s.fake,
                &s.store,
                &s.hw,
                &config,
                &patches(),
                false,
                &mut |e| audit.push(e),
            )
        };
        let stopped = run().unwrap_err();
        assert!(stopped.contains("apply again to go on"), "{stopped}");
        assert!(s.store.record().is_none(), "nothing staged");
        assert_eq!(s.store.taken(), None);

        let update = run().unwrap();
        assert_eq!(update.state, TrialState::Staged);
        let images: Vec<u64> = source
            .asked
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name.ends_with(".zfs"))
            .map(|(_, offset)| *offset)
            .collect();
        assert_eq!(images, [0, 1200], "the second download went on from 1200");
        assert!(
            s.audit.iter().any(|e| e.summary.contains("not downloaded")),
            "the stop is on the record"
        );
    }

    /// A check reads the release, never a kernel patch's rebuild: those
    /// come when the update is installed. A source may decide the channel:
    /// the org's console, by the computer's ring.
    #[test]
    fn adr0061_a_check_downloads_no_rebuild_and_follows_the_source_s_channel() {
        let s = setup();
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
        let file = rebuild(&s, "lkpi-wlan-suspend", "15.1-RELEASE");
        publish(&s, 7, "2026.11.1", &[], |r| {
            r.patches = vec![PatchRebuild {
                id: "lkpi-wlan-suspend".into(),
                bundle: file,
            }];
        });
        let source = flaky(&s, None, Some("pilot"));
        let config = OsUpdates {
            channel: "broad".into(),
            source: Some(source.clone()),
            ..config(&s)
        };
        let checked = check(&s.fake, &s.store, &s.hw, &config, &patches());
        assert_eq!(checked.channel, "pilot");
        let available = checked.available.unwrap();
        assert_eq!(available.blocked, None);
        assert!(
            available.patches[0].contains("comes with it"),
            "{available:?}"
        );
        let asked: Vec<String> = source
            .asked
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        assert_eq!(asked, ["release.json"]);

        // Fetched ahead: the image is downloaded, nothing is staged.
        let fetched = fetch(&s.fake, &s.store, &s.hw, &config, &patches());
        assert_eq!(fetched.checked, None);
        assert!(s.store.record().is_none());
        assert!(
            std::fs::read_dir(s.store.dir.join("incoming"))
                .unwrap()
                .count()
                == 1
        );
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

    /// The dev VM's update test (2026-10-25): an update tried after the
    /// kept one, which didn't start, left undo refusing the kept one, "isn't
    /// an OS update this computer installed".
    #[test]
    fn adr0061_a_kept_update_is_taken_out_after_a_later_one_rolled_back() {
        let mut s = setup();
        publish(&s, 7, "2026.11.1", &[], |_| {});
        let kept = apply_it(&mut s, false).unwrap();
        start(&mut s);
        assert_eq!(state(&s), TrialState::Kept);
        publish(
            &s,
            8,
            "2026.11.2",
            &[("boot/loader.conf", "glass_panic_test_load=\"YES\"\n")],
            |_| {},
        );
        s.fake.s().panics.push("glass_panic_test".into());
        apply_it(&mut s, false).unwrap();
        start(&mut s);
        assert_eq!(state(&s), TrialState::RolledBack);
        assert_eq!(s.fake.s().running, kept.env);

        let out = undo(&s.fake, &s.store, &mut |_| {}).unwrap();
        assert_eq!(
            (out.state, out.version.as_str()),
            (TrialState::Undone, "2026.11.1")
        );
        assert_eq!(s.fake.s().permanent, "default");
        start(&mut s);
        assert_eq!(s.fake.s().running, "default");
    }
}
