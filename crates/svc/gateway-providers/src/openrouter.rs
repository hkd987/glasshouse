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

use crate::http::{Again, Http, classify};

/// The public endpoint, unless policy names another.
pub const DEFAULT_ENDPOINT: &str = "https://openrouter.ai/api";
/// The router that picks a model for each call.
pub const AUTO: &str = "openrouter/auto";

#[derive(Debug)]
pub struct OpenRouterProvider {
    http: Http,
    endpoint: String,
    key: Secret,
    /// When a call is sent again (at once in tests).
    again: Again,
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
    /// Its `content` and any `tool_calls` (ADR 0041).
    message: serde_json::Value,
    #[serde(default)]
    finish_reason: Option<String>,
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
            again: Again::usual(),
        }
    }
}

impl ModelProvider for OpenRouterProvider {
    fn complete(&self, request: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError> {
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "messages": crate::tools::openai_messages(request.system, request.messages),
            "provider": { "zdr": true, "data_collection": "deny" },
        });
        if !request.tools.is_empty() {
            body["tools"] = crate::tools::openai_tools(request.tools);
        }
        let bearer = format!("Bearer {}", self.key.expose());
        // Sent again as every model client's calls are ([`Again`]): one
        // 429 or 503 used to fail over at once, or end the step.
        let reply = self.http.post_again(
            &format!("{}/v1/chat/completions", self.endpoint),
            &[
                ("authorization", bearer.as_str()),
                ("content-type", "application/json"),
                ("x-title", "Glasshouse"),
            ],
            body.to_string().as_bytes(),
            &self.again,
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
        let stop_reason =
            crate::tools::stop_reason(choice.as_ref().and_then(|c| c.finish_reason.clone()));
        let (text, tool_calls) = choice.map_or_else(Default::default, |c| {
            (
                c.message["content"].as_str().unwrap_or_default().to_owned(),
                crate::tools::openai_calls(&c.message),
            )
        });
        Ok(ProviderReply {
            text,
            tool_calls,
            input_tokens: response.usage.prompt_tokens,
            output_tokens: response.usage.completion_tokens,
            stop_reason,
            // Micro-dollars are plenty, as the gateway's own estimates.
            cost_usd: response
                .usage
                .cost
                .map(|c| (c * 1_000_000.0).round() / 1_000_000.0),
            served_by: response.model,
            blocks: Vec::new(),
            ..ProviderReply::default()
        })
    }
}

/// What OpenRouter says about a key: its credit limit, what is left of
/// it and what it has spent, in dollars.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct KeyUsage {
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub limit_remaining: Option<f64>,
    #[serde(default)]
    pub usage: f64,
    #[serde(default)]
    pub usage_monthly: f64,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Deserialize)]
struct KeyReply {
    data: KeyData,
    /// The key itself: only when it is made, and only that once.
    #[serde(default)]
    key: Option<String>,
}

#[derive(Deserialize)]
struct KeyData {
    hash: String,
    #[serde(flatten)]
    usage: KeyUsage,
}

/// Our OpenRouter account, in OpenRouter's dollars (ADR 0039).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Account {
    /// What's left of the credits we bought.
    pub balance: f64,
    /// What every org's key may still spend: the sum of their limits'
    /// remainders, which `balance` must cover.
    pub outstanding: f64,
}

impl Account {
    /// Whether every org could spend all it has left.
    pub fn covered(&self) -> bool {
        self.balance >= self.outstanding
    }
}

/// A key OpenRouter made: its id, for managing it, and the key itself,
/// which it shows only this once.
pub struct MadeKey {
    pub hash: String,
    pub key: Secret,
    pub usage: KeyUsage,
}

impl std::fmt::Debug for MadeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MadeKey")
            .field("hash", &self.hash)
            .field("usage", &self.usage)
            .finish_non_exhaustive()
    }
}

/// Our management key's side of OpenRouter (ADR 0039): it makes each
/// org's key with a credit limit, raises the limit when the org buys
/// more, and reads what the key has spent. It runs in the console, on our
/// servers; computers never hold a management key.
#[derive(Debug)]
pub struct OpenRouterKeys {
    http: Http,
    endpoint: String,
    management: Secret,
}

impl OpenRouterKeys {
    pub fn new(endpoint: Option<&str>, management: &str) -> Self {
        Self::with_http(Http::new().quick(), endpoint, Secret::new(management))
    }

    fn with_http(http: Http, endpoint: Option<&str>, management: Secret) -> Self {
        Self {
            http,
            endpoint: endpoint
                .unwrap_or(DEFAULT_ENDPOINT)
                .trim_end_matches('/')
                .to_owned(),
            management,
        }
    }

    /// `method` on `path` under the API, with our management key: the
    /// reply's body when it's 2xx.
    fn send(&self, method: &str, path: &str, body: &serde_json::Value) -> Result<String, String> {
        let bearer = format!("Bearer {}", self.management.expose());
        let body = if method == "GET" {
            Vec::new()
        } else {
            body.to_string().into_bytes()
        };
        let reply = self
            .http
            .send(
                method,
                &format!("{}{path}", self.endpoint),
                &[
                    ("authorization", bearer.as_str()),
                    ("content-type", "application/json"),
                ],
                &body,
            )
            .map_err(|e| format!("OpenRouter {e}"))?;
        if !(200..300).contains(&reply.status) {
            let message = serde_json::from_str::<ErrorBody>(&reply.body)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| "no details".into());
            return Err(format!("OpenRouter said HTTP {}: {message}", reply.status));
        }
        Ok(reply.body)
    }

    fn call(&self, method: &str, path: &str, body: &serde_json::Value) -> Result<KeyReply, String> {
        let reply = self.send(method, &format!("/v1/keys{path}"), body)?;
        serde_json::from_str(&reply).map_err(|e| format!("OpenRouter's reply: {e}"))
    }

    /// Our account (ADR 0039): what's left of the credits we bought, and
    /// what every org's key may still spend, which it must cover.
    pub fn account(&self) -> Result<Account, String> {
        #[derive(Deserialize)]
        struct Credits {
            data: Totals,
        }
        #[derive(Deserialize)]
        struct Totals {
            total_credits: f64,
            total_usage: f64,
        }
        #[derive(Deserialize)]
        struct Keys {
            data: Vec<KeyUsage>,
        }
        let credits: Credits =
            serde_json::from_str(&self.send("GET", "/v1/credits", &json!({}))?)
                .map_err(|e| format!("OpenRouter's credits: {e}"))?;
        let mut outstanding = 0.0;
        let mut offset = 0;
        // A page at a time, as OpenRouter lists keys; never forever.
        for _ in 0..100 {
            let page: Keys = serde_json::from_str(&self.send(
                "GET",
                &format!("/v1/keys?offset={offset}"),
                &json!({}),
            )?)
            .map_err(|e| format!("OpenRouter's keys: {e}"))?;
            if page.data.is_empty() {
                break;
            }
            offset += page.data.len();
            outstanding += page
                .data
                .iter()
                .filter(|k| !k.disabled)
                .filter_map(|k| {
                    k.limit_remaining
                        .or_else(|| k.limit.map(|l| (l - k.usage).max(0.0)))
                })
                .sum::<f64>();
        }
        Ok(Account {
            balance: credits.data.total_credits - credits.data.total_usage,
            outstanding,
        })
    }

    /// Makes a key named `name` that may spend `limit` dollars.
    pub fn create(&self, name: &str, limit: f64) -> Result<MadeKey, String> {
        let reply = self.call("POST", "", &json!({ "name": name, "limit": limit }))?;
        let key = reply
            .key
            .ok_or("OpenRouter made the key but didn't send it")?;
        Ok(MadeKey {
            hash: reply.data.hash,
            key: Secret::new(key),
            usage: reply.data.usage,
        })
    }

    /// Sets what the key `hash` may spend in all, in dollars.
    pub fn set_limit(&self, hash: &str, limit: f64) -> Result<KeyUsage, String> {
        Ok(self
            .call(
                "PATCH",
                &format!("/{}", path_id(hash)?),
                &json!({ "limit": limit }),
            )?
            .data
            .usage)
    }

    /// Turns the key `hash` off, as when it's replaced.
    pub fn disable(&self, hash: &str) -> Result<KeyUsage, String> {
        Ok(self
            .call(
                "PATCH",
                &format!("/{}", path_id(hash)?),
                &json!({ "disabled": true }),
            )?
            .data
            .usage)
    }

    /// What the key `hash` has spent, and what is left of its limit.
    pub fn usage(&self, hash: &str) -> Result<KeyUsage, String> {
        Ok(self
            .call("GET", &format!("/{}", path_id(hash)?), &json!({}))?
            .data
            .usage)
    }
}

/// A key's hash as a path segment: hex and nothing else.
fn path_id(hash: &str) -> Result<&str, String> {
    if !hash.is_empty() && hash.len() <= 128 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(hash)
    } else {
        Err(format!("{hash:?} isn't a key's hash"))
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
            tools: &[],
        }
    }

    fn hello() -> Vec<Message> {
        vec![Message::text(Role::User, "Hello [redacted:email#1]")]
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
            let mut provider = OpenRouterProvider::with_http(
                Http::insecure_for_tests(),
                Some(&url),
                Secret::new("k"),
            );
            // What a status means, once its further tries are spent.
            provider.again = Again::never();
            let messages = hello();
            let got = provider.complete(&request(&messages));
            assert_eq!(got, Err(expected.clone()));
            assert_eq!(got.unwrap_err().fails_over(), expected.fails_over());
        }
    }

    // Audit 3.6: one rate-limited call failed over at once, or ended the
    // step, where the same from Anthropic was tried again.
    #[test]
    fn a_rate_limited_call_is_sent_again_and_used_up_credits_are_not() {
        const SLOW_DOWN: &str = r#"{"error":{"code":429,"message":"Rate limited"}}"#;
        const OK: &str = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
        let again = |url: &str| {
            let mut provider = OpenRouterProvider::with_http(
                Http::insecure_for_tests(),
                Some(url),
                Secret::new("k"),
            );
            provider.again = Again::at_once(2);
            provider
        };
        let (url, got) = fake::serve_each(vec![(429, SLOW_DOWN), (502, SLOW_DOWN), (200, OK)]);
        let messages = hello();
        let reply = again(&url).complete(&request(&messages)).unwrap();
        assert_eq!(reply.text, "ok");
        assert_eq!(got.iter().take(3).count(), 3, "twice more, then the reply");

        // No credit left: asking again changes nothing, and the next
        // model on the route may still answer.
        let (url, got) = fake::serve(
            402,
            r#"{"error":{"code":402,"message":"Insufficient credits"}}"#,
        );
        let err = again(&url).complete(&request(&messages)).unwrap_err();
        assert!(err.fails_over(), "{err}");
        assert_eq!(got.iter().count(), 1, "asked once");
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

    #[test]
    fn adr0039_the_console_makes_an_orgs_key_with_a_limit_and_reads_what_it_spent() {
        let made = r#"{"data":{"hash":"abc123","name":"glasshouse-acme","label":"sk-or-v1-0e6...1c96","disabled":false,"limit":50,"limit_remaining":50,"usage":0,"usage_monthly":0},"key":"sk-or-v1-made-once"}"#;
        let (url, got) = fake::serve(201, made);
        let keys = OpenRouterKeys::with_http(
            Http::insecure_for_tests(),
            Some(&url),
            Secret::new("mgmt-not-real"),
        );
        let key = keys.create("glasshouse-acme", 50.0).unwrap();
        assert_eq!(key.hash, "abc123");
        assert_eq!(key.key.expose(), "sk-or-v1-made-once");
        assert_eq!(key.usage.limit_remaining, Some(50.0));
        assert!(!format!("{key:?}").contains("made-once"));
        let sent = got.recv().unwrap();
        assert!(
            sent.head.starts_with("POST /v1/keys HTTP/1.1"),
            "{}",
            sent.head
        );
        assert!(
            sent.head
                .to_ascii_lowercase()
                .contains("authorization: bearer mgmt-not-real")
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&sent.body).unwrap(),
            json!({ "name": "glasshouse-acme", "limit": 50.0 })
        );

        let raised = r#"{"data":{"hash":"abc123","limit":150,"limit_remaining":137.5,"usage":12.5,"usage_monthly":12.5,"disabled":false}}"#;
        let (url, got) = fake::serve(200, raised);
        let keys =
            OpenRouterKeys::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("m"));
        let usage = keys.set_limit("abc123", 150.0).unwrap();
        assert_eq!(
            (usage.limit, usage.limit_remaining, usage.usage),
            (Some(150.0), Some(137.5), 12.5)
        );
        let sent = got.recv().unwrap();
        assert!(
            sent.head.starts_with("PATCH /v1/keys/abc123 HTTP/1.1"),
            "{}",
            sent.head
        );

        let (url, got) = fake::serve(200, raised);
        let keys =
            OpenRouterKeys::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("m"));
        assert_eq!(keys.usage("abc123").unwrap().usage, 12.5);
        assert!(
            got.recv()
                .unwrap()
                .head
                .starts_with("GET /v1/keys/abc123 HTTP/1.1")
        );

        // Only a hash goes in the path; errors say what OpenRouter said.
        assert!(keys.usage("../credits").is_err());
        let (url, _got) = fake::serve(
            401,
            r#"{"error":{"code":401,"message":"No auth credentials found"}}"#,
        );
        let keys =
            OpenRouterKeys::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("m"));
        let err = keys.usage("abc123").unwrap_err();
        assert!(
            err.contains("401") && err.contains("No auth credentials"),
            "{err}"
        );
    }

    #[test]
    fn adr0039_our_balance_is_checked_against_every_orgs_unspent_limit() {
        let (url, got) = fake::serve_each(vec![
            (
                200,
                r#"{"data":{"total_credits":500.0,"total_usage":120.5}}"#,
            ),
            (
                200,
                r#"{"data":[{"hash":"a","limit":100.0,"limit_remaining":60.0,"usage":40.0},{"hash":"b","limit":300.0,"usage":10.0},{"hash":"c","limit":50.0,"limit_remaining":50.0,"usage":0.0,"disabled":true},{"hash":"d","usage":3.0}]}"#,
            ),
            (200, r#"{"data":[]}"#),
        ]);
        let keys =
            OpenRouterKeys::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("m"));
        let account = keys.account().unwrap();
        // 60 left on a, 290 on b; c is off and d has no limit.
        assert_eq!(account.balance, 379.5);
        assert_eq!(account.outstanding, 350.0);
        assert!(account.covered());
        assert!(
            got.recv()
                .unwrap()
                .head
                .starts_with("GET /v1/credits HTTP/1.1")
        );
        assert!(
            got.recv()
                .unwrap()
                .head
                .starts_with("GET /v1/keys?offset=0 HTTP/1.1")
        );
        assert!(
            got.recv()
                .unwrap()
                .head
                .starts_with("GET /v1/keys?offset=4 HTTP/1.1")
        );
        assert!(
            !Account {
                balance: 10.0,
                outstanding: 10.01
            }
            .covered()
        );
    }
}
