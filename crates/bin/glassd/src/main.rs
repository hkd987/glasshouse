//! `glassd`: the system daemon. Loads config and policy, assembles the
//! runtime with its backends, cleans up after any earlier run, and serves the
//! control socket until told to stop.

mod config;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::Parser;
use glass_actions::Registry;
use glass_audit::{AuditEvent, AuditLog, Durability, EventKind};
use glass_platform::sandbox::{HostSandbox, JailConfig, JailSandbox, Sandbox};
use glass_platform::snapshot::{CopySnapshotter, Snapshotter, ZfsSnapshotter};
use glass_policy::Policy;
use glass_runtime::{ControlApi, Runtime, RuntimeConfig, RuntimeParts};
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
    /// Validate the configuration and policy, print warnings, and exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    let config = Config::load(&args.config)?;
    let (policy, policy_source) = load_policy(&config.policy)?;
    let registry = registry(&config, &policy)?;
    for warning in policy.lint(&registry.lint_info()) {
        tracing::warn!("policy: {warning}");
    }
    if args.check {
        tracing::info!(policy = %policy_source, agents = config.agents.len(), "configuration is valid");
        return Ok(());
    }
    run(config, policy, policy_source, registry).await
}

fn load_policy(path: &Path) -> anyhow::Result<(Policy, String)> {
    if path.exists() {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let policy =
            Policy::from_toml(&text).with_context(|| format!("policy {}", path.display()))?;
        Ok((policy, path.display().to_string()))
    } else {
        let policy =
            Policy::from_toml(glass_policy::DEFAULT_POLICY).context("built-in default policy")?;
        Ok((policy, "built-in default".into()))
    }
}

fn registry(config: &Config, policy: &Policy) -> anyhow::Result<Registry> {
    Ok(glass_first_party::registry(glass_first_party::Setup {
        // IT's fix list decides what the IT agent may restart (AE-4).
        restartable_services: policy.fixes().services.clone(),
        helpdesk_outbox: config.state_dir.join("helpdesk/outbox"),
        system: Arc::new(glass_app_system::HostSystem),
    })?)
}

async fn run(
    config: Config,
    policy: Policy,
    policy_source: String,
    registry: Registry,
) -> anyhow::Result<()> {
    let run_dir = config
        .control_socket
        .parent()
        .context("control socket needs a parent directory")?;
    for (dir, mode) in [(config.state_dir.as_path(), 0o700), (run_dir, 0o755)] {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode))?;
    }

    let sandbox: Arc<dyn Sandbox> = match config.sandbox.backend {
        SandboxBackend::Jail => {
            let mut jail = JailConfig {
                state_dir: run_dir.join("jails"),
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
    });
    audit.append(started)?;
    let mut loaded = AuditEvent::new(
        EventKind::PolicyLoaded,
        format!("Policy loaded from {policy_source}"),
    );
    loaded.detail = serde_json::json!({ "source": policy_source });
    audit.append(loaded)?;

    let runtime = Runtime::new(RuntimeParts {
        config: RuntimeConfig {
            state_dir: config.state_dir.clone(),
            catalog: config.agents.clone(),
            agent_dirs: config.sandbox.agent_dirs.clone(),
            max_live_tasks_per_user: config.max_live_tasks_per_user,
            harness: config.harness.clone(),
            attended_dir: run_dir.join("attended"),
        },
        policy,
        policy_source: policy_source.clone(),
        registry,
        sandbox: Arc::clone(&sandbox),
        snapshots,
        audit,
    });

    // A new agent version re-checks every layer on it (TL-13).
    runtime.recheck_layers();

    let listener = bind_control_socket(&config.control_socket)?;
    tracing::info!(
        socket = %config.control_socket.display(),
        sandbox = sandbox.kind(),
        policy = %policy_source,
        agents = config.agents.len(),
        "glassd ready"
    );

    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let served = tokio::select! {
        result = glass_rpc::serve(listener, Arc::new(ControlApi::new(Arc::clone(&runtime)))) => result.map_err(anyhow::Error::from),
        _ = terminate.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
    };
    tracing::info!("shutting down: stopping running agents");
    runtime.shutdown().await;
    let _ = std::fs::remove_file(&config.control_socket);
    served
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
