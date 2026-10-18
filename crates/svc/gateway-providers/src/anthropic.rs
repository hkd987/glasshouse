//! The Anthropic Messages API with an org or team key (MP-4).

use glass_gateway::{ModelProvider, ProviderError, ProviderReply, ProviderRequest, Secret};
use serde::Deserialize;
use serde_json::json;

use crate::http::{Http, classify};

/// The public endpoint, unless policy names another.
pub const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

#[derive(Debug)]
pub struct AnthropicProvider {
    http: Http,
    endpoint: String,
    key: Secret,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    content: Vec<Block>,
    #[serde(default)]
    stop_reason: Option<String>,
    usage: Usage,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: String,
}

impl AnthropicProvider {
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

impl ModelProvider for AnthropicProvider {
    fn complete(&self, request: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError> {
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "messages": request.messages,
        });
        if let Some(system) = request.system {
            body["system"] = json!(system);
        }
        let reply = self.http.post(
            &format!("{}/v1/messages", self.endpoint),
            &[
                ("x-api-key", self.key.expose()),
                ("anthropic-version", API_VERSION),
                ("content-type", "application/json"),
            ],
            body.to_string().as_bytes(),
        )?;
        if reply.status != 200 {
            let message = serde_json::from_str::<ErrorBody>(&reply.body)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| "no details".into());
            return Err(classify(reply.status, message));
        }
        let response: Response = serde_json::from_str(&reply.body)
            .map_err(|e| ProviderError::Unavailable(format!("unreadable reply: {e}")))?;
        let text = response
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("");
        Ok(ProviderReply {
            text,
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            stop_reason: response.stop_reason,
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
            model: "claude-test",
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
    fn sends_the_messages_api_shape_and_reads_the_reply() {
        let (url, got) = fake::serve(
            200,
            r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"Hi "},{"type":"text","text":"there"}],"stop_reason":"end_turn","usage":{"input_tokens":12,"output_tokens":3}}"#,
        );
        let provider = AnthropicProvider::with_http(
            Http::insecure_for_tests(),
            Some(&url),
            Secret::new("test-key-not-real"),
        );
        let messages = hello();
        let reply = provider.complete(&request(&messages)).unwrap();
        assert_eq!(reply.text, "Hi there");
        assert_eq!((reply.input_tokens, reply.output_tokens), (12, 3));
        assert_eq!(reply.stop_reason.as_deref(), Some("end_turn"));

        let sent = got.recv().unwrap();
        assert!(
            sent.head.starts_with("POST /v1/messages HTTP/1.1"),
            "{}",
            sent.head
        );
        let head = sent.head.to_ascii_lowercase();
        assert!(head.contains("x-api-key: test-key-not-real"));
        assert!(head.contains("anthropic-version: 2023-06-01"));
        let body: serde_json::Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "claude-test",
                "max_tokens": 64,
                "system": "Be brief.",
                "messages": [{"role": "user", "content": "Hello [redacted:email#1]"}]
            })
        );
    }

    #[test]
    fn errors_say_whether_to_fail_over() {
        let cases = [
            (
                429,
                ProviderError::Unavailable("HTTP 429: slow down".into()),
            ),
            (
                529,
                ProviderError::Unavailable("HTTP 529: slow down".into()),
            ),
            (
                401,
                ProviderError::NotConfigured("HTTP 401: slow down".into()),
            ),
            (400, ProviderError::Rejected("HTTP 400: slow down".into())),
        ];
        for (status, expected) in cases {
            let (url, _got) = fake::serve(
                status,
                r#"{"type":"error","error":{"type":"x","message":"slow down"}}"#,
            );
            let provider = AnthropicProvider::with_http(
                Http::insecure_for_tests(),
                Some(&url),
                Secret::new("k"),
            );
            let messages = hello();
            assert_eq!(provider.complete(&request(&messages)), Err(expected));
        }
    }

    #[test]
    fn an_unreachable_endpoint_is_an_outage_and_https_is_required() {
        let provider = AnthropicProvider::with_http(
            Http::insecure_for_tests(),
            Some("http://127.0.0.1:9"),
            Secret::new("k"),
        );
        let messages = hello();
        let err = provider.complete(&request(&messages)).unwrap_err();
        assert!(err.fails_over(), "{err}");
        let provider = AnthropicProvider::new(Some("http://127.0.0.1:9"), Secret::new("k"));
        assert!(provider.complete(&request(&messages)).is_err());
    }
}
