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
use glass_managed::chat::{
    ChatLink, ChatSyncRequest, ChatSyncResponse, CreateConversation, Keys, LinkError, LookupKeys,
    PostMessage, RegisterKey,
};
use glass_managed::meet::{Joined, MediaKeys, MeetLink, MeetRequest};
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
        self.request(path, body).map_err(|e| e.to_string())
    }

    /// Posts `body` to `path`: the answer's text, or why not, with the
    /// console's own reason when it refused the request.
    fn request<T: serde::Serialize>(&self, path: &str, body: &T) -> Result<String, LinkError> {
        let body = serde_json::to_vec(body).map_err(|e| LinkError::Unreachable(e.to_string()))?;
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
                | ProviderError::Rejected(m) => LinkError::Unreachable(m),
            })?;
        match reply.status {
            200 => Ok(reply.body),
            401 | 403 => Err(LinkError::Unreachable(
                "the console didn't accept this device's enrollment".into(),
            )),
            400 | 404 | 409 => Err(LinkError::Refused(
                serde_json::from_str::<serde_json::Value>(&reply.body)
                    .ok()
                    .and_then(|v| v["error"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("the console answered {}", reply.status)),
            )),
            status => Err(LinkError::Unreachable(format!(
                "the console answered {status}"
            ))),
        }
    }

    fn answer<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, LinkError> {
        serde_json::from_str(text)
            .map_err(|e| LinkError::Unreachable(format!("the console's answer doesn't parse: {e}")))
    }

    fn seq(text: &str) -> Result<u64, LinkError> {
        Self::answer::<serde_json::Value>(text)?["seq"]
            .as_u64()
            .ok_or_else(|| LinkError::Unreachable("the console's answer has no seq".into()))
    }
}

/// Chat's device routes (ADR 0030): what glassd's chat store sends and
/// fetches, all of it sealed before it gets here.
impl ChatLink for ConsoleClient {
    fn register(&self, request: &RegisterKey) -> Result<(), LinkError> {
        self.request("/device/v1/chat/keys", request).map(drop)
    }

    fn lookup(&self, request: &LookupKeys) -> Result<Keys, LinkError> {
        Self::answer(&self.request("/device/v1/chat/keys/lookup", request)?)
    }

    fn create(&self, request: &CreateConversation) -> Result<u64, LinkError> {
        Self::seq(&self.request("/device/v1/chat/conversations", request)?)
    }

    fn post(&self, request: &PostMessage) -> Result<u64, LinkError> {
        Self::seq(&self.request("/device/v1/chat/messages", request)?)
    }

    fn rotate(&self, request: &glass_managed::chat::RotateEpoch) -> Result<u64, LinkError> {
        Self::seq(&self.request("/device/v1/chat/rotate", request)?)
    }

    fn sync(&self, request: &ChatSyncRequest) -> Result<ChatSyncResponse, LinkError> {
        Self::answer(&self.request("/device/v1/chat/sync", request)?)
    }
}

/// Meetings through the same console (ADR 0030 §6): each request signed by
/// the person's member key, the meeting's id in the path.
impl MeetLink for ConsoleClient {
    fn join(&self, request: &MeetRequest) -> Result<Joined, LinkError> {
        Self::answer(&self.request(&meet_path(request, "join")?, request)?)
    }

    fn keys(&self, request: &MeetRequest) -> Result<MediaKeys, LinkError> {
        Self::answer(&self.request(&meet_path(request, "keys")?, request)?)
    }

    fn rekey(&self, request: &MeetRequest) -> Result<MediaKeys, LinkError> {
        Self::answer(&self.request(&meet_path(request, "rekey")?, request)?)
    }

    fn leave(&self, request: &MeetRequest) -> Result<(), LinkError> {
        self.request(&meet_path(request, "leave")?, request)
            .map(drop)
    }
}

/// The shared space's sealed blobs (ADR 0033).
impl glass_managed::shared::BlobLink for ConsoleClient {
    fn put(&self, request: &glass_managed::shared::PutBlob) -> Result<(), LinkError> {
        self.request("/device/v1/blobs", request).map(drop)
    }

    fn get(
        &self,
        request: &glass_managed::shared::GetBlob,
    ) -> Result<glass_managed::shared::SealedBlob, LinkError> {
        Self::answer(&self.request("/device/v1/blobs/get", request)?)
    }
}

/// Sharing apps back to the org's store (ADR 0032): submissions, and the
/// bundles of approved apps, which glassd checks against the signed
/// catalog before serving.
impl glass_managed::studio::ShareLink for ConsoleClient {
    fn submit(
        &self,
        submission: &glass_managed::studio::Submission,
    ) -> Result<glass_managed::studio::Submitted, String> {
        Self::answer(
            &self
                .request("/device/v1/apps/submit", submission)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }

    fn bundle(
        &self,
        request: &glass_managed::studio::BundleRequest,
    ) -> Result<glass_managed::studio::Bundle, String> {
        Self::answer(
            &self
                .request("/device/v1/apps/bundle", request)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

/// `/device/v1/meet/<id>/<op>`, for a meeting id that is a conversation's.
fn meet_path(request: &MeetRequest, op: &str) -> Result<String, LinkError> {
    let id = &request.body.meeting;
    if !glass_managed::chat::is_conversation_id(id) {
        return Err(LinkError::Refused(format!("{id:?} isn't a meeting")));
    }
    Ok(format!("/device/v1/meet/{id}/{op}"))
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
                directory: None,
                chat: None,
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
    fn cf91_a_refused_chat_request_carries_the_consoles_reason() {
        let (url, got) = fake::serve(409, r#"{"error":"sam isn't in this conversation."}"#);
        let client = ConsoleClient::new(&url, "enroll-123", true).unwrap();
        let err = ChatLink::lookup(
            &client,
            &LookupKeys {
                logins: vec!["sam".into()],
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            LinkError::Refused("sam isn't in this conversation.".into())
        );
        assert!(
            got.recv()
                .unwrap()
                .head
                .starts_with("POST /device/v1/chat/keys/lookup ")
        );
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
                directory: None,
                chat: None,
                wait_secs: 0,
            })
            .unwrap_err();
        assert!(err.contains("enrollment"), "{err}");
        assert!(!err.contains("secret-token"));
    }
}
