//! Acting for a person on a shared agent host (ADR 0067): only the front
//! may, and only with a proof glassd checks itself.

use serde::{Deserialize, Serialize};

use glass_types::Timestamp;

/// `session.as`: the connection acts for the person this proves, from now
/// until it closes. Only the front's own account may ask, once per
/// connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "proof", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionAs {
    /// The load balancer's signed assertion (`x-amzn-oidc-data`).
    LoadBalancer { assertion: String },
    /// A token glassd issued for `glass` (`token.exchange`).
    Token { token: String },
    /// Development only: glassd takes it when its config says
    /// `dev_people = true`, never in a shipped config.
    Dev {
        sub: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        email: String,
    },
}

/// Who a session acts for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonView {
    /// Their sign-in subject.
    pub key: String,
    pub name: String,
    pub email: String,
    /// How this session's person was proven: "load_balancer", "token" or
    /// "dev".
    pub proven_by: String,
}

/// A token for `glass`, shown once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedToken {
    pub token: String,
    pub expires_at: Timestamp,
}

/// `token.code`: a one-time code for `glass login`, for a person signed in
/// at the load balancer. The browser carries it to `glass` on the person's
/// own computer, which trades it for a token (`token.exchange`): the token
/// itself never passes through the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginCode {
    pub code: String,
    pub expires_at: Timestamp,
}

/// `token.exchange`: a code from `token.code`, once, for a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenExchange {
    pub code: String,
}

/// What a `glass` token starts with, and a login code: the front tells
/// them apart by it.
pub const TOKEN_PREFIX: &str = "gat_";
pub const CODE_PREFIX: &str = "gac_";
