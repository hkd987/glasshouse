//! Schedules (ADR 0069): directions a person gives one of their agents to
//! carry out on a schedule, such as "weekdays 09:00", on their computer.
//! They're kept off the computer, so they run while it sleeps: the host
//! that claims a run wakes the computer and starts the task.

use glass_types::{TaskId, Timestamp};
use serde::{Deserialize, Serialize};

/// The most schedules a person keeps on.
pub const SCHEDULES_MAX: usize = 20;
/// Runs in a row that fail before a schedule turns itself off.
pub const SCHEDULE_FAILURES_MAX: u32 = 10;
/// The longest directions a schedule keeps, in characters.
pub const SCHEDULE_PROMPT_MAX_CHARS: usize = 2_000;

/// One of the caller's schedules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleView {
    /// `s` and 12 hex digits.
    pub id: String,
    /// The agent it directs, one of the person's (ADR 0068).
    pub persona: String,
    /// What the agent is asked each time.
    pub prompt: String,
    /// When, as written: "weekdays 09:00", "hourly".
    pub when: String,
    /// When, in words: "every weekday at 09:00".
    pub describe: String,
    /// The time zone `when` is in: "Europe/London".
    pub zone: String,
    pub enabled: bool,
    /// Why it's off, when it turned itself off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_why: Option<String>,
    /// When it runs next, while on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<Timestamp>,
    /// Runs in a row that failed.
    #[serde(default)]
    pub failures: u32,
    /// Its latest run, or a slot it let pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<ScheduleRunView>,
    pub created_at: Timestamp,
}

/// How a schedule's latest slot went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRunView {
    pub at: Timestamp,
    /// `running`, `done`, `failed`, `stopped`, `missed` (late by more than
    /// an hour: not replayed), `skipped` (the last run hadn't ended), or
    /// `refused` (it couldn't start).
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    /// Why, for one that didn't run or failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// `schedule.make`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleNew {
    pub persona: String,
    pub prompt: String,
    pub when: String,
    /// An IANA time zone; UTC when left out.
    #[serde(default)]
    pub zone: Option<String>,
}

/// `schedule.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRef {
    pub id: String,
}

/// `schedule.enable`: on again (its failures forgotten), or off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleEnable {
    pub id: String,
    pub enabled: bool,
}
