//! What tasks did, as code (NV-21, DA-4, ADR 0046). A finished task's
//! calls compile into replay code, deterministically; glassd keeps the
//! last ones per person, root only, so it can tell when a request was done
//! with the same steps before, and so the person can save any of them as a
//! routine with no model writing it.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use glass_api::{RoutineDraft, RoutineFromTask, RoutineView};
use glass_app_routines::{Draft, Repeat, Scope};
use glass_audit::{AuditEvent, EventKind};
use glass_types::{AgentId, TaskId, Tier, Timestamp};
use serde::{Deserialize, Serialize};

use crate::task::Task;
use crate::{Caller, Runtime, RuntimeError};

type Result<T> = std::result::Result<T, RuntimeError>;

/// The flows kept per person, newest first, and for how long.
const KEPT: usize = 50;
const KEPT_SECS: i64 = 30 * 24 * 3600;

/// People's flows files change one at a time: a task's end and the
/// person dismissing a repeat read and write the same file.
static WRITING: Mutex<()> = Mutex::new(());

/// One finished task's calls as code.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Flow {
    task: TaskId,
    /// The request as the same request again would be: agent, words and
    /// scope.
    key: String,
    agent: AgentId,
    prompt: String,
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
    script: String,
    steps: Vec<String>,
    /// What makes two flows the same steps (`Replay::shape`).
    #[serde(default)]
    shape: String,
    at: Timestamp,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Flows {
    flows: Vec<Flow>,
}

/// The same request: the same agent, the same words (case and spacing
/// aside) and the same folders.
fn key(agent: &AgentId, prompt: &str, read: &[PathBuf], write: &[PathBuf]) -> String {
    let words: Vec<String> = prompt.split_whitespace().map(str::to_lowercase).collect();
    let mut read: Vec<String> = read.iter().map(|p| p.display().to_string()).collect();
    let mut write: Vec<String> = write.iter().map(|p| p.display().to_string()).collect();
    read.sort();
    write.sort();
    format!(
        "{agent}\n{}\n{}\n{}",
        words.join(" ").trim_end_matches(['.', '!', '?']),
        read.join("\n"),
        write.join("\n")
    )
}

fn flow_of(task: &Task) -> std::result::Result<Flow, String> {
    let taken = task.lock().trace.taken();
    Ok(flow_with(task, taken.compile()?))
}

fn flow_with(task: &Task, replay: glass_replay::Replay) -> Flow {
    let (read, write): (Vec<_>, Vec<_>) = task.ctx.grants.iter().partition(|g| !g.writable);
    let read: Vec<PathBuf> = read.iter().map(|g| g.root.clone()).collect();
    let write: Vec<PathBuf> = write.iter().map(|g| g.root.clone()).collect();
    Flow {
        task: task.id,
        key: key(&task.entry.id, &task.prompt, &read, &write),
        agent: task.entry.id.clone(),
        prompt: task.prompt.clone(),
        read,
        write,
        script: replay.script,
        steps: replay.steps,
        shape: replay.shape,
        at: Timestamp::now(),
    }
}

impl Runtime {
    /// Where `login`'s flows are kept; none for a login that is a path.
    fn flows_file(&self, login: &str) -> Option<PathBuf> {
        let one_part =
            !login.is_empty() && !matches!(login, "." | "..") && !login.contains(['/', '\0']);
        one_part.then(|| {
            self.config
                .state_dir
                .join("flows")
                .join(format!("{login}.json"))
        })
    }

    fn load_flows(&self, login: &str) -> Flows {
        self.flows_file(login)
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn keep_flows(&self, login: &str, flows: &Flows) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let path = self
            .flows_file(login)
            .ok_or_else(|| std::io::Error::other("not a login"))?;
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let staged = path.with_extension("json.new");
        std::fs::write(&staged, serde_json::to_vec(flows)?)?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(staged, path)
    }

    /// At a task's end: its calls compiled, and let go. A `finished`
    /// task's code is kept, and the earlier task that did the same request
    /// with the same steps returned, if one did: then the request repeats
    /// as code, and the task says it can be a routine.
    pub(crate) fn remember_flow(&self, task: &Task, finished: bool) -> Option<TaskId> {
        let taken = task.lock().trace.taken();
        let compiled = taken.compile();
        task.lock().trace.end(compiled.clone());
        // An app's session isn't a request, a routine's run already is one,
        // and only a task that finished did what was asked.
        let replay = compiled.ok()?;
        if task.is_app() || task.routine.is_some() || !finished {
            return None;
        }
        let flow = flow_with(task, replay);
        let login = task.user.name.as_str();
        let _one_at_a_time = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
        let mut flows = self.load_flows(login);
        let now = Timestamp::now().unix_seconds();
        flows
            .flows
            .retain(|f| f.task != task.id && now - f.at.unix_seconds() < KEPT_SECS);
        // Offered only for an agent that runs routines: the person's own
        // harness waits for them, and can't (ADR 0047).
        let earlier = flows
            .flows
            .iter()
            .find(|f| f.key == flow.key && f.shape == flow.shape)
            .map(|f| f.task)
            .filter(|_| self.runs_routines(login, &flow.agent));
        flows.flows.insert(0, flow);
        flows.flows.truncate(KEPT);
        if let Err(e) = self.keep_flows(login, &flows) {
            tracing::warn!(task = %task.id, "keeping what the task did as code: {e}");
        }
        if let Some(earlier) = earlier {
            task.lock().note(
                glass_api::ActivityKind::Notice,
                format!(
                    "The same steps as task {} for this request: this computer can do it as a \
                     routine, with no model (glassctl routine save {} --name …)",
                    earlier.short(),
                    task.id.short()
                ),
            );
        }
        earlier
    }

    /// One of the caller's own tasks as code: from its calls while glassd
    /// has it, else as kept when it finished.
    fn flow(&self, caller: &Caller, id: TaskId) -> Result<Flow> {
        if let Some(task) = self.task(id) {
            if task.user.uid != caller.uid {
                return Err(RuntimeError::NotFound(format!("no task {}", id.short())));
            }
            return flow_of(&task).map_err(|why| {
                RuntimeError::Invalid(format!("task {} can't be replayed: {why}", id.short()))
            });
        }
        self.load_flows(&caller.name)
            .flows
            .into_iter()
            .find(|f| f.task == id)
            .ok_or_else(|| {
                RuntimeError::NotFound(format!(
                    "no task {} whose steps this computer kept as code",
                    id.short()
                ))
            })
    }

    /// `login`'s requests done more than once with the same steps, the
    /// latest first, leaving out those already one of their routines
    /// (ADR 0047).
    pub(crate) fn repeats_of(&self, login: &str, home: &std::path::Path) -> Vec<Repeat> {
        let flows = self.load_flows(login).flows;
        let routines: BTreeSet<String> = self
            .routines
            .get()
            .map(|r| r.current(login, home))
            .unwrap_or_default()
            .iter()
            .map(|r| key(&r.agent, &r.request, &r.read, &r.write))
            .collect();
        let mut seen = BTreeSet::new();
        let mut repeats = Vec::new();
        for flow in &flows {
            if routines.contains(&flow.key)
                || !seen.insert((&flow.key, &flow.shape))
                || !self.runs_routines(login, &flow.agent)
            {
                continue;
            }
            let times = flows
                .iter()
                .filter(|f| f.key == flow.key && f.shape == flow.shape)
                .count();
            if times >= 2 {
                repeats.push(Repeat {
                    task: flow.task,
                    request: flow.prompt.clone(),
                    agent: flow.agent.clone(),
                    times,
                    last_at: flow.at,
                    script: flow.script.clone(),
                    steps: flow.steps.clone(),
                    read: flow.read.clone(),
                    write: flow.write.clone(),
                });
            }
        }
        repeats
    }

    /// Whether `agent` runs routines for `login`: policy lets it read them,
    /// as it does the desktop agent's. An attended harness's tasks are the
    /// person's own work, and not one to repeat without them.
    fn runs_routines(&self, login: &str, agent: &AgentId) -> bool {
        let read: glass_types::ActionName = glass_app_routines::READ
            .parse()
            .unwrap_or_else(|_| unreachable!("a static action name"));
        self.policy()
            .effective(login, agent)
            .is_some_and(|p| p.tier_for(&read, Tier::Auto).tier != Tier::Never)
            && self
                .config
                .catalog
                .iter()
                .any(|entry| &entry.id == agent && !entry.attended)
    }

    /// Forgets the times `login` did the request the flow of `task` did,
    /// that way, so it isn't offered as a routine until it's done so twice
    /// again.
    pub(crate) fn dismiss_repeat(&self, login: &str, task: TaskId) -> Result<()> {
        let _one_at_a_time = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
        let mut flows = self.load_flows(login);
        let (key, shape) = flows
            .flows
            .iter()
            .find(|f| f.task == task)
            .map(|f| (f.key.clone(), f.shape.clone()))
            .ok_or_else(|| RuntimeError::NotFound(format!("no repeat {}", task.short())))?;
        flows.flows.retain(|f| f.key != key || f.shape != shape);
        self.keep_flows(login, &flows)
            .map_err(|e| RuntimeError::Internal(format!("forgetting the repeat: {e}")))
    }

    /// One of the caller's tasks as replay code (ADR 0046).
    pub fn routine_draft(&self, caller: &Caller, task: TaskId) -> Result<RoutineDraft> {
        let flow = self.flow(caller, task)?;
        Ok(RoutineDraft {
            task,
            request: flow.prompt,
            script: flow.script,
            steps: flow.steps,
            read: flow.read,
            write: flow.write,
        })
    }

    /// Saves one of the caller's tasks as a routine, its script written by
    /// this computer from the task's calls: their own act, so no ask, and
    /// on the record.
    pub fn routine_from_task(
        &self,
        caller: &Caller,
        request: RoutineFromTask,
    ) -> Result<RoutineView> {
        self.refuse_unrecorded()?;
        let routines = self.routines.get().ok_or_else(|| {
            RuntimeError::Conflict("routines aren't kept on this computer".into())
        })?;
        if self
            .task(request.task)
            .is_some_and(|t| t.lock().tile.is_live())
        {
            return Err(RuntimeError::Conflict(
                "that task is still running; save it once it's done".into(),
            ));
        }
        let flow = self.flow(caller, request.task)?;
        if !self.runs_routines(&caller.name, &flow.agent) {
            return Err(RuntimeError::Invalid(format!(
                "{} doesn't run routines; ask the desktop agent, and save what it does",
                flow.agent
            )));
        }
        let routine = routines
            .plan(
                &caller.name,
                &caller.home,
                Draft {
                    routine: None,
                    name: Some(request.name),
                    request: flow.prompt,
                    script: flow.script,
                    schedule: request.schedule,
                    note: format!(
                        "Written by this computer from task {}",
                        request.task.short()
                    ),
                },
                Scope {
                    agent: flow.agent,
                    read: flow.read,
                    write: flow.write,
                    task: request.task,
                },
            )
            .map_err(RuntimeError::Invalid)?;
        let owner = glass_platform::fs::Owner {
            uid: caller.uid,
            gid: caller.gid,
        };
        let saved = routines
            .keep(routine, &caller.home, owner)
            .map_err(|e| RuntimeError::Internal(format!("keeping the routine: {e}")))?;
        let mut event = AuditEvent::new(
            EventKind::RoutineSaved,
            format!(
                "{} saved task {} as the routine {}",
                caller.name,
                request.task.short(),
                saved.name
            ),
        );
        event.user = Some(caller.name.clone());
        event.task = Some(request.task);
        event.agent = Some(saved.agent.clone());
        event.detail = serde_json::json!({
            "routine": saved.id,
            "version": saved.version,
            "calls": saved.calls,
            "schedule": saved.schedule.map(|s| s.to_string()),
        });
        self.audit(event);
        Ok(crate::routines::view(saved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv21_the_same_request_is_the_same_words_agent_and_folders() {
        let agent: AgentId = "desktop-agent".parse().unwrap_or_else(|_| unreachable!());
        let read = [PathBuf::from("/home/demo/Invoices")];
        let a = key(&agent, "Archive the newsletters.", &read, &[]);
        assert_eq!(a, key(&agent, "  archive   the Newsletters", &read, &[]));
        assert_ne!(a, key(&agent, "Archive the newsletters", &[], &[]));
        assert_ne!(a, key(&agent, "Archive the receipts", &read, &[]));
    }
}
