//! Agent computers through the control socket (ADR 0065, ADR 0066): a
//! person's agents, each with a computer of its own, their screens, their
//! keyboards, their sleep. Who may use one is the caller's kernel-reported
//! uid; every step is on the record.

use std::sync::Arc;
use std::time::Duration;

use glass_api::{
    ComputerNew, ComputerView, DisplayInputRequest, FrameRequest, FrameView, PersonaNew,
    ScreenInput, ScreenRequest, ScreenView,
};
use glass_audit::{AuditEvent, EventKind};
use glass_computers::sandbox::{ComputerSandbox, LeaseNews};

use crate::error::Result;
use crate::runtime::blocking;
use crate::{Caller, Runtime, RuntimeError};

/// How long a line its person typed may run before it's stopped.
const INPUT_LIMIT: Duration = Duration::from_secs(120);

fn refused(e: glass_platform::PlatformError) -> RuntimeError {
    match e {
        glass_platform::PlatformError::Unsupported(what) => {
            RuntimeError::NotFound(format!("there's no {what}"))
        }
        glass_platform::PlatformError::Unsafe(why) => RuntimeError::Conflict(why),
        other => RuntimeError::Internal(other.to_string()),
    }
}

/// Computers put to sleep at once as their host leaves. Each save sends
/// [`glass_computers::store::CHUNKS_AT_ONCE`] chunks at a time, so this many
/// computers is a dozen or so uploads at once: several times one computer
/// after another, without the host's disk and network being all there is
/// to wait on.
pub const SLEEPS_AT_ONCE: usize = 4;

impl Runtime {
    pub(crate) fn computers(&self) -> Result<&Arc<ComputerSandbox>> {
        self.computers
            .as_ref()
            .ok_or_else(|| RuntimeError::Invalid("this system doesn't keep agent computers".into()))
    }

    fn computer_event(&self, caller: &Caller, id: &str, summary: String) -> AuditEvent {
        let mut event = AuditEvent::new(EventKind::Computer, summary);
        event.user = Some(caller.name.clone());
        event.detail = serde_json::json!({ "computer": id });
        event
    }

    /// With its live tasks filled in.
    fn with_tasks(&self, caller: &Caller, mut view: ComputerView) -> ComputerView {
        view.live_tasks = self
            .tasks()
            .values()
            .filter(|t| {
                t.user.uid == caller.uid
                    && t.sandbox.computer.as_deref() == Some(view.id.as_str())
                    && t.lock().tile.is_live()
            })
            .map(|t| t.id)
            .collect();
        view
    }

    pub fn computer_list(&self, caller: &Caller) -> Result<Vec<ComputerView>> {
        let computers = self.computers()?;
        Ok(blocking(|| computers.list(caller.uid))
            .into_iter()
            .map(|view| self.with_tasks(caller, view))
            .collect())
    }

    /// A new agent with a computer of its own: one of the catalog's agents
    /// the caller may run.
    pub fn computer_make(&self, caller: &Caller, new: ComputerNew) -> Result<ComputerView> {
        let computers = self.computers()?;
        if !self.agents_for(caller).iter().any(|a| a.id == new.agent) {
            return Err(RuntimeError::PermissionDenied(format!(
                "{} isn't an agent you may run",
                new.agent
            )));
        }
        let view = blocking(|| computers.make(caller.uid, caller.gid, &new)).map_err(refused)?;
        let mut event = self.computer_event(
            caller,
            &view.id,
            format!("Made {} a computer of its own", view.name),
        );
        event.agent = Some(view.agent.clone());
        event.detail["kind"] = view.kind.clone().into();
        self.audit(event);
        Ok(view)
    }

    /// A new agent on the caller's own computer (ADR 0068), made first if
    /// they have none: one of the catalog's agents they may run, with a
    /// name and a role of their choosing.
    pub fn persona_make(&self, caller: &Caller, new: PersonaNew) -> Result<ComputerView> {
        let computers = self.computers()?;
        if !self.agents_for(caller).iter().any(|a| a.id == new.agent) {
            return Err(RuntimeError::PermissionDenied(format!(
                "{} isn't an agent you may run",
                new.agent
            )));
        }
        let made_computer =
            blocking(|| computers.get(caller.uid, glass_api::PERSON_COMPUTER)).is_err();
        let view =
            blocking(|| computers.add_persona(caller.uid, caller.gid, "Your computer", &new))
                .map_err(refused)?;
        let name = view
            .personas
            .iter()
            .find(|p| p.id == new.id)
            .map_or_else(|| new.id.clone(), |p| p.name.clone());
        let summary = if made_computer {
            format!("Made a computer of their own, with {name} on it")
        } else {
            format!("Added {name} to their computer")
        };
        let mut event = self.computer_event(caller, &view.id, summary);
        event.agent = Some(new.agent.clone());
        event.detail["persona"] = new.id.clone().into();
        event.detail["kind"] = view.kind.clone().into();
        self.audit(event);
        Ok(self.with_tasks(caller, view))
    }

    /// The agent goes from the caller's computer; what it left there stays.
    pub fn persona_remove(&self, caller: &Caller, id: &str) -> Result<ComputerView> {
        let computers = self.computers()?;
        let view = blocking(|| computers.remove_persona(caller.uid, id)).map_err(refused)?;
        let mut event = self.computer_event(
            caller,
            &view.id,
            format!("Removed {id} from their computer"),
        );
        event.detail["persona"] = id.into();
        self.audit(event);
        Ok(self.with_tasks(caller, view))
    }

    pub fn computer_screen(&self, caller: &Caller, request: &ScreenRequest) -> Result<ScreenView> {
        let computers = self.computers()?;
        blocking(|| computers.screen(caller.uid, &request.id, request.offset)).map_err(refused)
    }

    /// What its display shows (ADR 0072): the picture, unless it's the
    /// version the caller has.
    pub fn computer_frame(&self, caller: &Caller, request: &FrameRequest) -> Result<FrameView> {
        let computers = self.computers()?;
        let (frame, version, unchanged) =
            blocking(|| computers.frame(caller.uid, &request.id, request.since.as_deref()))
                .map_err(refused)?;
        Ok(FrameView {
            version,
            width: frame.width,
            height: frame.height,
            png: (!unchanged).then(|| glass_managed::base64::encode(&frame.png)),
            taken_over: computers.taken_over(caller.uid, &request.id),
        })
    }

    /// Its person's pointer and keys on its display, while they have it.
    /// Taking it over and handing it back are on the record; what they do
    /// on it in between is theirs, as on their own laptop.
    pub fn computer_display(&self, caller: &Caller, request: &DisplayInputRequest) -> Result<()> {
        let computers = self.computers()?;
        blocking(|| computers.display_input(caller.uid, &request.id, &request.events))
            .map_err(refused)
    }

    /// Its person takes the keyboard. The agent's running tasks carry on;
    /// new ones wait until it's handed back.
    pub fn computer_take_over(&self, caller: &Caller, id: &str) -> Result<ComputerView> {
        let computers = self.computers()?;
        let view = computers.take_over(caller.uid, id).map_err(refused)?;
        self.audit(self.computer_event(caller, id, format!("Took over {}'s computer", view.name)));
        Ok(self.with_tasks(caller, view))
    }

    pub fn computer_hand_back(&self, caller: &Caller, id: &str) -> Result<ComputerView> {
        let computers = self.computers()?;
        let view = computers.hand_back(caller.uid, id).map_err(refused)?;
        self.audit(self.computer_event(
            caller,
            id,
            format!("Handed {}'s computer back", view.name),
        ));
        Ok(self.with_tasks(caller, view))
    }

    /// Puts it to sleep (ADR 0065 §11): saved to the store, gone from this
    /// host. Saving takes as long as its disk does.
    pub async fn computer_sleep(&self, caller: &Caller, id: &str) -> Result<ComputerView> {
        let computers = Arc::clone(self.computers()?);
        let (uid, owned) = (caller.uid, id.to_owned());
        let view = tokio::task::spawn_blocking(move || computers.sleep(uid, &owned))
            .await
            .map_err(|e| RuntimeError::Internal(e.to_string()))?
            .map_err(refused)?;
        let mut event =
            self.computer_event(caller, id, format!("Put {}'s computer to sleep", view.name));
        if let Some(stored) = &view.stored {
            event.detail["version"] = stored.version.clone().into();
            event.detail["bytes"] = stored.bytes.into();
            event.detail["place"] = stored.place.clone().into();
        }
        self.audit(event);
        Ok(self.with_tasks(caller, view))
    }

    pub async fn computer_wake(&self, caller: &Caller, id: &str) -> Result<ComputerView> {
        let computers = Arc::clone(self.computers()?);
        let (uid, gid, owned) = (caller.uid, caller.gid, id.to_owned());
        let view = tokio::task::spawn_blocking(move || computers.wake(uid, gid, &owned))
            .await
            .map_err(|e| RuntimeError::Internal(e.to_string()))?
            .map_err(refused)?;
        self.audit(self.computer_event(caller, id, format!("Woke {}'s computer", view.name)));
        Ok(self.with_tasks(caller, view))
    }

    /// Computers awake on this host, every person's: what's metered.
    pub fn computers_awake(&self) -> u32 {
        self.computers
            .as_ref()
            .map_or(0, |c| blocking(|| c.awake()))
    }

    /// This host is being replaced (its Auto Scaling group's lifecycle
    /// hook, ADR 0066): no computer starts or wakes here again, and each
    /// goes to sleep in the store as its work ends, for another host to
    /// wake. Returns once none is awake, or after `within`, when the group
    /// stops waiting; what's still awake then is on the record.
    pub async fn leave_host(&self, within: Duration) {
        let Some(computers) = self.computers.as_ref().map(Arc::clone) else {
            return;
        };
        computers.leave();
        self.audit(AuditEvent::new(
            EventKind::Computer,
            "This host is being replaced: its agent computers go to sleep, for another host to wake",
        ));
        let started = std::time::Instant::now();
        loop {
            let sleepy = {
                let computers = Arc::clone(&computers);
                tokio::task::spawn_blocking(move || computers.idle(Duration::ZERO))
                    .await
                    .unwrap_or_default()
            };
            // A few at a time (audit 7.1): one after another, about ten
            // computers' saves overran the time the group waits, and the
            // rest went with their work since they last slept. None is
            // begun once the time is up: it would be cut off half saved.
            let mut sleepy = sleepy.into_iter();
            let mut sleeping = tokio::task::JoinSet::new();
            loop {
                while sleeping.len() < SLEEPS_AT_ONCE && started.elapsed() < within {
                    let Some((uid, id)) = sleepy.next() else {
                        break;
                    };
                    let computers = Arc::clone(&computers);
                    sleeping.spawn_blocking(move || {
                        let slept = computers.sleep(uid, &id);
                        (uid, id, slept)
                    });
                }
                let Some(done) = sleeping.join_next().await else {
                    break;
                };
                let mut event = match done {
                    Ok((uid, id, slept)) => {
                        let summary = match slept {
                            Ok(view) => format!(
                                "Put {}'s computer to sleep: this host is being replaced",
                                view.name
                            ),
                            Err(e) => {
                                format!("Couldn't put a computer to sleep as this host goes: {e}")
                            }
                        };
                        let mut event = AuditEvent::new(EventKind::Computer, summary);
                        event.user = glass_platform::users::by_uid(uid).map(|u| u.name);
                        event.detail = serde_json::json!({ "computer": id, "leaving": true });
                        event
                    }
                    Err(e) => AuditEvent::new(
                        EventKind::Computer,
                        format!("Couldn't put a computer to sleep as this host goes: {e}"),
                    ),
                };
                if event.detail.is_null() {
                    event.detail = serde_json::json!({ "leaving": true });
                }
                self.audit(event);
            }
            let awake = self.computers_awake();
            if awake == 0 {
                return;
            }
            if started.elapsed() >= within {
                self.audit(AuditEvent::new(
                    EventKind::Computer,
                    format!(
                        "This host is going with {awake} agent computers still awake: their \
                         work since they last slept is lost"
                    ),
                ));
                return;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Keeps this host's leases on the computers awake here (ADR 0066):
    /// one host at a time has a computer, and says so in the store every
    /// [`glass_computers::store::RENEW_EVERY`]. glassd calls it that often.
    ///
    /// A computer another host took while this one couldn't reach the
    /// store has its tasks here stopped, and this host's copy of it set
    /// aside: both go on the record.
    pub async fn keep_computer_leases(&self) {
        let Some(computers) = self.computers.as_ref().map(Arc::clone) else {
            return;
        };
        let news = tokio::task::spawn_blocking(move || computers.keep_leases())
            .await
            .unwrap_or_default();
        let mut troubled = std::collections::HashSet::new();
        for item in news {
            let (uid, id, summary, detail) = match item {
                LeaseNews::Lost {
                    uid,
                    id,
                    name,
                    host,
                } => {
                    // What still runs in it works on a copy that's saved
                    // nowhere: it stops, and says why.
                    let live: Vec<_> = self
                        .tasks()
                        .values()
                        .filter(|t| {
                            t.user.uid == uid
                                && t.sandbox.computer.as_deref() == Some(id.as_str())
                                && t.lock().tile.is_live()
                        })
                        .cloned()
                        .collect();
                    for task in &live {
                        self.stop_task(
                            task,
                            "Stopped: its computer was woken on another host while this one \
                             couldn't reach the store",
                        );
                    }
                    (
                        uid,
                        id,
                        format!(
                            "{name}'s computer was taken by another host ({host}) while this \
                             one couldn't reach the store: nothing of this host's copy is \
                             saved over it"
                        ),
                        serde_json::json!({ "taken_by": host, "tasks_stopped": live.len() }),
                    )
                }
                LeaseNews::SetAside {
                    uid,
                    id,
                    name,
                    host,
                    aside,
                } => (
                    uid,
                    id,
                    format!(
                        "Set this host's copy of {name}'s computer aside: another host \
                         ({host}) has it"
                    ),
                    serde_json::json!({ "taken_by": host, "aside": aside }),
                ),
                LeaseNews::Trouble { uid, id, name, why } => {
                    // The store out of reach: said once per computer until
                    // it's back, not every half minute.
                    let first = !self
                        .lease_trouble
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains(&(uid, id.clone()));
                    troubled.insert((uid, id.clone()));
                    if first {
                        tracing::warn!(
                            computer = %id,
                            name = %name,
                            "couldn't tell the store this host still has a computer awake: {why}"
                        );
                    }
                    continue;
                }
            };
            let mut event = AuditEvent::new(EventKind::Computer, summary);
            event.user = glass_platform::users::by_uid(uid).map(|u| u.name);
            let mut detail = detail;
            detail["computer"] = serde_json::json!(id);
            event.detail = detail;
            self.audit(event);
        }
        *self.lease_trouble.lock().unwrap_or_else(|e| e.into_inner()) = troubled;
    }

    /// Puts to sleep the computers nobody used for `idle`, so nothing stays
    /// on a host it needn't (ADR 0065 §11). glassd calls it every minute.
    pub async fn sleep_idle_computers(&self, idle: Duration) {
        let Some(computers) = self.computers.as_ref().map(Arc::clone) else {
            return;
        };
        let sleepy = {
            let computers = Arc::clone(&computers);
            tokio::task::spawn_blocking(move || computers.idle(idle))
                .await
                .unwrap_or_default()
        };
        for (uid, id) in sleepy {
            let computers = Arc::clone(&computers);
            let owned = id.clone();
            let slept = tokio::task::spawn_blocking(move || computers.sleep(uid, &owned)).await;
            let name = glass_platform::users::by_uid(uid).map(|u| u.name);
            let summary = match &slept {
                Ok(Ok(view)) => format!(
                    "Put {}'s computer to sleep after {} minutes unused",
                    view.name,
                    idle.as_secs() / 60
                ),
                Ok(Err(e)) => format!("Couldn't put an unused computer to sleep: {e}"),
                Err(e) => format!("Couldn't put an unused computer to sleep: {e}"),
            };
            let mut event = AuditEvent::new(EventKind::Computer, summary);
            event.user = name;
            event.detail =
                serde_json::json!({ "computer": id, "idle_minutes": idle.as_secs() / 60 });
            self.audit(event);
        }
    }

    /// Runs a line its person typed, as root on the computer; what it
    /// prints goes to the screen. Returns once it started: a line may run
    /// for up to two minutes.
    pub fn computer_input(self: &Arc<Self>, caller: &Caller, input: ScreenInput) -> Result<()> {
        let computers = Arc::clone(self.computers()?);
        let (command, run) =
            blocking(|| computers.input(caller.uid, &input.id, &input.line)).map_err(refused)?;
        let mut event = self.computer_event(
            caller,
            &input.id,
            "Typed a line on an agent's computer".to_owned(),
        );
        event.detail["line"] = input.line.trim().into();
        if !self.audit(event) {
            computers.input_done(caller.uid, &input.id, &run);
            return Err(RuntimeError::Conflict(
                "the record can't take it, so it didn't run".into(),
            ));
        }
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                computers.input_done(caller.uid, &input.id, &run);
                return Err(RuntimeError::Internal(format!("running it: {e}")));
            }
        };
        let (uid, id) = (caller.uid, input.id);
        tokio::spawn(async move {
            if tokio::time::timeout(INPUT_LIMIT, child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
            computers.input_done(uid, &id, &run);
        });
        Ok(())
    }
}
