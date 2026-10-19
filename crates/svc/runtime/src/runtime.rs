use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use glass_actions::{Grant, Registry, TaskContext};
use glass_api::settings::SettingsView;
use glass_api::{
    ActivityKind, AgentSummary, AnswerApproval, ApprovalAnswer, ApprovalScope, ApprovalView,
    AuditVerifyResult, Event, HandoffCard, PowerResult, ScopeLine, ScopeRequest, SnapshotMarker,
    SystemStatus, TaskDetail, TaskRequest, TaskView, TileState,
};
pub use glass_app_settings::SettingsHandle;
use glass_audit::{ApprovalRef, AuditEvent, AuditLog, AuditRecord, EventKind};
use glass_broker::{ApprovalDesk, DeskError};
use glass_gateway::Gateway;
use glass_platform::fs::Owner;
use glass_platform::sandbox::{Mount, Sandbox, SandboxSpec};
use glass_platform::snapshot::{SnapshotHandle, Snapshotter};
use glass_policy::{ActionPattern, EffectivePolicy, Policy};
use glass_tailoring::LayerStore;
use glass_types::{AgentPrincipal, SnapshotId, TaskId, Tier, Timestamp, UserRef};
use serde_json::Value;
use tokio::sync::{broadcast, oneshot};

use crate::error::Result;

/// How long sleep waits for actions already under way.
const SLEEP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
use crate::task::{Continued, Gate, NewTask, TakenSnapshot, Task};
use crate::{Caller, CatalogEntry, RuntimeConfig, RuntimeError, supervise, title};

/// Everything the runtime is built from. `glassd` assembles these from its
/// config; tests assemble development backends.
pub struct RuntimeParts {
    pub config: RuntimeConfig,
    pub policy: Policy,
    /// Where the policy came from, for status output.
    pub policy_source: String,
    pub registry: Registry,
    pub sandbox: Arc<dyn Sandbox>,
    pub snapshots: Arc<dyn Snapshotter>,
    pub audit: Arc<AuditLog>,
    /// The model providers this machine can reach (ADR 0018). An empty
    /// gateway refuses every model call.
    pub gateway: Arc<Gateway>,
    /// Device settings (UX-1): the same handle the `settings` actions use.
    pub settings: SettingsHandle,
}

/// Events carry the owner's uid so subscribers only see their own tasks.
pub(crate) type OwnedEvent = (u32, Event);

/// The owner of an event for everyone, such as new device settings.
pub(crate) const EVERYONE_UID: u32 = u32::MAX;

pub struct Runtime {
    pub(crate) config: RuntimeConfig,
    /// Swapped whole when managed settings apply a new policy.
    settings: RwLock<Arc<Settings>>,
    policy_source: Mutex<String>,
    pub(crate) desk: ApprovalDesk,
    pub(crate) sandbox: Arc<dyn Sandbox>,
    pub(crate) snapshots: Arc<dyn Snapshotter>,
    pub(crate) gateway: Arc<Gateway>,
    /// Device settings (UX-1, ADR 0029).
    pub(crate) device_settings: SettingsHandle,
    /// Users' layers on catalog agents (TL-2).
    pub(crate) layers: LayerStore,
    audit: Arc<AuditLog>,
    audit_ok: AtomicBool,
    events: broadcast::Sender<OwnedEvent>,
    tasks: Mutex<BTreeMap<TaskId, Arc<Task>>>,
    supervisors: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Held while an app session opens, so calls in parallel can't all
    /// pass the per-person cap before any of them is counted.
    pub(crate) opening_app: Mutex<()>,
    /// When each person's data in each app last changed, for `app.status`
    /// waits (ADR 0030).
    pub(crate) app_changes: tokio::sync::watch::Sender<crate::apps::AppChanges>,
    /// Who is at the screen (ADR 0031).
    pub(crate) seat: Mutex<Option<crate::seat::Seat>>,
    /// How many times the computer has woken, for a sleep glassd asked
    /// for to hear of it (ADR 0031).
    pub(crate) wakes: tokio::sync::watch::Sender<u64>,
    /// Where the seat is kept across a restart of glassd (ADR 0031).
    pub(crate) seat_file: std::sync::OnceLock<PathBuf>,
    /// Who is signed in at the screen, for what follows them (ADR 0031).
    pub(crate) seat_user: tokio::sync::watch::Sender<Option<String>>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("sandbox", &self.sandbox.kind())
            .finish_non_exhaustive()
    }
}

/// The policy in force and the action registry built from it, which
/// managed settings replace together (ADR 0021, ADR 0023). Each operation
/// reads them once; tasks keep the effective policy they started with.
struct Settings {
    policy: Arc<Policy>,
    registry: Arc<Registry>,
}

/// A running agent, as a managed device reports it (AD-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningAgent {
    pub agent: glass_types::AgentId,
    pub user: String,
    /// Waiting on a person: an open ask.
    pub waiting: bool,
}

/// What planning works out, shared by `plan` and `start`.
struct Prepared {
    entry: CatalogEntry,
    policy: EffectivePolicy,
    grants: Vec<Grant>,
    card: HandoffCard,
    settings: BTreeMap<String, Value>,
    layer: Option<u32>,
    /// The request's title, or one made from the prompt.
    title: String,
}

/// Why a task is starting.
pub(crate) enum Origin {
    New,
    /// The Failed tile's Retry.
    RetryOf(TaskId),
    /// Take over on an ended task, in the user's harness.
    Continues(Continued),
}

impl Runtime {
    pub fn new(parts: RuntimeParts) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        parts.settings.set_policy(&parts.policy);
        let tell = events.clone();
        parts.settings.on_change(move |uid| {
            let _ = tell.send((uid.unwrap_or(EVERYONE_UID), Event::SettingsChanged));
        });
        Arc::new(Self {
            device_settings: parts.settings,
            layers: LayerStore::new(parts.config.state_dir.join("layers")),
            config: parts.config,
            settings: RwLock::new(Arc::new(Settings {
                policy: Arc::new(parts.policy),
                registry: Arc::new(parts.registry),
            })),
            policy_source: Mutex::new(parts.policy_source),
            desk: ApprovalDesk::new(),
            sandbox: parts.sandbox,
            snapshots: parts.snapshots,
            gateway: parts.gateway,
            audit: parts.audit,
            audit_ok: AtomicBool::new(true),
            events,
            tasks: Mutex::new(BTreeMap::new()),
            supervisors: Mutex::new(Vec::new()),
            opening_app: Mutex::new(()),
            app_changes: tokio::sync::watch::Sender::new(Default::default()),
            seat: Mutex::new(None),
            wakes: tokio::sync::watch::Sender::new(0),
            seat_file: std::sync::OnceLock::new(),
            seat_user: tokio::sync::watch::Sender::new(None),
        })
    }

    /// Stops every live task and waits (bounded) for their sandboxes to be
    /// torn down. Called when the daemon exits.
    pub async fn shutdown(&self) {
        let live: Vec<Arc<Task>> = self
            .tasks()
            .values()
            .filter(|t| t.lock().tile.is_live())
            .cloned()
            .collect();
        for task in &live {
            self.stop_task(task, "Stopped because the system is shutting down");
        }
        let handles: Vec<_> =
            std::mem::take(&mut *self.supervisors.lock().unwrap_or_else(|p| p.into_inner()));
        for handle in handles {
            if tokio::time::timeout(std::time::Duration::from_secs(20), handle)
                .await
                .is_err()
            {
                tracing::warn!("a task did not finish tearing down in time");
            }
        }
    }

    /// The caller's device settings, as their desktop applies them (UX-1).
    pub fn settings_view(&self, caller: &Caller) -> SettingsView {
        SettingsView {
            settings: self
                .device_settings
                .person(&caller.name, caller.uid)
                .iter()
                .map(glass_app_settings::Effective::view)
                .collect(),
            utc_offset_secs: glass_platform::system::utc_offset_secs(),
        }
    }

    /// Something every desktop reads with its settings changed, such as the
    /// computer's time zone (ADR 0031): they ask again.
    pub fn settings_changed(&self) {
        let _ = self.events.send((EVERYONE_UID, Event::SettingsChanged));
    }

    fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.settings.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// The policy in force for tasks starting now.
    pub(crate) fn policy(&self) -> Arc<Policy> {
        Arc::clone(&self.settings().policy)
    }

    /// The actions this machine offers, built from the policy in force.
    pub(crate) fn registry(&self) -> Arc<Registry> {
        Arc::clone(&self.settings().registry)
    }

    /// Puts a new policy, and the registry built from it, in force at once
    /// (ADR 0021, ADR 0023). The caller validated and linted it first.
    /// Tasks started from now on use it; running agents' tasks keep the
    /// effective policy they started with, the scope the person agreed
    /// to. Open app sessions follow it from their next call: an app has
    /// no handoff card, and stays open as long as the person is signed
    /// in, so a stricter policy has to reach it.
    pub fn apply_policy(&self, policy: Policy, registry: Registry, source: impl Into<String>) {
        // Device settings follow at once; desktops hear if theirs changed.
        self.device_settings.set_policy(&policy);
        *self
            .settings
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(Settings {
            policy: Arc::new(policy),
            registry: Arc::new(registry),
        });
        self.set_policy_source(source);
        let policy = self.policy();
        let apps: Vec<Arc<Task>> = self
            .tasks()
            .values()
            .filter(|t| t.is_app())
            .cloned()
            .collect();
        for task in apps {
            if let Some(manifest) = &task.app {
                // Taken out of the person's profile: the open app closes.
                if !policy.apps_for(&task.user.name).contains(&manifest.id) {
                    if task.lock().tile.is_live() {
                        self.end_task(
                            &task,
                            TileState::Stopped,
                            "IT took this app out of your profile",
                        );
                    }
                    continue;
                }
                task.follow_policy(policy.for_app(
                    &task.user.name,
                    &manifest.id,
                    &manifest.actions,
                    manifest.max_label(),
                ));
            }
        }
    }

    /// What `system.status` says about where the policy came from, such as
    /// a managed version and whether the console can be reached.
    pub fn set_policy_source(&self, source: impl Into<String>) {
        *self
            .policy_source
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = source.into();
    }

    /// Appends a record from outside the action pipeline, such as managed
    /// settings applied or refused. A failed append fails closed, as for
    /// the pipeline's own records.
    pub fn append_audit(&self, event: AuditEvent) {
        self.audit(event);
    }

    pub(crate) fn tasks(&self) -> std::sync::MutexGuard<'_, BTreeMap<TaskId, Arc<Task>>> {
        self.tasks.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn task(&self, id: TaskId) -> Option<Arc<Task>> {
        self.tasks().get(&id).cloned()
    }

    /// The task, if `caller` may see it. Others' tasks read as not found.
    pub(crate) fn authorized(&self, caller: &Caller, id: TaskId) -> Result<Arc<Task>> {
        self.task(id)
            .filter(|t| caller.is_admin() || t.user.uid == caller.uid)
            .ok_or_else(|| RuntimeError::NotFound(format!("no task {}", id.short())))
    }

    // ---- recording -------------------------------------------------------

    /// Appends to the audit log. A failed append marks the log unhealthy, and
    /// the action pipeline then refuses side effects (fail closed). The
    /// append waits for the disk (fsync), which takes long while agents
    /// write a lot, so it runs off the async workers that answer the shell.
    pub(crate) fn audit(&self, event: AuditEvent) {
        if let Err(e) = blocking(|| self.audit.append(event)) {
            tracing::error!(error = %e, "audit append failed; refusing further side effects");
            self.audit_ok.store(false, Ordering::SeqCst);
        }
    }

    pub(crate) fn audit_healthy(&self) -> bool {
        self.audit_ok.load(Ordering::SeqCst)
    }

    /// An audit event pre-filled with the task's principal.
    pub(crate) fn task_event(
        &self,
        task: &Task,
        kind: EventKind,
        summary: impl Into<String>,
    ) -> AuditEvent {
        let mut event = AuditEvent::new(kind, summary);
        event.user = Some(task.user.name.clone());
        event.agent = Some(task.entry.id.clone());
        event.task = Some(task.id);
        event
    }

    /// Tells subscribers the task's tile changed.
    ///
    /// An app session is a tile only while it needs the user (ADR 0024):
    /// its ask shows and is answered on the desktop like an agent's, and
    /// the rest of the time the app is just a window, so subscribers hear
    /// it removed.
    pub(crate) fn publish(&self, task: &Task) {
        let view = task.view(self.desk.pending_for_task(task.id));
        let event = if task.is_app() && view.state != TileState::NeedsYou {
            Event::TaskRemoved { task: task.id }
        } else {
            Event::TaskUpdated {
                task: Box::new(view),
            }
        };
        let _ = self.events.send((task.user.uid, event));
    }

    /// Who `uid` is, for an API call. On a development host with
    /// `dev_home` set, everyone's app data lives there instead.
    pub fn caller(&self, uid: u32) -> Option<Caller> {
        let mut caller = Caller::from_uid(uid)?;
        if let Some(home) = &self.config.dev_home {
            caller.home.clone_from(home);
        }
        Some(caller)
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<OwnedEvent> {
        self.events.subscribe()
    }

    // ---- queries ---------------------------------------------------------

    pub fn status(&self) -> SystemStatus {
        let tasks = self.tasks();
        SystemStatus {
            version: env!("CARGO_PKG_VERSION").into(),
            sandbox: self.sandbox.kind().into(),
            contained: self.sandbox.is_security_boundary(),
            policy: self
                .policy_source
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            tasks_running: tasks
                .values()
                .filter(|t| !t.is_app() && t.lock().tile.is_live())
                .count(),
            approvals_pending: self.desk.pending(None).len(),
        }
    }

    /// The agents running now, for whom, and whether each waits on a
    /// person: what a managed device reports for the fleet view (AD-4).
    /// Ids and login names only, never task text or paths.
    pub fn running_agents(&self) -> Vec<RunningAgent> {
        self.tasks()
            .values()
            .filter(|t| !t.is_app())
            .filter_map(|t| {
                let tile = t.lock().tile;
                tile.is_live().then(|| RunningAgent {
                    agent: t.entry.id.clone(),
                    user: t.user.name.clone(),
                    waiting: tile == TileState::NeedsYou,
                })
            })
            .collect()
    }

    /// Everyone who ran an agent or opened an app since glassd started.
    pub fn users_seen(&self) -> Vec<String> {
        let users: std::collections::BTreeSet<String> =
            self.tasks().values().map(|t| t.user.name.clone()).collect();
        users.into_iter().collect()
    }

    pub fn agents_for(&self, caller: &Caller) -> Vec<AgentSummary> {
        let policy = self.policy();
        let allowed = policy.agents_for(&caller.name);
        let reaches = self.catalog_reach(caller);
        self.config
            .catalog
            .iter()
            .filter(|e| allowed.contains(&&e.id) && reaches(&e.id))
            .map(CatalogEntry::summary)
            .collect()
    }

    /// Whether the org's catalog lets an item reach `caller` (DV-11): what
    /// it doesn't list is policy's alone to decide; what it makes available
    /// reaches them once they installed it from the Store; what it offers
    /// their groups nothing of, or blocks, not at all. Only ever narrows.
    pub(crate) fn catalog_reach<'a>(
        &'a self,
        caller: &'a Caller,
    ) -> impl Fn(&glass_types::AgentId) -> bool + 'a {
        let groups: Vec<String> = self
            .policy()
            .groups_of(&caller.name)
            .map(|g| g.name.clone())
            .collect();
        move |id| self.config.org_catalog.reaches(&groups, &caller.home, id)
    }

    /// One view per tile. App sessions are tiles only while they need the
    /// user (see [`Runtime::publish`]).
    pub fn list(&self, caller: &Caller) -> Vec<TaskView> {
        let tasks: Vec<Arc<Task>> = self.tasks().values().cloned().collect();
        tasks
            .iter()
            .filter(|t| caller.is_admin() || t.user.uid == caller.uid)
            .map(|t| (t.is_app(), t.view(self.desk.pending_for_task(t.id))))
            .filter(|(app, view)| !app || view.state == TileState::NeedsYou)
            .map(|(_, view)| view)
            .collect()
    }

    pub fn get(&self, caller: &Caller, id: TaskId) -> Result<TaskDetail> {
        let task = self.authorized(caller, id)?;
        Ok(task.detail(self.desk.pending_for_task(id)))
    }

    // ---- planning and starting ------------------------------------------

    /// The handoff card for a request, with no side effects (DA-2).
    pub fn plan(&self, caller: &Caller, request: &TaskRequest) -> Result<HandoffCard> {
        Ok(self.prepare(caller, request)?.card)
    }

    fn prepare(&self, caller: &Caller, request: &TaskRequest) -> Result<Prepared> {
        if request.prompt.trim().is_empty() {
            return Err(RuntimeError::Invalid(
                "describe the task in a sentence or two".into(),
            ));
        }
        let title = match &request.title {
            Some(given) => title::check(given).map_err(RuntimeError::Invalid)?,
            None => title::from_prompt(&request.prompt),
        };
        let entry = self
            .config
            .catalog
            .iter()
            .find(|e| e.id == request.agent)
            .cloned()
            .ok_or_else(|| {
                RuntimeError::NotFound(format!("no agent {} in the catalog", request.agent))
            })?;
        let mut policy = self
            .policy()
            .effective(&caller.name, &entry.id)
            .ok_or_else(|| {
                RuntimeError::PermissionDenied(format!(
                    "your groups are not allowed to run {}",
                    entry.id
                ))
            })?;
        if !self.catalog_reach(caller)(&entry.id) {
            return Err(RuntimeError::PermissionDenied(format!(
                "{} isn't installed: your organization's catalog offers it in the Store",
                entry.name
            )));
        }

        let known = self.registry().known_tiers();
        // The user's layer narrows before the card's own limits (TL-10).
        let layer = self.layer_for_task(caller, &entry)?;
        let in_use = layer.as_ref().and_then(|l| l.version.as_ref());
        for (pattern, tier) in in_use.map(|v| v.restrict.as_slice()).unwrap_or_default() {
            if let Ok(pattern) = pattern.parse::<ActionPattern>() {
                policy.narrow(pattern, *tier);
            }
        }
        let settings = glass_tailoring::resolve(
            &entry.customize,
            &in_use.map(|v| v.settings.clone()).unwrap_or_default(),
        );
        let layer_version = in_use.map(|v| v.version);
        for (pattern, tier) in &request.scope.restrict {
            let pattern: ActionPattern = pattern
                .parse()
                .map_err(|e: glass_policy::InvalidPattern| RuntimeError::Invalid(e.to_string()))?;
            policy
                .restrict(pattern, *tier, &known)
                .map_err(|e| RuntimeError::Invalid(e.to_string()))?;
        }
        let grants = resolve_scope(caller, &request.scope)?;

        let mut asks_before = Vec::new();
        let mut notifies = Vec::new();
        for def in self.registry().defs() {
            let line = def.title.clone();
            match policy.tier_for(&def.name, def.default_tier).tier {
                Tier::Ask => asks_before.push(line),
                Tier::Notify => notifies.push(line),
                _ => {}
            }
        }
        let plan = if entry.plan.is_empty() {
            vec![
                "Read what it was given".into(),
                "Do the task".into(),
                "Report what changed".into(),
            ]
        } else {
            entry.plan.clone()
        };
        let card = HandoffCard {
            agent: entry.summary(),
            prompt: request.prompt.clone(),
            plan,
            touches: grants
                .iter()
                .map(|g| ScopeLine {
                    path: g.root.clone(),
                    access: if g.writable {
                        "read and change".into()
                    } else {
                        "read only".into()
                    },
                })
                .collect(),
            asks_before,
            notifies,
            cant_touch: policy.cant_touch(&known, |name| {
                self.registry()
                    .get(name)
                    .map_or_else(|| name.to_string(), |(def, _)| def.title.clone())
            }),
            max_label: Some(policy.max_label()),
            // Where policy would route this task's data (DP-2, ADR 0018).
            model: self.model_line(&entry, &policy, &grants, &caller.home),
            layer: layer.map(|l| l.line),
        };
        Ok(Prepared {
            entry,
            policy,
            grants,
            card,
            settings,
            layer: layer_version,
            title,
        })
    }

    /// Starts a task (DA-3): the scope on the card becomes the task's grants.
    pub fn start(self: &Arc<Self>, caller: &Caller, request: TaskRequest) -> Result<TaskView> {
        self.start_task(caller, request, Origin::New)
    }

    /// The Failed tile's Retry: the same request as a new task. Policy and
    /// scope are checked again, as for any start. Only the task's own user
    /// may retry it, since the new task acts on their behalf.
    pub fn retry(self: &Arc<Self>, caller: &Caller, id: TaskId) -> Result<TaskView> {
        let task = self.authorized(caller, id)?;
        if task.user.uid != caller.uid {
            return Err(RuntimeError::PermissionDenied(format!(
                "only {} can retry their own task",
                task.user.name
            )));
        }
        if task.is_app() {
            return Err(RuntimeError::Conflict(
                "that was an app; open the app again instead".into(),
            ));
        }
        if task.lock().tile.is_live() {
            return Err(RuntimeError::Conflict(
                "that task is still running; stop it first".into(),
            ));
        }
        let request = TaskRequest {
            agent: task.entry.id.clone(),
            prompt: task.prompt.clone(),
            scope: task.scope.clone(),
            title: Some(task.lock().title.clone()),
        };
        self.start_task(caller, request, Origin::RetryOf(id))
    }

    pub(crate) fn start_task(
        self: &Arc<Self>,
        caller: &Caller,
        request: TaskRequest,
        origin: Origin,
    ) -> Result<TaskView> {
        let prepared = self.prepare(caller, &request)?;
        let live = self
            .tasks()
            .values()
            .filter(|t| t.user.uid == caller.uid && !t.is_app() && t.lock().tile.is_live())
            .count();
        if live >= self.config.max_live_tasks_per_user {
            return Err(RuntimeError::Conflict(format!(
                "you already have {live} agents running; stop or wait for one first"
            )));
        }

        let id = TaskId::new();
        let name = format!("glass-{}", id.short());
        let sandbox = SandboxSpec {
            root: self.config.state_dir.join("sandboxes").join(&name),
            name,
            mounts: self
                .config
                .agent_dirs
                .iter()
                .map(|dir| Mount {
                    source: dir.clone(),
                    target: dir.clone(),
                    writable: false,
                })
                .collect(),
            user: caller.name.clone(),
            uid: caller.uid,
            gid: caller.gid,
        };
        let ctx = TaskContext {
            principal: AgentPrincipal {
                agent: prepared.entry.id.clone(),
                on_behalf_of: UserRef {
                    name: caller.name.clone(),
                    uid: caller.uid,
                },
                task: id,
            },
            owner: Owner {
                uid: caller.uid,
                gid: caller.gid,
            },
            home: caller.home.clone(),
            grants: prepared.grants,
            groups: prepared.policy.groups().map(str::to_owned).collect(),
        };
        let layer_note = prepared.card.layer.as_ref().map(crate::tailor::note);
        let driver_socket = prepared
            .entry
            .attended
            .then(|| self.config.attended_dir.join(format!("{id}.sock")));
        let (retry_of, continues) = match origin {
            Origin::New => (None, None),
            Origin::RetryOf(first) => (Some(first), None),
            Origin::Continues(from) => (None, Some(from)),
        };
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = Arc::new(Task::new(NewTask {
            id,
            user: caller.clone(),
            entry: prepared.entry,
            prompt: request.prompt,
            scope: request.scope,
            plan: prepared.card.plan,
            policy: prepared.policy,
            ctx,
            sandbox,
            cant_touch: prepared.card.cant_touch,
            settings: prepared.settings,
            layer: prepared.layer,
            driver_socket,
            continues,
            app: None,
            title: prepared.title.clone(),
            title_fixed: request.title.is_some(),
            stop: stop_tx,
        }));
        self.tasks().insert(id, Arc::clone(&task));

        let mut event = self.task_event(
            &task,
            EventKind::TaskStarted,
            format!("Started: {}", task.prompt),
        );
        event.detail = serde_json::json!({
            "prompt": task.prompt,
            "title": prepared.title,
            "plan": task.plan,
            "grants": task.ctx.grants.iter().map(|g| serde_json::json!({
                "name": g.name, "path": g.root, "writable": g.writable
            })).collect::<Vec<_>>(),
            "sandbox": self.sandbox.kind(),
            "retry_of": retry_of,
            "continues": task.continues.as_ref().map(|c| c.id),
            "attended": task.entry.attended,
            "layer": task.layer,
            "settings": task.settings,
        });
        self.audit(event);
        {
            let mut state = task.lock();
            state.note(
                ActivityKind::State,
                match (retry_of, &task.continues) {
                    (Some(first), _) => {
                        format!("Started by {} (retry of {})", caller.name, first.short())
                    }
                    (None, Some(from)) => format!(
                        "Started by {}, continuing {} ({} {})",
                        caller.name,
                        from.id.short(),
                        from.agent,
                        from.ended
                    ),
                    (None, None) => format!("Started by {}", caller.name),
                },
            );
            if let Some(note) = layer_note {
                state.note(ActivityKind::Notice, note);
            }
        }
        self.publish(&task);
        self.supervise(&task, stop_rx);
        Ok(task.view(None))
    }

    /// Adds a new task and runs its supervisor, which sets it up and always
    /// tears it down.
    pub(crate) fn supervise(self: &Arc<Self>, task: &Arc<Task>, stop: oneshot::Receiver<()>) {
        let handle = tokio::spawn(supervise::run(Arc::clone(self), Arc::clone(task), stop));
        let mut supervisors = self.supervisors.lock().unwrap_or_else(|p| p.into_inner());
        supervisors.retain(|h| !h.is_finished());
        supervisors.push(handle);
    }

    // ---- lifecycle -------------------------------------------------------

    /// Holds the agent at its next safe point and shows `tile`.
    pub(crate) fn hold(&self, task: &Task, tile: TileState, note: &str) {
        task.gate.send_replace(Gate::Hold);
        let mut state = task.lock();
        state.tile = tile;
        state.note(ActivityKind::State, note);
    }

    fn live(&self, caller: &Caller, id: TaskId) -> Result<Arc<Task>> {
        let task = self.authorized(caller, id)?;
        if !task.lock().tile.is_live() {
            return Err(RuntimeError::Conflict("that task has already ended".into()));
        }
        Ok(task)
    }

    pub(crate) fn state_change(&self, task: &Task, summary: &str) {
        self.audit(self.task_event(task, EventKind::TaskStateChanged, summary));
        self.publish(task);
    }

    pub fn pause(&self, caller: &Caller, id: TaskId) -> Result<TaskView> {
        let task = self.live(caller, id)?;
        self.hold(&task, TileState::Paused, "Paused by you");
        self.state_change(&task, "Paused");
        Ok(task.view(self.desk.pending_for_task(id)))
    }

    pub fn resume(&self, caller: &Caller, id: TaskId) -> Result<TaskView> {
        let task = self.live(caller, id)?;
        if task.lock().tile == TileState::YouAreDriving {
            return Err(RuntimeError::Conflict(
                "you have control; hand back instead".into(),
            ));
        }
        self.release(&task, "Resumed");
        self.state_change(&task, "Resumed");
        Ok(task.view(self.desk.pending_for_task(id)))
    }

    /// Lets the agent continue; the tile shows "Needs you" again if an ask is
    /// still open.
    fn release(&self, task: &Task, note: &str) {
        let pending = self.desk.pending_for_task(task.id).is_some();
        {
            let mut state = task.lock();
            if let Some(caption) = state.sleeping.take() {
                state.caption = caption;
            }
            state.tile = if pending {
                TileState::NeedsYou
            } else {
                TileState::Working
            };
            state.note(ActivityKind::State, note);
        }
        task.gate.send_replace(Gate::Run);
    }

    /// Take over (AW-20): the agent pauses at its next safe point and the user
    /// has control. Available at any time, including during an ask (AW-22).
    pub async fn take_over(&self, caller: &Caller, id: TaskId) -> Result<TaskView> {
        let task = self.live(caller, id)?;
        if task.entry.attended {
            return Err(RuntimeError::Conflict(
                "you're already driving this task in your harness".into(),
            ));
        }
        if task.is_app() {
            return Err(RuntimeError::Conflict(
                "that's an app you're using; there's no agent to take over from".into(),
            ));
        }
        self.hold(&task, TileState::YouAreDriving, "You took over");
        let roots: Vec<PathBuf> = task
            .ctx
            .grants
            .iter()
            .filter(|g| g.writable)
            .map(|g| g.root.clone())
            .collect();
        if !roots.is_empty() {
            match self.take_snapshot(&task, &roots, "You took over").await {
                Ok(handle) => task.lock().takeover = Some(handle),
                Err(e) => {
                    tracing::warn!(error = %e, "no snapshot at take over; hand back can't summarize")
                }
            }
        }
        self.state_change(&task, "User took over");
        Ok(task.view(self.desk.pending_for_task(id)))
    }

    /// Hand back (AW-21): says in one sentence what the user changed, then
    /// lets the agent resume from there.
    pub async fn hand_back(&self, caller: &Caller, id: TaskId) -> Result<TaskView> {
        let task = self.live(caller, id)?;
        if task.entry.attended {
            return Ok(self.finish_attended(&task));
        }
        if task.lock().tile != TileState::YouAreDriving {
            return Err(RuntimeError::Conflict(
                "you don't have control of that task".into(),
            ));
        }
        let handle = task.lock().takeover.take();
        let summary = match handle {
            Some(handle) => {
                // The user's edits join the task's changes, undoable like
                // the agent's (ADR 0017).
                let edits = self.record_edits(&task, handle).await?;
                match edits.len() {
                    0 => "You handed back without changing any files.".to_owned(),
                    n => format!(
                        "While you drove you changed {n} file{}: {}.",
                        if n == 1 { "" } else { "s" },
                        edits[..n.min(5)].join(", ")
                    ),
                }
            }
            None => "You handed back.".to_owned(),
        };
        task.lock().caption = summary.clone();
        self.release(&task, &summary);
        self.state_change(&task, &summary);
        Ok(task.view(self.desk.pending_for_task(id)))
    }

    /// Stops the agent now. Work done so far stays (and stays undoable).
    pub fn stop(&self, caller: &Caller, id: TaskId) -> Result<TaskView> {
        let task = self.live(caller, id)?;
        self.stop_task(&task, "Stopped by you");
        Ok(task.view(None))
    }

    pub(crate) fn stop_task(&self, task: &Task, note: &str) {
        self.end_task(task, TileState::Stopped, note);
    }

    /// Ends a live task now, showing `tile`: the supervisor tears it down.
    pub(crate) fn end_task(&self, task: &Task, tile: TileState, note: &str) {
        {
            let mut state = task.lock();
            state.tile = tile;
            state.note(ActivityKind::State, note);
        }
        task.gate.send_replace(Gate::Ended);
        self.desk.end_task(task.id);
        if let Some(stop) = task.stop.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = stop.send(());
        }
        self.state_change(task, note);
    }

    /// Removes a finished tile. The audit log and snapshots stay.
    pub fn dismiss(&self, caller: &Caller, id: TaskId) -> Result<()> {
        let task = self.authorized(caller, id)?;
        if task.lock().tile.is_live() {
            return Err(RuntimeError::Conflict(
                "stop the task before dismissing it".into(),
            ));
        }
        self.tasks().remove(&id);
        let _ = self
            .events
            .send((task.user.uid, Event::TaskRemoved { task: id }));
        Ok(())
    }

    // ---- snapshots (AU-20) -------------------------------------------------

    /// Removes snapshots older than `max_age` that no task in memory could
    /// still undo from (AU-20: kept 30 days by default). A task's own
    /// snapshots stay as long as the task does, however old, so undo never
    /// finds one missing. Blocking: it lists and destroys snapshots.
    pub fn prune_snapshots(&self, max_age: std::time::Duration) -> Result<usize> {
        let keep: std::collections::BTreeSet<glass_types::SnapshotId> = self
            .tasks()
            .values()
            .flat_map(|task| {
                let state = task.lock();
                state
                    .snapshots
                    .iter()
                    .map(|s| s.handle.id)
                    .chain(state.takeover.as_ref().map(|h| h.id))
                    .collect::<Vec<_>>()
            })
            .collect();
        let before = std::time::SystemTime::now()
            .checked_sub(max_age)
            .unwrap_or(std::time::UNIX_EPOCH);
        let gone = self
            .snapshots
            .prune(before, &keep)
            .map_err(|e| RuntimeError::Internal(format!("removing old snapshots: {e}")))?;
        if gone > 0 {
            let days = max_age.as_secs() / 86_400;
            let mut event = AuditEvent::new(
                EventKind::SnapshotsRemoved,
                format!(
                    "{gone} snapshot(s) older than {days} days removed; undo can't go back to them"
                ),
            );
            event.detail = serde_json::json!({ "removed": gone, "older_than_days": days });
            self.audit(event);
        }
        Ok(gone)
    }

    // ---- power (PM-5) ------------------------------------------------------

    /// The machine is about to sleep: hold every running agent at its next
    /// safe point, and give actions already under way a moment to finish so
    /// nothing is mid-change when it sleeps. Never holds up sleep for long.
    /// Agents the user paused or is driving are left as they are.
    pub async fn suspend(&self, caller: &Caller) -> Result<PowerResult> {
        if !caller.is_admin() {
            return Err(RuntimeError::PermissionDenied(
                "only the system pauses agents for sleep".into(),
            ));
        }
        Ok(self.pause_for_sleep().await)
    }

    pub(crate) async fn pause_for_sleep(&self) -> PowerResult {
        let tasks: Vec<Arc<Task>> = self.tasks().values().cloned().collect();
        let mut paused = Vec::new();
        for task in tasks {
            let mut state = task.lock();
            let running = matches!(
                state.tile,
                TileState::Starting | TileState::Working | TileState::NeedsYou
            );
            if !running || state.sleeping.is_some() {
                continue;
            }
            task.gate.send_replace(Gate::Hold);
            let caption = std::mem::replace(&mut state.caption, "Paused for sleep".into());
            state.sleeping = Some(caption);
            state.tile = TileState::Paused;
            state.note(ActivityKind::State, "Paused for sleep");
            drop(state);
            paused.push(task);
        }
        let deadline = tokio::time::Instant::now() + SLEEP_GRACE;
        while paused
            .iter()
            .any(|t| t.in_flight.load(Ordering::SeqCst) > 0)
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        for task in &paused {
            self.state_change(task, "Paused for sleep");
        }
        PowerResult {
            tasks: paused.len(),
        }
    }

    /// The machine woke: agents paused for sleep carry on where they were.
    pub fn wake(&self, caller: &Caller) -> Result<PowerResult> {
        if !caller.is_admin() {
            return Err(RuntimeError::PermissionDenied(
                "only the system resumes agents after sleep".into(),
            ));
        }
        let resumed = self.resume_after_sleep();
        self.woke();
        Ok(resumed)
    }

    pub(crate) fn resume_after_sleep(&self) -> PowerResult {
        let sleeping: Vec<Arc<Task>> = self
            .tasks()
            .values()
            .filter(|t| t.lock().sleeping.is_some())
            .cloned()
            .collect();
        for task in &sleeping {
            self.release(task, "Resumed after sleep");
            self.state_change(task, "Resumed after sleep");
        }
        PowerResult {
            tasks: sleeping.len(),
        }
    }

    // ---- approvals -------------------------------------------------------

    pub fn approvals(&self, caller: &Caller) -> Vec<ApprovalView> {
        self.desk
            .pending((!caller.is_admin()).then_some(caller.uid))
    }

    /// Answers an ask (DA-11). Only the task's own user may.
    pub fn answer(&self, caller: &Caller, request: AnswerApproval) -> Result<ApprovalView> {
        let view = self
            .desk
            .answer(request.approval, request.answer, caller.uid)
            .map_err(|e| match e {
                DeskError::NotFound => RuntimeError::NotFound("no such pending approval".into()),
                DeskError::AlreadyAnswered(how) => RuntimeError::Conflict(format!("already {how}")),
            })?;
        let answer = match request.answer {
            ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            } => "approved once",
            ApprovalAnswer::Approve {
                scope: ApprovalScope::Task,
            } => "approved for the rest of the task",
            ApprovalAnswer::Deny => "denied",
        };
        if let Some(task) = self.task(view.task) {
            let mut event = self.task_event(
                &task,
                EventKind::ApprovalAnswered,
                format!("{}: {answer}", view.summary),
            );
            event.action = Some(view.action.clone());
            event.approval = Some(ApprovalRef {
                id: view.id,
                answered_by: Some(caller.name.clone()),
                answer: Some(answer.into()),
                answered_at: Some(Timestamp::now()),
            });
            self.audit(event);
        }
        Ok(view)
    }

    // ---- audit -----------------------------------------------------------

    /// Every record for one task, to rebuild it end to end (AU-3).
    pub fn audit_task(&self, caller: &Caller, id: TaskId) -> Result<Vec<AuditRecord>> {
        let task = self.authorized(caller, id)?;
        blocking(|| glass_audit::read(self.audit.path(), |r| r.event.task == Some(task.id)))
            .map_err(|e| RuntimeError::Internal(e.to_string()))
    }

    pub fn audit_verify(&self, caller: &Caller) -> Result<AuditVerifyResult> {
        if !caller.is_admin() {
            return Err(RuntimeError::PermissionDenied(
                "verifying the whole log needs root".into(),
            ));
        }
        let report = blocking(|| glass_audit::verify(self.audit.path()))
            .map_err(|e| RuntimeError::Internal(e.to_string()))?;
        Ok(AuditVerifyResult {
            records: report.records,
            last_hash: report.last_hash,
        })
    }

    // ---- snapshots -------------------------------------------------------

    /// Takes a snapshot of `roots`, records it on the task timeline (AW-15)
    /// and in the audit log.
    pub(crate) async fn take_snapshot(
        &self,
        task: &Task,
        roots: &[PathBuf],
        reason: &str,
    ) -> std::result::Result<SnapshotHandle, String> {
        let snapshots = Arc::clone(&self.snapshots);
        let roots = roots.to_vec();
        let id = SnapshotId::new();
        let handle = tokio::task::spawn_blocking(move || snapshots.snapshot(id, &roots))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let marker = SnapshotMarker {
            id,
            at: Timestamp::now(),
            reason: reason.to_owned(),
        };
        {
            let mut state = task.lock();
            state.note(ActivityKind::Snapshot, format!("Snapshot: {reason}"));
            state.snapshots.push(TakenSnapshot {
                marker,
                handle: handle.clone(),
            });
        }
        let mut event = self.task_event(
            task,
            EventKind::SnapshotTaken,
            format!("Snapshot: {reason}"),
        );
        event.snapshot = Some(id);
        event.targets = handle.parts.iter().map(|p| p.name.clone()).collect();
        self.audit(event);
        Ok(handle)
    }
}

/// Runs blocking work (writing and syncing a file, reading the audit log)
/// without stalling the async workers that answer everyone else: on
/// glassd's multi-threaded runtime the worker hands its other tasks to
/// another thread first (docs/dev/performance.md, rule 3). Elsewhere, such
/// as single-threaded test runtimes, it just runs it.
pub(crate) fn blocking<R>(work: impl FnOnce() -> R) -> R {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Validates and names the folders a task may touch. Users may grant only
/// folders inside their home; root may grant any.
fn resolve_scope(caller: &Caller, scope: &ScopeRequest) -> Result<Vec<Grant>> {
    let mut grants: Vec<Grant> = Vec::new();
    let requested = scope
        .write
        .iter()
        .map(|p| (p, true))
        .chain(scope.read.iter().map(|p| (p, false)));
    for (path, writable) in requested {
        if !path.is_absolute() {
            return Err(RuntimeError::Invalid(format!(
                "{} must be an absolute path",
                path.display()
            )));
        }
        let root = std::fs::canonicalize(path)
            .map_err(|_| RuntimeError::NotFound(format!("{} does not exist", path.display())))?;
        if !root.is_dir() {
            return Err(RuntimeError::Invalid(format!(
                "{} is not a folder; grant folders",
                path.display()
            )));
        }
        if !caller.is_admin() && !root.starts_with(&caller.home) {
            return Err(RuntimeError::PermissionDenied(format!(
                "{} is outside your home folder",
                root.display()
            )));
        }
        if let Some(existing) = grants.iter().find(|g| g.root == root) {
            // Write wins over read; it comes first.
            debug_assert!(existing.writable || !writable);
            continue;
        }
        let base = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".into());
        let mut name = base.clone();
        let mut n = 2;
        while grants.iter().any(|g| g.name == name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        grants.push(Grant {
            name,
            root,
            writable,
        });
    }
    Ok(grants)
}

/// A host path as the user thinks of it: `~/Reports/x.csv`.
pub(crate) fn display_path(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}
