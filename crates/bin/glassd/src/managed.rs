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

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glass_app_catalog::Applied;
use glass_audit::{AuditEvent, EventKind, Outcome};
use glass_first_party::{ChatHandle, ChatNotice};
use glass_gateway_providers::ConsoleClient;
use glass_managed::directory::{self, DirectoryRelease};
use glass_managed::{
    CatalogRelease, ChatRelease, DeviceReport, DocKind, MAX_WAIT_SECS, PolicyRelease, PublicKey,
    ReleaseStore, RunningAgent, Signed, SyncRequest, SyncResponse,
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
    directory_version: Option<u64>,
    chat_version: Option<u64>,
    /// Why the last release was refused or the last sync failed.
    last_error: Option<String>,
    /// Since when the console has been out of reach.
    offline_since: Option<Timestamp>,
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
            directory_version: None,
            chat_version: None,
            last_error: None,
            offline_since: None,
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
                        this.refused(DocKind::Policy, &signed, &e);
                    }
                }
                Err(e) => this.refused(DocKind::Policy, &signed, &e.to_string()),
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
        let client =
            match ConsoleClient::new(&self.managed.url, &token, self.managed.allow_insecure_http) {
                Ok(client) => client,
                Err(e) => {
                    tracing::error!("managed: {e}");
                    self.runtime
                        .set_policy_source(format!("{} ({e})", self.source()));
                    return;
                }
            };
        // Chat long-polls on a connection of its own (ADR 0030).
        match ConsoleClient::new(&self.managed.url, &token, self.managed.allow_insecure_http) {
            Ok(chat) => {
                self.org
                    .chat
                    .connect(&self.managed.org, &self.device, Arc::new(chat));
                let handle = self.org.chat.clone();
                std::thread::spawn(move || sync_chat(&handle));
            }
            Err(e) => tracing::error!("managed: chat: {e}"),
        }
        // Meetings (CF-21) go to the same console, on a client of their
        // own: a join mustn't wait behind chat's long poll.
        match ConsoleClient::new(&self.managed.url, &token, self.managed.allow_insecure_http) {
            Ok(meet) => self.org.meet.connect(Arc::new(meet)),
            Err(e) => tracing::error!("managed: meetings: {e}"),
        }
        // Sharing apps, and the approved ones' bundles (ADR 0032, R2).
        match ConsoleClient::new(&self.managed.url, &token, self.managed.allow_insecure_http) {
            Ok(share) => {
                let share: Arc<dyn glass_managed::studio::ShareLink> = Arc::new(share);
                self.org
                    .studio
                    .set_share(Arc::clone(&share), self.device.clone());
                self.org.org_apps.connect(share);
                self.fetch_org_apps();
            }
            Err(e) => tracing::error!("managed: sharing apps: {e}"),
        }
        let mut retry = FIRST_RETRY;
        let mut reported: Option<Instant> = None;
        loop {
            let request = SyncRequest {
                device: self.device.clone(),
                policy: self.policy_version,
                catalog: self.catalog_version,
                directory: self.directory_version,
                chat: self.chat_version,
                wait_secs: MAX_WAIT_SECS,
            };
            match client.sync(&request) {
                Ok(response) => {
                    retry = FIRST_RETRY;
                    let changed = self.handle(response);
                    if changed || reported.is_none_or(|at| at.elapsed() >= REPORT_EVERY) {
                        match client.report(&self.report()) {
                            Ok(()) => reported = Some(Instant::now()),
                            Err(e) => tracing::warn!("managed: report: {e}"),
                        }
                    }
                }
                Err(e) => {
                    self.offline(&e);
                    std::thread::sleep(retry);
                    retry = (retry * 2).min(LAST_RETRY);
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
                    self.fetch_org_apps();
                }
                Err(e) => self.refused(DocKind::Catalog, &signed, &e),
            }
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
        if self.offline_since.is_none() {
            tracing::warn!(
                "managed: the console can't be reached ({why}); keeping what's in force"
            );
            self.offline_since = Some(Timestamp::now());
        }
        self.last_error = Some(format!("the console can't be reached: {why}"));
        self.runtime.set_policy_source(self.source());
    }

    /// Verifies, validates whole, writes and swaps in a policy release.
    /// `again` is a release this device applied before, from its disk.
    fn apply_policy(&mut self, signed: &Signed, again: bool) -> Result<(), String> {
        let have = if again { None } else { self.policy_version };
        let release: PolicyRelease = signed
            .open(DocKind::Policy, &self.managed.org, self.keys(), have)
            .map_err(|e| e.to_string())?;
        let policy = Policy::from_toml(&release.policy).map_err(|e| e.to_string())?;
        let registry =
            crate::registry(&self.config, &policy, &self.org).map_err(|e| format!("{e:#}"))?;
        let warnings = policy.lint(&registry.lint_info());
        for warning in &warnings {
            tracing::warn!("managed policy v{}: {warning}", signed.version);
        }
        // The signed release first: if keeping it fails (a full disk), the
        // policy file and what's in force stay as they were, as the refusal
        // the caller audits says. If applying fails after, the kept release
        // is put back at the next start or poll.
        if !again {
            self.releases.save(signed).map_err(|e| e.to_string())?;
        }
        self.policies
            .apply(&self.config.policy, &release.policy)
            .map_err(|e| e.to_string())?;
        let _ = self.policies.remember(&glass_policy::store::Loaded {
            policy: policy.clone(),
            text: release.policy.clone(),
            origin: glass_policy::store::Origin::File(self.config.policy.clone()),
        });
        self.policy_version = Some(signed.version);
        self.runtime.apply_policy(policy, registry, self.source());
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
    /// kept only if it matches the signed digest, and on the record.
    fn fetch_org_apps(&self) {
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

    /// Verifies, checks and swaps in a catalog release.
    fn apply_catalog(&mut self, signed: &Signed, again: bool) -> Result<(), String> {
        let have = if again { None } else { self.catalog_version };
        let release: CatalogRelease = signed
            .open(DocKind::Catalog, &self.managed.org, self.keys(), have)
            .map_err(|e| e.to_string())?;
        release.check()?;
        if !again {
            self.releases.save(signed).map_err(|e| e.to_string())?;
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
    fn apply_directory(&mut self, signed: &Signed, again: bool) -> Result<(), String> {
        if signed.payload.len() > directory::MAX_PAYLOAD_BYTES {
            return Err(format!(
                "{} bytes is more than a directory release may be ({})",
                signed.payload.len(),
                directory::MAX_PAYLOAD_BYTES
            ));
        }
        let have = if again { None } else { self.directory_version };
        let release: DirectoryRelease = signed
            .open(DocKind::Directory, &self.managed.org, self.keys(), have)
            .map_err(|e| e.to_string())?;
        release.check()?;
        if !again {
            self.releases.save(signed).map_err(|e| e.to_string())?;
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
    fn apply_chat(&mut self, signed: &Signed, again: bool) -> Result<(), String> {
        let have = if again { None } else { self.chat_version };
        let release: ChatRelease = signed
            .open(DocKind::Chat, &self.managed.org, self.keys(), have)
            .map_err(|e| e.to_string())?;
        let key = release
            .key()
            .map_err(|e| format!("the compliance key: {e}"))?;
        if ChatRelease::new(&key) != release {
            return Err("the compliance key's fingerprint doesn't match it".into());
        }
        if !again {
            self.releases.save(signed).map_err(|e| e.to_string())?;
        }
        self.org
            .chat
            .set_compliance(&release.compliance, &release.fingerprint)?;
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

    /// A release was refused: audited and reported; nothing changes.
    fn refused(&mut self, kind: DocKind, signed: &Signed, why: &str) {
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
    let mut retry = FIRST_RETRY;
    loop {
        if chat.logins().is_empty() {
            // Nobody here has used chat yet.
            std::thread::sleep(FIRST_RETRY);
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

/// Actions policy blocked in the last 24 hours, from the audit log.
fn blocked_last_day(audit: &Path) -> u32 {
    let since = Timestamp::now().unix_seconds() - 24 * 60 * 60;
    glass_audit::read(audit, |r| {
        r.event.kind == EventKind::Action
            && r.ts.unix_seconds() >= since
            && r.event.outcome == Some(Outcome::Blocked)
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
}
