//! Personal layers on catalog agents (PRD "Tailoring catalog items").
//!
//! A layer is one user's changes to an agent, saved on top of the published
//! version instead of a copy: values for the customization points the
//! publisher declared (TL-1) and extra limits that can only narrow what the
//! agent may do (TL-10). Every change is a new version, and going back is
//! switching versions (TL-2). When the agent's version changes, the layer is
//! checked again and paused if it no longer fits (TL-13).
//!
//! This crate stores layers and checks values against customization points.
//! The runtime applies layers to tasks and checks limits against policy,
//! which it owns.

mod points;
mod store;

pub use points::{check_declared, check_settings, describe, resolve};
pub use store::{Layer, LayerStore, Recheck, StoreError, Version};
