//! Schedules (ADR 0069): directions a person gives one of their agents to
//! carry out on a schedule, on their computer (ADR 0068).
//!
//! They're kept off the computer, in a [`ScheduleStore`] the hosts share
//! (DynamoDB on an agent host, a file on a laptop), so a schedule runs while
//! its computer sleeps and whichever host is up. Each host looks for what's
//! due; the first to claim a run starts it, as the person, waking their
//! computer if it sleeps. A claim is a write that holds only if nobody
//! changed the schedule since it was read, so a run starts once however many
//! hosts look.
//!
//! - A slot late by more than [`LATE_GRACE_SECS`] isn't run: it's recorded
//!   as missed and the next one counted from now. Nothing is replayed.
//! - A slot that comes while the last run is going is let pass.
//! - [`SCHEDULE_FAILURES_MAX`] runs in a row that fail or can't start turn
//!   it off, with why; turning it on again forgets them.
//! - A person keeps [`SCHEDULES_MAX`] on.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use glass_api::{
    PERSON_COMPUTER, SCHEDULE_FAILURES_MAX, SCHEDULE_PROMPT_MAX_CHARS, SCHEDULES_MAX, ScheduleNew,
    ScheduleRunView, ScheduleView, ScopeRequest, TaskRequest, TileState,
};
use glass_app_routines::{Schedule, local_minute};
use glass_audit::{AuditEvent, EventKind};
use glass_types::{TaskId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::people::Claims;
use crate::runtime::{Origin, blocking};
use crate::task::{ScheduleRun, Task};
use crate::{Caller, Runtime, RuntimeError};

/// How late a slot may start; later, it's missed.
pub const LATE_GRACE_SECS: i64 = 3600;
/// How long a run is taken to be going when the host that started it can't
/// be asked: past this, a new slot runs.
const RUNNING_MAX_SECS: i64 = 6 * 3600;
/// Tries at a write that lost to another, before giving up on it.
const WRITE_TRIES: usize = 5;

/// A schedule as the store keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSchedule {
    /// Whose: the person's key in the store (ADR 0068), the same on every
    /// host.
    pub owner: String,
    pub id: String,
    /// Changes by one at every write; a write holds only over the version
    /// it read.
    pub version: u64,
    /// Who it runs as.
    pub holder: Holder,
    pub persona: String,
    pub prompt: String,
    pub when: String,
    pub zone: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_why: Option<String>,
    /// When its next slot starts, in Unix seconds; while it's on.
    pub next_run: i64,
    #[serde(default)]
    pub failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<LastRun>,
    pub created_at: Timestamp,
    /// What a newer version wrote here that this one doesn't know: kept,
    /// and written back with the rest (audit 4.11). Hosts are a version
    /// apart while an update goes round, and a host claims a run by
    /// writing the whole schedule: an older one wrote it back without the
    /// newer one's fields.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

/// Who a schedule runs as: what a host that never saw them needs to act
/// for them, and nothing a sign-in proves with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Holder {
    /// A signed-in person (ADR 0067).
    Person {
        sub: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        email: String,
    },
    /// An account on this computer: a laptop, a development host.
    Local { uid: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRun {
    pub at: i64,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl StoredSchedule {
    /// Whether the store's index of what's due should hold it.
    pub fn queued(&self) -> bool {
        self.enabled
    }
}

/// Why a write didn't hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// Someone changed it first: read it again.
    Changed,
    Failed(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed => f.write_str("it changed meanwhile"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// Where schedules are kept: shared by every host that may run them.
pub trait ScheduleStore: Send + Sync + fmt::Debug {
    /// Keeps a new one; [`StoreError::Changed`] if its id is taken.
    fn make(&self, schedule: &StoredSchedule) -> std::result::Result<(), StoreError>;
    /// `owner`'s, oldest first.
    fn of(&self, owner: &str) -> std::result::Result<Vec<StoredSchedule>, String>;
    /// Writes `schedule` whole if the kept one is still at version `seen`.
    fn replace(&self, schedule: &StoredSchedule, seen: u64) -> std::result::Result<(), StoreError>;
    /// Removes `owner`'s `id`; false if they have none.
    fn remove(&self, owner: &str, id: &str) -> std::result::Result<bool, String>;
    /// Those on with a slot at or before `now`. May lag a write.
    fn due(&self, now: i64) -> std::result::Result<Vec<StoredSchedule>, String>;
}

/// Schedules in a file, for one host: a laptop or a development host.
#[derive(Debug)]
pub struct FileSchedules {
    path: std::path::PathBuf,
    lock: std::sync::Mutex<()>,
}

impl FileSchedules {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            lock: std::sync::Mutex::new(()),
        }
    }

    fn read(&self) -> std::result::Result<Vec<StoredSchedule>, String> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("{} isn't readable: {e}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("reading {}: {e}", self.path.display())),
        }
    }

    fn write(&self, all: &[StoredSchedule]) -> std::result::Result<(), String> {
        glass_state::write_json_pretty(&self.path, all, glass_state::PRIVATE)
            .map_err(|e| format!("writing {}: {e}", self.path.display()))
    }

    fn locked<R>(&self, work: impl FnOnce() -> R) -> R {
        let _held = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        work()
    }
}

impl ScheduleStore for FileSchedules {
    fn make(&self, schedule: &StoredSchedule) -> std::result::Result<(), StoreError> {
        self.locked(|| {
            let mut all = self.read().map_err(StoreError::Failed)?;
            if all.iter().any(|s| s.id == schedule.id) {
                return Err(StoreError::Changed);
            }
            all.push(schedule.clone());
            self.write(&all).map_err(StoreError::Failed)
        })
    }

    fn of(&self, owner: &str) -> std::result::Result<Vec<StoredSchedule>, String> {
        self.locked(|| {
            Ok(self
                .read()?
                .into_iter()
                .filter(|s| s.owner == owner)
                .collect())
        })
    }

    fn replace(&self, schedule: &StoredSchedule, seen: u64) -> std::result::Result<(), StoreError> {
        self.locked(|| {
            let mut all = self.read().map_err(StoreError::Failed)?;
            let kept = all
                .iter_mut()
                .find(|s| s.owner == schedule.owner && s.id == schedule.id)
                .ok_or(StoreError::Changed)?;
            if kept.version != seen {
                return Err(StoreError::Changed);
            }
            *kept = schedule.clone();
            self.write(&all).map_err(StoreError::Failed)
        })
    }

    fn remove(&self, owner: &str, id: &str) -> std::result::Result<bool, String> {
        self.locked(|| {
            let mut all = self.read()?;
            let before = all.len();
            all.retain(|s| !(s.owner == owner && s.id == id));
            if all.len() == before {
                return Ok(false);
            }
            self.write(&all).map(|()| true)
        })
    }

    fn due(&self, now: i64) -> std::result::Result<Vec<StoredSchedule>, String> {
        self.locked(|| {
            Ok(self
                .read()?
                .into_iter()
                .filter(|s| s.queued() && s.next_run <= now)
                .collect())
        })
    }
}

/// `when` read as a schedule, or why not, for people.
fn parse_when(when: &str) -> Result<Schedule> {
    Schedule::from_str(when.trim()).map_err(|_| {
        RuntimeError::Invalid(format!(
            "{when:?} isn't a schedule: write \"daily 18:30\", \"weekdays 09:00\", \
             \"weekends 10:00\", \"mon,thu 08:30\" or \"hourly\""
        ))
    })
}

/// The start of `schedule`'s first slot after `after`, in `zone`, in Unix
/// seconds. The zone's offset is the one at that slot, daylight saving
/// included.
pub fn next_slot(schedule: &Schedule, zone: &str, after: i64) -> Result<i64> {
    let offset_at = |at: i64| {
        glass_platform::system::zone_offset_at(zone, at).ok_or_else(|| {
            RuntimeError::Invalid(format!("{zone:?} isn't a time zone this host knows"))
        })
    };
    let mut offset = offset_at(after)?;
    let mut slot = after;
    // An offset that changes between now and the slot moves it; twice at
    // most, across one change.
    for _ in 0..3 {
        let (day, hour, minute, _) = local_minute(after, offset);
        let ahead = schedule
            .minutes_until(day, hour, minute)
            .ok_or_else(|| RuntimeError::Invalid("that schedule never runs".into()))?;
        slot = after.div_euclid(60) * 60 + i64::from(ahead) * 60;
        let there = offset_at(slot)?;
        if there == offset {
            return Ok(slot);
        }
        offset = there;
    }
    Ok(slot)
}

fn at(secs: i64) -> Timestamp {
    Timestamp::from_unix_seconds(secs).unwrap_or_else(Timestamp::now)
}

fn view(s: &StoredSchedule) -> ScheduleView {
    ScheduleView {
        id: s.id.clone(),
        persona: s.persona.clone(),
        prompt: s.prompt.clone(),
        when: s.when.clone(),
        describe: Schedule::from_str(&s.when).map_or_else(|_| s.when.clone(), |w| w.describe()),
        zone: s.zone.clone(),
        enabled: s.enabled,
        disabled_why: s.disabled_why.clone(),
        next_run: s.enabled.then(|| at(s.next_run)),
        failures: s.failures,
        last: s.last.as_ref().map(|l| ScheduleRunView {
            at: at(l.at),
            outcome: l.outcome.clone(),
            task: l.task,
            note: l.note.clone(),
        }),
        created_at: s.created_at,
    }
}

fn random_id() -> Result<String> {
    glass_crypto::random::<6>()
        .map(|b| format!("s{}", hex::encode(b)))
        .map_err(|e| RuntimeError::Internal(format!("no randomness: {e}")))
}

fn failed(e: String) -> RuntimeError {
    RuntimeError::Internal(format!("schedules: {e}"))
}

/// One due schedule's turn, for the record and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    Started(TaskId),
    /// Another host claimed it first.
    Lost,
    Missed,
    /// The last run hadn't ended.
    Skipped,
    Refused(String),
}

/// What a host claims to do with a due slot.
enum Plan {
    Start,
    Missed,
    Skip,
    Off(String),
}

impl Runtime {
    /// Where schedules are kept, set once at startup.
    pub fn set_schedules(&self, store: Arc<dyn ScheduleStore>) {
        let _ = self.schedules.set(store);
    }

    fn schedule_store(&self) -> Result<&Arc<dyn ScheduleStore>> {
        self.schedules
            .get()
            .ok_or_else(|| RuntimeError::Conflict("schedules aren't kept on this host".into()))
    }

    /// The caller's key in the store, and who their schedules run as.
    fn schedule_owner(&self, caller: &Caller) -> Result<(String, Holder)> {
        if let Some(identity) = self.identity() {
            let person = identity.people.by_uid(caller.uid).ok_or_else(|| {
                RuntimeError::PermissionDenied("you aren't signed in on this host".into())
            })?;
            return Ok((
                person.owner_key(),
                Holder::Person {
                    sub: person.key.clone(),
                    name: person.name.clone(),
                    email: person.email.clone(),
                },
            ));
        }
        Ok((
            format!("u{}", caller.uid),
            Holder::Local { uid: caller.uid },
        ))
    }

    fn schedule_event(&self, user: &str, s: &StoredSchedule, summary: String) {
        let mut event = AuditEvent::new(EventKind::Schedule, summary);
        event.user = Some(user.to_owned());
        event.detail = serde_json::json!({
            "schedule": s.id, "persona": s.persona, "when": s.when, "zone": s.zone,
        });
        self.audit(event);
    }

    fn own_schedule(&self, caller: &Caller, id: &str) -> Result<(String, StoredSchedule)> {
        let (owner, _) = self.schedule_owner(caller)?;
        let store = self.schedule_store()?;
        let found = blocking(|| store.of(&owner))
            .map_err(failed)?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| RuntimeError::NotFound(format!("you have no schedule {id}")))?;
        Ok((owner, found))
    }

    /// `schedule.list`: the caller's.
    pub fn schedules_of(&self, caller: &Caller) -> Result<Vec<ScheduleView>> {
        let (owner, _) = self.schedule_owner(caller)?;
        let store = self.schedule_store()?;
        let mut all = blocking(|| store.of(&owner)).map_err(failed)?;
        all.sort_by_key(|s| s.created_at);
        Ok(all.iter().map(view).collect())
    }

    /// `schedule.make`: one of the caller's agents, directed on a schedule.
    pub fn schedule_make(&self, caller: &Caller, new: ScheduleNew) -> Result<ScheduleView> {
        self.schedule_make_at(caller, new, Timestamp::now().unix_seconds())
    }

    /// As [`Runtime::schedule_make`], as if it were `now`.
    pub fn schedule_make_at(
        &self,
        caller: &Caller,
        new: ScheduleNew,
        now: i64,
    ) -> Result<ScheduleView> {
        let store = self.schedule_store()?;
        let computers = self.computers()?;
        let (owner, holder) = self.schedule_owner(caller)?;
        // Only one of their own agents: what another person has isn't
        // theirs to direct.
        blocking(|| computers.persona(caller.uid, PERSON_COMPUTER, &new.persona))
            .map_err(|_| RuntimeError::NotFound(format!("you have no agent {:?}", new.persona)))?;
        let prompt = new.prompt.trim();
        if prompt.is_empty() {
            return Err(RuntimeError::Invalid("say what the agent should do".into()));
        }
        if prompt.chars().count() > SCHEDULE_PROMPT_MAX_CHARS {
            return Err(RuntimeError::Invalid(format!(
                "directions are at most {SCHEDULE_PROMPT_MAX_CHARS} characters"
            )));
        }
        let when = parse_when(&new.when)?;
        let zone = new.zone.as_deref().map_or("UTC", str::trim).to_owned();
        if !glass_platform::system::valid_zone(&zone) {
            return Err(RuntimeError::Invalid(format!(
                "{zone:?} isn't a time zone this host knows: write one such as \"Europe/London\""
            )));
        }
        let next_run = next_slot(&when, &zone, now)?;
        let mine = blocking(|| store.of(&owner)).map_err(failed)?;
        if mine.iter().filter(|s| s.enabled).count() >= SCHEDULES_MAX {
            return Err(RuntimeError::Conflict(format!(
                "you have {SCHEDULES_MAX} schedules on: turn one off or remove it first"
            )));
        }
        let made = StoredSchedule {
            owner,
            id: random_id()?,
            version: 1,
            holder,
            persona: new.persona,
            prompt: prompt.to_owned(),
            when: when.to_string(),
            zone,
            enabled: true,
            disabled_why: None,
            next_run,
            failures: 0,
            last: None,
            created_at: at(now),
            other: BTreeMap::new(),
        };
        blocking(|| store.make(&made)).map_err(|e| failed(e.to_string()))?;
        self.schedule_event(
            &caller.name,
            &made,
            format!("Scheduled {}: {}", made.persona, when.describe()),
        );
        Ok(view(&made))
    }

    /// `schedule.remove`
    pub fn schedule_remove(&self, caller: &Caller, id: &str) -> Result<()> {
        let (owner, found) = self.own_schedule(caller, id)?;
        let store = self.schedule_store()?;
        if !blocking(|| store.remove(&owner, id)).map_err(failed)? {
            return Err(RuntimeError::NotFound(format!("you have no schedule {id}")));
        }
        self.schedule_event(
            &caller.name,
            &found,
            format!("Removed the schedule for {}", found.persona),
        );
        Ok(())
    }

    /// `schedule.enable`: on again, its failures forgotten and its next
    /// slot counted from now; or off.
    pub fn schedule_enable(
        &self,
        caller: &Caller,
        id: &str,
        enabled: bool,
    ) -> Result<ScheduleView> {
        self.schedule_enable_at(caller, id, enabled, Timestamp::now().unix_seconds())
    }

    /// As [`Runtime::schedule_enable`], as if it were `now`.
    pub fn schedule_enable_at(
        &self,
        caller: &Caller,
        id: &str,
        enabled: bool,
        now: i64,
    ) -> Result<ScheduleView> {
        let store = self.schedule_store()?;
        for _ in 0..WRITE_TRIES {
            let (owner, found) = self.own_schedule(caller, id)?;
            if enabled && !found.enabled {
                let on = blocking(|| store.of(&owner))
                    .map_err(failed)?
                    .iter()
                    .filter(|s| s.enabled)
                    .count();
                if on >= SCHEDULES_MAX {
                    return Err(RuntimeError::Conflict(format!(
                        "you have {SCHEDULES_MAX} schedules on: turn one off or remove it first"
                    )));
                }
            }
            let mut changed = found.clone();
            changed.version += 1;
            changed.enabled = enabled;
            changed.disabled_why = None;
            if enabled {
                changed.failures = 0;
                changed.next_run = next_slot(&parse_when(&found.when)?, &found.zone, now)?;
            }
            match blocking(|| store.replace(&changed, found.version)) {
                Ok(()) => {
                    let summary = format!(
                        "Turned {} the schedule for {}",
                        if enabled { "on" } else { "off" },
                        found.persona
                    );
                    self.schedule_event(&caller.name, &changed, summary);
                    return Ok(view(&changed));
                }
                Err(StoreError::Changed) => {}
                Err(StoreError::Failed(e)) => return Err(failed(e)),
            }
        }
        Err(RuntimeError::Conflict("it kept changing; try again".into()))
    }

    /// Starts every schedule due at `now` that this host claims first. glassd
    /// calls it every half minute. Each schedule's turn, by id.
    pub fn run_due_schedules(self: &Arc<Self>, now: i64) -> Vec<(String, Turn)> {
        let Some(store) = self.schedules.get() else {
            return Vec::new();
        };
        // A host being replaced starts nothing (ADR 0066): another claims.
        if self.computers.as_ref().is_some_and(|c| c.leaving()) {
            return Vec::new();
        }
        let due = match store.due(now) {
            Ok(due) => due,
            Err(e) => {
                tracing::warn!("reading the schedules due: {e}");
                return Vec::new();
            }
        };
        due.into_iter()
            .filter(|s| s.enabled && s.next_run <= now)
            .map(|s| {
                let id = s.id.clone();
                (id, self.take_turn(store.as_ref(), s, now))
            })
            .collect()
    }

    fn take_turn(
        self: &Arc<Self>,
        store: &dyn ScheduleStore,
        seen: StoredSchedule,
        now: i64,
    ) -> Turn {
        let next = Schedule::from_str(&seen.when)
            .map_err(|_| RuntimeError::Invalid(format!("{:?} isn't a schedule", seen.when)))
            .and_then(|w| next_slot(&w, &seen.zone, now.max(seen.next_run)));
        let running = seen
            .last
            .as_ref()
            .filter(|l| l.outcome == "running")
            .is_some_and(|l| {
                match l.task.and_then(|t| self.tasks().get(&t).cloned()) {
                    // Here: going while its tile is.
                    Some(task) => task.lock().tile.is_live(),
                    // On another host, or one that stopped: going for a while.
                    None => now - l.at < RUNNING_MAX_SECS,
                }
            });
        let mut claim = seen.clone();
        claim.version += 1;
        let plan = match next {
            Err(e) => {
                claim.enabled = false;
                claim.disabled_why = Some(e.to_string());
                Plan::Off(e.to_string())
            }
            Ok(next) => {
                claim.next_run = next;
                // The run going stays the schedule's last, however late
                // this slot is: a slot marked missed over it would let the
                // next start a second run beside it (ScheduleClaims.tla).
                if running {
                    Plan::Skip
                } else if now - seen.next_run > LATE_GRACE_SECS {
                    claim.last = Some(LastRun {
                        at: now,
                        outcome: "missed".into(),
                        task: None,
                        note: format!("no host was up within an hour of {}", at(seen.next_run)),
                    });
                    Plan::Missed
                } else {
                    claim.last = Some(LastRun {
                        at: now,
                        outcome: "running".into(),
                        task: None,
                        note: String::new(),
                    });
                    Plan::Start
                }
            }
        };
        // The claim: holds only if no other host claimed first.
        match store.replace(&claim, seen.version) {
            Ok(()) => {}
            Err(StoreError::Changed) => return Turn::Lost,
            Err(StoreError::Failed(e)) => {
                tracing::warn!(schedule = %seen.id, "claiming a schedule's run: {e}");
                return Turn::Lost;
            }
        }
        let user = self.holder_name(&claim.holder);
        match plan {
            Plan::Start => match self.start_scheduled(&claim) {
                Ok(task) => {
                    self.note_run(store, &claim, now, |last, _| last.task = Some(task));
                    Turn::Started(task)
                }
                Err(why) => {
                    let note = why.to_string();
                    self.note_run(store, &claim, now, |last, s| {
                        last.outcome = "refused".into();
                        last.note.clone_from(&note);
                        s.failures += 1;
                        if s.failures >= SCHEDULE_FAILURES_MAX {
                            s.enabled = false;
                            s.disabled_why = Some(format!(
                                "{SCHEDULE_FAILURES_MAX} runs in a row didn't work; the last: {note}"
                            ));
                        }
                    });
                    self.schedule_event(
                        &user,
                        &claim,
                        format!(
                            "Couldn't start the scheduled run for {}: {note}",
                            claim.persona
                        ),
                    );
                    Turn::Refused(note)
                }
            },
            Plan::Missed => {
                self.schedule_event(
                    &user,
                    &claim,
                    format!("Let a late slot pass for {}", claim.persona),
                );
                Turn::Missed
            }
            Plan::Skip => Turn::Skipped,
            Plan::Off(why) => {
                self.schedule_event(
                    &user,
                    &claim,
                    format!("Turned off the schedule for {}: {why}", claim.persona),
                );
                Turn::Refused(why)
            }
        }
    }

    fn holder_name(&self, holder: &Holder) -> String {
        match holder {
            Holder::Person { sub, email, .. } if email.is_empty() => format!("sub:{sub}"),
            Holder::Person { email, .. } => email.to_lowercase(),
            Holder::Local { uid } => self
                .caller(*uid)
                .map_or_else(|| format!("uid {uid}"), |c| c.name),
        }
    }

    /// The person a schedule runs as, on this host: made here if they
    /// were never seen here, as their sign-in would.
    fn schedule_caller(&self, holder: &Holder) -> Result<Caller> {
        match holder {
            Holder::Local { uid } => self
                .caller(*uid)
                .ok_or_else(|| RuntimeError::NotFound(format!("there's no account {uid} here"))),
            Holder::Person { sub, name, email } => {
                let identity = self.identity().ok_or_else(|| {
                    RuntimeError::Invalid("this host doesn't act for people".into())
                })?;
                let person = identity
                    .people
                    .person(&Claims {
                        sub: sub.clone(),
                        name: name.clone(),
                        email: email.clone(),
                    })
                    .map_err(RuntimeError::Internal)?;
                let caller = self.caller_for(identity, &person)?;
                let mut event = AuditEvent::new(
                    EventKind::Session,
                    format!("Acting for {} (their schedule)", person.name),
                );
                event.user = Some(caller.name.clone());
                event.detail = serde_json::json!({
                    "person": person.key, "uid": person.uid, "proven_by": "schedule",
                });
                self.audit(event);
                Ok(caller)
            }
        }
    }

    fn start_scheduled(self: &Arc<Self>, s: &StoredSchedule) -> Result<TaskId> {
        let caller = self.schedule_caller(&s.holder)?;
        let computers = self.computers()?;
        let persona = blocking(|| computers.persona(caller.uid, PERSON_COMPUTER, &s.persona))
            .map_err(|_| {
                RuntimeError::NotFound(format!("{} is no longer one of their agents", s.persona))
            })?;
        let request = TaskRequest {
            persona: Some(s.persona.clone()),
            agent: persona.agent,
            prompt: s.prompt.clone(),
            scope: ScopeRequest::default(),
            title: None,
            computer: Some(PERSON_COMPUTER.into()),
        };
        let run = ScheduleRun {
            owner: s.owner.clone(),
            id: s.id.clone(),
        };
        let started = self.start_task(&caller, request, Origin::Schedule(run))?;
        self.schedule_event(
            &caller.name,
            s,
            format!(
                "Started the scheduled run for {} (task {})",
                s.persona, started.id
            ),
        );
        Ok(started.id)
    }

    /// Changes the run `claimed` started at `started_at`, if it's still the
    /// schedule's last: the person may have changed the schedule since.
    fn note_run(
        &self,
        store: &dyn ScheduleStore,
        claimed: &StoredSchedule,
        started_at: i64,
        change: impl Fn(&mut LastRun, &mut StoredSchedule),
    ) {
        for _ in 0..WRITE_TRIES {
            let Ok(all) = store.of(&claimed.owner) else {
                return;
            };
            let Some(kept) = all.into_iter().find(|s| s.id == claimed.id) else {
                return;
            };
            let mut changed = kept.clone();
            let Some(mut last) = changed.last.take().filter(|l| l.at == started_at) else {
                return;
            };
            change(&mut last, &mut changed);
            changed.last = Some(last);
            changed.version += 1;
            match store.replace(&changed, kept.version) {
                Ok(()) | Err(StoreError::Failed(_)) => return,
                Err(StoreError::Changed) => {}
            }
        }
    }

    /// At a scheduled run's end: how it went, kept with the schedule.
    pub(crate) fn remember_schedule_run(&self, task: &Task) {
        let (Some(run), Some(store)) = (&task.schedule, self.schedules.get()) else {
            return;
        };
        let (outcome, note) = {
            let state = task.lock();
            match state.tile {
                TileState::Done => ("done", String::new()),
                TileState::Stopped => ("stopped", String::new()),
                _ => ("failed", state.failure.clone().unwrap_or_default()),
            }
        };
        for _ in 0..WRITE_TRIES {
            let Ok(all) = store.of(&run.owner) else {
                return;
            };
            let Some(kept) = all.into_iter().find(|s| s.id == run.id) else {
                return;
            };
            let mut changed = kept.clone();
            let Some(last) = changed.last.as_mut().filter(|l| l.task == Some(task.id)) else {
                return;
            };
            last.outcome = outcome.into();
            last.note.clone_from(&note);
            match outcome {
                "done" => changed.failures = 0,
                "failed" => {
                    changed.failures += 1;
                    if changed.failures >= SCHEDULE_FAILURES_MAX && changed.enabled {
                        changed.enabled = false;
                        changed.disabled_why = Some(format!(
                            "{SCHEDULE_FAILURES_MAX} runs in a row didn't work; the last: {note}"
                        ));
                    }
                }
                _ => {}
            }
            changed.version += 1;
            match store.replace(&changed, kept.version) {
                Ok(()) => {
                    if !changed.enabled && kept.enabled {
                        self.schedule_event(
                            &task.user.name,
                            &changed,
                            format!(
                                "Turned off the schedule for {}: too many failed runs",
                                changed.persona
                            ),
                        );
                    }
                    return;
                }
                Err(StoreError::Failed(e)) => {
                    tracing::warn!(task = %task.id, schedule = %run.id, "keeping the run: {e}");
                    return;
                }
                Err(StoreError::Changed) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(id: &str, next_run: i64) -> StoredSchedule {
        StoredSchedule {
            owner: "p1".into(),
            id: id.into(),
            version: 1,
            holder: Holder::Local { uid: 1001 },
            persona: "reconciler".into(),
            prompt: "Match the invoices".into(),
            when: "daily 09:00".into(),
            zone: "UTC".into(),
            enabled: true,
            disabled_why: None,
            next_run,
            failures: 0,
            last: None,
            created_at: Timestamp::now(),
            other: BTreeMap::new(),
        }
    }

    #[test]
    fn adr0069_slots_are_in_the_schedules_zone_daylight_saving_included() {
        let daily = Schedule::from_str("daily 09:00").unwrap();
        // 2026-10-28 08:30 UTC → 09:00 the same day; at 09:00, the next.
        assert_eq!(
            next_slot(&daily, "UTC", 1_793_176_200).unwrap(),
            1_793_178_000
        );
        assert_eq!(
            next_slot(&daily, "UTC", 1_793_178_000).unwrap(),
            1_793_264_400
        );
        assert!(next_slot(&daily, "Mars/Olympus", 0).is_err());
        if std::path::Path::new("/usr/share/zoneinfo/America/New_York").is_file() {
            // The day before clocks go forward in New York (8 March 2026):
            // 09:00 the next day is 13:00 UTC, not 14:00.
            assert_eq!(
                next_slot(&daily, "America/New_York", 1_772_895_600).unwrap(),
                1_772_974_800
            );
        }
    }

    #[test]
    fn adr0069_a_claim_holds_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSchedules::new(dir.path().join("schedules.json"));
        store.make(&kept("s1", 100)).unwrap();
        assert_eq!(store.make(&kept("s1", 100)), Err(StoreError::Changed));
        let seen = store.due(100).unwrap().remove(0);
        assert!(store.due(99).unwrap().is_empty());
        // Two hosts read version 1 and claim; only the first holds.
        let mut a = seen.clone();
        a.version = 2;
        a.next_run = 200;
        let mut b = a.clone();
        b.last = Some(LastRun {
            at: 100,
            outcome: "running".into(),
            task: None,
            note: String::new(),
        });
        store.replace(&a, seen.version).unwrap();
        assert_eq!(store.replace(&b, seen.version), Err(StoreError::Changed));
        assert!(store.due(150).unwrap().is_empty());
        // Off, it isn't due; removed, it's gone.
        let mut off = a.clone();
        off.version = 3;
        off.enabled = false;
        store.replace(&off, 2).unwrap();
        assert!(store.due(10_000).unwrap().is_empty());
        assert_eq!(store.of("p1").unwrap().len(), 1);
        assert!(store.of("p2").unwrap().is_empty());
        assert!(!store.remove("p2", "s1").unwrap(), "only its owner's");
        assert!(store.remove("p1", "s1").unwrap());
        assert!(store.of("p1").unwrap().is_empty());
    }

    // Audit 4.11: hosts are a version apart while an update goes round,
    // and a claim writes the whole schedule. An older host wrote it back
    // without what the newer one had added.
    #[test]
    fn a_claim_writes_back_what_a_newer_version_added() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedules.json");
        // As a version on might keep it: two things this one doesn't know.
        let mut newer = serde_json::to_value(kept("s1", 100)).unwrap();
        newer["catch_up"] = true.into();
        newer["limits"] = serde_json::json!({ "minutes": 30, "tools": ["files.read"] });
        std::fs::write(&path, serde_json::to_vec(&[&newer]).unwrap()).unwrap();
        let store = FileSchedules::new(path.clone());
        let seen = store.due(100).unwrap().remove(0);
        assert_eq!(seen.other.len(), 2, "{:?}", seen.other);
        // This host claims the run, as `take_turn` does: what it read,
        // a version on, with what it changed.
        let mut claim = seen.clone();
        claim.version += 1;
        claim.next_run = 200;
        claim.last = Some(LastRun {
            at: 100,
            outcome: "running".into(),
            task: None,
            note: String::new(),
        });
        store.replace(&claim, seen.version).unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let written = &written[0];
        assert_eq!(written["version"], 2);
        assert_eq!(written["last"]["outcome"], "running");
        assert_eq!(written["catch_up"], newer["catch_up"]);
        assert_eq!(written["limits"], newer["limits"]);
        // And nothing is added to one that has nothing more.
        let plain = serde_json::to_value(kept("s2", 100)).unwrap();
        let read: StoredSchedule = serde_json::from_value(plain.clone()).unwrap();
        assert!(read.other.is_empty());
        assert_eq!(serde_json::to_value(&read).unwrap(), plain);
    }
}
