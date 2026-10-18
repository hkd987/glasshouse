//! Running one task's agent: set up, run, and always tear down.
//!
//! Setup prepares app state, takes the opening snapshot, creates the
//! sandbox, binds the task socket inside it and starts the agent. Teardown
//! runs whatever happened: it closes the socket, withdraws open asks and task
//! grants, and destroys the sandbox, so nothing the task was given outlives
//! it (PA-2).

use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;

use glass_api::{ActivityKind, TileState};
use glass_audit::EventKind;
use glass_platform::sandbox::{AGENT_SOCKET_PATH, ProcessSpec, WORK_DIR};
use tokio::net::UnixListener;
use tokio::sync::oneshot;

use crate::Runtime;
use crate::agent_api::AgentApi;
use crate::task::{Gate, Task};

enum Exit {
    Finished(ExitStatus),
    Stopped,
}

pub(crate) async fn run(rt: Arc<Runtime>, task: Arc<Task>, stop: oneshot::Receiver<()>) {
    let log = rt
        .config
        .state_dir
        .join("tasks")
        .join(task.id.to_string())
        .join("agent.log");
    let mut server = None;
    let result = setup_and_wait(&rt, &task, &log, &mut server, stop).await;

    // Teardown, whatever happened above.
    if let Some(server) = server {
        server.abort();
    }
    task.gate.send_replace(Gate::Ended);
    rt.desk.end_task(task.id);
    let teardown_error = match &task.driver_socket {
        // An attended task has no sandbox, only its socket.
        Some(socket) => match fs::remove_file(socket) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Some(e.to_string()),
            _ => None,
        },
        None => {
            let sandbox = Arc::clone(&rt.sandbox);
            let spec = task.sandbox.clone();
            match tokio::task::spawn_blocking(move || sandbox.destroy(&spec)).await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(e) => Some(e.to_string()),
            }
        }
    };
    if let Some(e) = &teardown_error {
        tracing::error!(task = %task.id, error = %e, "sandbox teardown failed");
    }

    let summary = {
        let mut state = task.lock();
        match (&result, state.tile) {
            // Set by Stop, or by hand back on an attended task.
            (_, TileState::Stopped | TileState::Done) => {}
            (Ok(Exit::Finished(status)), _) if status.success() => {
                // Keep the agent's last caption: it is usually its summary.
                state.tile = TileState::Done;
                state.progress = None;
            }
            (Ok(Exit::Finished(status)), _) => {
                state.tile = TileState::Failed;
                state.failure = Some(failure_reason(*status, &log));
            }
            (Ok(Exit::Stopped), _) => state.tile = TileState::Stopped,
            (Err(e), _) => {
                state.tile = TileState::Failed;
                state.failure = Some(e.clone());
            }
        }
        if let Some(e) = &teardown_error {
            state.note(ActivityKind::Notice, format!("Sandbox cleanup failed: {e}"));
        }
        let line = match state.tile {
            TileState::Done => "Finished".to_owned(),
            TileState::Stopped => "Stopped".to_owned(),
            _ => format!("Failed: {}", state.failure.as_deref().unwrap_or("unknown")),
        };
        state.note(ActivityKind::State, line.clone());
        line
    };
    let mut event = rt.task_event(&task, EventKind::TaskFinished, summary);
    let receipt = task.lock().receipt();
    event.detail = serde_json::json!({
        "actions": receipt.actions,
        "changes": receipt.changes,
        "undoable": receipt.undoable,
        "left_machine": receipt.left_machine,
        "flagged": receipt.flagged,
        "sandbox_teardown_error": teardown_error,
    });
    rt.audit(event);
    rt.publish(&task);
}

async fn setup_and_wait(
    rt: &Arc<Runtime>,
    task: &Arc<Task>,
    log: &Path,
    server: &mut Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    stop: oneshot::Receiver<()>,
) -> Result<Exit, String> {
    // App state (mail folders and the like) must exist before snapshots.
    let providers = rt.registry.providers();
    let ctx = task.ctx.clone();
    tokio::task::spawn_blocking(move || providers.iter().try_for_each(|p| p.prepare(&ctx)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("preparing apps: {e}"))?;

    let writable: Vec<PathBuf> = task
        .ctx
        .grants
        .iter()
        .filter(|g| g.writable)
        .map(|g| g.root.clone())
        .collect();
    if !writable.is_empty() {
        rt.take_snapshot(task, &writable, "Task started")
            .await
            .map_err(|e| format!("snapshot failed: {e}"))?;
    }

    if let Some(socket) = &task.driver_socket {
        return drive_attended(rt, task, socket, server, stop).await;
    }

    let sandbox = Arc::clone(&rt.sandbox);
    let spec = task.sandbox.clone();
    tokio::task::spawn_blocking(move || sandbox.create(&spec))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("sandbox: {e}"))?;

    let socket = rt
        .sandbox
        .host_path(&task.sandbox, Path::new(AGENT_SOCKET_PATH));
    let listener = bind_task_socket(&socket, task).map_err(|e| format!("task socket: {e}"))?;
    *server = Some(tokio::spawn(glass_rpc::serve(
        listener,
        Arc::new(AgentApi {
            rt: Arc::clone(rt),
            task: task.id,
        }),
    )));

    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("task log: {e}"))?;
    }
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| format!("task log: {e}"))?;
    let process = ProcessSpec {
        program: task.entry.program.clone(),
        args: task.entry.args.clone(),
        env: agent_env(rt, task),
    };
    let mut command = tokio::process::Command::from(rt.sandbox.command(&task.sandbox, &process));
    command
        .stdin(Stdio::null())
        .stdout(log_file.try_clone().map_err(|e| e.to_string())?)
        .stderr(log_file)
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|e| format!("starting {}: {e}", task.entry.program.display()))?;

    {
        let mut state = task.lock();
        if state.tile == TileState::Starting {
            state.tile = TileState::Working;
            state.caption = "Getting started".into();
        }
        // Users read this line, so it names no backend (BR-1); the audit
        // record carries the backend for security teams.
        state.note(
            ActivityKind::State,
            if rt.sandbox.is_security_boundary() {
                "Agent running in its own sealed sandbox"
            } else {
                "Agent running WITHOUT containment (development mode)"
            },
        );
    }
    rt.publish(task);

    tokio::select! {
        status = child.wait() => status.map(Exit::Finished).map_err(|e| e.to_string()),
        _ = stop => {
            let _ = child.kill().await;
            Ok(Exit::Stopped)
        }
    }
}

/// An attended task: the user's own harness connects to its socket through
/// `glass-mcp`, so there is no sandbox or program, and the task lasts until
/// the user hands back or stops it.
async fn drive_attended(
    rt: &Arc<Runtime>,
    task: &Arc<Task>,
    socket: &Path,
    server: &mut Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    stop: oneshot::Receiver<()>,
) -> Result<Exit, String> {
    if let Some(dir) = socket.parent() {
        // Root's, and traversable so each user reaches their own socket.
        fs::create_dir_all(dir).map_err(|e| format!("task socket: {e}"))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("task socket: {e}"))?;
    }
    let listener = bind_task_socket(socket, task).map_err(|e| format!("task socket: {e}"))?;
    *server = Some(tokio::spawn(glass_rpc::serve(
        listener,
        Arc::new(AgentApi {
            rt: Arc::clone(rt),
            task: task.id,
        }),
    )));
    {
        let mut state = task.lock();
        if state.tile == TileState::Starting {
            state.tile = TileState::Working;
            state.caption = format!("Waiting for {} to connect", task.entry.name);
        }
        state.note(
            ActivityKind::State,
            format!(
                "You're driving in {}: connect it with glass-mcp {}",
                task.entry.name,
                socket.display()
            ),
        );
    }
    rt.publish(task);
    let _ = stop.await;
    Ok(Exit::Stopped)
}

/// Binds the task socket so only the task's user (and root) can connect.
fn bind_task_socket(path: &Path, task: &Task) -> std::io::Result<UnixListener> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::os::unix::fs::chown(path, Some(task.user.uid), Some(task.user.gid))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// The agent's whole environment. Nothing leaks in from the daemon.
fn agent_env(rt: &Runtime, task: &Task) -> Vec<(String, String)> {
    let inside = |p: &str| {
        rt.sandbox
            .agent_path(&task.sandbox, Path::new(p))
            .display()
            .to_string()
    };
    vec![
        ("GLASS_AGENT_SOCKET".into(), inside(AGENT_SOCKET_PATH)),
        ("GLASS_TASK_ID".into(), task.id.to_string()),
        ("GLASS_TASK_PROMPT".into(), task.prompt.clone()),
        // The agent's customization points: defaults and the user's layer.
        (
            "GLASS_AGENT_SETTINGS".into(),
            serde_json::to_string(&task.settings).unwrap_or_else(|_| "{}".into()),
        ),
        ("GLASS_WORK".into(), inside(WORK_DIR)),
        ("HOME".into(), inside(WORK_DIR)),
        ("TMPDIR".into(), inside("/tmp")),
        ("PATH".into(), "/bin:/usr/bin".into()),
        ("LANG".into(), "C.UTF-8".into()),
    ]
}

fn failure_reason(status: ExitStatus, log: &Path) -> String {
    let last = fs::read_to_string(log).ok().and_then(|text| {
        text.lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(str::to_owned)
    });
    match last {
        Some(line) => format!("the agent exited ({status}): {line}"),
        None => format!("the agent exited ({status})"),
    }
}
