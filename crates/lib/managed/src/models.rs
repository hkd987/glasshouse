//! Model calls on the org's model credits (ADR 0039): the computer sends
//! each call to its org's admin console, signed with its own key (ADR 0030
//! slice P), and the console forwards it to OpenRouter with the org's key,
//! which never leaves our servers. What a call carries was redacted on the
//! computer already (DP-3); the console keeps no prompt or reply, only who
//! called, which model, the tokens and the cost.

use serde::{Deserialize, Serialize};

/// Where a computer sends a call.
pub const COMPLETE_PATH: &str = "/device/v1/models/complete";

/// The most a call may ask a model to write.
pub const MAX_OUTPUT_TOKENS: u32 = 32_768;

/// One call, as the computer's gateway would send it to a provider.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCall {
    /// The computer calling: the one whose key signed the request.
    pub device: String,
    /// The provider's model id, such as `openrouter/auto`.
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<ModelMessage>,
    pub max_tokens: u32,
}

/// The call's words never print: a log line or an error shows its shape.
impl std::fmt::Debug for ModelCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelCall")
            .field("device", &self.device)
            .field("model", &self.model)
            .field("messages", &self.messages.len())
            .field("max_tokens", &self.max_tokens)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMessage {
    /// `user` or `assistant`.
    pub role: String,
    pub content: String,
}

/// What the model answered, and what it cost the org.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelAnswer {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// In the org's dollars: what OpenRouter charged at the rate the org
    /// paid for its credits.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// The model OpenRouter's router chose.
    #[serde(default)]
    pub served_by: Option<String>,
}

impl std::fmt::Debug for ModelAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelAnswer")
            .field("input_tokens", &self.input_tokens)
            .field("output_tokens", &self.output_tokens)
            .field("cost_usd", &self.cost_usd)
            .field("served_by", &self.served_by)
            .finish_non_exhaustive()
    }
}
