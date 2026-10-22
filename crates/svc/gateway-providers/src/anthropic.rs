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
    /// Automatic prompt caching for the conversation's tail, a top-level
    /// field Anthropic's own API takes; endpoints policy names instead may
    /// not.
    auto_cache: bool,
}

#[derive(Deserialize)]
struct Response {
    /// Thinking, text and `tool_use` blocks (ADR 0041, ADR 0043).
    #[serde(default)]
    content: Vec<serde_json::Value>,
    #[serde(default)]
    stop_reason: Option<String>,
    /// On a refusal, its category when Anthropic says (ADR 0043).
    #[serde(default)]
    stop_details: Option<StopDetails>,
    usage: Usage,
}

#[derive(Deserialize)]
struct StopDetails {
    #[serde(default)]
    category: Option<String>,
}

/// Anthropic counts cached input apart: `input_tokens` is only what came
/// after the last cache breakpoint.
#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
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
        let endpoint = endpoint
            .unwrap_or(DEFAULT_ENDPOINT)
            .trim_end_matches('/')
            .to_owned();
        Self {
            http,
            auto_cache: endpoint == DEFAULT_ENDPOINT,
            endpoint,
            key,
        }
    }
}

impl ModelProvider for AnthropicProvider {
    fn complete(&self, request: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError> {
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "messages": crate::tools::anthropic_messages(request.messages),
        });
        // The prompt cache (ADR 0043): a breakpoint at the end of the
        // instructions, which the tools come before, so every step of a
        // tool loop reads them; and the tail, which grows by a turn a step.
        if let Some(system) = request.system {
            body["system"] = json!([{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }]);
        }
        if self.auto_cache {
            body["cache_control"] = json!({ "type": "ephemeral" });
        }
        if !request.tools.is_empty() {
            body["tools"] = crate::tools::anthropic_tools(request.tools);
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
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        let usage = &response.usage;
        Ok(ProviderReply {
            text,
            tool_calls: crate::tools::anthropic_calls(&response.content),
            input_tokens: usage.input_tokens
                + usage.cache_creation_input_tokens
                + usage.cache_read_input_tokens,
            output_tokens: usage.output_tokens,
            cache_write_tokens: usage.cache_creation_input_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            // "refusal": the gateway ends the call (ADR 0043).
            stop_reason: response.stop_reason,
            declined: response.stop_details.and_then(|d| d.category),
            cost_usd: None,
            served_by: None,
            // All of it goes back on the next call, thinking included.
            blocks: response.content,
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
            tools: &[],
        }
    }

    fn hello() -> Vec<Message> {
        vec![Message::text(Role::User, "Hello [redacted:email#1]")]
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
                "system": [{"type": "text", "text": "Be brief.", "cache_control": {"type": "ephemeral"}}],
                "messages": [{"role": "user", "content": "Hello [redacted:email#1]"}]
            })
        );
    }

    #[test]
    fn adr0043_the_cache_is_marked_and_what_it_saved_is_counted() {
        let (url, got) = fake::serve(
            200,
            r#"{"content":[{"type":"text","text":"Hi"}],"stop_reason":"end_turn","usage":{"input_tokens":50,"output_tokens":3,"cache_creation_input_tokens":200,"cache_read_input_tokens":1800}}"#,
        );
        let mut provider =
            AnthropicProvider::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("k"));
        assert!(!provider.auto_cache, "only Anthropic's own endpoint");
        provider.auto_cache = true;
        let messages = hello();
        let reply = provider.complete(&request(&messages)).unwrap();
        assert_eq!(reply.input_tokens, 2_050);
        assert_eq!(
            (reply.cache_write_tokens, reply.cache_read_tokens),
            (200, 1_800)
        );
        let body: serde_json::Value = serde_json::from_str(&got.recv().unwrap().body).unwrap();
        assert_eq!(body["cache_control"], json!({ "type": "ephemeral" }));
        assert_eq!(
            body["system"][0]["cache_control"],
            json!({ "type": "ephemeral" })
        );
        assert!(AnthropicProvider::new(None, Secret::new("k")).auto_cache);
    }

    #[test]
    fn adr0043_a_refusal_says_why() {
        let (url, _got) = fake::serve(
            200,
            r#"{"content":[],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"},"usage":{"input_tokens":40,"output_tokens":0}}"#,
        );
        let provider =
            AnthropicProvider::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("k"));
        let messages = hello();
        let reply = provider.complete(&request(&messages)).unwrap();
        assert_eq!(reply.stop_reason.as_deref(), Some("refusal"));
        assert_eq!(reply.declined.as_deref(), Some("cyber"));
        assert_eq!(reply.text, "");
    }

    #[test]
    fn adr0043_thinking_comes_back_whole_and_goes_back_as_written() {
        let thinking =
            json!({ "type": "thinking", "thinking": "", "signature": "EqQBCkgIARABGAIi" });
        let used = json!({ "type": "tool_use", "id": "toolu_1", "name": "files_list", "input": { "path": "Docs" } });
        let (url, got) = fake::serve(
            200,
            r#"{"content":[{"type":"thinking","thinking":"","signature":"EqQBCkgIARABGAIi"},{"type":"tool_use","id":"toolu_1","name":"files_list","input":{"path":"Docs"}}],"stop_reason":"tool_use","usage":{"input_tokens":20,"output_tokens":9}}"#,
        );
        let provider =
            AnthropicProvider::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("k"));
        let messages = hello();
        let reply = provider.complete(&request(&messages)).unwrap();
        assert_eq!(reply.blocks, vec![thinking.clone(), used.clone()]);
        assert_eq!(reply.tool_calls[0].id, "toolu_1");
        drop(got);

        // The next call: the assistant turn is those blocks, unchanged;
        // the result answers the call they make.
        let (url, got) = fake::serve(
            200,
            r#"{"content":[{"type":"text","text":"One file."}],"stop_reason":"end_turn","usage":{"input_tokens":30,"output_tokens":3}}"#,
        );
        let provider =
            AnthropicProvider::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("k"));
        let mut turn = hello();
        turn.push(Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: reply.tool_calls.clone(),
            tool_results: Vec::new(),
            replay: Some(glass_gateway::Replay {
                provider: "anthropic".into(),
                blocks: reply.blocks.clone(),
                seal: "checked by the gateway".into(),
            }),
        });
        turn.push(Message {
            role: Role::User,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![glass_gateway::ToolResult {
                id: "toolu_1".into(),
                content: "a.txt".into(),
                is_error: false,
            }],
            replay: None,
        });
        provider.complete(&request(&turn)).unwrap();
        let body: serde_json::Value = serde_json::from_str(&got.recv().unwrap().body).unwrap();
        assert_eq!(body["messages"][1]["content"], json!([thinking, used]));
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "toolu_1");
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
