//! Agents in Microsoft Teams (ADR 0065 §13): the logic of the Teams relay,
//! with no network of its own. The front (`glass-agents`) receives the Bot
//! Framework's calls at `POST /api/teams/messages` and sends through
//! `glass_gateway_providers::teams`; this crate decides.
//!
//! - [`verify`]: whether a call is one Microsoft's Bot Connector signed for
//!   this app, about Teams in the org's tenant. Refused, answer 401 and
//!   nothing else; the refusal's words go to the admin log.
//! - [`interpret`]: what a verified activity asks: a message to an agent,
//!   an answer from an approval card, a card asking for its current state,
//!   the app installed or removed for a person, or nothing for us.
//! - [`approval_card`]: the Adaptive Card for an approval in each state,
//!   with Universal Actions; [`invoke_response`] carries it back.
//! - [`ConversationRef`]: where to reach a person later, kept per person.
//!
//! What the front enforces, from what this surfaces: that whoever answered
//! ([`CardAnswer::by`], their Entra object id, never a name) is the person
//! the approval is for or a delegate; that each card answers its approval
//! once; and which approvals are confidential.

mod activity;
mod auth;
mod b64;
mod cards;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use activity::{
    Account, Activity, CardAnswer, CardRefresh, ConversationAccount, ConversationRef, Incoming,
    MessageToAgent, Scope, Who, interpret,
};
pub use auth::{BotAuth, CHANNEL, CLOCK_SKEW_SECS, ISSUER, Jwk, Jwks, Refusal, key_id, verify};
pub use cards::{
    CARD_CONTENT_TYPE, CARD_VERSION, CardContext, MAX_REFRESH_USERS, VERB_APPROVE, VERB_DENY,
    VERB_REFRESH, approval_activity, approval_card, card_activity, invoke_error, invoke_message,
    invoke_response, text_activity,
};

#[cfg(test)]
mod tests;
