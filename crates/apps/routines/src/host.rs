//! What the Routines app needs from the runtime (ADR 0047): starting a
//! routine now, and the requests the person did the same way more than
//! once, as the code this computer wrote from them (ADR 0046). glassd
//! implements it with its runtime; without it, those actions refuse.

use std::fmt;
use std::path::PathBuf;

use glass_types::{AgentId, TaskId, Timestamp};
use serde::Serialize;

pub trait Host: Send + Sync + fmt::Debug {
    /// Starts `login`'s routine `routine` now, as `glassctl routine run`
    /// does: the run's task.
    fn run(&self, login: &str, uid: u32, routine: &str) -> Result<TaskId, String>;

    /// `login`'s requests done more than once with the same steps and not
    /// kept as a routine, the latest first.
    fn repeats(&self, login: &str, uid: u32) -> Vec<Repeat>;

    /// Stops offering the request the repeat `task` did, until it's done
    /// the same way twice again.
    fn dismiss(&self, login: &str, uid: u32, task: TaskId) -> Result<(), String>;
}

/// A request done the same way more than once: what keeping it as a
/// routine keeps.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Repeat {
    /// The latest task that did it.
    pub task: TaskId,
    pub request: String,
    pub agent: AgentId,
    /// How many times it was done this way.
    pub times: usize,
    pub last_at: Timestamp,
    /// The code this computer wrote from what the task did.
    pub script: String,
    /// What it does, step by step.
    pub steps: Vec<String>,
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
}
