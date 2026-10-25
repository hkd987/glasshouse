//! This computer as Settings shows it (ADR 0057, ADR 0061): the version it
//! runs and its OS updates, and the kernel patches it tries or keeps.
//!
//! glassd gives the handle a [`Computer`] once the runtime exists. Who may
//! do what is decided here, never by anything in a request:
//!
//! - **Anyone at the screen** sees it all, and may take a kernel patch out
//!   (or back, before its restart): that only ever goes back to the
//!   computer as it was.
//! - **The computer's owner**, on a computer nobody manages (ADR 0038),
//!   checks for and installs OS updates. On a managed computer the update
//!   IT's ring gets installs by itself.
//! - **Anyone at the screen** chooses when the computer restarts into an
//!   update installed: now, or tonight.
//! - **No agent**, ever: policy makes these Never, and the actions that
//!   change anything are the person's own (`PERSONS_OWN`).

use glass_api::{OsUpdate, PatchStatus, UpdateStatus};
use serde_json::{Value, json};

/// The runtime's OS updates and kernel patches, as glassd hands them over.
pub trait Computer: Send + Sync {
    /// The version running and the update under way, without checking.
    fn updates(&self) -> Result<UpdateStatus, String>;
    /// And the newest release on the computer's channel.
    fn check(&self) -> Result<UpdateStatus, String>;
    fn install(&self) -> Result<OsUpdate, String>;
    /// Restarts into the update installed now, sets it for tonight, or
    /// takes tonight back.
    fn restart(&self, when: glass_api::RestartWhen) -> Result<UpdateStatus, String>;
    fn patches(&self) -> Result<PatchStatus, String>;
    /// Takes back the patch set up but not yet tried.
    fn cancel_patch(&self) -> Result<PatchStatus, String>;
    /// Takes out the patch the computer runs, on probation or kept.
    fn undo_patch(&self) -> Result<PatchStatus, String>;
}

/// What Settings shows: the updates (checked, when `checked` is given),
/// the kernel patches, and whether `owner` may install updates.
pub fn view(updates: &UpdateStatus, patches: &PatchStatus, owner: bool) -> Value {
    let open = |state: glass_api::TrialState| state.open();
    json!({
        "version": updates.version,
        "channel": updates.channel,
        "owner": owner,
        "update": updates.update.as_ref().filter(|u| open(u.state)).map(|u| json!({
            "version": u.version,
            "state": u.state,
            "patches": u.patches,
        })),
        "last_update": updates.update.as_ref().filter(|u| !open(u.state)).map(|u| json!({
            "version": u.version,
            "state": u.state,
            "note": u.note,
        })),
        "available": updates.available.as_ref().map(|a| json!({
            "version": a.version,
            "notes": a.notes,
            "patches": a.patches,
            "blocked": a.blocked,
        })),
        "checked": updates.checked,
        "restart_at": updates.restart_at,
        "patch": patches.trial.as_ref().filter(|t| open(t.state)).map(|t| json!({
            "id": t.id,
            "summary": t.summary,
            "review": t.review,
            "state": t.state,
            "label": t.state.label(),
            "ran_minutes": t.ran_minutes,
            "probation_minutes": t.probation_minutes,
            "woke": t.woke,
            "probation_resumes": t.probation_resumes,
        })),
        "kept": patches.kept.iter().map(|t| json!({
            "id": t.id,
            "revision": t.revision,
            "summary": t.summary,
            "review": t.review,
        })).collect::<Vec<_>>(),
    })
}
