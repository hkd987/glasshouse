//! The Glasshouse Agents API (ADR 0065 §12, ADR 0066): what the web app,
//! the `glass` CLI and the Teams relay read from the front and send to it,
//! as JSON over HTTP under [`PREFIX`].
//!
//! ```text
//! GET  /api/v1/session                  who is signed in
//! POST /api/v1/session                  sign in (development only)
//! GET  /api/v1/agents                   [AgentView], the person's agents
//! POST /api/v1/agents                   NewAgent → AgentView
//! GET  /api/v1/agents/<id>              AgentDetail
//! POST /api/v1/agents/<id>/ask          Ask → Asked
//! GET  /api/v1/agents/<id>/screen       ?since=<offset> → Screen
//! POST /api/v1/agents/<id>/take-over    → AgentView (the agent paused)
//! POST /api/v1/agents/<id>/hand-back    → AgentView
//! POST /api/v1/agents/<id>/input        Input → Screen (taken over only)
//! POST /api/v1/agents/<id>/undo         → UndoResult
//! POST /api/v1/agents/<id>/wake         → AgentView
//! POST /api/v1/agents/<id>/sleep        → AgentView
//! GET  /api/v1/approvals                [PendingApproval], the person's
//! POST /api/v1/approvals/<id>           Answer → ApprovalView
//! GET  /api/v1/templates                [Template] IT approved
//! GET  /api/v1/events                   ?since=<cursor> (long poll) → Events
//! POST /api/teams/messages              Microsoft's Bot Framework (ADR 0065 §13)
//! ```
//!
//! Errors are `{"error": "<words for people>"}` with a 4xx or 5xx status.

use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "/api/v1";

/// Who is signed in, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub person: Person,
    pub org: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// The account name: `dana`.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
}

/// `POST /api/v1/session`, development only: the front signs in whoever
/// runs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignIn {
    pub person: String,
}

/// An agent, as its card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentView {
    /// `reconciler`: letters, digits and `-`.
    pub id: String,
    pub name: String,
    /// What it's for: "Finance".
    pub role: String,
    /// The template it was made from.
    pub template: String,
    pub status: AgentStatus,
    /// What it's doing, or last did, in a line.
    pub doing: String,
    /// When that last changed (RFC 3339).
    pub updated_at: String,
    /// Questions waiting on the person.
    pub pending: u32,
    pub computer: ComputerView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Its computer is starting, or waking from storage.
    Starting,
    /// A task is running.
    Working,
    /// A question waits on the person.
    NeedsYou,
    /// The person has its computer: the agent waits.
    TakenOver,
    /// Nothing to do; its computer runs, and sleeps soon.
    Idle,
    /// Its computer is saved to storage, nothing running.
    Asleep,
}

/// An agent's computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerView {
    pub state: ComputerState,
    /// What runs it, for people: "Firecracker microVM", "Linux namespaces".
    pub kind: String,
    /// Whether it is a security boundary: a VM is; the namespaces are a
    /// weaker one; a development stand-in isn't.
    pub contained: bool,
    /// Bytes on its disk.
    #[serde(default)]
    pub disk_bytes: u64,
    /// Asleep: bytes it takes in storage, and when it went to sleep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored: Option<Stored>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerState {
    /// Never started: its disk is the template's image.
    New,
    Starting,
    /// On this host, nothing running in it.
    Ready,
    Running,
    Saving,
    Asleep,
    Waking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    pub bytes: u64,
    pub at: String,
    /// Where, for people: "S3: acme-glasshouse-agents".
    pub place: String,
}

/// `GET /api/v1/agents/<id>`: the agent, its conversation and its record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDetail {
    pub agent: AgentView,
    pub reach: Vec<Reach>,
    pub messages: Vec<Message>,
    /// The latest of what it did, on the audit record, newest last.
    pub record: Vec<RecordLine>,
}

/// What an agent can reach, from its template and policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reach {
    pub text: String,
    pub sub: String,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordLine {
    /// RFC 3339.
    pub at: String,
    pub text: String,
}

/// One message of a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Unique in the conversation, in order.
    pub id: String,
    pub at: String,
    pub from: From,
    /// The task it belongs to.
    pub task: String,
    pub body: Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum From {
    Person,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    Text {
        text: String,
        /// Files it was given, copied into its computer.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
    },
    /// What it did, step by step.
    Progress { steps: Vec<String> },
    /// A question: an approval on the desk.
    Approval(ApprovalView),
    /// Something it finished: what it says, and the files it changed.
    Done {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<String>,
        /// Whether the task can still be undone.
        undoable: bool,
    },
    /// Something the system says: asleep, woken, undone.
    Note { text: String },
}

/// A question an agent asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalView {
    pub id: String,
    /// The action it asks for: `mail.send`.
    pub action: String,
    /// What it wants to do, for people.
    pub summary: String,
    /// The detail: recipients, a site, a file.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The answer's button: "Send 3 emails".
    pub yes_label: String,
    pub state: ApprovalState,
    /// Confidential: its detail stays out of Teams and notifications
    /// (labels, ADR 0018).
    #[serde(default)]
    pub confidential: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved {
        by: String,
        at: String,
        via: Channel,
    },
    Denied {
        by: String,
        at: String,
        via: Channel,
    },
    /// Unanswered past its time, or its task ended.
    Expired,
}

/// Where an answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Web,
    Desktop,
    Cli,
    Teams,
    Api,
}

/// `GET /api/v1/approvals`: a question, with its agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub agent: String,
    pub agent_name: String,
    pub approval: ApprovalView,
}

/// `POST /api/v1/approvals/<id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub yes: bool,
    #[serde(default = "web")]
    pub via: Channel,
}

fn web() -> Channel {
    Channel::Web
}

/// `POST /api/v1/agents/<id>/ask`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub text: String,
    /// Paths in the person's files to give it, as copies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default = "web")]
    pub via: Channel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
    pub task: String,
}

/// `GET /api/v1/agents/<id>/screen`: its computer's terminal, from
/// `offset` on. A headless computer's screen is its terminal; a computer
/// with a desktop streams that instead (later).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    /// Where the next read starts.
    pub offset: u64,
    pub text: String,
    /// Whether the person has it now.
    pub taken_over: bool,
    pub state: ComputerState,
}

/// `POST /api/v1/agents/<id>/input`: a command line typed while taken
/// over, run in its computer as the person, on the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoResult {
    /// What it put back, for people.
    pub text: String,
    /// Files it couldn't put back, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left: Vec<String>,
}

/// An agent IT approved, to make one from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub team: String,
    pub does: String,
    pub reach: Vec<Reach>,
}

/// `POST /api/v1/agents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAgent {
    pub template: String,
    /// Its name, if not the template's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `GET /api/v1/events?since=<cursor>`: what changed since, waiting up to
/// 25 s for something to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Events {
    pub cursor: u64,
    /// Agents whose card, conversation or computer changed.
    pub agents: Vec<String>,
}

/// An error's body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

/// Whether `id` names an agent: 1 to 40 of `a-z`, `0-9` and `-`, not
/// starting with `-`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0066_messages_and_answers_read_as_the_web_app_expects() {
        let m = Message {
            id: "1".into(),
            at: "2026-10-27T10:54:00Z".into(),
            from: From::Agent,
            task: "t".into(),
            body: Body::Approval(ApprovalView {
                id: "a1".into(),
                action: "mail.send".into(),
                summary: "Send 3 emails?".into(),
                detail: String::new(),
                yes_label: "Send 3 emails".into(),
                state: ApprovalState::Approved {
                    by: "dana".into(),
                    at: "2026-10-27T10:58:00Z".into(),
                    via: Channel::Teams,
                },
                confidential: false,
            }),
        };
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["body"]["kind"], "approval");
        assert_eq!(json["body"]["state"]["state"], "approved");
        assert_eq!(json["body"]["state"]["via"], "teams");
        let back: Message = serde_json::from_value(json).unwrap();
        assert_eq!(back, m);
        let answer: Answer = serde_json::from_str(r#"{"yes": true}"#).unwrap();
        assert_eq!(answer.via, Channel::Web);
        assert!(valid_id("reconciler-2"));
        assert!(!valid_id("../x") && !valid_id("-a") && !valid_id(""));
    }
}
