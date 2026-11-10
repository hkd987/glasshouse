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
use crate::output::{Output, Sink};
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
    let mut output = None;
    let result = setup_and_wait(&rt, &task, &log, &mut server, &mut output, stop).await;

    // Teardown, whatever happened above.
    if let Some(server) = server {
        server.abort();
    }
    task.gate.send_replace(Gate::Ended);
    rt.desk.end_task(task.id);
    let teardown_error = match &task.driver_socket {
        // An app session has neither: its calls came over the app socket.
        _ if task.is_app() => None,
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
    // The last of what the agent printed, now that nothing of it runs.
    let last_line = match output {
        Some(output) => output.close().await,
        None => None,
    };

    // An ended task's calls, compiled and let go; a finished one's code is
    // kept, and compared with the last time the same request was done (ADR
    // 0046). Before the tile says Done, so that what it says then is all
    // there: the note that it repeats, the code to save.
    let tile = task.lock().tile;
    let finished = match (&result, tile) {
        (_, TileState::Done) => true,
        (_, TileState::Stopped) => false,
        (Ok(Exit::Finished(status)), _) => status.success(),
        _ => false,
    };
    let repeats = {
        let (rt, ended) = (Arc::clone(&rt), Arc::clone(&task));
        tokio::task::spawn_blocking(move || rt.remember_flow(&ended, finished))
            .await
            .ok()
            .flatten()
    };

    let summary = {
        let mut state = task.lock();
        // Ended while paused for sleep: nothing for the wake to resume.
        state.sleeping = None;
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
                state.failure = Some(failure_reason(*status, last_line.as_deref()));
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
            TileState::Done if task.is_app() => "Closed".to_owned(),
            TileState::Done => "Finished".to_owned(),
            TileState::Stopped => "Stopped".to_owned(),
            _ => format!("Failed: {}", state.failure.as_deref().unwrap_or("unknown")),
        };
        state.note(ActivityKind::State, line.clone());
        line
    };
    // A routine's run: how it went, beside the routine (ADR 0047).
    if task.routine.is_some() {
        let (rt, ended) = (Arc::clone(&rt), Arc::clone(&task));
        let _ = tokio::task::spawn_blocking(move || rt.remember_run(&ended)).await;
    }
    // A schedule's run: how it went, kept with the schedule (ADR 0069).
    if task.schedule.is_some() {
        let (rt, ended) = (Arc::clone(&rt), Arc::clone(&task));
        let _ = tokio::task::spawn_blocking(move || rt.remember_schedule_run(&ended)).await;
    }
    let mut event = rt.task_event(&task, EventKind::TaskFinished, summary);
    let receipt = task.lock().receipt();
    event.detail = serde_json::json!({
        "actions": receipt.actions,
        "changes": receipt.changes,
        "undoable": receipt.undoable,
        "left_machine": receipt.left_machine,
        "flagged": receipt.flagged,
        "repeats": repeats,
        "sandbox_teardown_error": teardown_error,
    });
    rt.audit(event);
    rt.publish(&task);
    rt.forget_if_unchanged(&task);
    // The task as it ended, on disk: it and its undo outlive this glassd
    // (ADR 0020).
    let (rt, ended) = (Arc::clone(&rt), Arc::clone(&task));
    let _ = tokio::task::spawn_blocking(move || rt.keep(&ended)).await;
}

async fn setup_and_wait(
    rt: &Arc<Runtime>,
    task: &Arc<Task>,
    log: &Path,
    server: &mut Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    output: &mut Option<Output>,
    stop: oneshot::Receiver<()>,
) -> Result<Exit, String> {
    // An app session was prepared as it opened, so its first call finds
    // the app's folders, and it has no grants to snapshot.
    if task.is_app() {
        return drive_app(rt, task, stop).await;
    }
    // App state (mail folders and the like) must exist before snapshots.
    let providers = rt.registry().providers();
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

    // A stop that came while the task was being prepared: nothing more is
    // made, and the agent never starts (AW-11, specs/tla/Containment.tla).
    if task.ended() {
        return Ok(Exit::Stopped);
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
    // One task's socket, its agent's alone: a handful of connections and
    // 32 calls under way between them, however many it asks for.
    *server = Some(tokio::spawn(glass_rpc::serve_within(
        listener,
        Arc::new(AgentApi {
            rt: Arc::clone(rt),
            task: task.id,
        }),
        glass_rpc::Limits::ONE_TASK,
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
    // The look before the agent starts: a stop by now keeps it from
    // starting. One landing after is seen by the wait below, which kills it.
    if task.ended() {
        return Ok(Exit::Stopped);
    }
    let mut command = tokio::process::Command::from(rt.sandbox.command(&task.sandbox, &process));
    // On an agent computer its person watches the screen, so the output
    // goes there too (ADR 0065).
    let screen = match rt.sandbox.screen(&task.sandbox) {
        Some(path) => Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("the computer's screen: {e}"))?,
        ),
        None => None,
    };
    // What it prints comes through glassd, which keeps the log to a size
    // (`output`). One pipe for both streams, so their lines stay in the
    // order the agent wrote them.
    let (printed, prints) = std::io::pipe().map_err(|e| format!("task log: {e}"))?;
    command
        .stdin(Stdio::null())
        .stdout(prints.try_clone().map_err(|e| format!("task log: {e}"))?)
        .stderr(prints)
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|e| format!("starting {}: {e}", task.entry.program.display()))?;
    // glassd's own ends of the pipe go with the command: the output ends
    // when the agent's do.
    drop(command);
    let printed = tokio::net::unix::pipe::Receiver::from_owned_fd(printed.into())
        .map_err(|e| format!("task log: {e}"))?;
    *output = Some(Output::copy(printed, Sink::new(log_file, screen)));

    {
        let mut state = task.lock();
        if state.tile == TileState::Starting {
            state.tile = TileState::Working;
            state.caption = "Getting started".into();
        }
        // Users read this line, so it names no backend (BR-1); the audit
        // record carries the backend for security teams. A task stopped
        // just as its agent started doesn't say it runs.
        if state.tile.is_live() {
            state.note(ActivityKind::State, rt.sandbox.running_note());
        }
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
    // One task's socket, its agent's alone: a handful of connections and
    // 32 calls under way between them, however many it asks for.
    *server = Some(tokio::spawn(glass_rpc::serve_within(
        listener,
        Arc::new(AgentApi {
            rt: Arc::clone(rt),
            task: task.id,
        }),
        glass_rpc::Limits::ONE_TASK,
    )));
    {
        let mut state = task.lock();
        if state.tile == TileState::Starting {
            state.tile = TileState::Working;
            state.caption = format!("Waiting for {} to connect", task.entry.name);
        }
        // A driver starts it for the person (ADR 0042); otherwise they
        // connect their own harness.
        let note = if task.entry.driver.is_some() {
            format!("Waiting for {} to start", task.entry.name)
        } else {
            format!(
                "You're driving in {}: connect it with glass-mcp {}",
                task.entry.name,
                socket.display()
            )
        };
        state.note(ActivityKind::State, note);
    }
    rt.publish(task);
    let _ = stop.await;
    Ok(Exit::Stopped)
}

/// An app session (ADR 0024): no sandbox, no program and no socket of its
/// own. The app's calls arrive on the app socket, and the session lasts
/// until the app closes it, its connection drops, or the user stops it.
async fn drive_app(
    rt: &Arc<Runtime>,
    task: &Arc<Task>,
    stop: oneshot::Receiver<()>,
) -> Result<Exit, String> {
    {
        let mut state = task.lock();
        if state.tile == TileState::Starting {
            state.tile = TileState::Working;
            state.caption = "Open".into();
        }
    }
    rt.publish(task);
    let _ = stop.await;
    Ok(Exit::Stopped)
}

/// Binds the task socket so only the task's user (and root) can connect.
/// It's made under another name and renamed into place once it listens and
/// is theirs: whoever waits for it to appear finds it ready, never in the
/// moment between its file and its listening (a refused connection).
fn bind_task_socket(path: &Path, task: &Task) -> std::io::Result<UnixListener> {
    let making = path.with_extension("sock.new");
    for old in [path, making.as_path()] {
        if old.exists() {
            fs::remove_file(old)?;
        }
    }
    let listener = UnixListener::bind(&making)?;
    std::os::unix::fs::chown(&making, Some(task.user.uid), Some(task.user.gid))?;
    fs::set_permissions(&making, fs::Permissions::from_mode(0o600))?;
    fs::rename(&making, path)?;
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
        // Its local time: the calendar's times are the computer's (ADR 0031).
        (
            "GLASS_UTC_OFFSET".into(),
            glass_platform::system::utc_offset_secs()
                .unwrap_or(0)
                .to_string(),
        ),
        // A routine's run (DA-4): the agent reads it and runs its script.
        (
            "GLASS_ROUTINE".into(),
            task.routine
                .as_ref()
                .map(|r| r.id.clone())
                .unwrap_or_default(),
        ),
        // Its own computer, kept between its tasks (ADR 0066): the agent
        // may run commands there. Empty in a sandbox that's gone after.
        (
            "GLASS_COMPUTER".into(),
            match (&task.sandbox.computer, rt.sandbox.keeps_computers()) {
                (Some(id), true) => id.clone(),
                (None, true) => "task".into(),
                _ => String::new(),
            },
        ),
        ("GLASS_WORK".into(), inside(WORK_DIR)),
        ("HOME".into(), inside(WORK_DIR)),
        ("TMPDIR".into(), inside("/tmp")),
        ("PATH".into(), "/bin:/usr/bin".into()),
        ("LANG".into(), "C.UTF-8".into()),
    ]
}

/// Why a task failed: how its agent exited and the last line it printed
/// (`output` keeps that line, at a line's length).
fn failure_reason(status: ExitStatus, last_line: Option<&str>) -> String {
    match last_line {
        Some(line) => format!("the agent exited ({status}): {line}"),
        None => format!("the agent exited ({status})"),
    }
}
