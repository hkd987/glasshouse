//! Model credits on a managed computer (ADR 0039): a provider that sends
//! each call to the org's admin console, signed with the computer's key,
//! and the console forwards it to OpenRouter with the org's key. No key for
//! OpenRouter is ever on the computer.

use std::sync::{Arc, PoisonError, RwLock};

use glass_gateway::{ModelProvider, ProviderError, ProviderReply, ProviderRequest, Role};
use glass_managed::models::{MAX_OUTPUT_TOKENS, ModelCall, ModelMessage};

use crate::ConsoleClient;

/// The computer's link to its console for model calls, made once managed
/// sync has the computer's key; until then, calls on credits fail over.
#[derive(Debug, Default)]
pub struct CreditsLink {
    client: RwLock<Option<(Arc<ConsoleClient>, String)>>,
}

impl CreditsLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Calls from now on go to the console `client` reaches, as `device`.
    pub fn connect(&self, client: Arc<ConsoleClient>, device: &str) {
        *self.client.write().unwrap_or_else(PoisonError::into_inner) =
            Some((client, device.to_owned()));
    }

    fn get(&self) -> Option<(Arc<ConsoleClient>, String)> {
        self.client
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The `credits` provider: calls through the org's admin console.
#[derive(Debug)]
pub struct CreditsProvider {
    link: Arc<CreditsLink>,
}

impl CreditsProvider {
    pub fn new(link: Arc<CreditsLink>) -> Self {
        Self { link }
    }
}

impl ModelProvider for CreditsProvider {
    fn complete(&self, request: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError> {
        let Some((client, device)) = self.link.get() else {
            return Err(ProviderError::NotConfigured(
                "this computer isn't connected to its admin console yet".into(),
            ));
        };
        let call = ModelCall {
            device,
            model: request.model.to_owned(),
            system: request.system.map(str::to_owned),
            messages: request
                .messages
                .iter()
                .map(|m| ModelMessage {
                    role: match m.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    }
                    .into(),
                    content: m.content.clone(),
                })
                .collect(),
            max_tokens: request.max_tokens.min(MAX_OUTPUT_TOKENS),
        };
        let answer = client.complete_model(&call)?;
        Ok(ProviderReply {
            text: answer.text,
            input_tokens: answer.input_tokens,
            output_tokens: answer.output_tokens,
            stop_reason: answer.stop_reason,
            cost_usd: answer.cost_usd,
            served_by: answer.served_by,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use glass_gateway::Message;
    use glass_managed::device::DeviceKey;

    use super::*;
    use crate::http::fake;

    fn linked(url: &str) -> CreditsProvider {
        let key = Arc::new(DeviceKey::generate("lab-1").unwrap());
        let client = ConsoleClient::new(url, "enroll", true)
            .unwrap()
            .signed_by(key);
        let link = CreditsLink::new();
        link.connect(Arc::new(client), "lab-1");
        CreditsProvider::new(link)
    }

    fn call(provider: &CreditsProvider) -> Result<ProviderReply, ProviderError> {
        let messages = [Message {
            role: Role::User,
            content: "Summarise [redacted:email#1]'s note".into(),
        }];
        provider.complete(&ProviderRequest {
            model: "openrouter/auto",
            system: Some("Be brief."),
            messages: &messages,
            max_tokens: 1_000_000,
        })
    }

    #[test]
    fn adr0039_a_call_on_credits_goes_to_the_console_signed_by_the_computer() {
        let (url, got) = fake::serve(
            200,
            r#"{"text":"Done.","input_tokens":12,"output_tokens":2,"stop_reason":"stop","cost_usd":0.00025,"served_by":"anthropic/claude-sonnet-4.5"}"#,
        );
        let reply = call(&linked(&url)).unwrap();
        assert_eq!(reply.text, "Done.");
        assert_eq!(reply.cost_usd, Some(0.00025));
        assert_eq!(
            reply.served_by.as_deref(),
            Some("anthropic/claude-sonnet-4.5")
        );

        let sent = got.recv().unwrap();
        assert!(
            sent.head
                .starts_with("POST /device/v1/models/complete HTTP/1.1"),
            "{}",
            sent.head
        );
        assert!(
            sent.head.contains("Glasshouse-Signature: "),
            "{}",
            sent.head
        );
        let body: serde_json::Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(body["device"], "lab-1");
        assert_eq!(body["model"], "openrouter/auto");
        assert_eq!(body["system"], "Be brief.");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["max_tokens"], MAX_OUTPUT_TOKENS);
        // No OpenRouter key: there is none on the computer to send.
        assert!(!sent.head.to_ascii_lowercase().contains("bearer"));
    }

    #[test]
    fn adr0039_used_up_credits_and_outages_fail_over_and_a_bad_call_does_not() {
        for (status, fails_over) in [(402, true), (503, true), (401, true), (400, false)] {
            let (url, _got) = fake::serve(status, r#"{"error":"The org's credits are used up."}"#);
            let err = call(&linked(&url)).unwrap_err();
            assert_eq!(err.fails_over(), fails_over, "{status}: {err}");
            if status == 402 {
                assert!(err.to_string().contains("used up"), "{err}");
            }
        }
        // Not connected yet.
        let err = call(&CreditsProvider::new(CreditsLink::new())).unwrap_err();
        assert!(matches!(err, ProviderError::NotConfigured(_)), "{err}");
    }
}
