use glass_types::{ActionName, AgentId, ApprovalId, TaskId, Timestamp};
use serde::{Deserialize, Serialize};

/// How far an approval reaches (DA-11). There is deliberately no "always":
/// standing permissions are set by IT policy, never from a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// Just this request.
    Once,
    /// This action for the rest of this task ("Allow sends for the rest of
    /// this task").
    Task,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ApprovalAnswer {
    Approve { scope: ApprovalScope },
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnswerApproval {
    pub approval: ApprovalId,
    pub answer: ApprovalAnswer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved {
        scope: ApprovalScope,
    },
    Denied,
    /// Nobody answered within the policy timeout; the task paused (DA-13).
    TimedOut,
    /// The task ended (stopped, failed) before anyone answered.
    Withdrawn,
}

/// One "Needs you" request. Batched asks carry their review list in `items`
/// so "Send 6 reminders" is one approval, not six (DA-12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalView {
    pub id: ApprovalId,
    pub task: TaskId,
    pub agent: AgentId,
    pub action: ActionName,
    /// One line, such as "Send 6 reminders".
    pub summary: String,
    /// The review list: one entry per item the action affects.
    pub items: Vec<String>,
    /// The rule that put this action at Ask.
    pub rule: String,
    pub requested_at: Timestamp,
    pub expires_at: Timestamp,
    pub state: ApprovalState,
}
