//! OS updates on this computer (ADR 0061): what `glassctl update` asks for
//! and shows.

use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

use crate::TrialState;

/// [`crate::methods::SYSTEM_UPDATE_APPLY`]. Root only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateApply {
    /// Take the update even though it neither fixes a kept kernel patch's
    /// problem nor brings a rebuild of it: that fix is gone after it.
    #[serde(default)]
    pub drop_patches: bool,
}

/// An OS update staged on this computer, or one before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsUpdate {
    /// The release number: only ever goes up.
    pub number: u64,
    /// For people: `2026.11.1`.
    pub version: String,
    pub channel: String,
    /// The base system release it's built on (admin output only).
    pub base: String,
    /// The boot environment it's installed in, and the one before.
    pub env: String,
    pub from_env: String,
    pub state: TrialState,
    pub staged_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// What it does with each kernel patch the computer keeps: fixed in
    /// it, rebuilt for it, or dropped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<String>,
}

/// A release this computer may take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAvailable {
    pub number: u64,
    pub version: String,
    pub base: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patches: Vec<String>,
    /// Why it can't be taken as things are, if it can't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

/// [`crate::methods::SYSTEM_UPDATE_STATUS`] and `CHECK`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// The boot environment the computer runs from.
    pub running: String,
    /// The version it runs, as its image says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub channel: String,
    /// The last release number taken.
    #[serde(default)]
    pub taken: u64,
    /// The update being installed, or the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<OsUpdate>,
    #[serde(default)]
    pub history: Vec<OsUpdate>,
    /// From a check: the newest release there is, if it's newer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<UpdateAvailable>,
    /// From a check: why there's nothing to say about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<String>,
}
