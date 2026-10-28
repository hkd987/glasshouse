//! Agent computers (ADR 0065, ADR 0066, ADR 0068): each person has a
//! computer of their own, kept between tasks, where their agents are root
//! and nothing asks. Their agents are personas on it ([`PersonaView`]),
//! each with its own name and role, all working on the one computer. The
//! person watches its screen, takes over its keyboard, and puts it to
//! sleep; tasks name it in [`crate::TaskRequest::computer`] and the
//! persona in [`crate::TaskRequest::persona`].

use glass_types::{AgentId, TaskId, Timestamp};
use serde::{Deserialize, Serialize};

/// One of the caller's agent computers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputerView {
    /// `reconciler`: lowercase letters, digits and `-`.
    pub id: String,
    /// What people call the agent: "Reconciler".
    pub name: String,
    /// What it's for, in a line.
    #[serde(default)]
    pub role: String,
    /// The catalog agent its tasks run.
    pub agent: AgentId,
    pub state: ComputerState,
    /// What runs it, for people who ask: "Firecracker microVM".
    pub kind: String,
    /// Whether it is a boundary (a VM, namespaces); false only in
    /// development.
    pub contained: bool,
    /// What its disk holds, in bytes.
    #[serde(default)]
    pub disk_bytes: u64,
    /// Its tasks running now.
    #[serde(default)]
    pub live_tasks: Vec<TaskId>,
    /// Its person has the keyboard: new tasks wait until they hand it back.
    #[serde(default)]
    pub taken_over: bool,
    /// Where it was kept when it last slept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored: Option<StoredView>,
    pub created_at: Timestamp,
    /// Its person's agents, all working on it (ADR 0068), oldest first.
    #[serde(default)]
    pub personas: Vec<PersonaView>,
}

/// One of a person's agents: a name and a role, a catalog agent to run,
/// and their computer to run on (ADR 0068).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaView {
    /// `reconciler`: lowercase letters, digits and `-`.
    pub id: String,
    /// What people call it: "Reconciler".
    pub name: String,
    #[serde(default)]
    pub role: String,
    /// The catalog agent its tasks run.
    pub agent: AgentId,
    pub created_at: Timestamp,
}

/// `persona.make`: a new agent on the caller's computer, made first if
/// they have none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaNew {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: String,
    pub agent: AgentId,
}

/// The id of a person's own computer, the one their agents share.
pub const PERSON_COMPUTER: &str = "computer";

/// The most agents a person keeps on their computer.
pub const PERSONAS_MAX: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerState {
    /// On this host, nothing running in it.
    Ready,
    /// A task, or its person, is using it.
    Running,
    /// Kept in the store, nothing of it on this host (ADR 0065 §11). A
    /// task or its person wakes it.
    Asleep,
}

/// Where a computer was kept as it went to sleep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredView {
    /// The version it was saved as.
    pub version: String,
    /// What it takes there, in bytes, before sharing pieces with others.
    pub bytes: u64,
    pub at: Timestamp,
    /// "S3: glasshouse-computers-…", or a folder in development.
    pub place: String,
}

/// `computer.make`: a new agent with a computer of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerNew {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub role: String,
    pub agent: AgentId,
}

/// Names one of the caller's agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaRef {
    pub id: String,
}

/// Names one of the caller's computers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerRef {
    pub id: String,
}

/// `computer.screen`: what it showed since `offset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenRequest {
    pub id: String,
    /// Where the last read ended; 0 for the start.
    #[serde(default)]
    pub offset: u64,
}

/// What a computer's screen shows: its tasks' output, and its person's
/// lines while they had the keyboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenView {
    pub text: String,
    /// Where to read from next.
    pub offset: u64,
    pub taken_over: bool,
    pub state: ComputerState,
}

/// `computer.input`: a line its person typed while they have the keyboard.
/// It runs as root on the computer, as the agent does, and shows on the
/// screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenInput {
    pub id: String,
    pub line: String,
}

/// The longest line a person types on a computer.
pub const INPUT_MAX_BYTES: usize = 4096;
