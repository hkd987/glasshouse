//! The pipeline tests' harness: a runtime on development backends (host
//! sandbox, copy snapshots) in a temporary directory, with the test playing
//! the agent over the task socket.

#![allow(dead_code, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use glass_actions::Registry;
use glass_api::{AppManifest, CustomizationPoint, PointKind, ScopeRequest, TaskRequest, TileState};
use glass_app_calendar::CalendarProvider;
use glass_app_catalog::{CatalogHandle, CatalogProvider};
use glass_app_chat::{ChatHandle, ChatProvider};
use glass_app_files::FilesProvider;
use glass_app_helpdesk::HelpdeskProvider;
use glass_app_mail::{MailHandle, MailProvider};
use glass_app_meet::{MeetHandle, MeetProvider};
use glass_app_people::PeopleProvider;
use glass_app_settings::{SettingsHandle, SettingsProvider};
use glass_app_studio::{Studio, StudioPaths, StudioProvider};
use glass_app_system::{SystemOps, SystemProvider};
use glass_audit::{AuditLog, Durability};
use glass_gateway::{Gateway, ModelAction, ProviderError, ScriptedProvider};
use glass_managed::DirectoryHandle;
use glass_platform::sandbox::HostSandbox;
use glass_platform::snapshot::CopySnapshotter;
use glass_platform::system;
use glass_policy::Policy;
use glass_rpc::Client;
use glass_rpc::mcp::CallToolResult;
use glass_runtime::{
    AppApi, Caller, CatalogEntry, ControlApi, Runtime, RuntimeConfig, RuntimeParts,
};
use glass_types::TaskId;
use serde_json::{Value, json};

pub struct Harness {
    pub dir: tempfile::TempDir,
    pub rt: Arc<Runtime>,
    pub me: Caller,
    pub machine: Arc<FakeMachine>,
    /// The model provider `scripted` in policy reaches; what it was sent.
    pub model: Arc<ScriptedProvider>,
    /// The org catalog `catalog.search` reads; empty until a test sets it.
    pub catalog: CatalogHandle,
    /// The org's people `people.search` reads and Mail and Calendar send
    /// as; empty until a test sets it.
    pub directory: DirectoryHandle,
    /// Device settings: policy's and the test user's choices (UX-1).
    pub settings: SettingsHandle,
    /// Chat on this computer (CF-91); unconnected until a test connects it.
    pub chat: ChatHandle,
    /// How meetings reach the console (CF-21); unconnected until a test
    /// connects it.
    pub meet: MeetHandle,
    /// Mail's link and sign-ins (ADR 0037); the local store until a test
    /// connects a service.
    pub mail: MailHandle,
    /// The apps people make (ADR 0032), bundled by a stand-in for esbuild.
    pub studio: Studio,
    /// Org apps glassd fetched (R2); none until a test sets them.
    pub org_apps: Arc<MemoryOrgApps>,
}

impl Harness {
    pub fn home(&self) -> PathBuf {
        self.me.home.clone()
    }

    /// The default policy with `from` replaced by `to`, then `extra`, put
    /// in force on the running runtime as managed settings do (ADR 0023).
    #[allow(dead_code)]
    pub fn apply_policy_replacing(&self, from: &str, to: &str, extra: &str) {
        assert!(glass_policy::DEFAULT_POLICY.contains(from), "{from}");
        let text = format!(
            "{}\n{}",
            glass_policy::DEFAULT_POLICY.replace(from, &to.replace("{me}", &self.me.name)),
            extra.replace("{me}", &self.me.name)
        );
        let policy = Policy::from_toml(&text).unwrap();
        let registry = test_registry(
            self.dir.path(),
            &self.machine,
            &self.catalog,
            &self.directory,
            &self.settings,
            &self.chat,
            &self.meet,
            &self.mail,
            &self.studio,
            &policy,
        );
        self.rt.apply_policy(policy, registry, "test");
    }

    /// Puts the default policy plus `extra` in force on the running
    /// runtime, with a new registry, as managed settings do (ADR 0023).
    #[allow(dead_code)]
    pub fn apply_policy(&self, extra: &str, source: &str) {
        let policy = test_policy(&self.me.home, &self.me.name, extra);
        let registry = test_registry(
            self.dir.path(),
            &self.machine,
            &self.catalog,
            &self.directory,
            &self.settings,
            &self.chat,
            &self.meet,
            &self.mail,
            &self.studio,
            &policy,
        );
        self.rt.apply_policy(policy, registry, source);
    }

    pub fn audit_path(&self) -> PathBuf {
        self.dir.path().join("audit.log")
    }

    pub fn socket(&self, task: TaskId) -> PathBuf {
        self.dir
            .path()
            .join("state/sandboxes")
            .join(format!("glass-{}", task.short()))
            .join("run/glasshouse/agent.sock")
    }

    pub fn request(&self, read: &[&str], write: &[&str]) -> TaskRequest {
        TaskRequest {
            agent: "invoice-demo".parse().unwrap(),
            prompt: "Reconcile this month's invoices and remind vendors".into(),
            scope: ScopeRequest {
                read: read.iter().map(|p| self.home().join(p)).collect(),
                write: write.iter().map(|p| self.home().join(p)).collect(),
                restrict: Vec::new(),
            },
            title: None,
        }
    }

    /// Connects as the agent and completes the MCP handshake.
    pub async fn agent(&self, task: TaskId) -> Client {
        let socket = self.socket(task);
        eventually(|| socket.exists()).await;
        let client = Client::connect(&socket).await.unwrap();
        let init: Value = client
            .call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}))
            .await
            .unwrap();
        let prompt = self.rt.get(&self.me, task).unwrap().prompt;
        assert!(init["instructions"].as_str().unwrap().contains(&prompt));
        client
    }

    /// Serves the app socket (ADR 0024) and returns where it listens.
    /// The control socket the desktop and glassctl use.
    #[allow(dead_code)]
    pub fn serve_control(&self) -> PathBuf {
        let path = self.dir.path().join("run/control.sock");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let api = Arc::new(ControlApi::new(Arc::clone(&self.rt)));
        tokio::spawn(glass_rpc::serve(listener, api));
        path
    }

    pub fn serve_apps(&self) -> PathBuf {
        let path = self.dir.path().join("run/apps.sock");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let api = Arc::new(AppApi::new(Arc::clone(&self.rt)));
        tokio::spawn(glass_rpc::serve(listener, api));
        path
    }

    pub fn tile(&self, task: TaskId) -> TileState {
        self.rt.get(&self.me, task).unwrap().view.state
    }
}

pub async fn tool(client: &Client, name: &str, arguments: Value) -> CallToolResult {
    client
        .call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
        .unwrap()
}

/// Polls until `check` holds, failing after five seconds.
pub async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never became true");
}

/// The default policy, the test home's Finance folder labeled confidential,
/// and `extra`, in which `{me}` is `me` and `{home}` is `home`.
fn test_policy(home: &Path, me: &str, extra: &str) -> Policy {
    let text = format!(
        "{}\n[[label]]\npath = \"{}/Finance\"\nlabel = \"confidential\"\n{}",
        glass_policy::DEFAULT_POLICY,
        home.display(),
        extra
            .replace("{me}", me)
            .replace("{home}", &home.display().to_string())
    );
    Policy::from_toml(&text).unwrap()
}

/// The apps' actions as glassd registers them, on fakes.
#[allow(clippy::too_many_arguments)]
fn test_registry(
    dir: &Path,
    machine: &Arc<FakeMachine>,
    catalog: &CatalogHandle,
    directory: &DirectoryHandle,
    settings: &SettingsHandle,
    chat: &ChatHandle,
    meet: &MeetHandle,
    mail: &MailHandle,
    studio: &Studio,
    policy: &Policy,
) -> Registry {
    let mut registry = Registry::new();
    registry.register(Arc::new(FilesProvider)).unwrap();
    registry
        .register(Arc::new(
            MailProvider::new(directory.clone())
                .settings(policy.mail().cloned())
                .mail(mail.clone()),
        ))
        .unwrap();
    // Meetings are conversations in chat's store (CF-21), as glassd wires it.
    let chat = ChatProvider::new(chat.clone(), directory.clone());
    let meetings: Arc<dyn glass_managed::meet::Meetings> = Arc::new(chat.clone());
    let shared: Arc<dyn glass_managed::shared::Shared> = Arc::new(chat.clone());
    registry
        .register(Arc::new(glass_app_artifacts::ArtifactsProvider::new(
            shared.clone(),
            directory.clone(),
        )))
        .unwrap();
    registry
        .register(Arc::new(glass_app_skills::SkillsProvider::new(
            shared.clone(),
            directory.clone(),
            dir.join("skills"),
        )))
        .unwrap();
    let docs = Arc::new(glass_app_docs::DocsProvider::new(shared, directory.clone()));
    registry.register(docs.clone()).unwrap();
    registry.register_live(docs);
    registry
        .register(Arc::new(
            CalendarProvider::new(directory.clone()).meetings(meetings.clone()),
        ))
        .unwrap();
    registry
        .register(Arc::new(MeetProvider::new(
            meetings,
            meet.clone(),
            directory.clone(),
        )))
        .unwrap();
    registry
        .register(Arc::new(PeopleProvider::new(directory.clone())))
        .unwrap();
    let ops: Arc<dyn SystemOps> = machine.clone();
    registry
        .register(Arc::new(SystemProvider::new(ops, vec!["ntpd".into()])))
        .unwrap();
    registry
        .register(Arc::new(HelpdeskProvider::new(dir.join("outbox"))))
        .unwrap();
    registry.register(Arc::new(ModelAction)).unwrap();
    registry
        .register(Arc::new(CatalogProvider::new(catalog.clone())))
        .unwrap();
    registry
        .register(Arc::new(SettingsProvider::new(settings.clone())))
        .unwrap();
    registry.register(Arc::new(chat)).unwrap();
    registry
        .register(Arc::new(StudioProvider::new(studio.clone())))
        .unwrap();
    registry
        .register(Arc::new(glass_app_routines::RoutinesProvider::new(
            routines(dir),
        )))
        .unwrap();
    registry
}

/// People's routines, kept where glassd keeps them (DA-4).
pub fn routines(dir: &Path) -> glass_app_routines::Routines {
    glass_app_routines::Routines::new(dir.join("state/routines"))
}

/// A harness whose policy is the default plus `policy_extra`, in which
/// `{me}` stands for the test user's login name and `{home}` for their home.
pub fn harness_with(policy_extra: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let meta = fs::metadata(dir.path()).unwrap();
    let account = glass_platform::users::by_uid(meta.uid()).expect("test user resolves");
    let home = dir.path().join("home");
    for folder in ["Invoices", "Reports", "Finance", "Private"] {
        fs::create_dir_all(home.join(folder)).unwrap();
    }
    for (i, amount) in ["120.00", "75.50", "310.25"].iter().enumerate() {
        fs::write(
            home.join(format!("Invoices/inv-00{}.txt", i + 1)),
            format!("Vendor {i}\nAmount: {amount}\n"),
        )
        .unwrap();
    }
    fs::write(home.join("Reports/existing.txt"), "original report\n").unwrap();
    fs::write(home.join("Finance/q3.csv"), "secret numbers\n").unwrap();

    let policy = test_policy(&home, &account.name, policy_extra);
    let machine = Arc::new(FakeMachine::default());
    let catalog = CatalogHandle::new();
    let directory = DirectoryHandle::new();
    let settings = SettingsHandle::new(dir.path().join("state/settings"));
    let chat = ChatHandle::new(dir.path().join("state/chat"));
    let meet = MeetHandle::new();
    let mail = MailHandle::new(dir.path().join("state/mail"));
    let apps_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
    // Our apps' source as images lay it out, linked to the repository's.
    let source = dir.path().join("source");
    fs::create_dir_all(&source).unwrap();
    std::os::unix::fs::symlink(&apps_dir, source.join("apps")).unwrap();
    std::os::unix::fs::symlink(apps_dir.join("sdk"), source.join("sdk")).unwrap();
    let studio = Studio::new(StudioPaths {
        store: dir.path().join("state/apps"),
        installed: apps_dir.clone(),
        source,
        bundler: "/nonexistent".into(),
    });
    studio.set_tools(Arc::new(glass_app_studio::testing::FakeBundler::default()));
    let org_apps = Arc::new(MemoryOrgApps::default());
    let registry = test_registry(
        dir.path(),
        &machine,
        &catalog,
        &directory,
        &settings,
        &chat,
        &meet,
        &mail,
        &studio,
        &policy,
    );
    // Policy names these providers; `down` is always rate limited (MP-8).
    let model = Arc::new(ScriptedProvider::echo());
    let gateway = Gateway::new().with("scripted", model.clone()).with(
        "down",
        Arc::new(ScriptedProvider::failing(ProviderError::Unavailable(
            "rate limited".into(),
        ))),
    );

    let rt = Runtime::new(RuntimeParts {
        config: RuntimeConfig {
            state_dir: dir.path().join("state"),
            catalog: vec![
                it_agent(),
                invoice_demo(),
                claude_code(),
                writer(),
                skill_evals(),
            ],
            agent_dirs: Vec::new(),
            max_live_tasks_per_user: 4,
            harness: Some("claude-code".parse().unwrap()),
            attended_dir: dir.path().join("run/attended"),
            apps: vec![
                mail_app(),
                calendar_app(),
                store_app(),
                settings_app(),
                chat_app(),
                meet_app(),
                artifacts_app(),
                docs_app(),
            ],
            // The app socket resolves callers by uid; their app data is
            // here, never in the real account's home.
            console_url: None,
            dev_home: Some(home.clone()),
            org_catalog: Arc::new(OrgCatalog(catalog.clone())),
            personal: Arc::new(studio.clone()),
            org_apps: org_apps.clone(),
        },
        policy,
        policy_source: "test".into(),
        registry,
        sandbox: Arc::new(HostSandbox),
        snapshots: Arc::new(CopySnapshotter::new(dir.path().join("snapshots"))),
        audit: Arc::new(
            AuditLog::open(dir.path().join("audit.log"), Durability::Buffered).unwrap(),
        ),
        gateway: Arc::new(gateway),
        settings: settings.clone(),
    });
    studio.set_surroundings(Arc::new(glass_runtime::StudioSurroundings::new(
        &rt,
        |_groups, _need| Vec::new(),
    )));
    rt.set_routines(routines(dir.path()));
    // New mail wakes Mail, as glassd wires it (ADR 0037).
    {
        let rt = Arc::downgrade(&rt);
        mail.on_notice(move |notice| {
            let glass_app_mail::Notice::Changed { login } = notice;
            if let Some(rt) = rt.upgrade() {
                rt.app_changed(login, "mail");
            }
        });
    }
    // News in chat wakes the app that shows it, as glassd wires it.
    {
        let rt = Arc::downgrade(&rt);
        chat.on_notice(move |notice| {
            if let (Some(rt), glass_app_chat::Notice::Changed { logins, kinds }) =
                (rt.upgrade(), notice)
            {
                for login in logins {
                    for kind in kinds {
                        rt.app_changed(login, kind.app());
                    }
                }
            }
        });
    }
    let me = Caller {
        name: account.name,
        uid: account.uid,
        gid: account.gid,
        home,
    };
    Harness {
        dir,
        rt,
        me,
        machine,
        model,
        catalog,
        directory,
        settings,
        chat,
        meet,
        mail,
        studio,
        org_apps,
    }
}

pub fn harness() -> Harness {
    harness_with("")
}

pub fn send_args(n: usize) -> Value {
    json!({ "messages": (0..n).map(|i| json!({
        "to": [format!("vendor{i}@example.com")], "subject": format!("Invoice {i} reminder"), "body": "Please pay."
    })).collect::<Vec<_>>() })
}

pub fn rebuild(h: &Harness, policy: &str) -> Arc<Runtime> {
    let mut demo = invoice_demo();
    demo.plan = Vec::new();
    rebuild_with(h, policy, vec![demo])
}

/// A new runtime on the same state, as after a restart with this policy and
/// catalog.
pub fn rebuild_with(h: &Harness, policy: &str, catalog: Vec<CatalogEntry>) -> Arc<Runtime> {
    let mut registry = Registry::new();
    registry.register(Arc::new(FilesProvider)).unwrap();
    registry
        .register(Arc::new(MailProvider::new(h.directory.clone())))
        .unwrap();
    Runtime::new(RuntimeParts {
        config: RuntimeConfig {
            state_dir: h.dir.path().join("state"),
            catalog,
            agent_dirs: Vec::new(),
            max_live_tasks_per_user: 4,
            harness: Some("claude-code".parse().unwrap()),
            attended_dir: h.dir.path().join("run/attended"),
            apps: Vec::new(),
            console_url: None,
            dev_home: None,
            org_catalog: Arc::new(glass_runtime::OpenCatalog),
            personal: Arc::new(glass_runtime::NoPersonalApps),
            org_apps: Arc::new(glass_runtime::NoOrgApps),
        },
        policy: Policy::from_toml(policy).unwrap(),
        policy_source: "test".into(),
        registry,
        sandbox: Arc::new(HostSandbox),
        snapshots: Arc::new(CopySnapshotter::new(h.dir.path().join("snapshots"))),
        audit: Arc::new(AuditLog::open(h.audit_path(), Durability::Buffered).unwrap()),
        gateway: Arc::new(Gateway::new()),
        settings: SettingsHandle::new(h.dir.path().join("state/settings")),
    })
}

/// The user's own Claude Code, driving through `glass-mcp` (attended).
pub fn claude_code() -> CatalogEntry {
    CatalogEntry {
        id: "claude-code".parse().unwrap(),
        name: "Claude Code".into(),
        version: "1".into(),
        model: String::new(),
        description: String::new(),
        mode: glass_api::AgentMode::Actions,
        program: PathBuf::new(),
        attended: true,
        driver: None,
        args: Vec::new(),
        plan: Vec::new(),
        customize: Vec::new(),
    }
}

pub fn it_agent() -> CatalogEntry {
    CatalogEntry {
        id: "it-agent".parse().unwrap(),
        name: "IT agent".into(),
        version: "0.1".into(),
        model: String::new(),
        description: String::new(),
        mode: glass_api::AgentMode::Actions,
        program: "/bin/sleep".into(),
        attended: false,
        driver: None,
        args: vec!["30".into()],
        plan: vec!["Check".into(), "Fix".into()],
        customize: Vec::new(),
    }
}

/// The invoice agent, with two settings users may change (TL-1).
pub fn invoice_demo() -> CatalogEntry {
    CatalogEntry {
        id: "invoice-demo".parse().unwrap(),
        name: "Invoice demo".into(),
        version: "0.1".into(),
        model: String::new(),
        description: String::new(),
        mode: glass_api::AgentMode::Actions,
        program: "/bin/sleep".into(),
        attended: false,
        driver: None,
        args: vec!["30".into()],
        plan: vec![
            "Read invoices".into(),
            "Write the summary".into(),
            "Remind vendors".into(),
        ],
        customize: vec![
            CustomizationPoint {
                key: "flag_over".into(),
                title: "Flag invoices over".into(),
                kind: PointKind::Number,
                choices: Vec::new(),
                default: None,
            },
            CustomizationPoint {
                key: "remind_vendors".into(),
                title: "Remind vendors".into(),
                kind: PointKind::Flag,
                choices: Vec::new(),
                default: Some(serde_json::Value::Bool(true)),
            },
        ],
    }
}

/// A model-backed agent (ADR 0018), whose users may pick a model in a
/// layer (TL-1). Policy decides whether that model may be used.
pub fn writer() -> CatalogEntry {
    CatalogEntry {
        id: "writer".parse().unwrap(),
        name: "Writer".into(),
        version: "1".into(),
        model: String::new(),
        description: String::new(),
        mode: glass_api::AgentMode::Actions,
        program: "/bin/sleep".into(),
        attended: false,
        driver: None,
        args: vec!["30".into()],
        plan: vec!["Read".into(), "Draft".into()],
        customize: vec![CustomizationPoint {
            key: "model".into(),
            title: "Model".into(),
            kind: PointKind::Text,
            choices: Vec::new(),
            default: None,
        }],
    }
}

/// The skill-evals agent (ADR 0034); the test plays it over its socket.
pub fn skill_evals() -> CatalogEntry {
    CatalogEntry {
        id: "skill-evals".parse().unwrap(),
        name: "Skill evals".into(),
        version: "1".into(),
        model: String::new(),
        description: String::new(),
        mode: glass_api::AgentMode::Actions,
        program: "/bin/sleep".into(),
        attended: false,
        driver: None,
        args: vec!["30".into()],
        plan: vec!["Read the skill".into(), "Run its cases".into()],
        customize: Vec::new(),
    }
}

/// The machine as the IT agent's actions see it (AE-4), without touching
/// the real one.
#[derive(Debug, Default)]
pub struct FakeMachine {
    pub restarted: std::sync::Mutex<Vec<String>>,
}

impl SystemOps for FakeMachine {
    fn network(&self) -> Result<system::NetworkStatus, String> {
        Err("not in tests".into())
    }
    fn resolve(&self, _host: &str) -> Result<Vec<std::net::IpAddr>, String> {
        Err("not in tests".into())
    }
    fn filesystem_usage(&self, _path: &std::path::Path) -> Result<system::FsUsage, String> {
        Ok(system::FsUsage {
            total_kb: 100,
            used_kb: 95,
            available_kb: 5,
        })
    }
    fn load(&self) -> Result<system::Load, String> {
        Err("not in tests".into())
    }
    fn top_processes(&self, _uid: u32) -> Result<Vec<system::Process>, String> {
        Ok(Vec::new())
    }
    fn service_enabled(&self, name: &str) -> bool {
        name == "ntpd"
    }
    fn restart_service(&self, name: &str) -> Result<(), String> {
        self.restarted.lock().unwrap().push(name.into());
        Ok(())
    }
}

/// The Mail app as its manifest declares it (apps/mail/manifest.json).
pub fn mail_app() -> AppManifest {
    serde_json::from_value(json!({
        "id": "mail", "name": "Mail", "version": "0.1.0",
        "actions": ["mail.account", "mail.list", "mail.read", "mail.draft", "mail.send", "mail.move", "mail.connect", "people.search"],
        "permissions": ["offline"], "labels": ["public", "internal"]
    }))
    .unwrap()
}

/// The Calendar app as its manifest declares it.
pub fn calendar_app() -> AppManifest {
    serde_json::from_value(json!({
        "id": "calendar", "name": "Calendar", "version": "0.1.0",
        "actions": ["calendar.list", "calendar.read", "calendar.create", "calendar.update", "calendar.invite", "people.search", "meet.list"],
        "permissions": ["offline"], "labels": ["public", "internal"]
    }))
    .unwrap()
}

/// The Store as its manifest declares it (DV-10, DV-11).
pub fn store_app() -> AppManifest {
    serde_json::from_value(json!({
        "id": "store", "name": "Store", "version": "0.1.0",
        "actions": ["catalog.list", "catalog.search", "catalog.install", "catalog.remove"],
        "permissions": ["offline"], "labels": ["public", "internal"]
    }))
    .unwrap()
}

/// The Settings app as its manifest declares it.
pub fn settings_app() -> AppManifest {
    serde_json::from_value(json!({
        "id": "settings", "name": "Settings", "version": "0.1.0",
        "actions": ["settings.list", "settings.set"],
        "permissions": ["offline"], "labels": ["public"]
    }))
    .unwrap()
}

/// Chat as its manifest declares it (ADR 0030): no offline storage, since
/// the page's storage is in the person's home.
pub fn chat_app() -> AppManifest {
    serde_json::from_value(json!({
        "id": "chat", "name": "Chat", "version": "0.1.0",
        "actions": ["chat.list", "chat.read", "chat.post", "chat.create", "chat.react", "chat.edit", "chat.delete", "chat.pin", "chat.topic", "chat.search", "chat.draft", "chat.notify", "chat.members", "people.search"],
        "permissions": [], "labels": ["public", "internal"]
    }))
    .unwrap()
}

/// Artifacts as ADR 0033 declares it (apps/artifacts/manifest.json).
pub fn artifacts_app() -> AppManifest {
    serde_json::from_value(json!({
        "id": "artifacts", "name": "Artifacts", "version": "0.1.0",
        "actions": ["artifact.list", "artifact.read", "artifact.share", "artifact.update", "artifact.comment", "artifact.resolve", "people.search", "chat.list"],
        "permissions": [], "labels": ["public", "internal", "confidential"]
    }))
    .unwrap()
}

/// Docs as ADR 0035 declares it (apps/docs/manifest.json): no
/// `doc.write`, since the page edits live (`app.live`).
pub fn docs_app() -> AppManifest {
    serde_json::from_value(json!({
        "id": "docs", "name": "Docs", "version": "0.1.0",
        "actions": ["doc.list", "doc.read", "doc.create", "doc.open", "doc.comment", "doc.resolve", "doc.version", "doc.roles", "people.search"],
        "permissions": [], "labels": ["public", "internal", "confidential"]
    }))
    .unwrap()
}

/// Meet as ADR 0030 declares it (CF-21): the four meeting actions, and the
/// camera and microphone (`media`), which only a meeting app may have.
pub fn meet_app() -> AppManifest {
    serde_json::from_value(json!({
        "id": "meet", "name": "Meet", "version": "0.1.0",
        "actions": ["meet.list", "meet.join", "meet.keys", "meet.leave"],
        "permissions": ["media"], "labels": ["public", "internal"]
    }))
    .unwrap()
}

/// The org's catalog as glassd gates by it (DV-11).
#[derive(Debug)]
pub struct OrgCatalog(pub CatalogHandle);

impl glass_runtime::CatalogGate for OrgCatalog {
    fn reaches(&self, groups: &[String], home: &Path, id: &glass_types::AgentId) -> bool {
        let catalog = self.0.current();
        catalog.is_none()
            || glass_app_catalog::reaches(
                catalog.as_deref(),
                groups,
                &glass_app_catalog::installed(home),
                id,
            )
    }

    fn lists(&self) -> std::collections::BTreeSet<glass_types::AgentId> {
        self.0
            .current()
            .map(|c| c.release.items.iter().map(|i| i.id.clone()).collect())
            .unwrap_or_default()
    }
}

/// The org apps glassd fetched (ADR 0032, R2), as a test sets them.
#[derive(Debug, Default)]
pub struct MemoryOrgApps(
    std::sync::Mutex<
        Vec<(
            glass_api::AppManifest,
            std::collections::BTreeMap<String, String>,
        )>,
    >,
);

impl MemoryOrgApps {
    pub fn set(
        &self,
        apps: Vec<(
            glass_api::AppManifest,
            std::collections::BTreeMap<String, String>,
        )>,
    ) {
        *self.0.lock().unwrap() = apps;
    }
}

impl glass_runtime::OrgApps for MemoryOrgApps {
    fn apps(&self) -> Vec<glass_api::AppManifest> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(m, _)| m.clone())
            .collect()
    }

    fn files(
        &self,
        id: &glass_types::AgentId,
    ) -> Option<(String, std::collections::BTreeMap<String, String>)> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|(m, _)| &m.id == id)
            .map(|(m, files)| (m.version.clone(), files.clone()))
    }
}
