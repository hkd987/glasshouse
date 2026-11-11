//! The Anthropic Messages API with an org or team key (MP-4).

use glass_gateway::{ModelProvider, ProviderError, ProviderReply, ProviderRequest, Secret};
use serde::Deserialize;
use serde_json::json;

use crate::http::{Again, Http, Reply, classify};

/// The public endpoint, unless policy names another.
pub const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
/// The beta that lets a call ask for the thinking-history check (ADR 0043).
const HISTORY_CHECK_BETA: &str = "thinking-binding-controls-2026-08-01";

#[derive(Debug)]
pub struct AnthropicProvider {
    http: Http,
    endpoint: String,
    key: Secret,
    /// Automatic prompt caching for the conversation's tail, a top-level
    /// field Anthropic's own API takes; endpoints policy names instead may
    /// not.
    auto_cache: bool,
    /// Ask the API to refuse thinking sent back after a different
    /// conversation (policy's `check_history`).
    check_history: bool,
    /// When a call is sent again (at once in tests).
    again: Again,
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
    #[serde(default, rename = "type")]
    kind: String,
    message: String,
}

impl AnthropicProvider {
    pub fn new(endpoint: Option<&str>, key: Secret) -> Self {
        Self::with_http(Http::new(), endpoint, key)
    }

    /// Through a relay on this machine, over plain HTTP: development, where
    /// a computer can't reach the API itself (`tools/dev/model-relay.py`).
    /// `None` unless `endpoint` is `http://` to a loopback address.
    pub fn relayed(endpoint: &str, key: Secret) -> Option<Self> {
        let target = crate::http::Target::parse(&format!("{}/", endpoint.trim_end_matches('/')))?;
        (!target.tls && crate::console::is_loopback(&target.host))
            .then(|| Self::with_http(Http::loopback(), Some(endpoint), key))
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
            check_history: false,
            again: Again::usual(),
        }
    }

    /// Every call asks for the thinking-history check (ADR 0043).
    pub fn checking_history(mut self, on: bool) -> Self {
        self.check_history = on;
        self
    }

    /// Posts `body`, sent again as every model client's calls are
    /// ([`Again`]): rate limits (429), overload (529), server errors and a
    /// connection that ended before a reply, as Anthropic's own SDKs do.
    fn post(&self, body: &[u8]) -> Result<Reply, ProviderError> {
        let url = format!("{}/v1/messages", self.endpoint);
        let mut headers = vec![
            ("x-api-key", self.key.expose()),
            ("anthropic-version", API_VERSION),
            ("content-type", "application/json"),
        ];
        if self.check_history {
            headers.push(("anthropic-beta", HISTORY_CHECK_BETA));
        }
        self.http.post_again(&url, &headers, body, &self.again)
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
        // Adaptive is how these models think anyway; the binding is what's
        // asked for.
        if self.check_history {
            body["thinking"] = json!({
                "type": "adaptive",
                "block_binding": { "prefix_mismatch_behavior": "error" },
            });
        }
        let reply = self.post(body.to_string().as_bytes())?;
        if reply.status != 200 {
            let (kind, message) = serde_json::from_str::<ErrorBody>(&reply.body)
                .map(|e| (e.error.kind, e.error.message))
                .unwrap_or_else(|_| (String::new(), "no details".into()));
            // The account's credit is used up, which Anthropic says with a
            // 402 or, today, a 400: another model on the route may still
            // answer, so it fails over, as OpenRouter's 402 does.
            if reply.status == 402
                || kind == "billing_error"
                || message.starts_with("Your credit balance is too low")
            {
                return Err(ProviderError::Unavailable(format!(
                    "HTTP {}: the credits for this key are used up ({message})",
                    reply.status
                )));
            }
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
    fn adr0043_checks_on_a_real_model_ask_for_the_thinking_history_check() {
        const OK: &str = r#"{"content":[{"type":"text","text":"Hi"}],"stop_reason":"end_turn","usage":{"input_tokens":12,"output_tokens":3}}"#;
        let (url, got) = fake::serve_each(vec![(200, OK), (200, OK)]);
        let messages = hello();
        for on in [true, false] {
            AnthropicProvider::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("k"))
                .checking_history(on)
                .complete(&request(&messages))
                .unwrap();
            let sent = got.recv().unwrap();
            let body: serde_json::Value = serde_json::from_str(&sent.body).unwrap();
            let beta = sent
                .head
                .to_ascii_lowercase()
                .contains("anthropic-beta: thinking-binding-controls-2026-08-01\r\n");
            let binding = &body["thinking"]["block_binding"]["prefix_mismatch_behavior"];
            match on {
                true => {
                    assert!(beta, "{}", sent.head);
                    assert_eq!(body["thinking"]["type"], "adaptive");
                    assert_eq!(binding, "error");
                }
                false => {
                    assert!(!beta && !sent.head.contains("anthropic-beta"));
                    assert!(body.get("thinking").is_none(), "{body}");
                }
            }
        }
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
            let mut provider = AnthropicProvider::with_http(
                Http::insecure_for_tests(),
                Some(&url),
                Secret::new("k"),
            );
            provider.again = Again::never();
            let messages = hello();
            assert_eq!(provider.complete(&request(&messages)), Err(expected));
        }
    }

    const OVERLOADED: &str =
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
    const OK: &str = r#"{"content":[{"type":"text","text":"Hi"}],"stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":1}}"#;

    fn retrying(url: &str) -> AnthropicProvider {
        let mut provider =
            AnthropicProvider::with_http(Http::insecure_for_tests(), Some(url), Secret::new("k"));
        provider.again = Again::at_once(2);
        provider
    }

    #[test]
    fn overload_and_rate_limits_are_retried_before_the_route_fails_over() {
        let (url, got) = fake::serve_headed(vec![
            (529, "", OVERLOADED),
            (
                429,
                "retry-after: 0\r\n",
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
            ),
            (200, "", OK),
        ]);
        let messages = hello();
        let reply = retrying(&url).complete(&request(&messages)).unwrap();
        assert_eq!(reply.text, "Hi");
        assert_eq!(got.iter().take(3).count(), 3, "two retries, then the reply");

        // Still overloaded after the retries: an outage the route fails
        // over from.
        let (url, got) = fake::serve_each(vec![(529, OVERLOADED); 3]);
        let err = retrying(&url).complete(&request(&messages)).unwrap_err();
        assert_eq!(
            err,
            ProviderError::Unavailable("HTTP 529: Overloaded".into())
        );
        assert!(err.fails_over());
        assert_eq!(got.iter().take(3).count(), 3);

        // What isn't worth retrying isn't.
        let (url, got) = fake::serve(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#,
        );
        assert_eq!(
            retrying(&url).complete(&request(&messages)),
            Err(ProviderError::Rejected("HTTP 400: bad".into()))
        );
        assert_eq!(got.iter().count(), 1);
    }

    #[test]
    fn used_up_credit_fails_over_and_says_so() {
        // What the API sent a key with no credit left (2026-10-22), and
        // its documented billing error.
        for (status, body) in [
            (
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."}}"#,
            ),
            (
                402,
                r#"{"type":"error","error":{"type":"billing_error","message":"There is a problem with your organization's billing."}}"#,
            ),
        ] {
            let (url, _got) = fake::serve(status, body);
            let messages = hello();
            let err = retrying(&url).complete(&request(&messages)).unwrap_err();
            assert!(err.fails_over(), "{err}");
            assert!(
                err.to_string().contains(&format!(
                    "HTTP {status}: the credits for this key are used up"
                )),
                "{err}"
            );
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
        // A relay is plain HTTP to this machine only.
        assert!(AnthropicProvider::relayed("http://127.0.0.1:8787", Secret::new("k")).is_some());
        assert!(AnthropicProvider::relayed("http://api.example.com", Secret::new("k")).is_none());
        assert!(AnthropicProvider::relayed("https://127.0.0.1:8787", Secret::new("k")).is_none());
    }
}
