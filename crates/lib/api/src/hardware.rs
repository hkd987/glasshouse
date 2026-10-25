//! Kernel patches tried on this computer (ADR 0057): what `glassctl
//! hardware patch` asks for and shows.

use std::path::PathBuf;

use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

/// [`crate::methods::HARDWARE_PATCH_APPLY`]: a bundle on this computer to
/// try. Root only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchApply {
    pub bundle: PathBuf,
}

/// Where a patch tried on this computer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialState {
    /// Its boot environment is being made.
    Staging,
    /// Ready: the next start is from its boot environment, once.
    Staged,
    /// It started and passed the check, and the computer runs on it, but
    /// its boot environment isn't the permanent one yet: a crash, a panic
    /// or a power cut goes back to the one before. A clean restart comes
    /// back to it.
    Probation,
    /// It served its probation: its boot environment is the one the
    /// computer starts from.
    Kept,
    /// It started but failed the check: the next start goes back.
    Failed,
    /// The computer didn't start on it, or staging didn't finish: it's back
    /// on the boot environment before.
    RolledBack,
    /// Taken back before a restart, or on probation.
    Cancelled,
    /// A kept patch taken out: the boot environment before is the one the
    /// computer starts from again.
    Undone,
}

impl TrialState {
    /// Still waiting for a restart or a check.
    pub fn open(self) -> bool {
        matches!(
            self,
            TrialState::Staging | TrialState::Staged | TrialState::Probation
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            TrialState::Staging => "Being set up",
            TrialState::Staged => "Restart to try it",
            TrialState::Probation => "On probation: a crash goes back",
            TrialState::Kept => "Kept",
            TrialState::Failed => "Failed its check: restart to go back",
            TrialState::RolledBack => "Rolled back",
            TrialState::Cancelled => "Taken back",
            TrialState::Undone => "Taken out",
        }
    }
}

/// One patch tried on this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchTrial {
    pub id: String,
    pub revision: u64,
    pub summary: String,
    /// `reviewed` or `community` (ADR 0057).
    pub review: String,
    /// The boot environment it's tried in.
    pub env: String,
    /// The boot environment it was made from, which the computer goes back to.
    pub from_env: String,
    pub state: TrialState,
    pub modules: Vec<String>,
    pub staged_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
    /// Why it failed or rolled back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Minutes the computer was in use on it, and the minutes of use its
    /// probation lasts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ran_minutes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub probation_minutes: u64,
    /// Times the computer woke from sleep on it, and the wakes its
    /// probation asks: a patch that breaks waking is never kept.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub woke: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub probation_resumes: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl PatchTrial {
    /// Its probation is served: the minutes of use, and the wakes.
    pub fn served(&self) -> bool {
        self.ran_minutes >= self.probation_minutes && self.woke >= self.probation_resumes
    }
}

/// [`crate::methods::HARDWARE_PATCH_STATUS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchStatus {
    /// The boot environment the computer is running from.
    pub running: String,
    /// The patch being tried, or the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<PatchTrial>,
    /// Patches tried before, newest first.
    #[serde(default)]
    pub history: Vec<PatchTrial>,
    /// The patches kept in the boot environment running: its own and those
    /// of the environments it was made from, newest first.
    #[serde(default)]
    pub kept: Vec<PatchTrial>,
}
