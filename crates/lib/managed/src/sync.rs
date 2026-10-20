//! What a device and its console say to each other (ADR 0023).
//!
//! ```text
//! POST /device/v1/sync    SyncRequest → SyncResponse
//! POST /device/v1/report  DeviceReport → {}
//! ```
//!
//! Both carry the org's enrollment token as a bearer token. A sync waits
//! up to `wait_secs` for a release newer than the versions the device
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

/// What a device asks for: anything newer than what it has in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRequest {
    pub device: String,
    #[serde(default)]
    pub policy: Option<u64>,
    #[serde(default)]
    pub catalog: Option<u64>,
    #[serde(default)]
    pub directory: Option<u64>,
    /// The chat release it has (ADR 0030).
    #[serde(default)]
    pub chat: Option<u64>,
    /// The version of the org's model credits key it has (ADR 0039).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<u64>,
    /// How long the console may hold the answer waiting for a release.
    #[serde(default)]
    pub wait_secs: u64,
}

/// The longest a sync is held open.
pub const MAX_WAIT_SECS: u64 = 25;

/// The console's answer to a sync: releases newer than the device named,
/// for the device's ring. `None` means nothing newer. Unknown fields are
/// let through, so a console that sends a kind of release a device
/// doesn't know yet doesn't stop it hearing of the ones it does: what a
/// device trusts is each release's signature, not the answer around it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncResponse {
    pub ring: Ring,
    #[serde(default)]
    pub policy: Option<Signed>,
    #[serde(default)]
    pub catalog: Option<Signed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<Signed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<Signed>,
    /// The org's model credits key, when it changed since the version the
    /// computer named (ADR 0039). Its own field, never in a release: it
    /// reaches only a computer whose key signed the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<ModelCredits>,
}

/// The org's key for its model credits (ADR 0039), at a version; no key
/// means the org has none now, and the computer forgets its own.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCredits {
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl std::fmt::Debug for ModelCredits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelCredits")
            .field("version", &self.version)
            .field("api_key", &self.api_key.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Chat's settings from IT (ADR 0030, decision 4), signed like the
/// directory and in force on every computer at once, without rings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRelease {
    /// IT's compliance key: uncompressed P-256, hex. Every epoch is sealed
    /// to it too.
    pub compliance: String,
    /// What every computer shows, so IT can compare it with its file.
    pub fingerprint: String,
}

impl ChatRelease {
    pub fn new(compliance: &glass_crypto::PublicKey) -> Self {
        let bytes = compliance.to_bytes();
        let digest = hex::encode_upper(glass_crypto::sha256(&bytes));
        let fingerprint = digest
            .as_bytes()
            .chunks(4)
            .take(8)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            compliance: hex::encode(bytes),
            fingerprint,
        }
    }

    pub fn key(&self) -> Result<glass_crypto::PublicKey, glass_crypto::CryptoError> {
        let bytes = hex::decode(&self.compliance).map_err(|_| glass_crypto::CryptoError::BadKey)?;
        glass_crypto::PublicKey::from_bytes(&bytes)
    }
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
    pub directory_version: Option<u64>,
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
    /// Every app people made on this computer (GA-10, ADR 0032): nothing
    /// they make is hidden from IT.
    #[serde(default)]
    pub made_here: Vec<MadeApp>,
    pub reported_at: Timestamp,
}

/// An app someone made on a computer, at its latest version: what IT's
/// inventory shows (GA-9, GA-10). What it declares, never its data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MadeApp {
    pub app: String,
    pub name: String,
    pub owner: String,
    pub version: u32,
    /// The app it remixes (`mail@0.1.0`); none for one built new.
    #[serde(default)]
    pub based_on: Option<String>,
    /// The actions it declares, and those beyond what it remixes.
    pub actions: Vec<String>,
    #[serde(default)]
    pub beyond_base: Vec<String>,
    pub saved_at: Timestamp,
}
