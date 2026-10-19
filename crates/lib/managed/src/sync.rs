//! What a device and its console say to each other (ADR 0023).
//!
//! ```text
//! GET  /device/v1/sync?device=<id>&policy=<v>&catalog=<v>&wait=<s>  → SyncResponse
//! POST /device/v1/report                                            ← DeviceReport
//! ```
//!
//! Both carry the org's enrollment token as a bearer token. A sync waits
//! up to `wait` seconds for a release newer than the versions the device
//! names, so a change reaches online devices in about a second without
//! them polling hard; it answers at once when there is one.

use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

use crate::Signed;

/// A policy release's payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRelease {
    /// The whole `policy.toml`, as the device will enforce it.
    pub policy: String,
    /// What changed, in IT's words.
    #[serde(default)]
    pub note: String,
    /// This release puts back the content of that earlier version (AD-3).
    #[serde(default)]
    pub rolls_back_to: Option<u64>,
}

/// Rollout rings (AD-3, AD-11): a release reaches pilot devices first,
/// then early, then everyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ring {
    Pilot,
    Early,
    Broad,
}

impl Ring {
    pub const ALL: [Ring; 3] = [Ring::Pilot, Ring::Early, Ring::Broad];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pilot => "pilot",
            Self::Early => "early",
            Self::Broad => "broad",
        }
    }
}

/// The console's answer to a sync: releases newer than the device named,
/// for the device's ring. `None` means nothing newer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncResponse {
    pub ring: Ring,
    #[serde(default)]
    pub policy: Option<Signed>,
    #[serde(default)]
    pub catalog: Option<Signed>,
}

/// An agent running on the device, for the fleet's live view (AD-4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunningAgent {
    pub agent: String,
    pub user: String,
    /// Waiting on a person (an open ask).
    #[serde(default)]
    pub waiting: bool,
}

/// What a device tells its console (AD-4). Counts and names only: no task
/// text, no paths, nothing an agent wrote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceReport {
    /// The device's enrolled id, kept in glassd's state directory.
    pub device: String,
    /// What IT calls it: the machine's name.
    pub name: String,
    /// The Glasshouse version it runs.
    pub os_version: String,
    /// People who ran agents on it since glassd started.
    #[serde(default)]
    pub users: Vec<String>,
    /// The signed policy release in force, if one is.
    #[serde(default)]
    pub policy_version: Option<u64>,
    #[serde(default)]
    pub catalog_version: Option<u64>,
    #[serde(default)]
    pub agents: Vec<RunningAgent>,
    #[serde(default)]
    pub approvals_pending: u32,
    /// Actions policy blocked in the last 24 hours (since glassd started,
    /// if that was later).
    #[serde(default)]
    pub blocked_24h: u32,
    /// Why the last release was refused or the last sync failed, if it was.
    #[serde(default)]
    pub last_error: Option<String>,
    pub reported_at: Timestamp,
}
