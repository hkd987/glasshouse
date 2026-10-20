//! What a model provider implements. Providers send what they're given; the
//! gateway has already routed, redacted and decided by then.

use std::fmt;

use crate::Message;

/// One request as it leaves the machine: already redacted.
#[derive(Debug, Clone, Copy)]
pub struct ProviderRequest<'a> {
    /// The provider's own model id.
    pub model: &'a str,
    pub system: Option<&'a str>,
    pub messages: &'a [Message],
    pub max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderReply {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Why the model stopped, as the provider says it ("end_turn",
    /// "max_tokens").
    pub stop_reason: Option<String>,
    /// What the provider charged for the call, in dollars, when it says
    /// (OpenRouter does); otherwise the gateway estimates from policy.
    pub cost_usd: Option<f64>,
    /// The model that answered, when the provider chose it (OpenRouter's
    /// auto router picks one per call).
    pub served_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// Down, overloaded, rate limited or unreachable. The next model on the
    /// route takes over (MP-8).
    #[error("is unavailable ({0})")]
    Unavailable(String),
    /// Not set up on this computer, such as no credentials. The next model
    /// on the route takes over.
    #[error("is not set up on this computer ({0})")]
    NotConfigured(String),
    /// Refused this request. Another provider would refuse it too, so the
    /// call ends here.
    #[error("refused the request ({0})")]
    Rejected(String),
}

impl ProviderError {
    /// Whether the next model on the route should try.
    pub fn fails_over(&self) -> bool {
        !matches!(self, ProviderError::Rejected(_))
    }
}

/// A model service glassd can call. Calls block; the runtime makes them
/// from its blocking pool.
pub trait ModelProvider: Send + Sync + fmt::Debug {
    fn complete(&self, request: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError>;
}
