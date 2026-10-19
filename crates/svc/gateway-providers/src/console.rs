//! The device's side of managed settings (ADR 0023): the one way glassd
//! reaches its org's admin console. Releases it brings back are verified
//! and validated by glassd, never trusted for arriving here.
//!
//! HTTPS only, through the environment's proxy like the model providers,
//! except plain HTTP to a loopback address when the config allows it
//! (development and tests): the org's signature, not the channel, is what
//! a device trusts. The enrollment token is sent as a bearer token and
//! never appears in an error.

use glass_gateway::ProviderError;
use glass_managed::{DeviceReport, SyncRequest, SyncResponse};

use crate::http::{Http, Target};

/// A client for one console.
#[derive(Debug)]
pub struct ConsoleClient {
    http: Http,
    base: String,
    token: String,
}

impl ConsoleClient {
    /// A client for the console at `url`. Plain `http://` is refused unless
    /// `allow_insecure_http` is set and the host is a loopback address.
    pub fn new(url: &str, token: &str, allow_insecure_http: bool) -> Result<Self, String> {
        let base = url.trim_end_matches('/').to_owned();
        let target = Target::parse(&format!("{base}/"))
            .ok_or_else(|| format!("the console address {url:?} is not an http(s) URL"))?;
        let http = if target.tls {
            Http::new()
        } else if allow_insecure_http && is_loopback(&target.host) {
            Http::loopback()
        } else {
            return Err(format!(
                "the console address {url:?} must use HTTPS (plain HTTP only to this machine, \
                 with allow_insecure_http, for development)"
            ));
        };
        if token.is_empty() || token.contains(['\r', '\n']) {
            return Err("the enrollment token is empty or has a line break".into());
        }
        Ok(Self {
            http,
            base,
            token: token.to_owned(),
        })
    }

    /// Asks for releases newer than what the device has in force, waiting
    /// up to `request.wait_secs` for one.
    pub fn sync(&self, request: &SyncRequest) -> Result<SyncResponse, String> {
        let reply = self.post("/device/v1/sync", request)?;
        serde_json::from_str(&reply).map_err(|e| format!("the console's answer doesn't parse: {e}"))
    }

    /// Tells the console how this device is doing (AD-4).
    pub fn report(&self, report: &DeviceReport) -> Result<(), String> {
        self.post("/device/v1/report", report).map(drop)
    }

    fn post<T: serde::Serialize>(&self, path: &str, body: &T) -> Result<String, String> {
        let body = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        let auth = format!("Bearer {}", self.token);
        let reply = self
            .http
            .post(
                &format!("{}{path}", self.base),
                &[
                    ("Authorization", auth.as_str()),
                    ("Content-Type", "application/json"),
                ],
                &body,
            )
            .map_err(|e| match e {
                ProviderError::Unavailable(m)
                | ProviderError::NotConfigured(m)
                | ProviderError::Rejected(m) => m,
            })?;
        match reply.status {
            200 => Ok(reply.body),
            401 | 403 => Err("the console didn't accept this device's enrollment".into()),
            status => Err(format!("the console answered {status}")),
        }
    }
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake;

    #[test]
    fn plain_http_only_to_this_machine_and_only_when_allowed() {
        assert!(ConsoleClient::new("http://127.0.0.1:7400", "t", true).is_ok());
        assert!(ConsoleClient::new("http://localhost:7400/", "t", true).is_ok());
        assert!(ConsoleClient::new("http://127.0.0.1:7400", "t", false).is_err());
        assert!(ConsoleClient::new("http://console.example:80", "t", true).is_err());
        assert!(ConsoleClient::new("https://console.example", "t", false).is_ok());
        assert!(ConsoleClient::new("ftp://x", "t", true).is_err());
        assert!(ConsoleClient::new("https://console.example", "a\nb", false).is_err());
    }

    #[test]
    fn a_sync_posts_the_versions_with_the_token_and_reads_the_answer() {
        let (url, got) = fake::serve(200, r#"{"ring":"pilot","policy":null,"catalog":null}"#);
        let client = ConsoleClient::new(&url, "enroll-123", true).unwrap();
        let answer = client
            .sync(&SyncRequest {
                device: "dev-1".into(),
                policy: Some(3),
                catalog: None,
                wait_secs: 0,
            })
            .unwrap();
        assert_eq!(answer.ring, glass_managed::Ring::Pilot);
        let request = got.recv().unwrap();
        assert!(
            request.head.starts_with("POST /device/v1/sync "),
            "{}",
            request.head
        );
        assert!(request.head.contains("Authorization: Bearer enroll-123"));
        assert!(request.body.contains("\"policy\":3"), "{}", request.body);
    }

    #[test]
    fn a_refused_enrollment_says_so_without_the_token() {
        let (url, _got) = fake::serve(401, "{}");
        let client = ConsoleClient::new(&url, "secret-token", true).unwrap();
        let err = client
            .sync(&SyncRequest {
                device: "d".into(),
                policy: None,
                catalog: None,
                wait_secs: 0,
            })
            .unwrap_err();
        assert!(err.contains("enrollment"), "{err}");
        assert!(!err.contains("secret-token"));
    }
}
