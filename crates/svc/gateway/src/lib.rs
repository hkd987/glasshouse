//! The model gateway (PRD "Data protection and model gateway", ADR 0018):
//! the one path from an agent to a model.
//!
//! An agent calls the `model.complete` action on its task socket like any
//! other action, so the runtime's pipeline holds, decides, asks and audits
//! it. Policy routes the call by the most sensitive label of the data the
//! task has read (`glass_policy::ModelRules`), and then this crate:
//!
//! - replaces secrets and configured PII with placeholders ([`Redactor`],
//!   DP-3),
//! - tries each model on the route until one answers, failing over on an
//!   outage or a provider that isn't set up ([`Gateway`], MP-8),
//! - puts PII back in the reply, never secrets,
//! - and describes the call for the audit log ([`CallRecord`], DP-5): model,
//!   provider, tokens, cost, label, redactions, and sizes and hashes rather
//!   than text.
//!
//! Providers implement [`ModelProvider`]. [`ScriptedProvider`] answers on
//! this machine for tests and development; the providers that leave the
//! machine live in `glass-gateway-providers`, so the HTTP and TLS stack stays
//! out of every other crate. Credentials come from [`Secrets`], the stand-in
//! for the secrets broker (MP-4).
//!
//! This crate decides nothing about permissions: by the time a call gets
//! here, the pipeline has checked its tier and policy has chosen its route.

mod action;
mod gateway;
mod provider;
mod redact;
mod scripted;
mod secrets;

pub use action::{
    CompleteInput, MAX_PROMPT_CHARS, MAX_TOOLS, MODEL_COMPLETE, Message, ModelAction, Role, Tool,
    ToolCall, ToolResult, definition,
};
pub use gateway::{Attempt, Call, CallRecord, Completion, Digest, Failure, Gateway};
pub use provider::{ModelProvider, ProviderError, ProviderReply, ProviderRequest};
pub use redact::{Redactor, SECRET_KINDS, Vault};
pub use scripted::{ScriptedProvider, Seen, scripted_call};
pub use secrets::{Secret, Secrets, SecretsError};
