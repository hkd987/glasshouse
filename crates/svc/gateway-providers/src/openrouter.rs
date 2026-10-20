//! OpenRouter (ADR 0039): the org's model credits bought from us, or an
//! individual's own key. One OpenAI-style chat completions endpoint in
//! front of many model providers; `openrouter/auto` picks a model per
//! call, and the reply says which one answered and what it cost.
//!
//! Every request asks OpenRouter to route only to providers that keep no
//! data (`zdr`) and don't collect it for training (`data_collection:
//! "deny"`): a call no such provider can serve fails rather than going
//! anywhere else.

use glass_gateway::{ModelProvider, ProviderError, ProviderReply, ProviderRequest, Secret};
use serde::Deserialize;
use serde_json::json;

use crate::http::{Http, classify};

/// The public endpoint, unless policy names another.
pub const DEFAULT_ENDPOINT: &str = "https://openrouter.ai/api";
/// The router that picks a model for each call.
pub const AUTO: &str = "openrouter/auto";

#[derive(Debug)]
pub struct OpenRouterProvider {
    http: Http,
    endpoint: String,
    key: Secret,
}

#[derive(Deserialize)]
struct Response {
    /// The model that answered.
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    /// What the call cost, in credits (a credit is a dollar).
    #[serde(default)]
    cost: Option<f64>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: String,
}

impl OpenRouterProvider {
    pub fn new(endpoint: Option<&str>, key: Secret) -> Self {
        Self::with_http(Http::new(), endpoint, key)
    }

    fn with_http(http: Http, endpoint: Option<&str>, key: Secret) -> Self {
        Self {
            http,
            endpoint: endpoint
                .unwrap_or(DEFAULT_ENDPOINT)
                .trim_end_matches('/')
                .to_owned(),
            key,
        }
    }
}

impl ModelProvider for OpenRouterProvider {
    fn complete(&self, request: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError> {
        let mut messages = Vec::with_capacity(request.messages.len() + 1);
        if let Some(system) = request.system {
            messages.push(json!({ "role": "system", "content": system }));
        }
        messages.extend(request.messages.iter().map(|m| json!(m)));
        let body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "messages": messages,
            "provider": { "zdr": true, "data_collection": "deny" },
        });
        let bearer = format!("Bearer {}", self.key.expose());
        let reply = self.http.post(
            &format!("{}/v1/chat/completions", self.endpoint),
            &[
                ("authorization", bearer.as_str()),
                ("content-type", "application/json"),
                ("x-title", "Glasshouse"),
            ],
            body.to_string().as_bytes(),
        )?;
        if reply.status != 200 {
            let message = serde_json::from_str::<ErrorBody>(&reply.body)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| "no details".into());
            // The key's credits are used up: another model on the route may
            // still answer, so it fails over rather than ending the call.
            if reply.status == 402 {
                return Err(ProviderError::Unavailable(format!(
                    "HTTP 402: the credits for this key are used up ({message})"
                )));
            }
            return Err(classify(reply.status, message));
        }
        let response: Response = serde_json::from_str(&reply.body)
            .map_err(|e| ProviderError::Unavailable(format!("unreadable reply: {e}")))?;
        let choice = response.choices.into_iter().next();
        let stop_reason = choice.as_ref().and_then(|c| c.finish_reason.clone());
        let text = choice.and_then(|c| c.message.content).unwrap_or_default();
        Ok(ProviderReply {
            text,
            input_tokens: response.usage.prompt_tokens,
            output_tokens: response.usage.completion_tokens,
            stop_reason,
            // Micro-dollars are plenty, as the gateway's own estimates.
            cost_usd: response
                .usage
                .cost
                .map(|c| (c * 1_000_000.0).round() / 1_000_000.0),
            served_by: response.model,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use glass_gateway::{Message, Role};

    use super::*;
    use crate::http::fake;

    fn request<'a>(messages: &'a [Message]) -> ProviderRequest<'a> {
        ProviderRequest {
            model: AUTO,
            system: Some("Be brief."),
            messages,
            max_tokens: 64,
        }
    }

    fn hello() -> Vec<Message> {
        vec![Message {
            role: Role::User,
            content: "Hello [redacted:email#1]".into(),
        }]
    }

    #[test]
    fn adr0039_asks_for_providers_that_keep_nothing_and_reads_who_answered_and_the_cost() {
        let (url, got) = fake::serve(
            200,
            r#"{"id":"gen-1","object":"chat.completion","created":1,"model":"anthropic/claude-sonnet-4.5","choices":[{"index":0,"message":{"role":"assistant","content":"Hi there"},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15,"cost":0.0000841234}}"#,
        );
        let provider = OpenRouterProvider::with_http(
            Http::insecure_for_tests(),
            Some(&url),
            Secret::new("sk-or-test-not-real"),
        );
        let messages = hello();
        let reply = provider.complete(&request(&messages)).unwrap();
        assert_eq!(reply.text, "Hi there");
        assert_eq!((reply.input_tokens, reply.output_tokens), (12, 3));
        assert_eq!(reply.stop_reason.as_deref(), Some("stop"));
        assert_eq!(
            reply.served_by.as_deref(),
            Some("anthropic/claude-sonnet-4.5")
        );
        assert_eq!(reply.cost_usd, Some(0.000084));

        let sent = got.recv().unwrap();
        assert!(
            sent.head.starts_with("POST /v1/chat/completions HTTP/1.1"),
            "{}",
            sent.head
        );
        let head = sent.head.to_ascii_lowercase();
        assert!(head.contains("authorization: bearer sk-or-test-not-real"));
        let body: serde_json::Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "openrouter/auto",
                "max_tokens": 64,
                "messages": [
                    {"role": "system", "content": "Be brief."},
                    {"role": "user", "content": "Hello [redacted:email#1]"}
                ],
                "provider": {"zdr": true, "data_collection": "deny"}
            })
        );
    }

    #[test]
    fn adr0039_used_up_credits_fail_over_and_errors_say_whether_to() {
        let cases = [
            (
                402,
                ProviderError::Unavailable(
                    "HTTP 402: the credits for this key are used up (Insufficient credits)".into(),
                ),
            ),
            (
                429,
                ProviderError::Unavailable("HTTP 429: Insufficient credits".into()),
            ),
            (
                401,
                ProviderError::NotConfigured("HTTP 401: Insufficient credits".into()),
            ),
            (
                400,
                ProviderError::Rejected("HTTP 400: Insufficient credits".into()),
            ),
        ];
        for (status, expected) in cases {
            let (url, _got) = fake::serve(
                status,
                r#"{"error":{"code":402,"message":"Insufficient credits"}}"#,
            );
            let provider = OpenRouterProvider::with_http(
                Http::insecure_for_tests(),
                Some(&url),
                Secret::new("k"),
            );
            let messages = hello();
            let got = provider.complete(&request(&messages));
            assert_eq!(got, Err(expected.clone()));
            assert_eq!(got.unwrap_err().fails_over(), expected.fails_over());
        }
    }

    #[test]
    fn a_reply_without_a_cost_or_model_leaves_them_to_the_gateway() {
        let (url, _got) = fake::serve(
            200,
            r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"length"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
        );
        let provider =
            OpenRouterProvider::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("k"));
        let messages = hello();
        let reply = provider.complete(&request(&messages)).unwrap();
        assert_eq!((reply.cost_usd, reply.served_by), (None, None));
        assert_eq!(reply.stop_reason.as_deref(), Some("length"));
    }
}
