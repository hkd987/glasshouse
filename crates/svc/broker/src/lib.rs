//! The permission broker (PRD "Containment and platform architecture").
//!
//! Every privileged call an agent makes passes through two pieces here:
//!
//! - [`decide`] turns policy, the action and the data it touches into a
//!   [`Verdict`]: run, ask, or block.
//! - [`ApprovalDesk`] holds the asks: one approval per call (batches carry a
//!   review list), answered once or for the rest of the task, never forever
//!   (DA-11), and withdrawn with every task grant when the task ends (PA-2).
//!
//! The broker decides; it never performs actions and never talks to agents.

mod decide;
mod desk;

pub use decide::{LabelLookup, Verdict, decide};
pub use desk::{ApprovalDesk, DeskError, Outcome, new_view};
