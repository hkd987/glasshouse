//! The seat (ADR 0031): who is at the screen, told by the sign-in screen,
//! and when they last used it, told by their desktop and no other program.
//! glassd sleeps the computer when the person at it has left it alone as
//! long as their `power.sleep_after_minutes` says, pausing agents first.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use glass_api::SeatBegin;
use glass_audit::{AuditEvent, EventKind, Outcome};
use glass_rpc::PeerCred;

use crate::error::Result;
use crate::{Caller, Runtime, RuntimeError};

/// The person at the screen and their desktop.
#[derive(Debug, Clone)]
pub(crate) struct Seat {
    user: String,
    uid: u32,
    /// The `su` the sign-in screen started; its one child is the desktop.
    session_pid: u32,
    /// The desktop's pid, once it reported from it.
    desktop_pid: Option<u32>,
    /// The last process refused, so asking again costs no `ps`.
    refused_pid: Option<u32>,
    last_input: Instant,
    /// glassd tried to sleep the computer since the person last used it,
    /// and it hasn't woken since: no second try until one or the other.
    slept: bool,
}

impl Seat {
    fn new(begin: SeatBegin) -> Result<Seat> {
        let person = glass_platform::users::by_name(&begin.user)
            .ok_or_else(|| RuntimeError::NotFound(format!("no account {}", begin.user)))?;
        Ok(Seat {
            user: begin.user,
            uid: person.uid,
            session_pid: begin.session_pid,
            desktop_pid: None,
            refused_pid: None,
            last_input: Instant::now(),
            slept: false,
        })
    }
}

/// Writes the seat to `path` whole (temp, then rename), for root alone.
fn keep(path: &Path, begin: &SeatBegin) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let new = path.with_extension("new");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&new)?;
    file.write_all(&serde_json::to_vec(begin).map_err(std::io::Error::other)?)?;
    std::fs::rename(&new, path)
}

/// What a computer sleeps after when nobody set it (the table's default).
const SLEEP_AFTER_MINUTES: i64 = 30;

/// How long glassd waits, after asking the computer to sleep, to hear it
/// woke (`/etc/rc.resume` calls `system.resume`) before deciding it never
/// slept and letting agents carry on itself.
pub const WAKE_WAIT: Duration = Duration::from_secs(30);

impl Runtime {
    fn seat_lock(&self) -> std::sync::MutexGuard<'_, Option<Seat>> {
        self.seat.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The sign-in screen started `begin.user`'s desktop (`seat.begin`).
    pub fn seat_begin(&self, caller: &Caller, begin: SeatBegin) -> Result<()> {
        if !caller.is_admin() {
            return Err(RuntimeError::PermissionDenied(
                "only the sign-in screen says who is at the screen".into(),
            ));
        }
        let seat = Seat::new(begin.clone())?;
        tracing::info!(user = %begin.user, session = begin.session_pid, "seat: signed in");
        self.audit(seat_event(&begin.user, true));
        *self.seat_lock() = Some(seat);
        if let Some(path) = self.seat_file.get()
            && let Err(e) = keep(path, &begin)
        {
            tracing::warn!("keeping the seat in {}: {e}", path.display());
        }
        Ok(())
    }

    /// Keeps who is at the screen in `path`, in glassd's run directory
    /// (emptied at boot), so a glassd that restarts still knows; and takes
    /// it back now if one is kept there and its `su` still runs. A pid
    /// used again by a later process doesn't count: the `su` must have
    /// started before the seat was kept.
    pub fn keep_seat_in(&self, path: PathBuf) {
        let kept = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SeatBegin>(&bytes).ok());
        let written = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let restored = kept.zip(written).and_then(|(begin, written)| {
            let age = glass_platform::system::process_age(begin.session_pid)?;
            let started = SystemTime::now().checked_sub(age)?;
            // `ps` counts whole seconds.
            (started <= written + Duration::from_secs(1))
                .then(|| Seat::new(begin).ok())
                .flatten()
        });
        match restored {
            Some(seat) => {
                tracing::info!(user = %seat.user, "seat: still signed in");
                *self.seat_lock() = Some(seat);
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        let _ = self.seat_file.set(path);
    }

    /// Their desktop ended (`seat.end`).
    pub fn seat_end(&self, caller: &Caller) -> Result<()> {
        if !caller.is_admin() {
            return Err(RuntimeError::PermissionDenied(
                "only the sign-in screen says who is at the screen".into(),
            ));
        }
        let seat = self.seat_lock().take();
        if let Some(seat) = seat {
            tracing::info!(user = %seat.user, "seat: signed out");
            self.audit(seat_event(&seat.user, false));
        }
        if let Some(path) = self.seat_file.get() {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    /// The person is using their desktop (`seat.active`), said by `peer`:
    /// taken only from the desktop the sign-in screen started, so no other
    /// program of theirs keeps the computer awake. Blocking: it may ask
    /// the base system for the peer's parent once.
    pub fn seat_active(&self, peer: PeerCred) -> Result<()> {
        let refused = || {
            RuntimeError::PermissionDenied(
                "only the desktop at the screen says the person is using it".into(),
            )
        };
        let pid = peer
            .pid
            .and_then(|p| u32::try_from(p).ok())
            .ok_or_else(refused)?;
        let (uid, session, known) = {
            let seat = self.seat_lock();
            let seat = seat.as_ref().ok_or_else(refused)?;
            if seat.refused_pid == Some(pid) {
                return Err(refused());
            }
            (seat.uid, seat.session_pid, seat.desktop_pid)
        };
        if peer.uid != uid {
            return Err(refused());
        }
        if known != Some(pid) && glass_platform::system::parent_pid(pid) != Some(session) {
            if let Some(seat) = self.seat_lock().as_mut() {
                seat.refused_pid = Some(pid);
            }
            return Err(refused());
        }
        let mut seat = self.seat_lock();
        let seat = seat
            .as_mut()
            .filter(|s| s.session_pid == session)
            .ok_or_else(refused)?;
        seat.desktop_pid = Some(pid);
        seat.last_input = Instant::now();
        seat.slept = false;
        Ok(())
    }

    /// Whether the person at the screen has left it alone long enough to
    /// sleep, at `now`.
    pub fn sleep_due(&self, now: Instant) -> bool {
        let Some((user, uid, last_input)) = self
            .seat_lock()
            .as_ref()
            .filter(|s| !s.slept)
            .map(|s| (s.user.clone(), s.uid, s.last_input))
        else {
            return false;
        };
        let minutes = self
            .device_settings
            .person(&user, uid)
            .into_iter()
            .find(|e| e.def.id == "power.sleep_after_minutes")
            .and_then(|e| match e.value {
                glass_policy::document::SettingValue::Number(n) => Some(n),
                _ => None,
            })
            .unwrap_or(SLEEP_AFTER_MINUTES);
        let Ok(minutes) = u64::try_from(minutes) else {
            return false;
        };
        minutes > 0
            && now.saturating_duration_since(last_input) >= Duration::from_secs(minutes * 60)
    }

    /// Sleeps the computer with `sleep`, which asks the base system to
    /// (`acpiconf -s 3`) and returns before it does. Agents pause first
    /// (PM-5) and carry on when `/etc/rc.resume` says it woke
    /// ([`Runtime::wake`]), which also starts the idle time over. If no
    /// wake comes within `wait` ([`WAKE_WAIT`]), or the ask fails, the computer
    /// didn't sleep: agents carry on now, and glassd doesn't ask again
    /// until the person has used the computer.
    pub async fn sleep_now(
        &self,
        wait: Duration,
        sleep: impl FnOnce() -> std::result::Result<(), String> + Send + 'static,
    ) -> Result<()> {
        let mut wakes = self.wakes.subscribe();
        let (user, idle) = {
            let mut seat = self.seat_lock();
            let Some(seat) = seat.as_mut() else {
                return Ok(());
            };
            seat.slept = true;
            (seat.user.clone(), seat.last_input.elapsed())
        };
        let paused = self.pause_for_sleep().await;
        tracing::info!(
            agents = paused.tasks,
            "sleeping: nobody has used the screen"
        );
        let slept = tokio::task::spawn_blocking(sleep)
            .await
            .map_err(|e| RuntimeError::Internal(e.to_string()))?;
        self.audit(slept_event(&user, idle, paused.tasks, slept.as_ref().err()));
        match slept {
            Ok(()) => {
                if tokio::time::timeout(wait, wakes.changed()).await.is_err() {
                    tracing::warn!("asked to sleep, but no wake came: agents carry on");
                    self.resume_after_sleep();
                }
                Ok(())
            }
            Err(e) => {
                self.resume_after_sleep();
                Err(RuntimeError::Internal(e))
            }
        }
    }

    /// The computer woke (`system.resume`, from `/etc/rc.resume`), however
    /// it went to sleep: the idle time starts over from now.
    pub(crate) fn woke(&self) {
        if let Some(seat) = self.seat_lock().as_mut() {
            seat.last_input = Instant::now();
            seat.slept = false;
        }
        self.wakes.send_modify(|n| *n += 1);
    }
}

/// The audit record of someone signing in at the screen, or out.
fn seat_event(user: &str, signed_in: bool) -> AuditEvent {
    let mut event = AuditEvent::new(
        EventKind::SeatChanged,
        if signed_in {
            format!("{user} signed in at the screen")
        } else {
            format!("{user} signed out at the screen")
        },
    );
    event.user = Some(user.to_owned());
    event.detail = serde_json::json!({ "signed_in": signed_in });
    event
}

/// The audit record of a sleep glassd asked for, after `idle` alone.
fn slept_event(user: &str, idle: Duration, agents: usize, failed: Option<&String>) -> AuditEvent {
    let minutes = idle.as_secs() / 60;
    let mut event = AuditEvent::new(
        EventKind::Slept,
        match failed {
            None => format!(
                "The computer went to sleep: {user} had left it alone for {minutes} min; {agents} agent(s) paused"
            ),
            Some(e) => format!("The computer couldn't sleep: {e}"),
        },
    );
    event.user = Some(user.to_owned());
    event.outcome = Some(match failed {
        None => Outcome::Completed,
        Some(e) => Outcome::Failed { error: e.clone() },
    });
    event.detail = serde_json::json!({ "idle_minutes": minutes, "agents_paused": agents });
    event
}
