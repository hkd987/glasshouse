//! Routines (DA-4, NV-21, ADR 0044): a task the person saved, run again
//! when they start it or on its schedule, by the agent that saved it,
//! with the scope it was saved with. Each run is a task like any other:
//! policy, tiers, asks, audit and undo as they are when it runs.

use std::sync::Arc;

use glass_api::{RoutineView, ScopeRequest, TaskRequest, TaskView};
use glass_app_routines::{Routine, Routines, Weekday, store};

use crate::runtime::Origin;
use crate::task::RoutineRun;
use crate::{Caller, Runtime, RuntimeError};

type Result<T> = std::result::Result<T, RuntimeError>;

impl Runtime {
    /// Where people's routines are kept, set once at startup.
    pub fn set_routines(&self, routines: Routines) {
        let _ = self.routines.set(routines);
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

    fn start_routine(
        self: &Arc<Self>,
        caller: &Caller,
        id: &str,
        scheduled: bool,
    ) -> Result<TaskView> {
        let version = store::chosen(&caller.home).get(id).copied();
        let routine = version
            .and_then(|v| self.routines.get()?.record(&caller.name, id, v))
            .ok_or_else(|| RuntimeError::NotFound(format!("you have no routine {id}")))?;
        // The request it was saved with, as a new task: policy and the
        // folders' access are checked again, as for any start.
        let request = TaskRequest {
            agent: routine.agent.clone(),
            prompt: routine.request.clone(),
            scope: ScopeRequest {
                read: routine.read.clone(),
                write: routine.write.clone(),
                restrict: Vec::new(),
            },
            title: Some(routine.name.clone()),
        };
        let run = RoutineRun {
            id: routine.id,
            version: routine.version,
            scheduled,
        };
        self.start_task(caller, request, Origin::Routine(run))
    }

    /// Starts every routine due in the minute that begins at `hour:minute`
    /// on `day`, in local time, as the person it belongs to (`person` finds
    /// them by login; someone it can't find has none). glassd calls it once
    /// a minute. Returns each start, or why it couldn't.
    pub fn run_due_routines(
        self: &Arc<Self>,
        day: Weekday,
        hour: u8,
        minute: u8,
        person: impl Fn(&str) -> Option<Caller>,
    ) -> Vec<(String, Result<TaskView>)> {
        let Some(routines) = self.routines.get() else {
            return Vec::new();
        };
        let mut started = Vec::new();
        for login in routines.logins() {
            let Some(caller) = person(&login).filter(|c| c.name == login) else {
                continue;
            };
            for routine in routines.current(&login, &caller.home) {
                if routine.schedule.is_some_and(|s| s.due(day, hour, minute)) {
                    let id = format!("{login}/{}", routine.id);
                    started.push((id, self.start_routine(&caller, &routine.id, true)));
                }
            }
        }
        started
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
