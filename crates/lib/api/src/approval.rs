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
    /// What each item says in full, by line, in `items`' order, where it
    /// has words of its own: an email's body. Shown under its item; never
    /// recorded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub texts: Vec<Vec<String>>,
    /// The rule that put this action at Ask.
    pub rule: String,
    pub requested_at: Timestamp,
    pub expires_at: Timestamp,
    pub state: ApprovalState,
    /// How the two answers read, as the action declares them.
    #[serde(default)]
    pub labels: AskLabels,
}

/// The words on an ask's two answers, so it reads as what it does: "Review
/// drafts" and "Approve and send" rather than "Review" and "Approve". Each
/// fits a tile button: one line of at most [`AskLabels::MAX_CHARS`]
/// characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AskLabels {
    /// Opens the review list.
    pub review: String,
    /// Approves the action.
    pub approve: String,
}

impl AskLabels {
    pub const MAX_CHARS: usize = 20;

    pub fn new(review: &str, approve: &str) -> Self {
        Self {
            review: review.into(),
            approve: approve.into(),
        }
    }

    /// Whether both labels fit a tile button.
    pub fn fit(&self) -> bool {
        [&self.review, &self.approve].iter().all(|label| {
            (1..=Self::MAX_CHARS).contains(&label.chars().count())
                && label.trim() == label.as_str()
                && label.chars().all(|c| crate::display_char(c) == c)
        })
    }
}

impl Default for AskLabels {
    fn default() -> Self {
        Self::new("Review", "Approve")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_default_per_field_and_must_fit_a_button() {
        let labels: AskLabels = serde_json::from_str(r#"{"approve":"Approve and send"}"#).unwrap();
        assert_eq!(labels, AskLabels::new("Review", "Approve and send"));
        assert!(labels.fit() && AskLabels::default().fit());
        assert!(!AskLabels::new("", "Approve").fit());
        assert!(!AskLabels::new("Review", "Approve and send them all").fit());
        assert!(!AskLabels::new("Review\nall", "Approve").fit());
        assert!(!AskLabels::new("Review", "Approve\u{202E}").fit());
        assert!(!AskLabels::new("Review ", "Approve").fit());
    }
}
