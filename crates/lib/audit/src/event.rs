use glass_types::{ActionName, AgentId, ApprovalId, SnapshotId, TaskId, Tier, Timestamp};
use serde::{Deserialize, Serialize};

/// What happened. One audit record holds exactly one event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub kind: EventKind,
    /// The human the work is for (the requesting user, AU-1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionName>,
    /// What the action touched: paths, recipients, record ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<PolicyDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// A plain-language line for people reading the log.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Extra structured fields specific to `kind`.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub detail: serde_json::Value,
}

impl AuditEvent {
    pub fn new(kind: EventKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            user: None,
            agent: None,
            task: None,
            action: None,
            targets: Vec::new(),
            decision: None,
            approval: None,
            snapshot: None,
            outcome: None,
            summary: summary.into(),
            detail: serde_json::Value::Null,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// The log was opened after an unacknowledged partial write was dropped.
    LogRecovered,
    DaemonStarted,
    PolicyLoaded,
    TaskPlanned,
    TaskStarted,
    TaskStateChanged,
    TaskFinished,
    /// One agent action and its final outcome.
    Action,
    ApprovalRequested,
    ApprovalAnswered,
    SnapshotTaken,
    Rollback,
    /// A user saved, switched or turned off a layer on an agent, or a new
    /// agent version re-checked it (TL-2, TL-13).
    LayerChanged,
    /// Someone asked to open an app that isn't in their profile, and glassd
    /// refused (role-based delivery).
    AppRefused,
    /// A catalog release from the admin console was applied (ADR 0023).
    CatalogLoaded,
    /// A directory release (the org's people) from the admin console was
    /// applied (ADR 0023, AD-1).
    DirectoryLoaded,
    /// A release from the admin console was refused; the device kept what
    /// it had (ADR 0023).
    ManagedRefused,
    /// A chat member key was made and registered for a person on this
    /// computer (ADR 0030).
    ChatKeyMade,
    /// A chat record from the console failed a check and was kept out
    /// (ADR 0030).
    ChatRefused,
    /// IT's Chat release (the compliance key) was applied (ADR 0030).
    ChatLoaded,
    /// glassd applied a setting IT set for the whole computer, such as its
    /// time zone or USB drives, or failed to (ADR 0031).
    DeviceSettingApplied,
    /// Someone signed in at the screen, or signed out (ADR 0031).
    SeatChanged,
    /// The computer was put to sleep after the person at it left it alone,
    /// with the agents it paused (ADR 0031, PM-5).
    Slept,
    /// Snapshots past their time were removed (AU-20: kept 30 days).
    SnapshotsRemoved,
}

/// The policy decision behind an action (AU-1, principle 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub tier: Tier,
    /// The rule that decided it, such as "group ap-team rule files.delete".
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRef {
    pub id: ApprovalId,
    /// Who answered, when answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Failed {
        error: String,
    },
    /// Policy put the action at Never.
    Blocked,
    /// The user said no.
    Denied,
    /// Nobody answered the Ask in time (DA-13).
    TimedOut,
}
