//! Managed settings from the org's admin console (ADR 0023, ADR 0021).
//!
//! Boot never waits for any of this: glassd starts from the policy file
//! on disk, and the catalog it last applied is read once glassd is up.
//! Then a background thread long-polls the console for newer releases.
//! Each one is verified (kind, org, a trusted key, a version above the one
//! in force), then validated whole: the policy parsed and linted against a
//! registry built from it, the catalog checked. Only then is it written,
//! with the previous version kept, and swapped in at once. Anything
//! refused is audited and reported, and the device keeps what it has.
//!
//! Offline, a failed sync backs off and changes nothing: the device keeps
//! enforcing what it last applied, and `glassctl status` says since when
//! the console has been out of reach.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glass_app_catalog::Applied;
use glass_audit::{AuditEvent, EventKind, Outcome};
use glass_first_party::{ChatHandle, ChatNotice};
use glass_gateway_providers::ConsoleClient;
use glass_managed::device;
use glass_managed::directory::{self, DirectoryRelease};
use glass_managed::{
    CatalogRelease, ChatRelease, DeviceReport, DocKind, MAX_WAIT_SECS, ModelCredits, PolicyRelease,
    PublicKey, Refusal, ReleaseStore, RunningAgent, Signed, SyncRequest, SyncResponse,
};
use glass_policy::Policy;
use glass_policy::store::PolicyStore;
use glass_runtime::Runtime;
use glass_types::Timestamp;

use crate::OrgData;
use crate::config::{Config, ManagedConfig};

/// Backoff after a failed sync: 2 s, doubling, to 5 minutes.
const FIRST_RETRY: Duration = Duration::from_secs(2);
const LAST_RETRY: Duration = Duration::from_secs(300);
/// How often a device reports when nothing changed.
const REPORT_EVERY: Duration = Duration::from_secs(60);

/// The device's managed state: what it has in force and how to reach the
/// console.
pub struct Managed {
    config: Config,
    managed: ManagedConfig,
    device: String,
    releases: ReleaseStore,
    policies: PolicyStore,
    runtime: Arc<Runtime>,
    org: OrgData,
    /// Versions in force, from releases this device verified.
    policy_version: Option<u64>,
    catalog_version: Option<u64>,
    /// When a bundle the catalog lists was last fetched again (GA-13).
    org_apps_retried: Option<Instant>,
    directory_version: Option<u64>,
    chat_version: Option<u64>,
    /// The model credits version the console last told this computer of
    /// (ADR 0039): only that it keeps no key.
    credits_version: Option<u64>,
    /// Why the last release was refused or the last sync failed.
    last_error: Option<String>,
    /// Since when the console has been out of reach.
    offline_since: Option<Timestamp>,
    /// Releases IT published whose content this computer refused: the
    /// next syncs ask past them, until IT publishes another (ADR 0051).
    skip: HashMap<DocKind, u64>,
    /// A policy release kept on disk but not in force yet (its file
    /// couldn't be written): a release no newer is refused, and it goes in
    /// force from disk before the next sync.
    kept_policy: Option<u64>,
    /// A release was refused, or couldn't be kept: the loop waits before
    /// it syncs again.
    backoff: bool,
}

/// Why a release didn't go in force (ADR 0051).
enum Refused {
    /// A release IT published, signed with a key this computer trusts and
    /// newer than what it has, whose content it can't use: asked past
    /// until IT publishes another.
    Content(String),
    /// Anything else: forged, replayed or signed with a key not trusted
    /// yet, or this computer failing to keep or write it. Asked for again
    /// after a pause; none of them moves what the device asks for, so
    /// nobody on the way to the console can make it skip a release.
    Retry(String),
}

impl Refused {
    fn why(&self) -> &str {
        match self {
            Self::Content(why) | Self::Retry(why) => why,
        }
    }
}

/// A release that didn't open: only one whose signature checked out but
/// whose payload doesn't parse is IT's to fix.
fn unopened(e: Refusal) -> Refused {
    match e {
        Refusal::Payload(_) => Refused::Content(e.to_string()),
        other => Refused::Retry(other.to_string()),
    }
}

/// This computer couldn't keep or write a release: asked for again later.
fn local(e: impl std::fmt::Display) -> Refused {
    Refused::Retry(e.to_string())
}

impl Managed {
    /// Picks up what this device applied before, from its own disk: the
    /// catalog and the directory go back in force; a policy release whose
    /// text isn't the one in force (the file was changed, or didn't
    /// validate) is put back.
    pub fn start(config: &Config, runtime: Arc<Runtime>, org: OrgData) -> Option<Self> {
        let managed = config.managed.clone()?;
        let device = managed.device.clone().unwrap_or_else(hostname);
        let mut this = Self {
            config: config.clone(),
            device,
            releases: ReleaseStore::new(config.state_dir.join("managed")),
            policies: PolicyStore::new(config.state_dir.join("policy")),
            managed,
            runtime,
            org,
            policy_version: None,
            catalog_version: None,
            org_apps_retried: None,
            directory_version: None,
            chat_version: None,
            credits_version: None,
            last_error: None,
            offline_since: None,
            skip: HashMap::new(),
            kept_policy: None,
            backoff: false,
        };
        match this.load(DocKind::Catalog) {
            Some(signed) => {
                if let Err(e) = this.apply_catalog(&signed, true) {
                    this.refused(DocKind::Catalog, &signed, &e);
                }
            }
            None => tracing::info!("managed: no catalog from the console yet"),
        }
        match this.load(DocKind::Directory) {
            Some(signed) => {
                if let Err(e) = this.apply_directory(&signed, true) {
                    this.refused(DocKind::Directory, &signed, &e);
                }
            }
            None => tracing::info!("managed: no directory from the console yet"),
        }
        if let Some(signed) = this.load(DocKind::Chat)
            && let Err(e) = this.apply_chat(&signed, true)
        {
            this.refused(DocKind::Chat, &signed, &e);
        }
        if let Some(signed) = this.load(DocKind::Policy) {
            let in_force = std::fs::read_to_string(&this.config.policy).unwrap_or_default();
            match signed.open::<PolicyRelease>(
                DocKind::Policy,
                &this.managed.org,
                this.keys(),
                None,
            ) {
                Ok(release) if release.policy == in_force => {
                    this.policy_version = Some(signed.version);
                    this.runtime.set_policy_source(this.source());
                }
                Ok(_) => {
                    tracing::warn!(
                        "managed: the policy file isn't the one IT published; putting back version {}",
                        signed.version
                    );
                    if let Err(e) = this.apply_policy(&signed, true) {
                        // Kept and verified, but not in force: it still
                        // holds back anything older (DV-10), and goes in
                        // from disk before the next sync.
                        this.refused(DocKind::Policy, &signed, &e);
                        if matches!(e, Refused::Retry(_)) {
                            this.kept_policy = Some(signed.version);
                        }
                    }
                }
                Err(e) => this.refused(DocKind::Policy, &signed, &unopened(e)),
            }
        }
        Some(this)
    }

    fn keys(&self) -> &[PublicKey] {
        &self.managed.org_keys
    }

    /// A release kept on disk, checked again; `None` when there is none or
    /// it can't be trusted (logged).
    fn load(&self, kind: DocKind) -> Option<Signed> {
        match self.releases.load(kind, &self.managed.org, self.keys()) {
            Ok(signed) => signed,
            Err(e) => {
                tracing::warn!("managed: the {kind} kept on this device can't be trusted: {e}");
                None
            }
        }
    }

    /// What `glassctl status` says about the policy's source.
    fn source(&self) -> String {
        let version = self
            .policy_version
            .map_or_else(|| "policy file".to_owned(), |v| format!("managed v{v}"));
        match &self.offline_since {
            Some(since) => format!(
                "{version} from {} (console unreachable since {since}; keeping what's in force)",
                self.managed.url
            ),
            None => format!("{version} from {}", self.managed.url),
        }
    }

    /// Syncs until glassd stops: long polls while the console answers,
    /// backs off while it doesn't, reports how the device is doing.
    pub fn run(mut self) {
        let token = match read_token(&self.managed.token_file) {
            Ok(token) => token,
            Err(e) => {
                tracing::error!("managed: {e}; this device won't sync until it is fixed");
                self.runtime
                    .set_policy_source(format!("{} ({e})", self.source()));
                return;
            }
        };
        // The computer's own key signs every request but registering it
        // (ADR 0030 slice P).
        let key = match device::kept(&self.config.state_dir.join(DEVICE_KEY), &self.device) {
            Ok(key) => Arc::new(key),
            Err(e) => {
                tracing::error!("managed: {e}; this device won't sync until it is fixed");
                self.runtime
                    .set_policy_source(format!("{} ({e})", self.source()));
                return;
            }
        };
        let console = |token: &str| {
            ConsoleClient::new(&self.managed.url, token, self.managed.allow_insecure_http)
                .map(|c| c.signed_by(Arc::clone(&key)))
        };
        let client = match console(&token) {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("managed: {e}");
                self.runtime
                    .set_policy_source(format!("{} ({e})", self.source()));
                return;
            }
        };
        // Chat long-polls on a connection of its own (ADR 0030).
        match console(&token) {
            Ok(chat) => {
                self.org
                    .chat
                    .connect(&self.managed.org, &self.device, Arc::new(chat));
                let handle = self.org.chat.clone();
                std::thread::spawn(move || sync_chat(&handle));
            }
            Err(e) => tracing::error!("managed: chat: {e}"),
        }
        // Model calls on the org's credits (ADR 0039) go to the console too,
        // on a client of their own: a long answer mustn't hold up the rest.
        match console(&token).map(ConsoleClient::for_model_calls) {
            Ok(models) => self.org.credits.connect(Arc::new(models), &self.device),
            Err(e) => tracing::error!("managed: model credits: {e}"),
        }
        // Meetings (CF-21) go to the same console, on a client of their
        // own: a join mustn't wait behind chat's long poll.
        match console(&token) {
            Ok(meet) => self.org.meet.connect(Arc::new(meet)),
            Err(e) => tracing::error!("managed: meetings: {e}"),
        }
        // Sharing apps, and the approved ones' bundles (ADR 0032, R2).
        match console(&token) {
            Ok(share) => {
                let share: Arc<dyn glass_managed::studio::ShareLink> = Arc::new(share);
                self.org
                    .studio
                    .set_share(Arc::clone(&share), self.device.clone());
                self.org.org_apps.connect(share);
                self.fetch_org_apps(false);
            }
            Err(e) => tracing::error!("managed: sharing apps: {e}"),
        }
        let mut retry = FIRST_RETRY;
        let mut reported: Option<Instant> = None;
        let mut registered = false;
        loop {
            // Registering the key comes first, and again whenever the
            // console says it doesn't know this computer.
            if !registered {
                match client.register_device() {
                    Ok(()) => {
                        tracing::info!(device = %self.device, "managed: this computer's key is registered");
                        registered = true;
                    }
                    Err(e) => {
                        self.offline(&e);
                        std::thread::sleep(retry);
                        retry = (retry * 2).min(LAST_RETRY);
                        continue;
                    }
                }
            }
            // A policy release kept but not in force goes in from disk first.
            if self.kept_policy.is_some() && !self.retry_kept() {
                std::thread::sleep(retry);
                retry = (retry * 2).min(LAST_RETRY);
                continue;
            }
            let request = self.request();
            match client.sync(&request) {
                Ok(response) => {
                    let changed = self.handle(response);
                    // A refusal waits before the next sync: the console
                    // answers at once while it has something newer.
                    if std::mem::take(&mut self.backoff) {
                        std::thread::sleep(retry);
                        retry = (retry * 2).min(LAST_RETRY);
                    } else {
                        retry = FIRST_RETRY;
                    }
                    if changed || reported.is_none_or(|at| at.elapsed() >= REPORT_EVERY) {
                        match client.report(&self.report()) {
                            Ok(()) => reported = Some(Instant::now()),
                            Err(e) => tracing::warn!("managed: report: {e}"),
                        }
                    }
                }
                Err(e) => {
                    registered &= !e.contains("isn't registered");
                    self.offline(&e);
                    std::thread::sleep(retry);
                    retry = (retry * 2).min(LAST_RETRY);
                }
            }
        }
    }

    /// What the next sync asks for: anything newer than what's in force,
    /// than a policy release kept but not in force yet, and than a release
    /// whose content this computer refused (ADR 0051).
    fn request(&self) -> SyncRequest {
        let past = |kind: DocKind, have: Option<u64>| have.max(self.skip.get(&kind).copied());
        SyncRequest {
            device: self.device.clone(),
            policy: past(DocKind::Policy, self.policy_held()),
            catalog: past(DocKind::Catalog, self.catalog_version),
            directory: past(DocKind::Directory, self.directory_version),
            chat: past(DocKind::Chat, self.chat_version),
            credits: self.credits_version,
            wait_secs: MAX_WAIT_SECS,
        }
    }

    /// The newest policy release this computer holds: in force, or kept
    /// and not in force yet. Nothing no newer is applied over it (DV-10).
    fn policy_held(&self) -> Option<u64> {
        self.policy_version.max(self.kept_policy)
    }

    #[cfg(test)]
    fn backing_off(&self) -> bool {
        self.backoff
    }

    /// Puts the policy release kept on disk in force, when it is newer
    /// than the one in force. Whether there's nothing left to put in.
    fn retry_kept(&mut self) -> bool {
        let Some(signed) = self
            .load(DocKind::Policy)
            .filter(|s| Some(s.version) > self.policy_version)
        else {
            self.kept_policy = None;
            return true;
        };
        match self.apply_policy(&signed, true) {
            Ok(()) => {
                self.kept_policy = None;
                self.runtime.set_policy_source(self.source());
                true
            }
            Err(e) => {
                self.refused(DocKind::Policy, &signed, &e);
                match e {
                    Refused::Retry(_) => false,
                    Refused::Content(_) => {
                        self.kept_policy = None;
                        true
                    }
                }
            }
        }
    }

    /// Applies what the console sent. Returns whether anything changed.
    pub fn handle(&mut self, response: SyncResponse) -> bool {
        if self.offline_since.take().is_some() {
            tracing::info!("managed: the console is reachable again");
            self.last_error = None;
        }
        let mut changed = false;
        if let Some(credits) = response.credits {
            match self.apply_credits(&credits) {
                Ok(()) => changed = true,
                Err(e) => tracing::warn!("managed: model credits key: {e}"),
            }
        }
        if let Some(signed) = response.policy {
            match self.apply_policy(&signed, false) {
                Ok(()) => changed = true,
                Err(e) => self.refused(DocKind::Policy, &signed, &e),
            }
        }
        if let Some(signed) = response.catalog {
            match self.apply_catalog(&signed, false) {
                Ok(()) => {
                    changed = true;
                    self.fetch_org_apps(false);
                }
                Err(e) => self.refused(DocKind::Catalog, &signed, &e),
            }
        } else {
            self.retry_org_apps();
        }
        if let Some(signed) = response.directory {
            match self.apply_directory(&signed, false) {
                Ok(()) => changed = true,
                Err(e) => self.refused(DocKind::Directory, &signed, &e),
            }
        }
        if let Some(signed) = response.chat {
            match self.apply_chat(&signed, false) {
                Ok(()) => changed = true,
                Err(e) => self.refused(DocKind::Chat, &signed, &e),
            }
        }
        self.runtime.set_policy_source(self.source());
        changed
    }

    /// The console couldn't be reached: nothing changes.
    pub fn offline(&mut self, why: &str) {
        // The console answering no to this computer (ADR 0030 slice P) is
        // not the console being out of reach.
        let refused = why.contains("removed from the organization")
            || why.contains("isn't registered")
            || why.contains("enrolled with another key");
        let what = if refused {
            "the console refused this computer"
        } else {
            "the console can't be reached"
        };
        if self.offline_since.is_none() {
            tracing::warn!("managed: {what} ({why}); keeping what's in force");
            self.offline_since = Some(Timestamp::now());
        }
        self.last_error = Some(format!("{what}: {why}"));
        self.runtime.set_policy_source(self.source());
    }

    /// Keeps the org's model credits key root only, or forgets it when the
    /// org has none now, and rebuilds the gateway with it (ADR 0039). The
    /// key is never in a log or the audit record.
    /// Calls on the org's model credits go through its console, which
    /// keeps the org's key (ADR 0039): a computer keeps none. One it was
    /// sent before goes, on the record; a key sent now is never kept.
    fn apply_credits(&mut self, credits: &ModelCredits) -> Result<(), String> {
        let path = crate::credits_path(&self.config);
        let removed = match std::fs::remove_file(&path) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        self.credits_version = Some(credits.version);
        if removed || credits.api_key.is_some() {
            let mut event = AuditEvent::new(
                EventKind::CreditsKeyApplied,
                "The org's model credits key is no longer kept on this computer: model calls on credits go through the admin console",
            );
            event.detail = serde_json::json!({
                "console": self.managed.url,
                "version": credits.version,
                "removed": removed,
                "sent_and_refused": credits.api_key.is_some(),
            });
            self.runtime.append_audit(event);
            tracing::info!(
                version = credits.version,
                "managed: the model credits key is gone from this computer"
            );
        }
        Ok(())
    }

    /// Verifies, validates whole, writes and swaps in a policy release.
    /// `again` is a release this device applied before, from its disk.
    fn apply_policy(&mut self, signed: &Signed, again: bool) -> Result<(), Refused> {
        let have = if again { None } else { self.policy_held() };
        let release: PolicyRelease = signed
            .open(DocKind::Policy, &self.managed.org, self.keys(), have)
            .map_err(unopened)?;
        let policy =
            Policy::from_toml(&release.policy).map_err(|e| Refused::Content(e.to_string()))?;
        let registry = crate::registry(&self.config, &policy, &self.org)
            .map_err(|e| Refused::Content(format!("{e:#}")))?;
        let warnings = policy.lint(&registry.lint_info());
        for warning in &warnings {
            tracing::warn!("managed policy v{}: {warning}", signed.version);
        }
        // The signed release first: if keeping it fails (a full disk), the
        // policy file and what's in force stay as they were, as the refusal
        // the caller audits says. If applying fails after, the kept release
        // is put back at the next start or poll.
        if !again {
            self.releases.save(signed).map_err(local)?;
        }
        if let Err(e) = self.policies.apply(&self.config.policy, &release.policy) {
            // Kept, not in force: it goes in from disk later (ADR 0051).
            self.kept_policy = self.kept_policy.max(Some(signed.version));
            return Err(local(e));
        }
        let _ = self.policies.remember(&glass_policy::store::Loaded {
            policy: policy.clone(),
            text: release.policy.clone(),
            origin: glass_policy::store::Origin::File(self.config.policy.clone()),
        });
        self.policy_version = Some(signed.version);
        if self.kept_policy <= self.policy_version {
            self.kept_policy = None;
        }
        // Its model providers, with the keys this computer has (ADR 0039).
        let models = crate::gateway(&self.config, &policy, &self.org);
        self.runtime.apply_policy(policy, registry, self.source());
        self.runtime.set_gateway(models.gateway);
        let mut event = AuditEvent::new(
            EventKind::PolicyLoaded,
            format!(
                "Policy v{} from the admin console is in force",
                signed.version
            ),
        );
        event.detail = serde_json::json!({
            "source": "managed",
            "console": self.managed.url,
            "version": signed.version,
            "key": signed.key,
            "payload_sha256": signed.payload_sha256,
            "note": release.note,
            "rolls_back_to": release.rolls_back_to,
            "warnings": warnings,
            "from_disk": again,
        });
        self.runtime.append_audit(event);
        tracing::info!(version = signed.version, "managed: policy applied");
        Ok(())
    }

    /// Fetches the bundles of the org apps the catalog lists (R2): each
    /// kept only if it matches the signed digest, and on the record. Tried
    /// `again`, a refusal already on the record is only logged.
    fn fetch_org_apps(&self, again: bool) {
        for fetched in self.org.org_apps.fetch() {
            let event = match fetched {
                crate::org_apps::Fetched::Installed { app, version } => {
                    tracing::info!(%app, %version, "managed: org app installed");
                    let mut event = AuditEvent::new(
                        EventKind::CatalogLoaded,
                        format!("App {app} {version} from the org's catalog installed"),
                    );
                    event.detail = serde_json::json!({ "app": app, "version": version });
                    event
                }
                crate::org_apps::Fetched::Refused { app, why } => {
                    tracing::warn!(%app, "managed: org app refused: {why}");
                    if again {
                        continue;
                    }
                    let mut event = AuditEvent::new(
                        EventKind::ManagedRefused,
                        format!("App {app} from the org's catalog refused: {why}"),
                    );
                    event.outcome = Some(Outcome::Blocked);
                    event.detail = serde_json::json!({ "app": app, "reason": why });
                    event
                }
            };
            self.runtime.append_audit(event);
        }
    }

    /// Fetches again, once a minute, a bundle the catalog in force lists
    /// and this computer lacks: one whose fetch failed after its sync (the
    /// network dropped) is otherwise never fetched until the next catalog
    /// (GA-13, specs/tla/StoreShare.tla).
    fn retry_org_apps(&mut self) {
        let listed = self.org.catalog.current().map_or(0, |c| {
            c.release.items.iter().filter(|i| i.app.is_some()).count()
        });
        let kept = glass_runtime::OrgApps::apps(&self.org.org_apps).len();
        if kept >= listed
            || self
                .org_apps_retried
                .is_some_and(|at| at.elapsed() < REPORT_EVERY)
        {
            return;
        }
        self.org_apps_retried = Some(Instant::now());
        self.fetch_org_apps(true);
    }

    /// Verifies, checks and swaps in a catalog release.
    fn apply_catalog(&mut self, signed: &Signed, again: bool) -> Result<(), Refused> {
        let have = if again { None } else { self.catalog_version };
        let release: CatalogRelease = signed
            .open(DocKind::Catalog, &self.managed.org, self.keys(), have)
            .map_err(unopened)?;
        release.check().map_err(Refused::Content)?;
        if !again {
            self.releases.save(signed).map_err(local)?;
        }
        let items = release.items.len();
        self.org.catalog.set(Applied {
            version: signed.version,
            release,
        });
        self.catalog_version = Some(signed.version);
        let mut event = AuditEvent::new(
            EventKind::CatalogLoaded,
            format!(
                "Catalog v{} from the admin console is in force ({items} items)",
                signed.version
            ),
        );
        event.detail = serde_json::json!({
            "console": self.managed.url,
            "version": signed.version,
            "key": signed.key,
            "payload_sha256": signed.payload_sha256,
            "items": items,
            "from_disk": again,
        });
        self.runtime.append_audit(event);
        tracing::info!(version = signed.version, items, "managed: catalog applied");
        Ok(())
    }

    /// Verifies, checks and swaps in a directory release (AD-1). Its size
    /// is checked before it's read: every change sends the whole list.
    fn apply_directory(&mut self, signed: &Signed, again: bool) -> Result<(), Refused> {
        // Checked before it's read, so before its signature: asked for
        // again later, as anything not shown to be IT's.
        if signed.payload.len() > directory::MAX_PAYLOAD_BYTES {
            return Err(Refused::Retry(format!(
                "{} bytes is more than a directory release may be ({})",
                signed.payload.len(),
                directory::MAX_PAYLOAD_BYTES
            )));
        }
        let have = if again { None } else { self.directory_version };
        let release: DirectoryRelease = signed
            .open(DocKind::Directory, &self.managed.org, self.keys(), have)
            .map_err(unopened)?;
        release.check().map_err(Refused::Content)?;
        if !again {
            self.releases.save(signed).map_err(local)?;
        }
        let people = release.people.len();
        self.org.directory.set(directory::Applied {
            version: signed.version,
            release,
        });
        self.directory_version = Some(signed.version);
        let mut event = AuditEvent::new(
            EventKind::DirectoryLoaded,
            format!(
                "Directory v{} from the admin console is in force ({people} people)",
                signed.version
            ),
        );
        event.detail = serde_json::json!({
            "console": self.managed.url,
            "version": signed.version,
            "key": signed.key,
            "payload_sha256": signed.payload_sha256,
            "people": people,
            "from_disk": again,
        });
        self.runtime.append_audit(event);
        tracing::info!(
            version = signed.version,
            people,
            "managed: directory applied"
        );
        Ok(())
    }

    /// Verifies and applies IT's Chat release: the compliance key every
    /// epoch this computer makes is sealed to (ADR 0030, decision 4).
    fn apply_chat(&mut self, signed: &Signed, again: bool) -> Result<(), Refused> {
        let have = if again { None } else { self.chat_version };
        let release: ChatRelease = signed
            .open(DocKind::Chat, &self.managed.org, self.keys(), have)
            .map_err(unopened)?;
        let key = release
            .key()
            .map_err(|e| Refused::Content(format!("the compliance key: {e}")))?;
        if ChatRelease::new(&key) != release {
            return Err(Refused::Content(
                "the compliance key's fingerprint doesn't match it".into(),
            ));
        }
        if !again {
            self.releases.save(signed).map_err(local)?;
        }
        self.org
            .chat
            .set_compliance(&release.compliance, &release.fingerprint)
            .map_err(local)?;
        self.chat_version = Some(signed.version);
        let mut event = AuditEvent::new(
            EventKind::ChatLoaded,
            format!(
                "Chat v{} from the admin console is in force (compliance key {})",
                signed.version, release.fingerprint
            ),
        );
        event.detail = serde_json::json!({
            "console": self.managed.url,
            "version": signed.version,
            "key": signed.key,
            "fingerprint": release.fingerprint,
            "from_disk": again,
        });
        self.runtime.append_audit(event);
        tracing::info!(version = signed.version, "managed: chat release applied");
        Ok(())
    }

    /// A release was refused: audited and reported; nothing changes but
    /// what the next syncs ask for, or how soon (ADR 0051).
    fn refused(&mut self, kind: DocKind, signed: &Signed, refused: &Refused) {
        match refused {
            Refused::Content(_) => {
                let past = self.skip.entry(kind).or_default();
                *past = (*past).max(signed.version);
            }
            Refused::Retry(_) => self.backoff = true,
        }
        let why = refused.why();
        tracing::warn!(
            "managed: refused {kind} v{} from {}: {why}; keeping what's in force",
            signed.version,
            self.managed.url
        );
        let mut event = AuditEvent::new(
            EventKind::ManagedRefused,
            format!("Refused {kind} v{} from the admin console", signed.version),
        );
        event.outcome = Some(Outcome::Blocked);
        event.detail = serde_json::json!({
            "kind": kind,
            "version": signed.version,
            "key": signed.key,
            "reason": why,
        });
        self.runtime.append_audit(event);
        self.last_error = Some(format!("refused {kind} v{}: {why}", signed.version));
    }

    /// What the fleet view gets (AD-4): counts, ids and login names only.
    pub fn report(&self) -> DeviceReport {
        let status = self.runtime.status();
        DeviceReport {
            device: self.device.clone(),
            name: hostname(),
            os_version: format!("Glasshouse {}", env!("CARGO_PKG_VERSION")),
            users: self.runtime.users_seen(),
            policy_version: self.policy_version,
            catalog_version: self.catalog_version,
            directory_version: self.directory_version,
            agents: self
                .runtime
                .running_agents()
                .into_iter()
                .map(|a| RunningAgent {
                    agent: a.agent.to_string(),
                    user: a.user,
                    waiting: a.waiting,
                })
                .collect(),
            approvals_pending: u32::try_from(status.approvals_pending).unwrap_or(u32::MAX),
            blocked_24h: blocked_last_day(&self.config.audit_log()),
            last_error: self.last_error.clone(),
            made_here: self
                .org
                .studio
                .inventory()
                .into_iter()
                .map(|(record, manifest)| glass_managed::MadeApp {
                    app: record.app.to_string(),
                    name: record.name,
                    owner: record.owner,
                    version: record.version,
                    based_on: record.based_on.map(|b| format!("{}@{}", b.id, b.version)),
                    actions: manifest.actions.iter().map(ToString::to_string).collect(),
                    beyond_base: record.beyond_base.iter().map(ToString::to_string).collect(),
                    saved_at: record.saved_at,
                })
                .collect(),
            reported_at: Timestamp::now(),
        }
    }
}

/// Keeps this computer's chat in step with the console until glassd stops:
/// long polls while there is someone to sync for, backs off while the
/// console can't be reached (ADR 0030).
fn sync_chat(chat: &ChatHandle) {
    // The first person to use chat here starts the long poll at once: their
    // first conversation's answer mustn't wait out a sleep (CF-91's 1 s).
    let (someone, first) = std::sync::mpsc::channel();
    chat.on_notice(move |notice| {
        if matches!(notice, ChatNotice::KeyMade { .. }) {
            let _ = someone.send(());
        }
    });
    let mut retry = FIRST_RETRY;
    loop {
        if chat.logins().is_empty() {
            // Nobody here has used chat yet.
            let _ = first.recv_timeout(FIRST_RETRY);
            continue;
        }
        match chat.sync_once(MAX_WAIT_SECS) {
            Ok(_) => retry = FIRST_RETRY,
            Err(e) => {
                tracing::warn!("chat: sync: {e}");
                std::thread::sleep(retry);
                retry = (retry * 2).min(LAST_RETRY);
            }
        }
    }
}

/// The audit record for a chat notice (AU-1): key ids, conversation ids and
/// reasons, never a message.
pub(crate) fn chat_event(notice: &ChatNotice) -> Option<AuditEvent> {
    match notice {
        ChatNotice::KeyMade { login, key } => {
            let mut event = AuditEvent::new(
                EventKind::ChatKeyMade,
                format!("Chat key {key} made for {login} on this computer"),
            );
            event.user = Some(login.clone());
            event.detail = serde_json::json!({ "key": key });
            Some(event)
        }
        ChatNotice::Refused {
            login,
            conversation,
            reason,
        } => {
            let mut event = AuditEvent::new(
                EventKind::ChatRefused,
                format!("Kept a chat record out: {reason}"),
            );
            event.outcome = Some(Outcome::Blocked);
            event.user = (!login.is_empty()).then(|| login.clone());
            event.detail = serde_json::json!({ "conversation": conversation, "reason": reason });
            Some(event)
        }
        ChatNotice::Changed { .. } => None,
    }
}

/// Actions policy blocked in the last 24 hours, from the audit log: read
/// from its end back, since this runs every minute and the log only grows
/// (ADR 0051).
fn blocked_last_day(audit: &Path) -> u32 {
    let since = Timestamp::now().unix_seconds() - 24 * 60 * 60;
    glass_audit::read_since(audit, since, |r| {
        r.event.kind == EventKind::Action && r.event.outcome == Some(Outcome::Blocked)
    })
    .map_or(0, |records| {
        u32::try_from(records.len()).unwrap_or(u32::MAX)
    })
}

/// This machine's name, as IT sees it in the fleet.
fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unnamed".into())
}

/// The enrollment token, from a file nobody but glassd's user can read.
/// The computer's key, in glassd's state directory, root only.
const DEVICE_KEY: &str = "device.key";

fn read_token(path: &Path) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("the enrollment token {}: {e}", path.display()))?;
    if meta.mode() & 0o077 != 0 {
        return Err(format!(
            "the enrollment token {} can be read by others; make it mode 0600",
            path.display()
        ));
    }
    let token = std::fs::read_to_string(path)
        .map_err(|e| format!("the enrollment token {}: {e}", path.display()))?;
    Ok(token.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use glass_audit::{AuditLog, Durability};
    use glass_gateway::Gateway;
    use glass_managed::{CatalogItem, Declared, InstallMode, ItemKind, OrgKey};
    use glass_platform::sandbox::HostSandbox;
    use glass_platform::snapshot::CopySnapshotter;
    use glass_runtime::{RuntimeConfig, RuntimeParts};

    use super::*;

    struct Device {
        dir: tempfile::TempDir,
        config: Config,
        runtime: Arc<Runtime>,
        org: OrgData,
    }

    impl Device {
        fn policy_text(&self) -> String {
            std::fs::read_to_string(&self.config.policy).unwrap()
        }

        fn audited(&self, kind: EventKind) -> usize {
            glass_audit::read(&self.dir.path().join("audit.log"), |r| r.event.kind == kind)
                .unwrap()
                .len()
        }

        fn managed(&self) -> Managed {
            Managed::start(&self.config, Arc::clone(&self.runtime), self.org.clone()).unwrap()
        }
    }

    /// A managed device on the development backends, enrolled in `acme`
    /// with `key`'s public half.
    fn device(key: &OrgKey) -> Device {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display();
        let token = dir.path().join("token");
        std::fs::write(&token, "enroll-acme\n").unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let path = dir.path().join("glassd.toml");
        std::fs::write(
            &path,
            format!(
                "state_dir = \"{d}/state\"\npolicy = \"{d}/policy.toml\"\n\
                 [sandbox]\nbackend = \"host-insecure\"\nallow_insecure = true\n\
                 [managed]\nurl = \"http://127.0.0.1:9\"\norg = \"acme\"\n\
                 org_keys = [\"{}\"]\ntoken_file = \"{d}/token\"\ndevice = \"dev-1\"\n\
                 allow_insecure_http = true\n",
                key.public()
            ),
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        std::fs::write(&config.policy, glass_policy::DEFAULT_POLICY).unwrap();
        let policy = Policy::from_toml(glass_policy::DEFAULT_POLICY).unwrap();
        let org = OrgData::new(&config);
        let registry = crate::registry(&config, &policy, &org).unwrap();
        let runtime = Runtime::new(RuntimeParts {
            config: RuntimeConfig {
                state_dir: config.state_dir.clone(),
                catalog: Vec::new(),
                agent_dirs: Vec::new(),
                max_live_tasks_per_user: 4,
                harness: None,
                attended_dir: dir.path().join("attended"),
                apps: Vec::new(),
                console_url: Some("http://127.0.0.1:9".into()),
                dev_home: None,
                org_catalog: Arc::new(crate::OrgCatalog(org.catalog.clone())),
                personal: Arc::new(org.studio.clone()),
                org_apps: Arc::new(org.org_apps.clone()),
                hardware: Default::default(),
                update: Default::default(),
            },
            policy,
            policy_source: "policy file".into(),
            registry,
            sandbox: Arc::new(HostSandbox),
            snapshots: Arc::new(CopySnapshotter::new(dir.path().join("snapshots"))),
            audit: Arc::new(
                AuditLog::open(dir.path().join("audit.log"), Durability::Buffered).unwrap(),
            ),
            gateway: Arc::new(Gateway::new()),
            settings: org.settings.clone(),
        });
        Device {
            dir,
            config,
            runtime,
            org,
        }
    }

    fn policy_release(key: &OrgKey, version: u64, policy: &str) -> Signed {
        let payload = serde_json::to_string(&PolicyRelease {
            policy: policy.into(),
            note: format!("v{version}"),
            rolls_back_to: None,
        })
        .unwrap();
        key.sign(DocKind::Policy, "acme", version, payload)
    }

    fn with_tier(tier: &str) -> String {
        format!(
            "{}\n[[group]]\nname = \"ap-ops\"\ntiers = {{ \"mail.send\" = \"{tier}\" }}\n",
            glass_policy::DEFAULT_POLICY
        )
    }

    fn catalog_release(key: &OrgKey, version: u64) -> Signed {
        let release = CatalogRelease {
            items: vec![CatalogItem {
                id: "vendor-disputes".parse().unwrap(),
                kind: ItemKind::App,
                name: "Vendor Disputes".into(),
                version: "1.0.0".into(),
                publisher: "Acme IT".into(),
                publisher_verified: true,
                internal: true,
                summary: "Track vendor disputes".into(),
                keywords: Vec::new(),
                declares: Declared::default(),
                groups: [("everyone".to_owned(), InstallMode::Available)].into(),
                pinned: false,
                provenance: Default::default(),
                app: None,
            }],
        };
        key.sign(
            DocKind::Catalog,
            "acme",
            version,
            serde_json::to_string(&release).unwrap(),
        )
    }

    fn sync(policy: Option<Signed>, catalog: Option<Signed>) -> SyncResponse {
        SyncResponse {
            ring: glass_managed::Ring::Pilot,
            policy,
            catalog,
            directory: None,
            chat: None,
            credits: None,
        }
    }

    fn directory_release(key: &OrgKey, version: u64, people: &[(&str, &str)]) -> Signed {
        let release = DirectoryRelease {
            people: people
                .iter()
                .map(|(login, email)| glass_managed::Person {
                    login: (*login).into(),
                    name: login.to_uppercase(),
                    email: (*email).into(),
                    title: String::new(),
                    department: String::new(),
                })
                .collect(),
        };
        key.sign(
            DocKind::Directory,
            "acme",
            version,
            serde_json::to_string(&release).unwrap(),
        )
    }

    fn directory_sync(signed: Signed) -> SyncResponse {
        SyncResponse {
            directory: Some(signed),
            ..sync(None, None)
        }
    }

    #[tokio::test]
    async fn ad3_a_signed_policy_is_verified_validated_written_and_swapped_in() {
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let mut m = d.managed();
        assert!(m.handle(sync(Some(policy_release(&key, 1, &with_tier("ask"))), None)));
        assert_eq!(d.policy_text(), with_tier("ask"));
        assert!(d.runtime.status().policy.starts_with("managed v1 from"));
        assert_eq!(d.audited(EventKind::PolicyLoaded), 1);
        let report = m.report();
        assert_eq!(report.policy_version, Some(1));
        assert_eq!(
            report.os_version,
            format!("Glasshouse {}", env!("CARGO_PKG_VERSION"))
        );
        assert!(
            !report.os_version.to_lowercase().contains("freebsd"),
            "BR-1"
        );

        // The previous policy is kept for rollback (ADR 0021).
        let previous = std::fs::read_to_string(d.config.state_dir.join("policy/previous.toml"));
        assert_eq!(previous.unwrap(), glass_policy::DEFAULT_POLICY);
    }

    #[tokio::test]
    async fn dv1_refused_releases_change_nothing_and_are_audited() {
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let mut m = d.managed();
        assert!(m.handle(sync(Some(policy_release(&key, 2, &with_tier("ask"))), None)));
        let in_force = d.policy_text();

        // Replayed, from another key, and a policy that doesn't validate.
        let stranger = OrgKey::from_seed([8; 32]);
        for signed in [
            policy_release(&key, 2, &with_tier("never")),
            policy_release(&key, 1, &with_tier("never")),
            policy_release(&stranger, 3, &with_tier("never")),
            policy_release(&key, 3, "version = 1\nnonsense = true\n"),
        ] {
            assert!(!m.handle(sync(Some(signed), None)));
            assert_eq!(d.policy_text(), in_force);
        }
        let mut tampered = policy_release(&key, 4, &with_tier("never"));
        tampered.payload = tampered.payload.replace("never", "auto");
        assert!(!m.handle(sync(Some(tampered), None)));
        assert_eq!(d.policy_text(), in_force);
        assert_eq!(d.audited(EventKind::ManagedRefused), 5);
        assert!(d.runtime.status().policy.starts_with("managed v2"));
        assert!(m.report().last_error.unwrap().contains("refused policy"));
    }

    #[tokio::test]
    async fn dv1_a_proper_release_refused_is_asked_past_and_no_other_refusal_moves_the_ask() {
        // ADR 0051: a refused release was fetched and refused again on every
        // sync, at once, without end: the console answers at once while it
        // has anything newer than what the device names.
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let mut m = d.managed();
        assert!(m.handle(sync(Some(policy_release(&key, 1, &with_tier("ask"))), None)));
        assert!(!m.handle(sync(
            Some(policy_release(&key, 2, "version = 1\nnonsense = true\n")),
            None
        )));
        assert_eq!(m.request().policy, Some(2), "asked past what IT must fix");
        assert!(!m.backing_off());
        // A forged release, or a replay, never moves what it asks for: the
        // loop waits a little and asks again.
        let stranger = OrgKey::from_seed([8; 32]);
        assert!(!m.handle(sync(
            Some(policy_release(&stranger, 9, &with_tier("never"))),
            None
        )));
        assert_eq!(m.request().policy, Some(2));
        assert!(m.backing_off());
        // What's in force is still what the report says.
        assert_eq!(m.report().policy_version, Some(1));
        assert!(m.handle(sync(
            Some(policy_release(&key, 3, &with_tier("notify"))),
            None
        )));
        assert_eq!(m.request().policy, Some(3));
    }

    #[tokio::test]
    async fn dv10_a_kept_release_not_back_in_force_still_refuses_an_older_one() {
        // ADR 0051: version 3 was kept, then glassd stopped before its file
        // was written; at the next start writing it failed, the device
        // forgot which version it held, and a replayed version 1 went in.
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let mut m = d.managed();
        assert!(m.handle(sync(Some(policy_release(&key, 2, &with_tier("ask"))), None)));
        let v2 = d.policy_text();
        ReleaseStore::new(d.config.state_dir.join("managed"))
            .save(&policy_release(&key, 3, &with_tier("notify")))
            .unwrap();
        // The disk refuses the policy file at the next start.
        let aside = d.dir.path().join("policy.aside");
        std::fs::rename(&d.config.policy, &aside).unwrap();
        std::fs::create_dir(&d.config.policy).unwrap();
        let mut m = d.managed();
        std::fs::remove_dir(&d.config.policy).unwrap();
        std::fs::rename(&aside, &d.config.policy).unwrap();
        assert_eq!(m.request().policy, Some(3), "it holds what it kept");
        assert!(!m.handle(sync(
            Some(policy_release(&key, 1, &with_tier("never"))),
            None
        )));
        assert_eq!(d.policy_text(), v2, "no rollback");
        // And the kept one goes in force from disk once it can.
        assert!(m.retry_kept());
        assert_eq!(d.policy_text(), with_tier("notify"));
        assert_eq!(m.report().policy_version, Some(3));
    }

    #[tokio::test]
    async fn adr0021_offline_the_device_keeps_what_it_has_and_says_so() {
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let mut m = d.managed();
        assert!(m.handle(sync(Some(policy_release(&key, 1, &with_tier("ask"))), None)));
        m.offline("network error connecting to 127.0.0.1:9: ConnectionRefused");
        let status = d.runtime.status().policy;
        assert!(status.contains("managed v1"), "{status}");
        assert!(status.contains("console unreachable since"), "{status}");
        assert_eq!(d.policy_text(), with_tier("ask"));
        assert!(m.report().last_error.unwrap().contains("can't be reached"));

        // Back online: nothing newer, and the status says so.
        assert!(!m.handle(sync(None, None)));
        assert!(!d.runtime.status().policy.contains("unreachable"));
    }

    #[tokio::test]
    async fn ga2_the_catalog_is_applied_and_back_in_force_after_a_restart_offline() {
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let mut m = d.managed();
        assert!(d.org.catalog.current().is_none());
        assert!(m.handle(sync(None, Some(catalog_release(&key, 5)))));
        assert_eq!(d.org.catalog.current().unwrap().version, 5);
        assert_eq!(d.audited(EventKind::CatalogLoaded), 1);

        // A restart with no console: the catalog comes back from disk,
        // checked again.
        let fresh = OrgData::new(&d.config);
        let again = Managed::start(&d.config, Arc::clone(&d.runtime), fresh.clone()).unwrap();
        assert_eq!(fresh.catalog.current().unwrap().version, 5);
        assert_eq!(again.catalog_version, Some(5));
    }

    /// The console, whose first bundle fetch drops.
    #[derive(Debug)]
    struct Flaky(
        std::sync::atomic::AtomicUsize,
        glass_managed::studio::Bundle,
    );

    impl glass_managed::studio::ShareLink for Flaky {
        fn submit(
            &self,
            _: &glass_managed::studio::Submission,
        ) -> Result<glass_managed::studio::Submitted, String> {
            Err("no".into())
        }
        fn bundle(
            &self,
            _: &glass_managed::studio::BundleRequest,
        ) -> Result<glass_managed::studio::Bundle, String> {
            if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Err("the network dropped".into());
            }
            Ok(self.1.clone())
        }
    }

    /// GA-13 (specs/tla/StoreShare.tla): an approved app whose bundle
    /// fetch failed after the sync reaches the computer at a later sync,
    /// without waiting for IT's next catalog.
    #[tokio::test]
    async fn ga13_a_bundle_whose_fetch_failed_is_fetched_again() {
        use glass_runtime::OrgApps;
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let bundle = glass_managed::studio::Bundle {
            app: "mail-with-write".parse().unwrap(),
            version: "1".into(),
            files: [(
                "index.html".to_owned(),
                "<title>Mail, with Write</title>".to_owned(),
            )]
            .into(),
        };
        let manifest: glass_api::AppManifest = serde_json::from_value(serde_json::json!({
            "id": "mail-with-write", "name": "Mail, with Write", "version": "1",
            "actions": ["mail.list"], "labels": ["internal"],
        }))
        .unwrap();
        let mut item = serde_json::from_str::<CatalogRelease>(&catalog_release(&key, 1).payload)
            .unwrap()
            .items
            .remove(0);
        item.id = "mail-with-write".parse().unwrap();
        item.app = Some(glass_managed::studio::OrgApp {
            manifest,
            bundle_sha256: bundle.digest(),
            source_sha256: String::new(),
            made_by: "dana".into(),
            based_on: None,
        });
        let release = key.sign(
            DocKind::Catalog,
            "acme",
            2,
            serde_json::to_string(&CatalogRelease { items: vec![item] }).unwrap(),
        );
        d.org.org_apps.connect(Arc::new(Flaky(0.into(), bundle)));
        let mut m = d.managed();
        assert!(m.handle(sync(None, Some(release))));
        assert!(d.org.org_apps.apps().is_empty(), "the first fetch dropped");
        // The next sync brings no new catalog; the bundle is fetched again.
        m.handle(sync(None, None));
        assert_eq!(d.org.org_apps.apps().len(), 1);
        assert!(
            d.org
                .org_apps
                .files(&"mail-with-write".parse().unwrap())
                .is_some()
        );
    }

    #[test]
    fn adr0039_credits_go_through_the_console_and_no_key_is_kept_on_the_computer() {
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let policy = Policy::from_toml(&format!(
            "{}\n[[models.provider]]\nid = \"credits\"\nkind = \"credits\"\nname = \"Glasshouse model credits\"\n",
            glass_policy::DEFAULT_POLICY
        ))
        .unwrap();
        let path = crate::credits_path(&d.config);
        // A managed computer's gateway has the credits provider, on the
        // link to its console, whatever keys it holds.
        let providers = |policy: &Policy| {
            crate::gateway(&d.config, policy, &d.org)
                .gateway
                .providers()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert!(providers(&policy).contains(&"credits".to_owned()));

        // A key an earlier console sent is removed, on the record; one sent
        // now is never kept.
        crate::write_root_only(&path, b"[credits]\napi_key = \"sk-or-v1-old\"\n").unwrap();
        let mut m = d.managed();
        let response = |api_key: Option<&str>, version| SyncResponse {
            ring: glass_managed::Ring::Broad,
            policy: None,
            catalog: None,
            directory: None,
            chat: None,
            credits: Some(ModelCredits {
                version,
                api_key: api_key.map(str::to_owned),
            }),
        };
        assert!(m.handle(response(None, 2)));
        assert!(!path.exists());
        assert_eq!(d.audited(EventKind::CreditsKeyApplied), 1);
        assert!(m.handle(response(Some("sk-or-v1-sent-anyway"), 3)));
        assert!(!path.exists());
        let log = std::fs::read_to_string(d.dir.path().join("audit.log")).unwrap();
        assert!(!log.contains("sk-or-v1"), "no key on the record");
        assert_eq!(m.credits_version, Some(3));
    }

    #[tokio::test]
    async fn ad1_the_directory_is_applied_and_back_in_force_after_a_restart_offline() {
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let mut m = d.managed();
        assert!(d.org.directory.current().is_none());
        let people = [("dana", "dana@acme.example"), ("sam", "sam@acme.example")];
        assert!(m.handle(directory_sync(directory_release(&key, 2, &people))));
        assert_eq!(d.org.directory.current().unwrap().version, 2);
        assert_eq!(
            d.org.directory.person("sam").map(|p| p.email),
            Some("sam@acme.example".into())
        );
        assert_eq!(d.audited(EventKind::DirectoryLoaded), 1);
        assert_eq!(m.report().directory_version, Some(2));

        // A restart with no console: the directory comes back from disk.
        let fresh = OrgData::new(&d.config);
        let again = Managed::start(&d.config, Arc::clone(&d.runtime), fresh.clone()).unwrap();
        assert_eq!(fresh.directory.current().unwrap().version, 2);
        assert_eq!(again.directory_version, Some(2));
    }

    #[tokio::test]
    async fn ad1_a_directory_with_duplicates_or_too_large_is_refused_and_changes_nothing() {
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let mut m = d.managed();
        assert!(m.handle(directory_sync(directory_release(
            &key,
            1,
            &[("dana", "dana@acme.example")]
        ))));
        let twice = [("dana", "dana@acme.example"), ("sam", "DANA@acme.example")];
        assert!(!m.handle(directory_sync(directory_release(&key, 2, &twice))));
        let mut huge = directory_release(&key, 3, &[]);
        huge.payload = " ".repeat(glass_managed::directory::MAX_PAYLOAD_BYTES + 1);
        assert!(!m.handle(directory_sync(huge)));
        assert_eq!(d.org.directory.current().unwrap().version, 1);
        assert_eq!(d.audited(EventKind::ManagedRefused), 2);
    }

    #[tokio::test]
    async fn a_policy_file_changed_by_hand_is_put_back_from_the_signed_release() {
        let key = OrgKey::from_seed([7; 32]);
        let d = device(&key);
        let mut m = d.managed();
        assert!(m.handle(sync(Some(policy_release(&key, 3, &with_tier("ask"))), None)));
        std::fs::write(&d.config.policy, glass_policy::DEFAULT_POLICY).unwrap();
        let again = d.managed();
        assert_eq!(d.policy_text(), with_tier("ask"));
        assert_eq!(again.policy_version, Some(3));
    }

    #[test]
    fn adr0030_the_device_key_is_root_only() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(super::DEVICE_KEY);
        let key = glass_managed::device::kept(&path, "lab-07").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            0o600,
            "made for root alone"
        );
        // The same key comes back after a restart.
        let again = glass_managed::device::kept(&path, "lab-07").unwrap();
        assert_eq!(
            again.registration().unwrap().key,
            key.registration().unwrap().key
        );
        // One others could read isn't used, and isn't replaced either.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = glass_managed::device::kept(&path, "lab-07").unwrap_err();
        assert!(err.contains("read by others"), "{err}");
        assert!(!err.contains(&hex::encode(key.to_bytes().unwrap())));
    }
}
