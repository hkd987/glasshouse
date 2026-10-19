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

mod config;

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
fn gateway(config: &Config, policy: &Policy) -> Models {
    let mut notes = Vec::new();
    let secrets = match Secrets::load(
        &config.model_secrets,
        glass_platform::users::effective_uid(),
    ) {
        Ok(secrets) => secrets,
        Err(e) => {
            notes.push(e.to_string());
            Secrets::default()
        }
    };
    let (gateway, more) =
        glass_gateway_providers::connect(&policy.document().models.providers, &secrets);
    notes.extend(more);
    for note in &notes {
        tracing::warn!("models: {note}");
    }
    Models {
        gateway: Arc::new(gateway),
        notes,
    }
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
    store: PolicyStore,
    registry: Registry,
    models: Models,
    /// Installed apps whose manifests check out (UI-5, DV-3).
    apps: Vec<AppManifest>,
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
    let registry = registry(&config, &loaded.policy)?;
    for warning in loaded.policy.lint(&registry.lint_info()) {
        tracing::warn!("policy: {warning}");
    }
    let models = gateway(&config, &loaded.policy);
    let (apps, skipped) = config.load_apps(&registry);
    for reason in &skipped {
        tracing::warn!("app not installed: {reason}");
    }
    Ok(Prepared {
        config,
        loaded,
        store,
        registry,
        models,
        apps,
    })
}

/// One line on the system console (BR-1: it names Glasshouse, never the
/// base system). Best effort: there may be no console to write to.
fn console(message: &str) {
    if let Ok(mut console) = std::fs::OpenOptions::new().write(true).open("/dev/console") {
        let _ = writeln!(console, "\r\nGlasshouse: {message}\r");
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

async fn run(prepared: Prepared) -> anyhow::Result<()> {
    let Prepared {
        config,
        loaded,
        store,
        registry,
        models,
        apps,
    } = prepared;
    let policy_source = loaded.origin.to_string();
    let run_dir = config
        .control_socket
        .parent()
        .context("control socket needs a parent directory")?;
    for (dir, mode) in [(config.state_dir.as_path(), 0o700), (run_dir, 0o755)] {
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
                state_dir: run_dir.join("jails"),
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
            dev_home: config.dev_home.clone(),
        },
        policy: loaded.policy.clone(),
        policy_source: policy_source.clone(),
        registry,
        sandbox: Arc::clone(&sandbox),
        snapshots,
        audit,
        gateway: models.gateway,
    });

    // A new agent version re-checks every layer on it (TL-13).
    runtime.recheck_layers();

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
    // writes and syncs files, so it happens off the start path.
    tokio::task::spawn_blocking(move || match store.remember(&loaded) {
        Ok(true) => tracing::info!("recorded the policy in force as the last good policy"),
        Ok(false) => {}
        Err(e) => tracing::warn!("couldn't record the last good policy: {e}"),
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
