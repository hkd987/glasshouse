//! Samples the machine every two seconds on its own thread: the
//! processors and memory, the person's processes, and their agents from
//! glassd. It also carries out what the window asks: stop an agent
//! (`task.stop`, the same call as the tile's Stop) or quit an app.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glass_api::{TaskRef, TaskView, methods};
use glass_platform::system;
use glass_rpc::{Client, ClientError};
use glass_types::TaskId;
use glass_window::Waker;
use tokio::sync::mpsc;

use crate::model::{self, Snapshot};

/// How often the numbers change.
pub const EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Stop(TaskId),
    Quit(u32),
}

/// The window's side: the latest sample, and a way to ask.
pub struct Sampler {
    latest: Arc<Mutex<Shared>>,
    requests: mpsc::UnboundedSender<Request>,
}

#[derive(Default)]
struct Shared {
    snapshot: Option<Snapshot>,
    /// What went wrong with the last request, said once.
    problem: Option<String>,
}

impl Sampler {
    pub fn start(waker: Waker, socket: PathBuf, uid: u32) -> anyhow::Result<Self> {
        let latest = Arc::new(Mutex::new(Shared::default()));
        let (requests, asked) = mpsc::unbounded_channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let shared = Arc::clone(&latest);
        std::thread::Builder::new()
            .name("sampler".into())
            .spawn(move || runtime.block_on(run(shared, asked, waker, socket, uid)))?;
        Ok(Self { latest, requests })
    }

    /// The newest sample and any problem, if they came since last asked.
    pub fn take(&self) -> (Option<Snapshot>, Option<String>) {
        let mut shared = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        (shared.snapshot.take(), shared.problem.take())
    }

    pub fn ask(&self, request: Request) {
        let _ = self.requests.send(request);
    }
}

async fn run(
    shared: Arc<Mutex<Shared>>,
    mut asked: mpsc::UnboundedReceiver<Request>,
    waker: Waker,
    socket: PathBuf,
    uid: u32,
) {
    let own = std::process::id();
    let mut glassd: Option<Client> = None;
    let mut before = system::cpu_time().ok();
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            request = asked.recv() => {
                let Some(request) = request else { return };
                let problem = carry_out(&mut glassd, &socket, request).await.err();
                shared.lock().unwrap_or_else(|e| e.into_inner()).problem = problem;
                // Sample now, so what was stopped goes at once.
                tick.reset();
            }
        }
        let now = system::cpu_time().ok();
        let cpu_percent = before.zip(now).map(|(b, n)| n.percent_busy_since(&b));
        before = now;
        let processes = system::user_processes(uid).unwrap_or_default();
        let tasks = tasks(&mut glassd, &socket).await;
        let (agents, apps) = model::group(&processes, tasks.as_deref().unwrap_or(&[]), own);
        let snapshot = Snapshot {
            cpu_percent,
            memory: system::memory().unwrap_or_default(),
            agents,
            apps,
            agents_known: tasks.is_some(),
        };
        shared.lock().unwrap_or_else(|e| e.into_inner()).snapshot = Some(snapshot);
        waker.wake();
    }
}

/// The person's tasks, connecting to glassd again if it went away.
async fn tasks(glassd: &mut Option<Client>, socket: &PathBuf) -> Option<Vec<TaskView>> {
    let client = connected(glassd, socket).await?;
    match client.call(methods::TASK_LIST, ()).await {
        Ok(tasks) => Some(tasks),
        Err(_) => {
            *glassd = None;
            None
        }
    }
}

async fn connected<'a>(glassd: &'a mut Option<Client>, socket: &PathBuf) -> Option<&'a Client> {
    if glassd.is_none() {
        *glassd = Client::connect(socket).await.ok();
    }
    glassd.as_ref()
}

async fn carry_out(
    glassd: &mut Option<Client>,
    socket: &PathBuf,
    request: Request,
) -> Result<(), String> {
    match request {
        Request::Stop(task) => {
            let client = connected(glassd, socket)
                .await
                .ok_or("The system isn't answering, so the agent wasn't stopped.")?;
            client
                .call::<TaskView>(methods::TASK_STOP, TaskRef { task })
                .await
                .map(|_| ())
                .map_err(|e| match e {
                    // glassd's own refusal reads as a sentence.
                    ClientError::Rpc(e) => e.message,
                    _ => "The system isn't answering, so the agent wasn't stopped.".into(),
                })
        }
        // kill(1) from the base system (ADR 0007): only the person's own
        // processes, so nothing here needs more than their rights.
        Request::Quit(pid) => tokio::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await
            .ok()
            .filter(|s| s.success())
            .map(|_| ())
            .ok_or_else(|| "That app had already quit.".to_owned()),
    }
}
