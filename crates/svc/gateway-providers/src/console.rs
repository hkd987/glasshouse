//! The device's side of managed settings (ADR 0023): the one way glassd
//! reaches its org's admin console. Releases it brings back are verified
//! and validated by glassd, never trusted for arriving here.
//!
//! HTTPS only, through the environment's proxy like the model providers,
//! except plain HTTP to a loopback address when the config allows it
//! (development and tests): the org's signature, not the channel, is what
//! a device trusts. The enrollment token is sent as a bearer token to one
//! route, where the computer registers its own key (ADR 0030 slice P); every
//! other request is signed with that key. Neither ever appears in an error.

use std::sync::Arc;

use glass_gateway::ProviderError;
use glass_managed::chat::{
    ChatLink, ChatSyncRequest, ChatSyncResponse, CreateConversation, Keys, LinkError, LookupKeys,
    PostMessage, RegisterKey,
};
use glass_managed::device::{DeviceKey, REGISTER_PATH};
use glass_managed::meet::{Joined, MediaKeys, MeetLink, MeetRequest};
use glass_managed::models::{COMPLETE_PATH, ModelAnswer, ModelCall};
use glass_managed::{DeviceReport, SyncRequest, SyncResponse};

use crate::http::{Http, Target};

/// A client for one console.
#[derive(Debug)]
pub struct ConsoleClient {
    http: Http,
    base: String,
    token: String,
    /// The computer's key, which signs every request but registering it.
    key: Option<Arc<DeviceKey>>,
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
            key: None,
        })
    }

    /// Signs every request with `key`, the computer's own.
    #[must_use]
    pub fn signed_by(mut self, key: Arc<DeviceKey>) -> Self {
        self.key = Some(key);
        self
    }

    /// Registers the computer's key with the console, with the enrollment
    /// token: the first thing an enrolled computer does, and harmless again
    /// with the same key.
    pub fn register_device(&self) -> Result<(), String> {
        let key = self.key.as_ref().ok_or("this computer has no device key")?;
        let registration = key.registration()?;
        self.post(REGISTER_PATH, &registration).map(drop)
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

    /// A model call on the org's credits (ADR 0039), signed with this
    /// computer's key: the console forwards it with the org's key. Errors
    /// say whether the next model on the route should try.
    pub fn complete_model(&self, call: &ModelCall) -> Result<ModelAnswer, ProviderError> {
        let body = serde_json::to_vec(call)
            .map_err(|e| ProviderError::Rejected(format!("the call doesn't encode: {e}")))?;
        let reply = self.send(COMPLETE_PATH, &body)?;
        let why = || Self::reason(&reply.body).unwrap_or_else(|| format!("HTTP {}", reply.status));
        match reply.status {
            200 => serde_json::from_str(&reply.body).map_err(|e| {
                ProviderError::Unavailable(format!("the console's answer doesn't parse: {e}"))
            }),
            // The org's credits are used up, or it has none.
            402 => Err(ProviderError::Unavailable(format!("HTTP 402: {}", why()))),
            // This computer isn't registered, or the console doesn't sell
            // credits: another model on the route might answer.
            401 | 403 | 404 => Err(ProviderError::NotConfigured(format!(
                "HTTP {}: {}",
                reply.status,
                why()
            ))),
            400 | 413 | 422 => Err(ProviderError::Rejected(format!(
                "HTTP {}: {}",
                reply.status,
                why()
            ))),
            status => Err(ProviderError::Unavailable(format!(
                "HTTP {status}: {}",
                why()
            ))),
        }
    }

    /// Posts `body` to `path`, signed with this computer's key.
    fn send(&self, path: &str, body: &[u8]) -> Result<crate::http::Reply, ProviderError> {
        let key = self.key.as_ref().ok_or_else(|| {
            ProviderError::NotConfigured("this computer has no device key yet".into())
        })?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let mut headers = vec![("Content-Type", "application/json".to_owned())];
        headers.extend(
            key.sign("POST", path, now, body)
                .map_err(ProviderError::NotConfigured)?,
        );
        let headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.http
            .post(&format!("{}{path}", self.base), &headers, body)
    }

    /// Posts `body` to `path`: the answer's text, or why not, with the
    /// console's own reason when it refused the request.
    fn request<T: serde::Serialize>(&self, path: &str, body: &T) -> Result<String, LinkError> {
        let body = serde_json::to_vec(body).map_err(|e| LinkError::Unreachable(e.to_string()))?;
        // The token registers the key; the key signs everything else.
        let mut headers = vec![("Content-Type", "application/json".to_owned())];
        if path == REGISTER_PATH {
            headers.push(("Authorization", format!("Bearer {}", self.token)));
        } else {
            let key = self.key.as_ref().ok_or_else(|| {
                LinkError::Unreachable("this computer has no device key yet".into())
            })?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            headers.extend(
                key.sign("POST", path, now, &body)
                    .map_err(LinkError::Unreachable)?,
            );
        }
        let headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let reply = self
            .http
            .post(&format!("{}{path}", self.base), &headers, &body)
            .map_err(|e| match e {
                ProviderError::Unavailable(m)
                | ProviderError::NotConfigured(m)
                | ProviderError::Rejected(m) => LinkError::Unreachable(m),
            })?;
        match reply.status {
            200 => Ok(reply.body),
            401 | 403 => Err(LinkError::Unreachable(
                Self::reason(&reply.body)
                    .unwrap_or_else(|| "the console didn't accept this device".into()),
            )),
            400 | 404 | 409 => Err(LinkError::Refused(
                Self::reason(&reply.body)
                    .unwrap_or_else(|| format!("the console answered {}", reply.status)),
            )),
            status => Err(LinkError::Unreachable(format!(
                "the console answered {status}"
            ))),
        }
    }

    /// The console's own words for a refusal.
    fn reason(body: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_owned))
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

    fn signed(url: &str, token: &str) -> ConsoleClient {
        let key = DeviceKey::generate("dev-1").unwrap();
        ConsoleClient::new(url, token, true)
            .unwrap()
            .signed_by(Arc::new(key))
    }

    #[test]
    fn adr0030_a_sync_is_signed_by_the_computers_key_and_carries_no_token() {
        let (url, got) = fake::serve(200, r#"{"ring":"pilot","policy":null,"catalog":null}"#);
        let client = signed(&url, "enroll-123");
        let answer = client
            .sync(&SyncRequest {
                device: "dev-1".into(),
                policy: Some(3),
                catalog: None,
                directory: None,
                chat: None,
                credits: None,
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
        assert!(!request.head.contains("enroll-123"), "{}", request.head);
        assert!(!request.head.contains("Authorization"), "{}", request.head);
        assert!(request.head.contains("Glasshouse-Device: dev-1"));
        assert!(request.head.contains("Glasshouse-Signature: "));
        assert!(request.body.contains("\"policy\":3"), "{}", request.body);
    }

    #[test]
    fn adr0030_registering_sends_the_token_and_a_proof_of_the_key() {
        let (url, got) = fake::serve(200, "{}");
        signed(&url, "enroll-123").register_device().unwrap();
        let request = got.recv().unwrap();
        assert!(request.head.starts_with("POST /device/v1/register "));
        assert!(request.head.contains("Authorization: Bearer enroll-123"));
        let registration: glass_managed::device::RegisterDevice =
            serde_json::from_str(&request.body).unwrap();
        assert_eq!(registration.device, "dev-1");
        assert!(registration.verify().is_ok());
    }

    #[test]
    fn adr0030_without_a_key_nothing_but_registering_is_sent() {
        let client = ConsoleClient::new("http://127.0.0.1:9", "t", true).unwrap();
        let err = ChatLink::lookup(&client, &LookupKeys { logins: vec![] }).unwrap_err();
        assert!(err.to_string().contains("no device key"), "{err}");
        assert!(client.register_device().is_err());
    }

    #[test]
    fn cf91_a_refused_chat_request_carries_the_consoles_reason() {
        let (url, got) = fake::serve(409, r#"{"error":"sam isn't in this conversation."}"#);
        let client = signed(&url, "enroll-123");
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
    fn a_refused_computer_says_why_without_the_token() {
        let (url, _got) = fake::serve(
            403,
            r#"{"error":"This computer was removed from the organization."}"#,
        );
        let client = signed(&url, "secret-token");
        let err = client
            .sync(&SyncRequest {
                device: "d".into(),
                policy: None,
                catalog: None,
                directory: None,
                chat: None,
                credits: None,
                wait_secs: 0,
            })
            .unwrap_err();
        assert!(err.contains("removed from the organization"), "{err}");
        assert!(!err.contains("secret-token"));
    }
}
