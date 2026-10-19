//! The pipeline tests' harness: a runtime on development backends (host
//! sandbox, copy snapshots) in a temporary directory, with the test playing
//! the agent over the task socket.

#![allow(dead_code, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use glass_actions::Registry;
use glass_api::{CustomizationPoint, PointKind, ScopeRequest, TaskRequest, TileState};
use glass_app_calendar::CalendarProvider;
use glass_app_files::FilesProvider;
use glass_app_helpdesk::HelpdeskProvider;
use glass_app_mail::MailProvider;
use glass_app_system::{SystemOps, SystemProvider};
use glass_audit::{AuditLog, Durability};
use glass_gateway::{Gateway, ModelAction, ProviderError, ScriptedProvider};
use glass_platform::sandbox::HostSandbox;
use glass_platform::snapshot::CopySnapshotter;
use glass_platform::system;
use glass_policy::Policy;
use glass_rpc::Client;
use glass_rpc::mcp::CallToolResult;
use glass_runtime::{Caller, CatalogEntry, Runtime, RuntimeConfig, RuntimeParts};
use glass_types::TaskId;
use serde_json::{Value, json};

pub struct Harness {
    pub dir: tempfile::TempDir,
    pub rt: Arc<Runtime>,
    pub me: Caller,
    pub machine: Arc<FakeMachine>,
    /// The model provider `scripted` in policy reaches; what it was sent.
    pub model: Arc<ScriptedProvider>,
}

impl Harness {
    pub fn home(&self) -> PathBuf {
        self.me.home.clone()
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

    let policy_text = format!(
        "{}\n[[label]]\npath = \"{}/Finance\"\nlabel = \"confidential\"\n{}",
        glass_policy::DEFAULT_POLICY,
        home.display(),
        policy_extra
            .replace("{me}", &account.name)
            .replace("{home}", &home.display().to_string())
    );
    let policy = Policy::from_toml(&policy_text).unwrap();
    let mut registry = Registry::new();
    registry.register(Arc::new(FilesProvider)).unwrap();
    registry.register(Arc::new(MailProvider)).unwrap();
    registry.register(Arc::new(CalendarProvider)).unwrap();
    let machine = Arc::new(FakeMachine::default());
    let ops: Arc<dyn SystemOps> = machine.clone();
    registry
        .register(Arc::new(SystemProvider::new(ops, vec!["ntpd".into()])))
        .unwrap();
    registry
        .register(Arc::new(HelpdeskProvider::new(dir.path().join("outbox"))))
        .unwrap();
    registry.register(Arc::new(ModelAction)).unwrap();
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
            catalog: vec![it_agent(), invoice_demo(), claude_code(), writer()],
            agent_dirs: Vec::new(),
            max_live_tasks_per_user: 4,
            harness: Some("claude-code".parse().unwrap()),
            attended_dir: dir.path().join("run/attended"),
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
    });
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
    registry.register(Arc::new(MailProvider)).unwrap();
    Runtime::new(RuntimeParts {
        config: RuntimeConfig {
            state_dir: h.dir.path().join("state"),
            catalog,
            agent_dirs: Vec::new(),
            max_live_tasks_per_user: 4,
            harness: Some("claude-code".parse().unwrap()),
            attended_dir: h.dir.path().join("run/attended"),
        },
        policy: Policy::from_toml(policy).unwrap(),
        policy_source: "test".into(),
        registry,
        sandbox: Arc::new(HostSandbox),
        snapshots: Arc::new(CopySnapshotter::new(h.dir.path().join("snapshots"))),
        audit: Arc::new(AuditLog::open(h.audit_path(), Durability::Buffered).unwrap()),
        gateway: Arc::new(Gateway::new()),
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
