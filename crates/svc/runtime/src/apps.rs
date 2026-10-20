//! App sessions (UI-5, ADR 0024): an installed web app acting for the user
//! who opened it.
//!
//! A session is a task whose principal is the app: every call runs the
//! same pipeline as an agent's (decide → ask → snapshot → invoke → audit),
//! attributed "by [app] for [user]". The app may call only what its
//! manifest declares (DV-3): the manifest comes from the app's installed
//! copy, read by glassd, and both this module and the session's policy
//! ([`glass_policy::Policy::for_app`]) refuse anything else. A session has
//! no sandbox, no program and no grants of folders: the app reaches the
//! user's data only through its declared actions.

use std::collections::BTreeMap;
use std::sync::Arc;

use glass_actions::TaskContext;
use glass_api::{
    APP_STATUS_MAX_WAIT_SECS, ActivityKind, AgentMode, AppAction, AppCall, AppCallResult, AppFiles,
    AppManifest, AppSession, AppStatus, AppStatusRequest, AppWaiting, DisplaySafe, ProfileApp,
    ProfileAppKind, ScopeRequest, TileState,
};
use glass_audit::{AuditEvent, EventKind, Outcome as AuditOutcome, PolicyDecision};
use glass_platform::fs::Owner;
use glass_platform::sandbox::SandboxSpec;
use glass_policy::EffectivePolicy;
use glass_types::{ActionName, AgentId, AgentPrincipal, TaskId, Tier, UserRef};
use tokio::sync::oneshot;

use crate::error::Result;
use crate::task::{NewTask, Task};
use crate::{Caller, CatalogEntry, PersonalApp, Runtime, RuntimeError};

/// When each person's data in each app last changed: a counter across
/// the device, and per login and app the count at its last change.
#[derive(Debug, Default)]
pub(crate) struct AppChanges {
    seq: u64,
    last: BTreeMap<(String, String), u64>,
    /// Bumped with each new policy: open apps' tiers may have changed.
    rules: u64,
}

/// Apps one user may have open at once. Each open window is a session.
pub const MAX_OPEN_APPS_PER_USER: usize = 16;

/// The admin console in a profile (ADR 0023). It is served from the
/// console's own servers and reads from them, not from this device.
pub const CONSOLE_APP: &str = "console";

/// Native apps a profile may name (ADR 0025), with their names. The Browser
/// is the browser engine's (CF-15, ADR 0027); the desktop starts it.
const NATIVE_APPS: &[(&str, &str)] = &[
    ("activity-monitor", "Activity Monitor"),
    ("browser", "Browser"),
    ("terminal", "Terminal"),
];

impl Runtime {
    /// The installed app with this id.
    fn installed_app(&self, id: &AgentId) -> Option<Arc<AppManifest>> {
        self.config
            .apps
            .iter()
            .find(|m| &m.id == id)
            .map(|m| Arc::new(m.clone()))
    }

    /// The org's editions of our apps (ADR 0032 §6): org apps the catalog
    /// lists under one of our installed apps' ids. Each replaces ours for
    /// the people it reaches; everyone else keeps ours.
    pub(crate) fn editions(&self) -> std::collections::BTreeSet<AgentId> {
        let listed = self.config.org_catalog.lists();
        self.config
            .org_apps
            .apps()
            .into_iter()
            .map(|m| m.id)
            .filter(|id| listed.contains(id) && self.installed_app(id).is_some())
            .collect()
    }

    /// Whether `caller` may have app `id` at all, as far as the catalog
    /// says: an app it gates must reach them, but an edition never takes
    /// ours away from the people it doesn't reach.
    pub(crate) fn app_reach<'a>(&'a self, caller: &'a Caller) -> impl Fn(&AgentId) -> bool + 'a {
        let editions = self.editions();
        let reaches = self.catalog_reach(caller);
        move |id| editions.contains(id) || reaches(id)
    }

    /// `caller`'s own apps they may open now (ADR 0032): a remix while the
    /// app it remixes is in their profile; an app built from the template
    /// always; none while IT has remixing off (TL-6).
    pub(crate) fn own_apps(&self, caller: &Caller) -> Vec<PersonalApp> {
        let policy = self.policy();
        // TL-6: IT turned remixing off; their apps wait until it's back on.
        if !policy.remixing() {
            return Vec::new();
        }
        let profile = policy.apps_for(&caller.name);
        let reaches = self.app_reach(caller);
        self.config
            .personal
            .apps(&caller.name, &caller.home)
            .into_iter()
            .filter(|own| own.manifest.id.is_personal())
            .filter(|own| {
                own.based_on
                    .as_ref()
                    .is_none_or(|base| profile.contains(base) && reaches(base))
            })
            .collect()
    }

    /// What `caller`'s own `app` serves now (`app.files`): the bridge gets
    /// a person's apps from glassd, never from a folder they could change.
    pub fn app_files(&self, caller: &Caller, app: &AgentId) -> Result<AppFiles> {
        let not_found = || RuntimeError::NotFound(format!("no app {app} of yours"));
        if !app.is_personal() {
            // An org app (R2), to someone the catalog lets it reach.
            if !self.org_apps(caller).iter().any(|m| &m.id == app) {
                return Err(not_found());
            }
            let (version, files) = self.config.org_apps.files(app).ok_or_else(not_found)?;
            return Ok(AppFiles {
                app: app.clone(),
                version: version.parse().unwrap_or(0),
                files,
            });
        }
        if !self
            .own_apps(caller)
            .iter()
            .any(|own| &own.manifest.id == app)
        {
            return Err(not_found());
        }
        let (version, files) = self
            .config
            .personal
            .files(&caller.name, &caller.home, app)
            .ok_or_else(not_found)?;
        Ok(AppFiles {
            app: app.clone(),
            version,
            files,
        })
    }

    /// The org apps (ADR 0032, R2) that reach `caller`: those the catalog
    /// requires for their groups, or offers and they installed (DV-11).
    /// One the catalog doesn't list reaches nobody. Editions of our apps
    /// are among them, for the people they reach, if ours is in their
    /// profile.
    pub(crate) fn org_apps(&self, caller: &Caller) -> Vec<AppManifest> {
        let reaches = self.catalog_reach(caller);
        let listed = self.config.org_catalog.lists();
        let policy = self.policy();
        let profile = policy.apps_for(&caller.name);
        self.config
            .org_apps
            .apps()
            .into_iter()
            .filter(|m| !m.id.is_personal())
            .filter(|m| self.installed_app(&m.id).is_none() || profile.contains(&m.id))
            .filter(|m| listed.contains(&m.id) && reaches(&m.id))
            .collect()
    }

    /// The apps in `caller`'s profile, as the launcher offers them: web
    /// apps installed here, native apps, and the admin console when this
    /// device is managed. Policy decides the profile per group (AD-7).
    pub fn profile_apps(&self, caller: &Caller) -> Vec<ProfileApp> {
        let policy = self.policy();
        let reaches = self.app_reach(caller);
        let org = self.org_apps(caller);
        let mut apps: Vec<ProfileApp> = policy
            .apps_for(&caller.name)
            .into_iter()
            .filter(|id| reaches(id))
            .filter_map(|id| {
                if let Some(manifest) = self.config.apps.iter().find(|m| &m.id == id) {
                    // The org's edition of it, where it reaches them.
                    let edition = org.iter().find(|m| &m.id == id);
                    return Some(ProfileApp {
                        id: id.clone(),
                        name: edition.map_or(&manifest.name, |m| &m.name).clone(),
                        kind: ProfileAppKind::Web,
                        url: None,
                        based_on: None,
                    });
                }
                if id.as_str() == CONSOLE_APP {
                    // Only a managed device has a console to open.
                    return self.config.console_url.as_ref().map(|url| ProfileApp {
                        id: id.clone(),
                        name: "Admin console".into(),
                        kind: ProfileAppKind::Console,
                        url: Some(url.clone()),
                        based_on: None,
                    });
                }
                // A first-party native app; anything else the profile names
                // isn't on this device (a web app not installed here), so
                // it isn't offered.
                let (_, name) = NATIVE_APPS
                    .iter()
                    .find(|(native, _)| *native == id.as_str())?;
                Some(ProfileApp {
                    id: id.clone(),
                    name: (*name).to_owned(),
                    kind: ProfileAppKind::Native,
                    url: None,
                    based_on: None,
                })
            })
            .collect();
        // Apps people made that IT approved (R2); editions are above.
        let editions = self.editions();
        apps.extend(
            org.into_iter()
                .filter(|m| !editions.contains(&m.id))
                .map(|m| ProfileApp {
                    id: m.id,
                    name: m.name,
                    kind: ProfileAppKind::Web,
                    url: None,
                    based_on: None,
                }),
        );
        // Their own apps, after the org's (ADR 0032).
        apps.extend(self.own_apps(caller).into_iter().map(|own| ProfileApp {
            id: own.manifest.id,
            name: own.manifest.name,
            kind: ProfileAppKind::Web,
            url: None,
            based_on: own.based_on,
        }));
        apps
    }

    /// Opens `app` for `caller` (`app.open`): a new session, whose calls
    /// act for them. Blocking: it prepares the app's data folders.
    pub fn open_app(self: &Arc<Self>, caller: &Caller, app: &AgentId) -> Result<AppSession> {
        let (manifest, allowed) = if app.is_personal() {
            // Their own app (ADR 0032): only they open it, and a remix only
            // while the app it remixes is theirs to open.
            let own = self
                .config
                .personal
                .apps(&caller.name, &caller.home)
                .into_iter()
                .find(|own| &own.manifest.id == app)
                .ok_or_else(|| RuntimeError::NotFound(format!("no app {app} of yours")))?;
            let allowed = self.own_apps(caller).iter().any(|o| &o.manifest.id == app);
            (Arc::new(own.manifest), allowed)
        } else if let Some(org) = self.org_apps(caller).into_iter().find(|m| &m.id == app) {
            // An org app (R2), or the org's edition of one of ours: the
            // catalog decided it reaches them.
            (Arc::new(org), true)
        } else if self.installed_app(app).is_none()
            && let Some(org) = self
                .config
                .org_apps
                .apps()
                .into_iter()
                .find(|m| &m.id == app)
        {
            // An org app that doesn't reach them.
            (Arc::new(org), false)
        } else {
            let manifest = self
                .installed_app(app)
                .ok_or_else(|| RuntimeError::NotFound(format!("no app {app} is installed")))?;
            // Role-based delivery: only apps in the caller's profile open,
            // and one the org's catalog makes available only once installed.
            let allowed =
                self.policy().apps_for(&caller.name).contains(app) && self.app_reach(caller)(app);
            (manifest, allowed)
        };
        if !allowed {
            let mut event = AuditEvent::new(
                EventKind::AppRefused,
                format!("{} isn't in {}'s profile", manifest.name, caller.name),
            );
            event.user = Some(caller.name.clone());
            event.agent = Some(app.clone());
            self.audit(event);
            return Err(RuntimeError::PermissionDenied(format!(
                "{} isn't in your profile. IT decides which apps you get.",
                manifest.name
            )));
        }
        // Counted and added under one lock (released when this returns).
        let _opening = self.opening_app.lock().unwrap_or_else(|p| p.into_inner());
        let open = self
            .tasks()
            .values()
            .filter(|t| t.is_app() && t.user.uid == caller.uid && t.lock().tile.is_live())
            .count();
        if open >= MAX_OPEN_APPS_PER_USER {
            return Err(RuntimeError::Conflict(format!(
                "you already have {open} app windows open; close one first"
            )));
        }
        // Declaration ∩ policy (DV-3, ADR 0008): the manifest's actions at
        // the tiers org and group rules give them, and no more than its
        // most sensitive label.
        let policy =
            self.policy()
                .for_app(&caller.name, app, &manifest.actions, manifest.max_label());
        let id = TaskId::new();
        let ctx = TaskContext {
            principal: AgentPrincipal {
                agent: app.clone(),
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
            grants: Vec::new(),
            groups: policy.groups().map(str::to_owned).collect(),
            data_label: None,
        };
        // The app's data folders (~/Mail, ~/Calendar) exist before its
        // first call, as they do before an agent's.
        let mut providers = Vec::new();
        for action in &manifest.actions {
            if let Some((_, provider)) = self.registry().get(action)
                && !providers.iter().any(|p| Arc::ptr_eq(p, provider))
            {
                providers.push(Arc::clone(provider));
            }
        }
        for provider in &providers {
            provider
                .prepare(&ctx)
                .map_err(|e| RuntimeError::Internal(format!("{}: {e}", manifest.name)))?;
        }

        let entry = CatalogEntry {
            id: app.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            model: String::new(),
            description: manifest.description.clone(),
            mode: AgentMode::Actions,
            program: Default::default(),
            attended: false,
            args: Vec::new(),
            plan: Vec::new(),
            customize: Vec::new(),
        };
        let name = format!("app-{}", id.short());
        let sandbox = SandboxSpec {
            // Never created: an app session has no sandbox.
            root: self.config.state_dir.join("sandboxes").join(&name),
            name,
            mounts: Vec::new(),
            user: caller.name.clone(),
            uid: caller.uid,
            gid: caller.gid,
        };
        let actions = self.app_actions(&manifest, &policy);
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = Arc::new(Task::new(NewTask {
            id,
            user: caller.clone(),
            entry,
            prompt: format!("Use {}", manifest.name),
            scope: ScopeRequest::default(),
            plan: Vec::new(),
            policy,
            ctx,
            sandbox,
            cant_touch: Vec::new(),
            settings: BTreeMap::new(),
            layer: None,
            driver_socket: None,
            continues: None,
            app: Some(Arc::clone(&manifest)),
            title: manifest.name.clone(),
            title_fixed: true,
            stop: stop_tx,
        }));
        self.tasks().insert(id, Arc::clone(&task));
        let mut event = self.task_event(
            &task,
            EventKind::TaskStarted,
            format!("Opened {}", manifest.name),
        );
        event.detail = serde_json::json!({
            "app": app,
            "version": manifest.version,
            "declares": manifest.actions,
            "max_label": manifest.max_label(),
        });
        self.audit(event);
        task.lock()
            .note(ActivityKind::State, format!("Opened by {}", caller.name));
        self.supervise(&task, stop_rx);

        // The manifest's text was checked as glassd installed it, and the
        // titles are the providers' own.
        Ok(AppSession {
            session: id,
            app: app.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            user: caller.name.clone(),
            actions,
            permissions: manifest.permissions.clone(),
        })
    }

    /// What `manifest`'s app may call and how each runs under `policy`.
    fn app_actions(&self, manifest: &AppManifest, policy: &EffectivePolicy) -> Vec<AppAction> {
        let registry = self.registry();
        manifest
            .actions
            .iter()
            .filter_map(|name| registry.get(name))
            .map(|(def, _)| AppAction {
                name: def.name.clone(),
                title: def.title.clone(),
                tier: policy.tier_for(&def.name, def.default_tier).tier,
                side_effect: def.side_effect,
            })
            .collect()
    }

    /// A new policy: open apps hear their tiers again.
    pub(crate) fn rules_changed(&self) {
        self.app_changes.send_modify(|c| c.rules += 1);
    }

    /// The caller's own live app session.
    fn app_session(&self, caller: &Caller, session: TaskId) -> Result<Arc<Task>> {
        self.task(session)
            .filter(|t| t.is_app() && t.user.uid == caller.uid)
            .ok_or_else(|| RuntimeError::NotFound(format!("no app session {}", session.short())))
    }

    /// Runs one call for an app (`app.call`). Refusals (undeclared, blocked
    /// by policy, declined on the desktop) are results, each audited.
    pub async fn app_call(
        self: &Arc<Self>,
        caller: &Caller,
        call: AppCall,
    ) -> Result<AppCallResult> {
        let task = self.app_session(caller, call.session)?;
        let manifest = task.app.clone().expect("an app session has its manifest");
        if !manifest.declares(&call.action) {
            return Ok(self.refuse_undeclared(&task, &manifest, &call.action));
        }
        let registry = self.registry();
        let Some((def, provider)) = registry.get(&call.action) else {
            return Ok(self.refuse_undeclared(&task, &manifest, &call.action));
        };
        let (def, provider) = (def.clone(), Arc::clone(provider));
        let result = self.run_action(&task, def, provider, call.input).await;
        Ok(AppCallResult {
            ok: !result.is_error,
            text: result.text(),
            data: result.structured_content.unwrap_or_default(),
        })
    }

    /// DV-3: an app calls only what its manifest declares. The refusal is
    /// audited like any blocked call, so an app probing for more shows.
    fn refuse_undeclared(
        &self,
        task: &Task,
        manifest: &AppManifest,
        action: &ActionName,
    ) -> AppCallResult {
        let mut event = self.task_event(
            task,
            EventKind::Action,
            format!("Not declared by {}: {action}", manifest.name),
        );
        event.action = Some(action.clone());
        event.outcome = Some(AuditOutcome::Blocked);
        event.decision = Some(PolicyDecision {
            tier: Tier::Never,
            rule: format!("app manifest {} {}", manifest.id, manifest.version),
        });
        self.audit(event);
        task.lock().note(
            ActivityKind::Blocked,
            format!("Blocked: {action} isn't in {}'s manifest", manifest.name),
        );
        AppCallResult {
            ok: false,
            text: format!(
                "{} doesn't declare {action} in its manifest, so the system won't run it.",
                manifest.name
            ),
            data: serde_json::Value::Null,
        }
    }

    /// Whether the session is open, and what it is waiting on the user for
    /// (`app.status`), so the app can say so.
    pub fn app_status(&self, caller: &Caller, session: TaskId) -> Result<AppStatus> {
        let task = self.app_session(caller, session)?;
        let live = task.lock().tile.is_live();
        let waiting = live
            .then(|| self.desk.pending_for_task(task.id))
            .flatten()
            .map(|ask| {
                let mut waiting = AppWaiting {
                    summary: ask.summary,
                    items: ask.items,
                };
                waiting.make_display_safe();
                waiting
            });
        let manifest = task.app.clone().expect("an app session has its manifest");
        let actions = self.app_actions(&manifest, &task.policy());
        let changes = self.app_changes.borrow();
        Ok(AppStatus {
            live,
            waiting,
            cursor: changes.seq,
            changed: Vec::new(),
            actions,
        })
    }

    /// `login`'s data in `app` changed, such as a chat message that
    /// arrived: wakes that app's `app.status` waits (ADR 0030).
    pub fn app_changed(&self, login: &str, app: &str) {
        self.app_changes.send_modify(|c| {
            c.seq += 1;
            c.last.insert((login.to_owned(), app.to_owned()), c.seq);
        });
    }

    /// `app.status`, waiting up to `request.wait_secs` for the caller's
    /// data in this session's app to change after `request.since`.
    pub async fn app_status_wait(
        &self,
        caller: &Caller,
        request: AppStatusRequest,
    ) -> Result<AppStatus> {
        let task = self.app_session(caller, request.session)?;
        let key = (caller.name.clone(), task.entry.id.to_string());
        let wait = std::time::Duration::from_secs(request.wait_secs.min(APP_STATUS_MAX_WAIT_SECS));
        let mut changes = self.app_changes.subscribe();
        let rules = changes.borrow().rules;
        let changed = |c: &AppChanges| c.last.get(&key).is_some_and(|at| *at > request.since);
        let _ =
            tokio::time::timeout(wait, changes.wait_for(|c| changed(c) || c.rules != rules)).await;
        let mut status = self.app_status(caller, request.session)?;
        if changed(&self.app_changes.borrow()) {
            status.changed.push(key.1);
        }
        Ok(status)
    }

    /// Closes the caller's app session (`app.close`).
    pub fn close_app(&self, caller: &Caller, session: TaskId) -> Result<()> {
        let task = self.app_session(caller, session)?;
        self.end_app(&task);
        Ok(())
    }

    /// Ends an app session, if it is still open: the supervisor withdraws
    /// its asks and records it closed. Also how a dropped connection ends
    /// its sessions, so nothing outlives the app (PA-2).
    pub(crate) fn end_app(&self, task: &Task) {
        if task.lock().tile.is_live() {
            self.end_task(task, TileState::Done, "Closing");
        }
    }

    /// Ends the session `id` whoever's it is. For the app socket's own
    /// bookkeeping when a connection drops.
    pub(crate) fn end_app_by_id(&self, id: TaskId) {
        if let Some(task) = self.task(id).filter(|t| t.is_app()) {
            self.end_app(&task);
        }
    }

    /// A closed app session that changed nothing leaves only its audit
    /// trail; one that changed something stays for review and undo.
    pub(crate) fn forget_if_unchanged(&self, task: &Task) {
        if task.is_app() && task.lock().changes.is_empty() {
            self.tasks().remove(&task.id);
        }
    }
}
