//! The Glasshouse Agents API from a person's own computer (ADR 0066): what
//! the `glass` CLI calls. HTTPS to the company's stack, or plain HTTP to a
//! front on this machine's loopback address in development.

use crate::http::Http;
use glass_gateway::ProviderError;

/// The front at `url`, called with `token`.
#[derive(Debug)]
pub struct AgentsClient {
    http: Http,
    url: String,
    token: String,
}

/// What the front answered: its status and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub body: String,
}

impl AgentsClient {
    /// `url` is `https://agents.example.com`, or `http://127.0.0.1:<port>`
    /// for a front on this machine; nothing else goes over plain HTTP.
    pub fn new(url: &str, token: &str) -> Result<Self, ProviderError> {
        let url = url.trim_end_matches('/');
        let http = if url.starts_with("https://") {
            Http::new()
        } else if url.starts_with("http://127.0.0.1:")
            || url.starts_with("http://localhost:")
            || url.starts_with("http://[::1]:")
        {
            Http::loopback()
        } else {
            return Err(ProviderError::Rejected(format!(
                "{url} isn't HTTPS: Glasshouse Agents is only reached over HTTPS"
            )));
        };
        Ok(Self {
            http: http.outwaiting_providers(),
            url: url.to_owned(),
            token: token.to_owned(),
        })
    }

    /// `GET /api/v1/<path>`.
    pub fn get(&self, path: &str) -> Result<Answer, ProviderError> {
        self.call("GET", path, b"")
    }

    /// `POST /api/v1/<path>` with a JSON body.
    pub fn post(&self, path: &str, body: &serde_json::Value) -> Result<Answer, ProviderError> {
        self.call("POST", path, body.to_string().as_bytes())
    }

    fn call(&self, method: &str, path: &str, body: &[u8]) -> Result<Answer, ProviderError> {
        let url = format!("{}/api/v1/{}", self.url, path.trim_start_matches('/'));
        let auth = format!("Bearer {}", self.token);
        let mut headers = vec![
            ("Authorization", auth.as_str()),
            ("Accept", "application/json"),
        ];
        if method == "POST" {
            headers.push(("Content-Type", "application/json"));
        }
        let reply = self.http.send(method, &url, &headers, body)?;
        Ok(Answer {
            status: reply.status,
            body: reply.body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::serve_each;

    #[test]
    fn adr0066_the_cli_calls_the_front_with_its_token() {
        let (url, requests) = serve_each(vec![(200, r#"[]"#), (200, r#"{"task":"t"}"#)]);
        let client = AgentsClient::new(&url, "secret-token").unwrap();
        let got = client.get("agents").unwrap();
        assert_eq!(
            got,
            Answer {
                status: 200,
                body: "[]".into()
            }
        );
        let asked = client
            .post(
                "agents/reconciler/ask",
                &serde_json::json!({"text": "Match March"}),
            )
            .unwrap();
        assert_eq!(asked.body, r#"{"task":"t"}"#);

        let first = requests.recv().unwrap();
        assert!(
            first.head.starts_with("GET /api/v1/agents HTTP/1.1"),
            "{}",
            first.head
        );
        assert!(first.head.contains("Authorization: Bearer secret-token"));
        let second = requests.recv().unwrap();
        assert!(
            second
                .head
                .starts_with("POST /api/v1/agents/reconciler/ask")
        );
        assert!(second.head.contains("Content-Type: application/json"));
        assert_eq!(second.body, r#"{"text":"Match March"}"#);
    }

    #[test]
    fn adr0066_only_https_leaves_this_machine() {
        assert!(AgentsClient::new("http://agents.example.com", "t").is_err());
        assert!(AgentsClient::new("https://agents.example.com", "t").is_ok());
        assert!(AgentsClient::new("http://127.0.0.1:7500", "t").is_ok());
    }
}
