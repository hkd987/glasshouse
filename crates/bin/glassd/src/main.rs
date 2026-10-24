//! `glassd`: the system daemon. Loads config and policy, assembles the
//! runtime with its backends, cleans up after any earlier run, and serves the
//! control socket until told to stop.
//!
//! It checks its config and policy itself as it starts, from local disk
//! only, so boot never waits for a second process or the network (ADR
//! 0021). A policy file that doesn't validate falls back to the last policy
//! that did; a config that doesn't validate, or no good policy at all, stops
//! it (fail closed), and with `--console-errors` (how rc.d/glassd starts it)
//! the reason also goes to the system console, where it shows at boot.

mod browser;
mod config;
mod device;
mod managed;
mod meetings;
mod org_apps;
mod personal;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, bail};
use clap::Parser;
use glass_actions::Registry;
use glass_api::AppManifest;
use glass_app_catalog::CatalogHandle;
use glass_audit::{AuditEvent, AuditLog, Durability, EventKind};
use glass_gateway::{Gateway, Secrets};
use glass_platform::sandbox::{HostSandbox, JailConfig, JailSandbox, Sandbox};
use glass_platform::snapshot::{CopySnapshotter, Snapshotter, ZfsSnapshotter};
use glass_policy::Policy;
use glass_policy::store::{Loaded, Origin, PolicyStore};
use glass_runtime::{AppApi, ControlApi, Runtime, RuntimeConfig, RuntimeParts};
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::prelude::*;

use crate::config::{Config, SandboxBackend, SnapshotBackend};

#[derive(Debug, Parser)]
#[command(version, about = "Glasshouse system daemon")]
struct Args {
    /// Configuration file.
    #[arg(long, default_value = config::DEFAULT_CONFIG)]
    config: PathBuf,
    /// Check the configuration and policy as a start would, print what it
    /// would enforce and any warnings, and exit.
    #[arg(long)]
    check: bool,
    /// Also write why glassd didn't start, or which policy it fell back
    /// to, on the system console, where it shows at boot.
    #[arg(long)]
    console_errors: bool,
}

/// Set once the control socket is up: an error before it means glassd
/// didn't start.
static READY: AtomicBool = AtomicBool::new(false);

/// How long snapshots are kept (AU-20), and how often that is checked.
const SNAPSHOTS_KEPT: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);
const SNAPSHOT_CHECK: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

fn main() -> ExitCode {
    // GLASS_LOG is `level` or `target=level,...`, e.g. `glass_runtime=debug,info`.
    let filter = std::env::var("GLASS_LOG")
        .ok()
        .and_then(|spec| spec.parse::<Targets>().ok())
        .unwrap_or_else(|| Targets::new().with_default(LevelFilter::INFO));
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(filter)
        .init();
    let args = Args::parse();
    let result = prepare(&args).and_then(|prepared| {
        if args.check {
            tracing::info!(
                policy = %prepared.loaded.origin,
                agents = prepared.config.agents.len(),
                apps = prepared.apps.len(),
                "configuration is valid"
            );
            return Ok(());
        }
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("starting the async runtime")?
            .block_on(run(prepared))
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            if args.console_errors {
                let what = if READY.load(Ordering::SeqCst) {
                    "stopped"
                } else {
                    "didn't start"
                };
                console(&format!(
                    "the system service {what}, so agents can't run: {e:#}. \
                     Details are in the service log."
                ));
            }
            ExitCode::FAILURE
        }
    }
}

/// The model gateway for this machine (ADR 0018): a provider for each one
/// policy names that has what it needs. A secrets file that isn't private
/// is refused, and calls then go only to providers that need no secret.
/// Nothing here logs a credential.
fn gateway(config: &Config, policy: &Policy, org: &OrgData) -> Models {
    let mut notes = Vec::new();
    let uid = glass_platform::users::effective_uid();
    let mut secrets = match Secrets::load(&config.model_secrets, uid) {
        Ok(secrets) => secrets,
        Err(e) => {
            notes.push(e.to_string());
            Secrets::default()
        }
    };
    // On a computer nobody manages, its owner's own key (ADR 0039); IT's
    // own file wins any clash. A managed computer's model credits go
    // through its console, which keeps the org's key.
    if config.managed.is_none() {
        match Secrets::load(&personal::path(config), uid) {
            Ok(more) => secrets.merge(more),
            Err(e) => notes.push(e.to_string()),
        }
    }
    let credits = config.managed.is_some().then_some(&org.credits);
    let (gateway, more) =
        glass_gateway_providers::connect(&policy.document().models.providers, &secrets, credits);
    notes.extend(more);
    for note in &notes {
        tracing::warn!("models: {note}");
    }
    Models {
        gateway: Arc::new(gateway),
        notes,
    }
}

/// Where an earlier glassd kept the org's model credits key: removed when
/// the console says so, since calls on credits go through the console,
/// which keeps the key (ADR 0039).
pub(crate) fn credits_path(config: &Config) -> PathBuf {
    config.state_dir.join("models/credits.toml")
}

struct Models {
    gateway: Arc<Gateway>,
    /// Why a provider policy names can't be used here, for the start record.
    notes: Vec<String>,
}

/// Everything checked before glassd touches the machine.
struct Prepared {
    config: Config,
    loaded: Loaded,
    /// The policy file's, before an owner's AI models are added (ADR 0039).
    base_policy: Policy,
    store: PolicyStore,
    registry: Registry,
    models: Models,
    /// Installed apps whose manifests check out (UI-5, DV-3).
    apps: Vec<AppManifest>,
    /// What the org's console sent this device, for actions to read.
    org: OrgData,
}

/// What the org's admin console sent this device (ADR 0023): the catalog
/// `catalog.search` and the Store read (GA-2, DV-11) and the people
/// directory (AD-1). The sync replaces each whole; actions read the one in
/// force.
#[derive(Debug, Clone)]
pub(crate) struct OrgData {
    pub(crate) catalog: CatalogHandle,
    pub(crate) directory: glass_managed::DirectoryHandle,
    /// Device settings: policy's, and each person's choices, kept in the
    /// state directory by uid (UX-1, ADR 0029).
    pub(crate) settings: glass_runtime::SettingsHandle,
    /// The org's chat: member keys and the sealed store, root only in the
    /// state directory (CF-91, ADR 0030). Connected once managed sync runs.
    pub(crate) chat: glass_first_party::ChatHandle,
    /// How meetings reach the console's meeting routes (CF-21). Meetings
    /// are conversations in `chat`'s store; the media key is never kept.
    pub(crate) meet: glass_first_party::MeetHandle,
    /// The link model calls on the org's credits take to its console (ADR
    /// 0039), connected once managed sync has the computer's key.
    pub(crate) credits: Arc<glass_gateway_providers::CreditsLink>,
    /// Mail (ADR 0037): the link `[mail]` names, and each person's sign-in
    /// and sync state, root only in the state directory.
    pub(crate) mail: glass_first_party::MailHandle,
    /// The apps people make (ADR 0032), kept root only in the state
    /// directory; bundled in jails once the sandbox is up.
    pub(crate) studio: glass_first_party::Studio,
    /// Apps people made that IT approved (R2): bundles fetched from the
    /// console and checked against the signed catalog.
    pub(crate) org_apps: org_apps::OrgAppStore,
    /// People's routines (DA-4), root only in the state directory.
    pub(crate) routines: glass_first_party::Routines,
}

impl OrgData {
    pub(crate) fn new(config: &Config) -> Self {
        let state_dir = &config.state_dir;
        let catalog = CatalogHandle::new();
        Self {
            org_apps: org_apps::OrgAppStore::new(state_dir.join("apps/.org"), catalog.clone()),
            catalog,
            directory: glass_managed::DirectoryHandle::new(),
            settings: {
                let settings = glass_runtime::SettingsHandle::new(state_dir.join("settings"));
                // A computer no organization manages is its owner's (ADR 0038).
                if config.managed.is_none() {
                    settings.set_owner(device::owner(state_dir));
                }
                settings
            },
            chat: glass_first_party::ChatHandle::new(state_dir.join("chat")),
            meet: glass_first_party::MeetHandle::new(),
            credits: glass_gateway_providers::CreditsLink::new(),
            mail: glass_first_party::MailHandle::new(state_dir.join("mail")),
            studio: glass_first_party::Studio::new(glass_app_studio::StudioPaths {
                store: state_dir.join("apps"),
                installed: config.apps_dir.clone(),
                source: config.studio.source.clone(),
                bundler: config.studio.bundler.clone(),
                tryout: Some(config.studio.tryout.clone()),
            }),
            routines: glass_first_party::Routines::new(state_dir.join("routines")),
        }
    }
}

/// Reads and checks the config and policy, from local disk only.
fn prepare(args: &Args) -> anyhow::Result<Prepared> {
    let config = Config::load(&args.config)?;
    let store = PolicyStore::new(config.state_dir.join("policy"));
    let loaded = store.load(&config.policy)?;
    if let Origin::LastGood { rejected, reason } = &loaded.origin {
        tracing::warn!(
            rejected = %rejected.display(),
            "policy doesn't validate ({reason}); enforcing the last policy that did"
        );
        if args.console_errors {
            console(&format!(
                "{} has an error, so the last policy that worked is in force: {reason}",
                rejected.display()
            ));
        }
    }
    let org = OrgData::new(&config);
    // On a computer nobody manages, its owner's AI models join its policy
    // (ADR 0039).
    let mut loaded = loaded;
    let base_policy = loaded.policy.clone();
    if config.managed.is_none() {
        loaded.policy = personal::with_models(
            loaded.policy,
            personal::connected(&config).as_deref(),
            &personal::chosen(&config),
        );
    }
    let registry = registry(&config, &loaded.policy, &org)?;
    for warning in loaded.policy.lint(&registry.lint_info()) {
        tracing::warn!("policy: {warning}");
    }
    let models = gateway(&config, &loaded.policy, &org);
    let (apps, skipped) = config.load_apps(&registry);
    for reason in &skipped {
        tracing::warn!("app not installed: {reason}");
    }
    Ok(Prepared {
        config,
        loaded,
        base_policy,
        store,
        registry,
        models,
        apps,
        org,
    })
}

/// One line on the system console (BR-1: it names Glasshouse, never the
/// base system). Best effort: there may be no console to write to.
fn console(message: &str) {
    if let Ok(mut console) = std::fs::OpenOptions::new().write(true).open("/dev/console") {
        let _ = writeln!(console, "\r\nGlasshouse: {message}\r");
    }
}

/// What the org's catalog lets reach a person (DV-11): an available item
/// once they install it from the Store. Read only when a catalog arrived.
#[derive(Debug)]
pub(crate) struct OrgCatalog(pub(crate) CatalogHandle);

impl glass_runtime::CatalogGate for OrgCatalog {
    fn reaches(
        &self,
        groups: &[String],
        home: &std::path::Path,
        id: &glass_types::AgentId,
    ) -> bool {
        let Some(catalog) = self.0.current() else {
            return true;
        };
        let installed = glass_app_catalog::installed(home);
        glass_app_catalog::reaches(Some(&catalog), groups, &installed, id)
    }

    fn lists(&self) -> std::collections::BTreeSet<glass_types::AgentId> {
        self.0
            .current()
            .map(|c| c.release.items.iter().map(|i| i.id.clone()).collect())
            .unwrap_or_default()
    }
}

/// What the org's catalog offers someone in `groups` for `need` (GA-2),
/// for the studio to suggest before anything is built (GA-12).
fn studio_offers(
    catalog: &CatalogHandle,
    groups: &[String],
    need: &str,
) -> Vec<(glass_types::AgentId, String)> {
    let Some(applied) = catalog.current() else {
        return Vec::new();
    };
    glass_managed::catalog::search(&applied.release.items, groups, need, None, 5)
        .into_iter()
        .filter(|m| m.score > 0)
        .map(|m| (m.item.id.clone(), m.item.name.clone()))
        .collect()
}

/// Mail's link from `[mail]` (ADR 0037, EM0): IMAP and SMTP submission
/// for `service = "imap"`, to the hosts the signed policy names; the local
/// store in `~/Mail` for anything else, until its link lands.
fn connect_mail(mail: &glass_first_party::MailHandle, policy: &Policy) {
    use glass_policy::document::MailService;
    match policy.mail() {
        Some(settings) if settings.service == MailService::Imap => {
            match (&settings.incoming, &settings.outgoing) {
                (Some(incoming), Some(outgoing)) => {
                    mail.connect(glass_first_party::MailRemote {
                        link: Arc::new(glass_gateway_providers::ImapLink::new()),
                        incoming: glass_first_party::mail_server(incoming),
                        outgoing: glass_first_party::mail_server(outgoing),
                    });
                }
                _ => mail.disconnect(),
            }
        }
        _ => mail.disconnect(),
    }
}

fn registry(config: &Config, policy: &Policy, org: &OrgData) -> anyhow::Result<Registry> {
    // The link and the actions come from the same policy.
    connect_mail(&org.mail, policy);
    Ok(glass_first_party::registry(glass_first_party::Setup {
        // IT's fix list decides what the IT agent may restart (AE-4).
        restartable_services: policy.fixes().services.clone(),
        helpdesk_outbox: config.state_dir.join("helpdesk/outbox"),
        system: Arc::new(glass_app_system::HostSystem),
        catalog: org.catalog.clone(),
        directory: org.directory.clone(),
        // Mail's service from IT (NV-3): a new policy brings it, with the
        // registry rebuilt around it.
        mail: policy.mail().cloned(),
        mail_handle: org.mail.clone(),
        settings: org.settings.clone(),
        chat: org.chat.clone(),
        meet: org.meet.clone(),
        managed: config.managed.is_some(),
        studio: org.studio.clone(),
        skills: config.state_dir.join("skills"),
        routines: org.routines.clone(),
    })?)
}

async fn run(prepared: Prepared) -> anyhow::Result<()> {
    let Prepared {
        config,
        loaded,
        base_policy,
        store,
        registry,
        models,
        apps,
        org,
    } = prepared;
    let policy_source = loaded.origin.to_string();
    let run_dir = config
        .control_socket
        .parent()
        .context("control socket needs a parent directory")?;
    // Chat's member keys live under the state folder, root only (ADR 0030);
    // made here so the containment probe has something to try. So do mail
    // sign-ins (ADR 0037).
    let chat_dir = config.state_dir.join("chat");
    let mail_dir = config.state_dir.join("mail");
    // Drafts and eval results of skills, per person (ADR 0034), root only.
    let skills_dir = config.state_dir.join("skills");
    // People's routines (DA-4), and what their tasks did as code (ADR
    // 0046), root only: the probe tries to open them.
    let routines_dir = config.state_dir.join("routines");
    let flows_dir = config.state_dir.join("flows");
    for (dir, mode) in [
        (config.state_dir.as_path(), 0o700),
        (chat_dir.as_path(), 0o700),
        (mail_dir.as_path(), 0o700),
        (skills_dir.as_path(), 0o700),
        (routines_dir.as_path(), 0o700),
        (flows_dir.as_path(), 0o700),
        (run_dir, 0o755),
    ] {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode))?;
    }
    if let Some(dir) = config.app_socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    let sandbox: Arc<dyn Sandbox> = match config.sandbox.backend {
        SandboxBackend::Jail => {
            let machine = glass_platform::system::resources();
            let mut limits = config
                .sandbox
                .limits
                .resolve(machine.cpus, machine.physical_memory);
            if !machine.accounting {
                tracing::warn!(
                    "resource accounting is off (kern.racct.enable), so agents run without \
                     CPU, memory and disk limits until the machine restarts with it on"
                );
                limits = limits.priority_only();
            }
            tracing::info!(?limits, "agent resource limits");
            let mut jail = JailConfig {
                // Beside the sandboxes' roots, kept across a restart of
                // the computer: the boot empties the run folder (ADR 0053).
                state_dir: config.state_dir.join("sandboxes"),
                limits,
                ..JailConfig::default()
            };
            if let Some(mounts) = &config.sandbox.base_mounts {
                jail.base_mounts.clone_from(mounts);
            }
            Arc::new(JailSandbox::new(jail))
        }
        SandboxBackend::HostInsecure => {
            tracing::warn!(
                "SANDBOX IS NOT A SECURITY BOUNDARY: agents run unconfined (development only)"
            );
            Arc::new(HostSandbox)
        }
    };
    let snapshots: Arc<dyn Snapshotter> = match config.snapshots.backend {
        SnapshotBackend::Zfs => Arc::new(ZfsSnapshotter::new()),
        SnapshotBackend::Copy => Arc::new(CopySnapshotter::new(config.state_dir.join("snapshots"))),
    };

    let audit_path = config.audit_log();
    if let Some(dir) = audit_path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let audit = Arc::new(AuditLog::open(&audit_path, Durability::Sync)?);
    std::fs::set_permissions(&audit_path, std::fs::Permissions::from_mode(0o600))?;

    // Attended tasks' sockets from an earlier run have no tasks behind them.
    let _ = std::fs::remove_dir_all(run_dir.join("attended"));

    let cleaned = {
        let sandbox = Arc::clone(&sandbox);
        tokio::task::spawn_blocking(move || sandbox.cleanup_stale()).await??
    };
    for name in &cleaned {
        tracing::warn!(sandbox = %name, "removed a sandbox left behind by an earlier run");
    }

    let mut started = AuditEvent::new(
        EventKind::DaemonStarted,
        format!("glassd {} started", env!("CARGO_PKG_VERSION")),
    );
    started.detail = serde_json::json!({
        "sandbox": sandbox.kind(),
        "snapshots": snapshots.kind(),
        "stale_sandboxes_removed": cleaned,
        "model_providers": models.gateway.providers().collect::<Vec<_>>(),
        "model_notes": models.notes,
    });
    audit.append(started)?;
    let mut policy_event = AuditEvent::new(
        EventKind::PolicyLoaded,
        format!("Policy loaded from {policy_source}"),
    );
    policy_event.detail = match &loaded.origin {
        Origin::LastGood { rejected, reason } => serde_json::json!({
            "source": "last-good",
            "path": store.last_good_path(),
            "rejected": rejected,
            "reason": reason,
        }),
        _ => serde_json::json!({ "source": policy_source }),
    };
    audit.append(policy_event)?;

    let runtime = Runtime::new(RuntimeParts {
        config: RuntimeConfig {
            state_dir: config.state_dir.clone(),
            catalog: config.agents.clone(),
            agent_dirs: config.sandbox.agent_dirs.clone(),
            max_live_tasks_per_user: config.max_live_tasks_per_user,
            harness: config.harness.clone(),
            attended_dir: run_dir.join("attended"),
            apps: apps.clone(),
            console_url: config.managed.as_ref().map(|m| m.url.clone()),
            dev_home: config.dev_home.clone(),
            org_catalog: Arc::new(OrgCatalog(org.catalog.clone())),
            personal: Arc::new(org.studio.clone()),
            org_apps: Arc::new(org.org_apps.clone()),
            hardware: glass_runtime::hwpatch::HardwarePatches {
                keys: config.hardware.keys.clone(),
                community: config.hardware.community,
                probation_minutes: config.hardware.probation_minutes,
            },
        },
        policy: loaded.policy.clone(),
        policy_source: policy_source.clone(),
        registry,
        sandbox: Arc::clone(&sandbox),
        snapshots,
        audit,
        gateway: models.gateway,
        settings: org.settings.clone(),
    });

    // Apps people make are bundled in jails like agents' (ADR 0032), and
    // checked against the policy in force and the org's catalog.
    org.studio
        .set_tools(Arc::clone(&sandbox) as Arc<dyn glass_platform::sandbox::ToolRunner>);
    {
        let catalog = org.catalog.clone();
        org.studio
            .set_surroundings(Arc::new(glass_runtime::StudioSurroundings::new(
                &runtime,
                move |groups, need| studio_offers(&catalog, groups, need),
            )));
    }

    // A new agent version re-checks every layer on it (TL-13).
    runtime.recheck_layers();

    // People's routines start on their schedules (DA-4), in this computer's
    // local time, each as the person it belongs to. A minute the computer
    // slept through doesn't run late, and one that comes again (a restart,
    // the clock set back, daylight saving ending) doesn't run twice: the
    // runtime claims each minute on disk (ADR 0051).
    runtime.set_routines(org.routines.clone());
    {
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
            let mut last = None;
            loop {
                tick.tick().await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                // Once a minute; offsets are whole minutes.
                if last.replace(now.div_euclid(60)) == Some(now.div_euclid(60)) {
                    continue;
                }
                let offset = glass_platform::system::utc_offset_secs().unwrap_or(0);
                let (day, hour, minute, number) = glass_app_routines::local_minute(now, offset);
                let runtime = Arc::clone(&runtime);
                let started = tokio::task::spawn_blocking(move || {
                    runtime.run_due_routines(day, hour, minute, number, |login| {
                        runtime.caller(glass_platform::users::by_name(login)?.uid)
                    })
                })
                .await
                .unwrap_or_default();
                for (routine, result) in started {
                    match result {
                        Ok(task) => {
                            tracing::info!(routine, task = %task.id, "started a routine on its schedule")
                        }
                        Err(e) => {
                            tracing::warn!(routine, "a routine couldn't start on its schedule: {e}")
                        }
                    }
                }
            }
        });
    }

    // The computer sleeps when the person at the screen has left it alone
    // as long as their setting says, agents paused first (ADR 0031).
    {
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
            loop {
                tick.tick().await;
                if !runtime.sleep_due(std::time::Instant::now()) {
                    continue;
                }
                let slept = runtime
                    .sleep_now(glass_runtime::WAKE_WAIT, || {
                        glass_platform::system::sleep_machine().map_err(|e| e.to_string())
                    })
                    .await;
                if let Err(e) = slept {
                    tracing::warn!("couldn't sleep the computer: {e}");
                }
            }
        });
    }

    // Snapshots are kept 30 days (AU-20), and for as long as a task in
    // memory could undo from them: checked at start and then daily.
    {
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(SNAPSHOT_CHECK);
            loop {
                tick.tick().await;
                let runtime = Arc::clone(&runtime);
                match tokio::task::spawn_blocking(move || runtime.prune_snapshots(SNAPSHOTS_KEPT))
                    .await
                {
                    Ok(Ok(0)) => {}
                    Ok(Ok(gone)) => tracing::info!(gone, "removed snapshots older than 30 days"),
                    Ok(Err(e)) => tracing::warn!("removing old snapshots: {e}"),
                    Err(e) => tracing::warn!("removing old snapshots: {e}"),
                }
            }
        });
    }

    // "Hey Glass" in a transcribed meeting starts the person's desktop
    // agent on what they asked (ADR 0045).
    org.meet.agents(Arc::new(meetings::Asks::new(&runtime)));

    // Chat's keys and the records it kept out go in the audit log (AU-1);
    // news wakes the app that shows it: Chat, Calendar or Artifacts (ADR
    // 0030, 0033).
    {
        let runtime = Arc::downgrade(&runtime);
        org.chat.on_notice(move |notice| {
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            if let glass_first_party::ChatNotice::Changed { logins, kinds } = notice {
                for login in logins {
                    for kind in kinds {
                        runtime.app_changed(login, kind.app());
                    }
                }
            }
            if let Some(event) = managed::chat_event(notice) {
                runtime.append_audit(event);
            }
        });
    }

    // New mail wakes Mail (ADR 0037): a watcher per person signed in waits
    // on their Inbox (IMAP IDLE) and syncs it into ~/Mail.
    {
        let runtime = Arc::downgrade(&runtime);
        org.mail.on_notice(move |notice| {
            let glass_first_party::MailNotice::Changed { login } = notice;
            if let Some(runtime) = runtime.upgrade() {
                runtime.app_changed(login, "mail");
            }
        });
        mail_watcher(org.mail.clone());
    }

    // Who is at the screen outlasts a restart of glassd, not a reboot; taken
    // back before anything that follows them is applied (ADR 0031).
    runtime.keep_seat_in(run_dir.join("seat"));

    // A kernel patch the computer was set to try: on probation if it
    // started on it and it passes its check, rolled back if it came back
    // without it (ADR 0057). Off the start path: the check loads modules.
    // Then its minutes of use, counted while the computer is awake, keep it
    // once they reach its probation.
    {
        let runtime = Arc::clone(&runtime);
        let every = patch_tick(config.hardware.probation_minutes);
        tokio::spawn(async move {
            let at_start = Arc::clone(&runtime);
            let _ = tokio::task::spawn_blocking(move || at_start.hardware_at_start()).await;
            let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
            // Asleep, it isn't in use: one tick on waking, not the ones missed.
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let runtime = Arc::clone(&runtime);
                let minutes = every.as_secs() / 60;
                let _ = tokio::task::spawn_blocking(move || runtime.hardware_tick(minutes)).await;
            }
        });
    }

    // The browser's settings from policy (UX-1), for whoever is signed in:
    // written now, and again whenever a policy changes them or someone
    // signs in or out (ADR 0031).
    if let Some(dir) = config.browser_policies.clone() {
        device::follow(&org.settings, &runtime, move |computer, _| {
            if let Err(e) = browser::write(&dir, computer) {
                tracing::warn!("writing the browser's policy in {}: {e}", dir.display());
            }
        });
    }

    // What IT sets for the computer that glassd applies itself: the time
    // zone, and USB drives for whoever is signed in (ADR 0031).
    device::watch(&org.settings, &runtime, config.studio.source.clone());
    // First-run setup makes the owner of a computer nobody manages after
    // glassd has started: learnt as they sign in (ADR 0038).
    if config.managed.is_none() {
        let state_dir = config.state_dir.clone();
        device::learn_owner(org.settings.clone(), runtime.watch_seat(), move || {
            device::owner(&state_dir)
        });
    }
    // The owner of a computer nobody manages connects their AI models in
    // Settings (ADR 0039).
    if config.managed.is_none() {
        let personal = personal::PersonalModels::new(
            config.clone(),
            org.clone(),
            base_policy,
            policy_source.clone(),
        );
        personal.attach(&runtime);
        org.settings.set_model_keys(personal);
    }

    let listener = bind_control_socket(&config.control_socket)?;
    // The app socket (ADR 0024): world-connectable too, authorized by uid,
    // and carrying only the app methods.
    let app_listener = bind_control_socket(&config.app_socket)?;
    tracing::info!(
        socket = %config.control_socket.display(),
        app_socket = %config.app_socket.display(),
        apps = apps.len(),
        sandbox = sandbox.kind(),
        policy = %policy_source,
        agents = config.agents.len(),
        "glassd ready"
    );
    READY.store(true, Ordering::SeqCst);

    // The policy now in force is the one to fall back on next time. That
    // writes and syncs files, so it happens off the start path. Then, on a
    // managed device, the catalog it last applied goes back in force and
    // syncing with the console starts (ADR 0023), never before glassd is up.
    let managed_runtime = Arc::clone(&runtime);
    let managed_config = config.clone();
    std::thread::spawn(move || {
        match store.remember(&loaded) {
            Ok(true) => tracing::info!("recorded the policy in force as the last good policy"),
            Ok(false) => {}
            Err(e) => tracing::warn!("couldn't record the last good policy: {e}"),
        }
        if let Some(managed) = managed::Managed::start(&managed_config, managed_runtime, org) {
            managed.run();
        }
    });

    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let served = tokio::select! {
        result = glass_rpc::serve(listener, Arc::new(ControlApi::new(Arc::clone(&runtime)))) => result.map_err(anyhow::Error::from),
        result = glass_rpc::serve(app_listener, Arc::new(AppApi::new(Arc::clone(&runtime)))) => result.map_err(anyhow::Error::from),
        _ = terminate.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
    };
    tracing::info!("shutting down: stopping running agents");
    runtime.shutdown().await;
    let _ = std::fs::remove_file(&config.control_socket);
    let _ = std::fs::remove_file(&config.app_socket);
    served
}

/// How often a kernel patch on probation counts its minutes of use: every
/// five, or a quarter of a short probation (tests), at least one.
fn patch_tick(probation_minutes: u64) -> std::time::Duration {
    std::time::Duration::from_secs((probation_minutes / 4).clamp(1, 5) * 60)
}

/// How long one wait on an Inbox lasts before it starts again (servers
/// may end IDLE after 30 minutes, RFC 2177).
const MAIL_WAIT: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Starts a thread per person whose mail is connected, as they appear,
/// each waiting on their Inbox and syncing it when it changes. A failure
/// (offline, a changed password) is retried a minute later.
fn mail_watcher(mail: glass_first_party::MailHandle) {
    std::thread::spawn(move || {
        let mut watching = std::collections::BTreeSet::new();
        loop {
            for login in mail.watched() {
                if !watching.insert(login.clone()) {
                    continue;
                }
                let mail = mail.clone();
                std::thread::spawn(move || {
                    loop {
                        if let Err(e) = mail.watch(&login, MAIL_WAIT) {
                            tracing::debug!(%login, "mail: not watching the Inbox for now: {e}");
                            std::thread::sleep(std::time::Duration::from_secs(60));
                        }
                    }
                });
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

/// The control socket is world-connectable; every call is authorized by the
/// caller's kernel-reported uid.
fn bind_control_socket(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            bail!("another glassd is already listening on {}", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

/// Writes `bytes` to `path` whole, readable by root alone, in a folder only
/// root can enter: the model keys glassd keeps (ADR 0039).
pub(crate) fn write_root_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let tmp = path.with_extension("toml.new");
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config whose policy file and state live in `dir`.
    fn args(dir: &Path) -> Args {
        let config = dir.join("glassd.toml");
        std::fs::write(
            &config,
            format!(
                "state_dir = \"{dir}/state\"\npolicy = \"{dir}/policy.toml\"\n",
                dir = dir.display()
            ),
        )
        .unwrap();
        Args {
            config,
            check: true,
            console_errors: false,
        }
    }

    #[test]
    fn a_policy_that_doesnt_validate_falls_back_to_the_last_good_one() {
        // ADR 0021: one bad file doesn't stop agents; the last good policy
        // stays in force and the source says why.
        let tmp = tempfile::tempdir().unwrap();
        let args = args(tmp.path());
        let policy = tmp.path().join("policy.toml");
        std::fs::write(&policy, glass_policy::DEFAULT_POLICY).unwrap();
        let first = prepare(&args).unwrap();
        assert!(matches!(first.loaded.origin, Origin::File(_)));
        assert!(first.store.remember(&first.loaded).unwrap());

        std::fs::write(&policy, "version = 1\nnonsense = 1\n").unwrap();
        let second = prepare(&args).unwrap();
        assert!(
            matches!(second.loaded.origin, Origin::LastGood { .. }),
            "{:?}",
            second.loaded.origin
        );
        assert_eq!(second.loaded.text, glass_policy::DEFAULT_POLICY);
    }

    #[test]
    fn with_no_good_policy_it_refuses_to_start_and_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let args = args(tmp.path());
        std::fs::write(
            tmp.path().join("policy.toml"),
            "version = 1\nnonsense = 1\n",
        )
        .unwrap();
        let err = format!("{:#}", prepare(&args).err().expect("fails closed"));
        assert!(err.contains("policy.toml"), "{err}");
        assert!(err.contains("no earlier policy"), "{err}");
        assert!(!err.to_lowercase().contains("freebsd"), "BR-1: {err}");
    }
}
