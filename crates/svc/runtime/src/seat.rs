//! The seat (ADR 0031): who is at the screen, told by the sign-in screen,
//! and when they last used it, told by their desktop and no other program.
//! glassd sleeps the computer when the person at it has left it alone as
//! long as their `power.sleep_after_minutes` says, pausing agents first.

use std::time::{Duration, Instant};

use glass_api::SeatBegin;
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
}

/// What a computer sleeps after when nobody set it (the table's default).
const SLEEP_AFTER_MINUTES: i64 = 30;

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
        let person = glass_platform::users::by_name(&begin.user)
            .ok_or_else(|| RuntimeError::NotFound(format!("no account {}", begin.user)))?;
        tracing::info!(user = %begin.user, session = begin.session_pid, "seat: signed in");
        *self.seat_lock() = Some(Seat {
            user: begin.user,
            uid: person.uid,
            session_pid: begin.session_pid,
            desktop_pid: None,
            refused_pid: None,
            last_input: Instant::now(),
        });
        Ok(())
    }

    /// Their desktop ended (`seat.end`).
    pub fn seat_end(&self, caller: &Caller) -> Result<()> {
        if !caller.is_admin() {
            return Err(RuntimeError::PermissionDenied(
                "only the sign-in screen says who is at the screen".into(),
            ));
        }
        if let Some(seat) = self.seat_lock().take() {
            tracing::info!(user = %seat.user, "seat: signed out");
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
        Ok(())
    }

    /// Whether the person at the screen has left it alone long enough to
    /// sleep, at `now`.
    pub fn sleep_due(&self, now: Instant) -> bool {
        let Some((user, uid, last_input)) = self
            .seat_lock()
            .as_ref()
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

    /// Sleeps the computer with `sleep`, which returns once it wakes:
    /// agents pause first and carry on after (PM-5), and the idle time
    /// starts over from the wake.
    pub async fn sleep_now(
        &self,
        sleep: impl FnOnce() -> std::result::Result<(), String> + Send + 'static,
    ) -> Result<()> {
        let paused = self.pause_for_sleep().await;
        tracing::info!(
            agents = paused.tasks,
            "sleeping: nobody has used the screen"
        );
        let slept = tokio::task::spawn_blocking(sleep)
            .await
            .map_err(|e| RuntimeError::Internal(e.to_string()))?;
        self.resume_after_sleep();
        if let Some(seat) = self.seat_lock().as_mut() {
            seat.last_input = Instant::now();
        }
        slept.map_err(RuntimeError::Internal)
    }
}
