//! Amazon Bedrock in the org's AWS account (MP-1, MP-2), through the
//! provider-neutral Converse API, signed with SigV4.
//!
//! Credentials must be temporary (an access key id starting `ASIA`, with a
//! session token), as IAM Identity Center or an assumed role issues them: no
//! long-lived AWS keys on a device (MP-1). Until the secrets broker fetches
//! and refreshes them, something with the right to do so writes them to the
//! secrets file; expired ones are refused, and the route fails over.

use glass_gateway::{
    ModelProvider, ProviderError, ProviderReply, ProviderRequest, Secret, Secrets,
};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::http::{Http, classify};
use crate::sigv4::{self, Credentials, uri_encode};

/// Temporary AWS credentials (MP-1).
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    access_key_id: Secret,
    secret_access_key: Secret,
    session_token: Secret,
    expires: Option<OffsetDateTime>,
}

impl AwsCredentials {
    /// Reads `[<provider>]` from the secrets file: `access_key_id`,
    /// `secret_access_key`, `session_token` and optionally `expires`.
    pub fn from_secrets(secrets: &Secrets, provider: &str) -> Result<Self, String> {
        let get = |name: &str| {
            secrets
                .get(provider, name)
                .cloned()
                .ok_or_else(|| format!("no {name} in the secrets file"))
        };
        let access_key_id = get("access_key_id")?;
        if access_key_id.expose().starts_with("AKIA") {
            return Err(
                "long-lived AWS access keys are refused (MP-1); use temporary credentials from \
                 IAM Identity Center or a role"
                    .into(),
            );
        }
        let session_token = get("session_token")
            .map_err(|_| "temporary AWS credentials need a session_token (MP-1)".to_owned())?;
        let expires = match secrets.get(provider, "expires") {
            Some(when) => Some(
                OffsetDateTime::parse(when.expose(), &Rfc3339)
                    .map_err(|_| "expires must be an RFC 3339 time".to_owned())?,
            ),
            None => None,
        };
        Ok(Self {
            access_key_id,
            secret_access_key: get("secret_access_key")?,
            session_token,
            expires,
        })
    }

    fn check(&self, now: OffsetDateTime) -> Result<(), ProviderError> {
        match self.expires {
            Some(at) if at <= now => Err(ProviderError::NotConfigured(format!(
                "its temporary AWS credentials expired at {}",
                at.format(&Rfc3339).unwrap_or_default()
            ))),
            _ => Ok(()),
        }
    }
}

#[derive(Debug)]
pub struct BedrockProvider {
    http: Http,
    region: String,
    /// Scheme and host: the regional endpoint or a PrivateLink one (MP-2).
    base: String,
    host: String,
    credentials: AwsCredentials,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    output: Output,
    #[serde(default)]
    stop_reason: Option<String>,
    usage: Usage,
}

#[derive(Deserialize)]
struct Output {
    message: OutMessage,
}

#[derive(Deserialize)]
struct OutMessage {
    #[serde(default)]
    content: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

impl BedrockProvider {
    pub fn new(region: &str, endpoint: Option<&str>, credentials: AwsCredentials) -> Self {
        let base = endpoint.map_or_else(
            || format!("https://bedrock-runtime.{region}.amazonaws.com"),
            |e| e.trim_end_matches('/').to_owned(),
        );
        Self::with_http(Http::new(), region, &base, credentials)
    }

    fn with_http(http: Http, region: &str, base: &str, credentials: AwsCredentials) -> Self {
        let host = base
            .split_once("://")
            .map_or(base, |(_, rest)| rest)
            .split('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        Self {
            http,
            region: region.to_owned(),
            base: base.split('/').take(3).collect::<Vec<_>>().join("/"),
            host,
            credentials,
        }
    }

    fn send(
        &self,
        request: &ProviderRequest<'_>,
        now: OffsetDateTime,
    ) -> Result<ProviderReply, ProviderError> {
        self.credentials.check(now)?;
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|m| json!({ "role": m.role, "content": [{ "text": m.content }] }))
            .collect();
        let mut body = json!({
            "messages": messages,
            "inferenceConfig": { "maxTokens": request.max_tokens },
        });
        if let Some(system) = request.system {
            body["system"] = json!([{ "text": system }]);
        }
        let body = body.to_string();
        let segment = uri_encode(request.model);
        let canonical = format!("/model/{}/converse", uri_encode(&segment));
        let headers = [
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ];
        let signed = sigv4::sign(
            &sigv4::Request {
                method: "POST",
                host: &self.host,
                canonical_path: &canonical,
                query: "",
                headers: &headers,
                body: body.as_bytes(),
            },
            &Credentials {
                access_key_id: self.credentials.access_key_id.expose(),
                secret_access_key: self.credentials.secret_access_key.expose(),
                session_token: Some(self.credentials.session_token.expose()),
            },
            &self.region,
            "bedrock",
            now,
        );
        let mut all: Vec<(&str, &str)> = headers.to_vec();
        all.extend(signed.iter().map(|(k, v)| (*k, v.as_str())));
        let reply = self.http.post(
            &format!("{}/model/{segment}/converse", self.base),
            &all,
            body.as_bytes(),
        )?;
        if reply.status != 200 {
            let message = serde_json::from_str::<Value>(&reply.body)
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .or_else(|| v.get("Message"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "no details".into());
            return Err(classify(reply.status, message));
        }
        let response: Response = serde_json::from_str(&reply.body)
            .map_err(|e| ProviderError::Unavailable(format!("unreadable reply: {e}")))?;
        let text = response
            .output
            .message
            .content
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
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

impl ModelProvider for BedrockProvider {
    fn complete(&self, request: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError> {
        self.send(request, OffsetDateTime::now_utc())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use glass_gateway::{Message, Role};
    use time::macros::datetime;

    use super::*;
    use crate::http::fake;

    // AWS's documentation example secret; the rest are placeholders.
    fn secrets(key_id: &str, extra: &str) -> Secrets {
        Secrets::parse(&format!(
            "[bedrock]\naccess_key_id = \"{key_id}\"\n\
             secret_access_key = \"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY\"\n{extra}"
        ))
        .unwrap()
    }

    fn temporary() -> AwsCredentials {
        AwsCredentials::from_secrets(
            &secrets(
                "ASIAEXAMPLE",
                "session_token = \"token-example\"\nexpires = \"2026-10-19T00:00:00Z\"\n",
            ),
            "bedrock",
        )
        .unwrap()
    }

    #[test]
    fn mp1_only_temporary_credentials_are_used() {
        let long_lived = AwsCredentials::from_secrets(
            &secrets("AKIAIOSFODNN7EXAMPLE", "session_token = \"t\"\n"),
            "bedrock",
        )
        .unwrap_err();
        assert!(
            long_lived.contains("long-lived AWS access keys are refused"),
            "{long_lived}"
        );
        let no_token =
            AwsCredentials::from_secrets(&secrets("ASIAEXAMPLE", ""), "bedrock").unwrap_err();
        assert!(no_token.contains("session_token"), "{no_token}");
        assert!(AwsCredentials::from_secrets(&Secrets::default(), "bedrock").is_err());

        let creds = temporary();
        assert!(creds.check(datetime!(2026-10-18 22:00 UTC)).is_ok());
        let expired = creds.check(datetime!(2026-10-19 00:00 UTC)).unwrap_err();
        assert!(
            expired.fails_over(),
            "another model on the route can take it"
        );
        assert!(
            expired
                .to_string()
                .contains("expired at 2026-10-19T00:00:00Z")
        );
        assert!(!format!("{creds:?}").contains("EXAMPLEKEY"));
    }

    #[test]
    fn mp2_sends_a_signed_converse_request_and_reads_the_reply() {
        let (url, got) = fake::serve(
            200,
            r#"{"output":{"message":{"role":"assistant","content":[{"text":"Hi"},{"text":" there"}]}},"stopReason":"end_turn","usage":{"inputTokens":9,"outputTokens":2,"totalTokens":11}}"#,
        );
        let provider =
            BedrockProvider::with_http(Http::insecure_for_tests(), "us-east-1", &url, temporary());
        let messages = vec![Message {
            role: Role::User,
            content: "Hello".into(),
        }];
        let reply = provider
            .send(
                &ProviderRequest {
                    model: "us.anthropic.claude-test-v1:0",
                    system: Some("Be brief."),
                    messages: &messages,
                    max_tokens: 64,
                },
                datetime!(2026-10-18 22:00 UTC),
            )
            .unwrap();
        assert_eq!(reply.text, "Hi there");
        assert_eq!((reply.input_tokens, reply.output_tokens), (9, 2));

        let sent = got.recv().unwrap();
        assert!(
            sent.head
                .starts_with("POST /model/us.anthropic.claude-test-v1%3A0/converse HTTP/1.1"),
            "{}",
            sent.head
        );
        let head = sent.head.to_ascii_lowercase();
        assert!(head.contains(
            "authorization: aws4-hmac-sha256 credential=asiaexample/20261018/us-east-1/bedrock/aws4_request"
        ), "{head}");
        assert!(head.contains("x-amz-security-token: token-example"));
        assert!(head.contains("x-amz-date: 20261018t220000z"));
        let body: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(
            body,
            json!({
                "messages": [{"role": "user", "content": [{"text": "Hello"}]}],
                "system": [{"text": "Be brief."}],
                "inferenceConfig": {"maxTokens": 64}
            })
        );
    }

    #[test]
    fn throttling_fails_over_and_validation_errors_stop() {
        for (status, fails_over) in [(429, true), (503, true), (403, true), (400, false)] {
            let (url, _got) = fake::serve(status, r#"{"message":"nope"}"#);
            let provider = BedrockProvider::with_http(
                Http::insecure_for_tests(),
                "us-east-1",
                &url,
                temporary(),
            );
            let messages = vec![Message {
                role: Role::User,
                content: "Hello".into(),
            }];
            let err = provider
                .send(
                    &ProviderRequest {
                        model: "m",
                        system: None,
                        messages: &messages,
                        max_tokens: 8,
                    },
                    datetime!(2026-10-18 22:00 UTC),
                )
                .unwrap_err();
            assert_eq!(err.fails_over(), fails_over, "{status}: {err}");
            assert!(err.to_string().contains("nope"));
        }
    }

    #[test]
    fn the_endpoint_defaults_to_the_region() {
        let provider = BedrockProvider::new("eu-central-1", None, temporary());
        assert_eq!(
            provider.base,
            "https://bedrock-runtime.eu-central-1.amazonaws.com"
        );
        assert_eq!(provider.host, "bedrock-runtime.eu-central-1.amazonaws.com");
        let private = BedrockProvider::new(
            "us-east-1",
            Some("https://vpce-0abc.bedrock-runtime.us-east-1.vpce.amazonaws.com/"),
            temporary(),
        );
        assert_eq!(
            private.host,
            "vpce-0abc.bedrock-runtime.us-east-1.vpce.amazonaws.com"
        );
    }
}
