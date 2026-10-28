//! Routines (DA-4, NV-21, ADR 0044): a task the person saved, run again
//! when they start it or on its schedule, by the agent that saved it,
//! with the scope it was saved with. Each run is a task like any other:
//! policy, tiers, asks, audit and undo as they are when it runs.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use glass_api::{RoutineView, ScopeRequest, TaskRequest, TaskView, TileState};
use glass_app_routines::{APP, Host, Outcome, Repeat, Routine, Routines, Run, Weekday, store};
use glass_types::{TaskId, Timestamp};

use crate::runtime::Origin;
use crate::task::{RoutineRun, Task};
use crate::{Caller, Runtime, RuntimeError};

type Result<T> = std::result::Result<T, RuntimeError>;

/// The longest a run's answer is kept in its record, in characters.
const SAID_CHARS: usize = 200;

impl Runtime {
    /// Where people's routines are kept, set once at startup, and the
    /// runtime as what the Routines app asks of it (ADR 0047).
    pub fn set_routines(self: &Arc<Self>, routines: Routines) {
        routines.set_host(Arc::new(RoutinesHost {
            runtime: Arc::downgrade(self),
            tokio: tokio::runtime::Handle::try_current().ok(),
        }));
        // Runs going when glassd last stopped ended with it: kept as cut
        // off (ADR 0051).
        let cut_off = routines.recover();
        if cut_off > 0 {
            tracing::info!(cut_off, "kept the routine runs a restart cut off");
        }
        let _ = self.routines.set(routines);
    }

    /// At a routine's run's end: how it went, kept beside the routine
    /// (ADR 0047).
    pub(crate) fn remember_run(&self, task: &Task) {
        let (Some(run), Some(routines)) = (&task.routine, self.routines.get()) else {
            return;
        };
        let record = {
            let state = task.lock();
            let receipt = state.receipt();
            let said = state
                .answer
                .as_deref()
                .and_then(|a| a.lines().find(|l| !l.trim().is_empty()))
                .unwrap_or_default();
            Run {
                task: task.id,
                version: run.version,
                scheduled: run.scheduled,
                started_at: task.started_at,
                ended_at: Timestamp::now(),
                outcome: match state.tile {
                    TileState::Done => Outcome::Done,
                    TileState::Stopped => Outcome::Stopped,
                    _ => Outcome::Failed,
                },
                model: !receipt.models.is_empty(),
                said: said.trim().chars().take(SAID_CHARS).collect(),
                changes: receipt.changes,
            }
        };
        if let Err(e) = routines.record_run(&task.user.name, &run.id, record) {
            tracing::warn!(task = %task.id, routine = %run.id, "keeping the routine's run: {e}");
        }
        // Routines' windows show how it went without asking (ADR 0047).
        self.app_changed(&task.user.name, APP);
    }

    fn current_routines(&self, caller: &Caller) -> Vec<Routine> {
        self.routines
            .get()
            .map(|r| r.current(&caller.name, &caller.home))
            .unwrap_or_default()
    }

    /// The caller's routines, at the versions they chose.
    pub fn routines_of(&self, caller: &Caller) -> Vec<RoutineView> {
        self.current_routines(caller)
            .into_iter()
            .map(view)
            .collect()
    }

    /// Starts one of the caller's routines now (DA-4).
    pub fn run_routine(self: &Arc<Self>, caller: &Caller, id: &str) -> Result<TaskView> {
        self.start_routine(caller, id, false)
    }

    /// Starts a routine as its own task. What the person chose is checked
    /// and the run started in one step under the choices lock (ADR 0051):
    /// a routine runs one at a time, and its schedule never starts one the
    /// person paused, whatever they do meanwhile.
    fn start_routine(
        self: &Arc<Self>,
        caller: &Caller,
        id: &str,
        scheduled: bool,
    ) -> Result<TaskView> {
        let started = store::locked(|| self.start_routine_locked(caller, id, scheduled))?;
        // Routines' windows show it running.
        self.app_changed(&caller.name, APP);
        Ok(started)
    }

    fn start_routine_locked(
        self: &Arc<Self>,
        caller: &Caller,
        id: &str,
        scheduled: bool,
    ) -> Result<TaskView> {
        let version = store::chosen(&caller.home).get(id).copied();
        let routine = version
            .and_then(|v| self.routines.get()?.record(&caller.name, id, v))
            .ok_or_else(|| RuntimeError::NotFound(format!("you have no routine {id}")))?;
        if let Some(task) = self.routines_running(&caller.name).get(id) {
            return Err(RuntimeError::Conflict(format!(
                "{} is running now (task {}): a routine runs one at a time",
                routine.name,
                task.short()
            )));
        }
        if scheduled && store::paused(&caller.home).contains(id) {
            return Err(RuntimeError::Conflict(format!(
                "{} is paused: it runs only when started",
                routine.name
            )));
        }
        // The request it was saved with, as a new task: policy and the
        // folders' access are checked again, as for any start.
        let request = TaskRequest {
            persona: None,
            agent: routine.agent.clone(),
            prompt: routine.request.clone(),
            scope: ScopeRequest {
                read: routine.read.clone(),
                write: routine.write.clone(),
                restrict: Vec::new(),
            },
            title: Some(routine.name.clone()),
            computer: None,
        };
        let run = RoutineRun {
            id: routine.id.clone(),
            version: routine.version,
            scheduled,
        };
        let started = self.start_task(caller, request, Origin::Routine(run))?;
        // Going: kept even if glassd stops before its end.
        if let Some(routines) = self.routines.get() {
            let now = Timestamp::now();
            let going = Run {
                task: started.id,
                version: routine.version,
                scheduled,
                started_at: now,
                ended_at: now,
                outcome: Outcome::Failed,
                model: false,
                said: String::new(),
                changes: 0,
            };
            if let Err(e) = routines.run_started(&caller.name, &routine.id, going) {
                tracing::warn!(task = %started.id, routine = %routine.id, "noting the routine's run: {e}");
            }
        }
        Ok(started)
    }

    /// `login`'s routines running now: each one's run, by routine id.
    fn routines_running(&self, login: &str) -> BTreeMap<String, TaskId> {
        let tasks: Vec<Arc<Task>> = self.tasks().values().cloned().collect();
        tasks
            .into_iter()
            .filter(|t| t.user.name == login)
            .filter_map(|t| {
                let run = t.routine.as_ref()?;
                t.lock().tile.is_live().then(|| (run.id.clone(), t.id))
            })
            .collect()
    }

    /// Starts every routine due in the minute that begins at `hour:minute`
    /// on `day`, in local time, as the person it belongs to (`person` finds
    /// them by login; someone it can't find has none). glassd calls it once
    /// a minute. Returns each start, or why it couldn't.
    ///
    /// `local_minute` is that minute's number (`local_minute`'s): it is
    /// claimed first, on disk, so a minute starts its routines once,
    /// whether glassd restarts in it or the clock comes back to it (set
    /// back, or daylight saving ending) (ADR 0051).
    pub fn run_due_routines(
        self: &Arc<Self>,
        day: Weekday,
        hour: u8,
        minute: u8,
        local_minute: i64,
        person: impl Fn(&str) -> Option<Caller>,
    ) -> Vec<(String, Result<TaskView>)> {
        let Some(routines) = self.routines.get() else {
            return Vec::new();
        };
        match routines.claim_minute(local_minute) {
            Ok(true) => {}
            Ok(false) => return Vec::new(),
            Err(e) => {
                tracing::warn!("claiming the schedule's minute: {e}; nothing starts in it");
                return Vec::new();
            }
        }
        let mut started = Vec::new();
        for login in routines.logins() {
            let Some(caller) = person(&login).filter(|c| c.name == login) else {
                continue;
            };
            // A paused routine runs only when started (ADR 0047).
            let paused = store::paused(&caller.home);
            for routine in routines.current(&login, &caller.home) {
                if !paused.contains(&routine.id)
                    && routine.schedule.is_some_and(|s| s.due(day, hour, minute))
                {
                    let id = format!("{login}/{}", routine.id);
                    started.push((id, self.start_routine(&caller, &routine.id, true)));
                }
            }
        }
        started
    }
}

/// The runtime as the Routines app's actions reach it (ADR 0047): as the
/// person the session is for, found by uid and login both.
#[derive(Debug)]
struct RoutinesHost {
    runtime: Weak<Runtime>,
    /// glassd's, so a run started from an action's worker thread can
    /// spawn its supervisor.
    tokio: Option<tokio::runtime::Handle>,
}

impl RoutinesHost {
    fn person(&self, login: &str, uid: u32) -> std::result::Result<(Arc<Runtime>, Caller), String> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| "this computer is shutting down".to_owned())?;
        let caller = runtime
            .caller(uid)
            .filter(|c| c.name == login)
            .ok_or_else(|| format!("{login} isn't a person on this computer"))?;
        Ok((runtime, caller))
    }
}

impl Host for RoutinesHost {
    fn run(&self, login: &str, uid: u32, routine: &str) -> std::result::Result<TaskId, String> {
        let (runtime, caller) = self.person(login, uid)?;
        let _tokio = self.tokio.as_ref().map(tokio::runtime::Handle::enter);
        runtime
            .run_routine(&caller, routine)
            .map(|view| view.id)
            .map_err(|e| e.to_string())
    }

    fn repeats(&self, login: &str, uid: u32) -> Vec<Repeat> {
        self.person(login, uid)
            .map(|(runtime, caller)| runtime.repeats_of(login, &caller.home))
            .unwrap_or_default()
    }

    fn dismiss(&self, login: &str, uid: u32, task: TaskId) -> std::result::Result<(), String> {
        let (runtime, _) = self.person(login, uid)?;
        runtime
            .dismiss_repeat(login, task)
            .map_err(|e| e.to_string())
    }

    fn running(&self, login: &str, uid: u32) -> BTreeMap<String, TaskId> {
        self.person(login, uid)
            .map(|(runtime, _)| runtime.routines_running(login))
            .unwrap_or_default()
    }

    fn changed(&self, login: &str) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.app_changed(login, APP);
        }
    }

    fn run_of(&self, task: TaskId) -> Option<(String, u32)> {
        let runtime = self.runtime.upgrade()?;
        let task = runtime.tasks().get(&task).cloned()?;
        task.routine
            .as_ref()
            .map(|run| (run.id.clone(), run.version))
    }
}

/// A routine as the person sees it.
pub(crate) fn view(r: Routine) -> RoutineView {
    RoutineView {
        runs: r
            .schedule
            .map_or_else(|| "when started".to_owned(), |s| s.describe()),
        routine: r.id,
        name: r.name,
        version: r.version,
        request: r.request,
        agent: r.agent,
        calls: r.calls,
        read: r.read,
        write: r.write,
    }
}
