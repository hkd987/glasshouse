//! Microsoft Teams (ADR 0065 §13): the Teams relay's calls out to
//! Microsoft. [`BotKeys`] fetches and keeps the Bot Connector's signing
//! keys, which `glass_teams::verify` checks its calls with; [`BotConnector`]
//! gets the app's token from the org's Entra tenant and sends and updates
//! messages in a conversation.
//!
//! Followed:
//! - "Authenticate requests with the Bot Connector API",
//!   <https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication>:
//!   the OpenID metadata at a static address and its `jwks_uri`, refreshed
//!   at least daily; a single-tenant app's token by client credentials
//!   from `login.microsoftonline.com/<tenant>/oauth2/v2.0/token`, scope
//!   `https://api.botframework.com/.default`, sent only to the Bot
//!   Connector.
//! - The Bot Connector REST API,
//!   <https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-api-reference>:
//!   Send to Conversation (`POST /v3/conversations/{id}/activities`),
//!   Update Activity (`PUT …/activities/{activityId}`) and Create
//!   Conversation (`POST /v3/conversations`), at the conversation's
//!   `serviceUrl`.
//! - Teams' proactive messages,
//!   <https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/conversations/send-proactive-messages>:
//!   a one-to-one conversation made with the person's id and the tenant;
//!   403 when they blocked or removed the app.
//!
//! The app's secret and its tokens never appear in an error. The token
//! goes only to a `serviceUrl` on Microsoft's Bot Connector hosts.

use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use glass_gateway::{ProviderError, Secret};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::console::is_loopback;
use crate::http::{Ask, Http, Reply, Target};
use crate::sigv4::uri_encode;

/// The Bot Connector's OpenID metadata (a static address).
pub const OPENID_METADATA: &str =
    "https://login.botframework.com/v1/.well-known/openidconfiguration";
/// Microsoft Entra's login service.
pub const LOGIN: &str = "https://login.microsoftonline.com";
/// What the app's token is for: the Bot Connector.
pub const SCOPE: &str = "https://api.botframework.com/.default";
/// Hosts a conversation's `serviceUrl` may name: exactly, or ending in an
/// entry that starts with `.`. Teams' public cloud, GCC, GCC High and DoD.
pub const SERVICE_HOSTS: &[&str] = &[
    "smba.trafficmanager.net",
    ".botframework.com",
    ".teams.microsoft.com",
    ".teams.microsoft.us",
];
/// Keys are fetched again after this long: Microsoft asks for at least
/// daily.
pub const KEYS_MAX_AGE: Duration = Duration::from_secs(12 * 3600);
/// A token signed with a key we don't have fetches the keys again, at
/// most this often.
pub const REFETCH_AFTER: Duration = Duration::from_secs(5 * 60);
/// With no keys at all, how long a failed fetch waits before the next.
const RETRY_EMPTY: Duration = Duration::from_secs(10);
/// A token is renewed this long before it expires.
pub const TOKEN_MARGIN: Duration = Duration::from_secs(5 * 60);
/// The most of a refusal's text kept in an error.
const MAX_REASON: usize = 300;

/// Where Microsoft is. Every address can be another, for tests against
/// stand-ins and for sovereign clouds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The Bot Connector's OpenID metadata: [`OPENID_METADATA`].
    pub openid_metadata: String,
    /// Entra's login service: [`LOGIN`].
    pub login: String,
    /// The token's scope: [`SCOPE`].
    pub scope: String,
    /// [`SERVICE_HOSTS`].
    pub service_hosts: Vec<String>,
    /// Plain HTTP to this machine's loopback address too, for stand-ins in
    /// tests and development. Never in production.
    pub allow_loopback_http: bool,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            openid_metadata: OPENID_METADATA.into(),
            login: LOGIN.into(),
            scope: SCOPE.into(),
            service_hosts: SERVICE_HOSTS.iter().map(|h| (*h).to_owned()).collect(),
            allow_loopback_http: false,
        }
    }
}

/// Why a call to Microsoft didn't work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamsError {
    /// Something we were given is wrong: an address, the app's
    /// registration or its secret. Repeating won't help.
    Config(String),
    /// Microsoft refused the request as it is.
    Refused(String),
    /// The person blocked or removed the app, or the conversation or the
    /// message is gone: stop sending there.
    Gone(String),
    /// Try again later: the network, a 429, or a 5xx; after `retry_after`
    /// when Microsoft said.
    Unavailable {
        why: String,
        retry_after: Option<Duration>,
    },
}

impl fmt::Display for TeamsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(why) | Self::Refused(why) | Self::Gone(why) => f.write_str(why),
            Self::Unavailable { why, .. } => f.write_str(why),
        }
    }
}

impl std::error::Error for TeamsError {}

/// The two clients: HTTPS, and plain HTTP for loopback stand-ins.
#[derive(Debug)]
struct Client {
    tls: Http,
    plain: Http,
    allow_loopback_http: bool,
}

impl Client {
    fn new(allow_loopback_http: bool) -> Self {
        Self {
            tls: Http::new(),
            plain: Http::loopback(),
            allow_loopback_http,
        }
    }

    /// The client for `url`: HTTPS, or plain HTTP to this machine when
    /// allowed.
    fn pick(&self, url: &str, what: &str) -> Result<&Http, TeamsError> {
        let target = Target::parse(url)
            .ok_or_else(|| TeamsError::Config(format!("{what} {url:?} isn't an http(s) URL")))?;
        if target.tls {
            Ok(&self.tls)
        } else if self.allow_loopback_http && is_loopback(&target.host) {
            Ok(&self.plain)
        } else {
            Err(TeamsError::Config(format!("{what} {url:?} must use HTTPS")))
        }
    }

    fn get(&self, url: &str, what: &str) -> Result<Value, TeamsError> {
        let reply = self
            .pick(url, what)?
            .send("GET", url, &[("accept", "application/json")], b"")
            .map_err(|e| from_provider(what, &e))?;
        if reply.status != 200 {
            return Err(status_error(what, &reply, ""));
        }
        serde_json::from_str(&reply.body).map_err(|e| TeamsError::Unavailable {
            why: format!("{what} isn't JSON: {e}"),
            retry_after: None,
        })
    }
}

/// The Bot Connector's signing keys, fetched when first needed, again
/// daily, and again for a key we don't have, at most every few minutes.
#[derive(Debug)]
pub struct BotKeys {
    client: Client,
    metadata: String,
    state: Mutex<KeysState>,
}

#[derive(Debug, Default)]
struct KeysState {
    /// The keys document, as served.
    jwks: Option<String>,
    kids: Vec<String>,
    fetched: Option<Instant>,
    tried: Option<Instant>,
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(default)]
    jwks_uri: String,
    #[serde(default)]
    id_token_signing_alg_values_supported: Option<Vec<String>>,
}

impl BotKeys {
    pub fn new(endpoints: &Endpoints) -> Result<Self, TeamsError> {
        let client = Client::new(endpoints.allow_loopback_http);
        client.pick(
            &endpoints.openid_metadata,
            "the Bot Connector's metadata address",
        )?;
        Ok(Self {
            client,
            metadata: endpoints.openid_metadata.clone(),
            state: Mutex::new(KeysState::default()),
        })
    }

    /// The keys document (a JWKS, as JSON) that should hold `kid`, the key
    /// a request's token names (`glass_teams::key_id`). Fetched if there
    /// are none yet or they're over [`KEYS_MAX_AGE`] old, or if `kid` isn't
    /// among them and the last fetch was over [`REFETCH_AFTER`] ago. A
    /// fetch that fails keeps the keys already here. Call it only for a
    /// request with a token, so nothing else makes it fetch.
    pub fn signing_keys(&self, kid: Option<&str>) -> Result<String, TeamsError> {
        self.signing_keys_at(kid, Instant::now())
    }

    fn signing_keys_at(&self, kid: Option<&str>, now: Instant) -> Result<String, TeamsError> {
        let mut keys = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let since = |t: Option<Instant>| t.map(|t| now.saturating_duration_since(t));
        let wait = if keys.jwks.is_some() {
            REFETCH_AFTER
        } else {
            RETRY_EMPTY
        };
        let may_try = since(keys.tried).is_none_or(|d| d >= wait);
        let stale = since(keys.fetched).is_none_or(|d| d >= KEYS_MAX_AGE);
        let missing = kid.is_some_and(|k| !keys.kids.iter().any(|have| have == k));
        if may_try && (stale || missing) {
            keys.tried = Some(now);
            match self.fetch() {
                Ok((jwks, kids)) => {
                    keys.jwks = Some(jwks);
                    keys.kids = kids;
                    keys.fetched = Some(now);
                }
                Err(e) if keys.jwks.is_none() => return Err(e),
                // Microsoft's keys change rarely: the ones we have serve
                // until a fetch works.
                Err(_) => {}
            }
        }
        keys.jwks.clone().ok_or_else(|| TeamsError::Unavailable {
            why: "the Bot Connector's signing keys haven't been fetched yet".into(),
            retry_after: Some(wait),
        })
    }

    /// The metadata, then the keys document it names.
    fn fetch(&self) -> Result<(String, Vec<String>), TeamsError> {
        let metadata: Metadata = serde_json::from_value(
            self.client
                .get(&self.metadata, "the Bot Connector's OpenID metadata")?,
        )
        .map_err(|e| TeamsError::Unavailable {
            why: format!("the Bot Connector's OpenID metadata: {e}"),
            retry_after: None,
        })?;
        if metadata
            .id_token_signing_alg_values_supported
            .is_some_and(|algs| !algs.iter().any(|a| a == "RS256"))
        {
            return Err(TeamsError::Config(
                "the Bot Connector's metadata no longer lists RS256, the only algorithm checked"
                    .into(),
            ));
        }
        let what = "the Bot Connector's keys document";
        let jwks = self.client.get(&metadata.jwks_uri, what)?;
        let kids: Vec<String> = jwks["keys"]
            .as_array()
            .ok_or_else(|| TeamsError::Unavailable {
                why: format!("{what} has no keys"),
                retry_after: None,
            })?
            .iter()
            .filter_map(|k| k["kid"].as_str().map(str::to_owned))
            .collect();
        Ok((jwks.to_string(), kids))
    }
}

/// The app's side of the Bot Connector: its token, and messages sent and
/// updated in a conversation.
#[derive(Debug)]
pub struct BotConnector {
    client: Client,
    login: String,
    scope: String,
    service_hosts: Vec<String>,
    app_id: String,
    tenant_id: String,
    secret: Secret,
    token: Mutex<Option<(Secret, Instant)>>,
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Deserialize)]
struct Made {
    #[serde(default)]
    id: String,
}

impl BotConnector {
    /// The app registered as `app_id` (its Microsoft App ID) in the org's
    /// Entra `tenant_id`, with its client `secret`.
    pub fn new(
        app_id: &str,
        tenant_id: &str,
        secret: &str,
        endpoints: &Endpoints,
    ) -> Result<Self, TeamsError> {
        let id_like = |s: &str| {
            !s.is_empty()
                && s.len() <= 128
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        };
        if !id_like(app_id) {
            return Err(TeamsError::Config(format!("{app_id:?} isn't an app id")));
        }
        if !id_like(tenant_id) {
            return Err(TeamsError::Config(format!(
                "{tenant_id:?} isn't a tenant id"
            )));
        }
        if secret.is_empty() {
            return Err(TeamsError::Config(
                "the app's client secret is empty".into(),
            ));
        }
        let client = Client::new(endpoints.allow_loopback_http);
        let login = endpoints.login.trim_end_matches('/').to_owned();
        client.pick(&format!("{login}/"), "the login service address")?;
        Ok(Self {
            client,
            login,
            scope: endpoints.scope.clone(),
            service_hosts: endpoints.service_hosts.clone(),
            app_id: app_id.to_owned(),
            tenant_id: tenant_id.to_owned(),
            secret: Secret::new(secret),
            token: Mutex::new(None),
        })
    }

    /// The app's token for the Bot Connector, kept until [`TOKEN_MARGIN`]
    /// before it expires.
    pub fn token(&self) -> Result<Secret, TeamsError> {
        self.token_at(Instant::now())
    }

    fn token_at(&self, now: Instant) -> Result<Secret, TeamsError> {
        let mut kept = self
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((token, until)) = kept.as_ref()
            && now < *until
        {
            return Ok(token.clone());
        }
        let what = "Microsoft's login service";
        let url = format!("{}/{}/oauth2/v2.0/token", self.login, self.tenant_id);
        let body = [
            ("grant_type", "client_credentials"),
            ("client_id", self.app_id.as_str()),
            ("client_secret", self.secret.expose()),
            ("scope", self.scope.as_str()),
        ]
        .iter()
        .map(|(k, v)| format!("{k}={}", uri_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
        let reply = self
            .client
            .pick(&url, "the login service address")?
            .post(
                &url,
                &[
                    ("content-type", "application/x-www-form-urlencoded"),
                    ("accept", "application/json"),
                ],
                body.as_bytes(),
            )
            .map_err(|e| from_provider(what, &e))?;
        if reply.status != 200 {
            return Err(match status_error(what, &reply, self.secret.expose()) {
                TeamsError::Refused(why) | TeamsError::Gone(why) => TeamsError::Config(format!(
                    "{why} (check the app's id, tenant and client secret)"
                )),
                other => other,
            });
        }
        let token: Token =
            serde_json::from_str(&reply.body).map_err(|_| TeamsError::Unavailable {
                why: format!("{what} answered without a token"),
                retry_after: None,
            })?;
        let token_value = Secret::new(token.access_token);
        let lasts = Duration::from_secs(token.expires_in).saturating_sub(TOKEN_MARGIN);
        *kept = Some((token_value.clone(), now + lasts));
        Ok(token_value)
    }

    /// Sends `activity` (a message: `glass_teams::approval_activity`) to
    /// the conversation, and returns the new message's id, to update it
    /// later.
    pub fn send(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity: &Value,
    ) -> Result<String, TeamsError> {
        let path = format!(
            "/v3/conversations/{}/activities",
            uri_encode(conversation_id)
        );
        let reply = self.call("POST", service_url, &path, activity, "sending to Teams")?;
        Ok(made(&reply))
    }

    /// Replaces the message `activity_id` in the conversation with
    /// `activity`: a card answered, answered elsewhere or expired.
    pub fn update(
        &self,
        service_url: &str,
        conversation_id: &str,
        activity_id: &str,
        activity: &Value,
    ) -> Result<(), TeamsError> {
        if activity_id.is_empty() {
            return Err(TeamsError::Refused("no message to update".into()));
        }
        let path = format!(
            "/v3/conversations/{}/activities/{}",
            uri_encode(conversation_id),
            uri_encode(activity_id)
        );
        self.call(
            "PUT",
            service_url,
            &path,
            activity,
            "updating a Teams message",
        )?;
        Ok(())
    }

    /// Opens the app's one-to-one chat with a person (`user_id` their
    /// Teams id or Entra object id) in the org's tenant, and returns its
    /// conversation id. For someone the app is installed for whose
    /// conversation wasn't kept.
    pub fn create_conversation(
        &self,
        service_url: &str,
        user_id: &str,
    ) -> Result<String, TeamsError> {
        let parameters = json!({
            "bot": {"id": format!("28:{}", self.app_id)},
            "members": [{"id": user_id}],
            "channelData": {"tenant": {"id": self.tenant_id}},
            "tenantId": self.tenant_id,
            "isGroup": false,
        });
        let reply = self.call(
            "POST",
            service_url,
            "/v3/conversations",
            &parameters,
            "opening a chat in Teams",
        )?;
        let id = made(&reply);
        if id.is_empty() {
            return Err(TeamsError::Unavailable {
                why: "Teams opened the chat without saying its id".into(),
                retry_after: None,
            });
        }
        Ok(id)
    }

    /// `method` on the Bot Connector at `service_url`, with the app's token.
    /// A 401 drops the token and tries once more with a new one.
    fn call(
        &self,
        method: &str,
        service_url: &str,
        path: &str,
        body: &Value,
        what: &str,
    ) -> Result<Reply, TeamsError> {
        let base = self.service_base(service_url)?;
        let url = format!("{base}{path}");
        let http = self.client.pick(&url, "the conversation's serviceUrl")?;
        let body = body.to_string();
        let mut renewed = false;
        loop {
            let bearer = format!("Bearer {}", self.token()?.expose());
            let headers = [
                ("authorization", bearer.as_str()),
                ("content-type", "application/json; charset=utf-8"),
                ("accept", "application/json"),
            ];
            let reply = if method == "PUT" {
                put(http, &url, &headers, body.as_bytes())
            } else {
                http.send(method, &url, &headers, body.as_bytes())
            }
            .map_err(|e| from_provider(&format!("the Bot Connector, {what},"), &e))?;
            match reply.status {
                200..=299 => return Ok(reply),
                401 if !renewed => {
                    renewed = true;
                    *self
                        .token
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                }
                401 => {
                    return Err(TeamsError::Config(format!(
                        "the Bot Connector refused the app's token {what} (HTTP 401): check the app's registration"
                    )));
                }
                _ => {
                    return Err(status_error(
                        &format!("the Bot Connector, {what},"),
                        &reply,
                        "",
                    ));
                }
            }
        }
    }

    /// `service_url` without its trailing `/`, if it's the Bot Connector's.
    fn service_base(&self, service_url: &str) -> Result<String, TeamsError> {
        let base = service_url.trim_end_matches('/');
        let target = Target::parse(&format!("{base}/")).ok_or_else(|| {
            TeamsError::Config(format!(
                "the serviceUrl {service_url:?} isn't an http(s) URL"
            ))
        })?;
        let host = target.host.to_ascii_lowercase();
        let known = self.service_hosts.iter().any(|h| {
            let h = h.to_ascii_lowercase();
            if h.starts_with('.') {
                host.ends_with(&h)
            } else {
                host == h
            }
        });
        let stand_in = !target.tls && self.client.allow_loopback_http && is_loopback(&host);
        if !(target.tls && known) && !stand_in {
            return Err(TeamsError::Config(format!(
                "the serviceUrl {service_url:?} isn't the Bot Connector's: the app's token goes nowhere else"
            )));
        }
        Ok(base.to_owned())
    }
}

/// A PUT. `Http::send` speaks GET, POST, PATCH and DELETE; `Http::download`
/// sends any method, and its reply here is a small JSON body.
fn put(
    http: &Http,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Reply, ProviderError> {
    let mut out = Vec::new();
    let ask = Ask {
        method: "PUT",
        url,
        headers,
        body,
    };
    let got = http.download(&ask, 0, &mut out, http.max_reply as u64)?;
    Ok(Reply {
        status: got.status,
        body: if (200..300).contains(&got.status) {
            String::from_utf8_lossy(&out).into_owned()
        } else {
            got.body
        },
        retry_after: None,
    })
}

/// The id in a ResourceResponse (`{"id": "…"}`), if any.
fn made(reply: &Reply) -> String {
    serde_json::from_str::<Made>(&reply.body)
        .map(|m| m.id)
        .unwrap_or_default()
}

fn from_provider(what: &str, e: &ProviderError) -> TeamsError {
    let why = format!("{what} {e}");
    match e {
        ProviderError::Unavailable(_) => TeamsError::Unavailable {
            why,
            retry_after: None,
        },
        ProviderError::NotConfigured(_) => TeamsError::Config(why),
        ProviderError::Rejected(_) => TeamsError::Refused(why),
    }
}

/// An error for a reply that isn't a success, in Microsoft's words where it
/// has them, with `secret` (if any) taken out.
fn status_error(what: &str, reply: &Reply, secret: &str) -> TeamsError {
    let body: Value = serde_json::from_str(&reply.body).unwrap_or(Value::Null);
    let said = [
        &body["error_description"],
        &body["error"]["message"],
        &body["message"],
        &body["error"],
    ]
    .into_iter()
    .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
    .unwrap_or("no details");
    let said = if secret.is_empty() {
        said.to_owned()
    } else {
        said.replace(secret, "…")
    };
    let said: String = said.chars().take(MAX_REASON).collect();
    let why = format!("{what} said HTTP {}: {said}", reply.status);
    match reply.status {
        403 | 404 => TeamsError::Gone(why),
        408 | 429 | 500..=599 => TeamsError::Unavailable {
            why,
            retry_after: reply.retry_after,
        },
        _ => TeamsError::Refused(why),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::mpsc::Receiver;

    use super::*;
    use crate::http::fake::{self, Request};

    const APP: &str = "8f0c5a8e-1b2c-4d3e-9f40-5a6b7c8d9e0f";
    const TENANT: &str = "b28fdbfd-2b78-4f93-b0f8-8881793f0f8f";
    const SECRET: &str = "Qx8~s3cr3t.v4lue_with+symbols&=";
    const CHAT: &str = "a:1qhNLqpUtmuI6U35gzjsJn7uRnCkW8NiZALHfN8AMxdbprS1uta2aT";

    /// A reply as a stand-in server sends it.
    fn reply(status: u16, headers: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn ok(body: &str) -> Vec<u8> {
        reply(200, "", body)
    }

    fn endpoints(metadata: &str, login: &str) -> Endpoints {
        Endpoints {
            openid_metadata: format!("{metadata}/v1/.well-known/openidconfiguration"),
            login: login.into(),
            allow_loopback_http: true,
            ..Endpoints::default()
        }
    }

    fn jwks(kids: &[&str]) -> String {
        let keys: Vec<Value> = kids
            .iter()
            .map(|k| json!({"kty": "RSA", "use": "sig", "kid": k, "n": "AQAB", "e": "AQAB", "endorsements": ["msteams"]}))
            .collect();
        json!({"keys": keys}).to_string()
    }

    fn next(got: &Receiver<Request>) -> Request {
        got.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    fn quiet(got: &Receiver<Request>) -> bool {
        got.recv_timeout(Duration::from_millis(200)).is_err()
    }

    #[test]
    fn adr0065_the_relay_fetches_microsofts_keys_and_refetches_for_an_unknown_one() {
        let (keys_url, keys_got) =
            fake::serve_raw(vec![ok(&jwks(&["k1"])), ok(&jwks(&["k1", "k2"]))]);
        let metadata = json!({
            "issuer": "https://api.botframework.com",
            "authorization_endpoint": "https://invalid.botframework.com",
            "jwks_uri": format!("{keys_url}/v1/.well-known/keys"),
            "id_token_signing_alg_values_supported": ["RS256"],
            "token_endpoint_auth_methods_supported": ["private_key_jwt"]
        })
        .to_string();
        let (meta_url, meta_got) = fake::serve_raw(vec![ok(&metadata), ok(&metadata)]);
        let keys = BotKeys::new(&endpoints(&meta_url, "http://127.0.0.1:9")).unwrap();
        let t0 = Instant::now();

        // First use: the metadata, then the keys it names.
        let first = keys.signing_keys_at(Some("k1"), t0).unwrap();
        assert!(first.contains("\"k1\"") && !first.contains("\"k2\""));
        assert!(
            next(&meta_got)
                .head
                .starts_with("GET /v1/.well-known/openidconfiguration HTTP/1.1")
        );
        assert!(
            next(&keys_got)
                .head
                .starts_with("GET /v1/.well-known/keys HTTP/1.1")
        );

        // Kept.
        assert_eq!(
            keys.signing_keys_at(Some("k1"), t0 + Duration::from_secs(1))
                .unwrap(),
            first
        );
        assert_eq!(
            keys.signing_keys_at(None, t0 + Duration::from_secs(2))
                .unwrap(),
            first
        );
        // An unknown key within five minutes of the last fetch: no fetch, so
        // junk tokens can't make the relay hammer Microsoft.
        assert_eq!(
            keys.signing_keys_at(Some("k2"), t0 + Duration::from_secs(60))
                .unwrap(),
            first
        );
        assert!(quiet(&meta_got));

        // Past five minutes, an unknown key fetches again.
        let second = keys
            .signing_keys_at(Some("k2"), t0 + REFETCH_AFTER + Duration::from_secs(1))
            .unwrap();
        assert!(second.contains("\"k2\""));
        next(&meta_got);
        next(&keys_got);
        assert_eq!(
            keys.signing_keys_at(Some("k3"), t0 + REFETCH_AFTER + Duration::from_secs(61))
                .unwrap(),
            second
        );
        assert!(quiet(&meta_got));

        // A day on, they're fetched again; when Microsoft can't be reached,
        // the keys already here still serve.
        assert_eq!(
            keys.signing_keys_at(None, t0 + KEYS_MAX_AGE + REFETCH_AFTER * 2)
                .unwrap(),
            second
        );
    }

    #[test]
    fn adr0065_keys_are_fetched_only_from_where_microsoft_says() {
        // With none fetched yet, a failure is an error to retry.
        let (meta_url, _got) = fake::serve_raw(vec![reply(503, "", "{}")]);
        let keys = BotKeys::new(&endpoints(&meta_url, "http://127.0.0.1:9")).unwrap();
        let err = keys.signing_keys(Some("k1")).unwrap_err();
        assert!(matches!(err, TeamsError::Unavailable { .. }), "{err}");
        // And isn't tried again at once.
        let err = keys.signing_keys(Some("k1")).unwrap_err();
        assert!(
            err.to_string().contains("haven't been fetched yet"),
            "{err}"
        );

        // Metadata naming a keys document over plain HTTP elsewhere, or
        // another algorithm.
        let bad = json!({"jwks_uri": "http://keys.example/keys"}).to_string();
        let hs = json!({"jwks_uri": "https://keys.example/keys", "id_token_signing_alg_values_supported": ["HS256"]}).to_string();
        let (meta_url, _got) = fake::serve_raw(vec![ok(&bad), ok(&hs)]);
        let keys = BotKeys::new(&endpoints(&meta_url, "http://127.0.0.1:9")).unwrap();
        let t0 = Instant::now();
        let err = keys.signing_keys_at(None, t0).unwrap_err();
        assert!(err.to_string().contains("must use HTTPS"), "{err}");
        let err = keys.signing_keys_at(None, t0 + RETRY_EMPTY).unwrap_err();
        assert!(err.to_string().contains("no longer lists RS256"), "{err}");

        // Only HTTPS, but for a stand-in on this machine when allowed.
        let production = Endpoints {
            openid_metadata: "http://127.0.0.1:9/meta".into(),
            ..Endpoints::default()
        };
        assert!(matches!(
            BotKeys::new(&production),
            Err(TeamsError::Config(_))
        ));
        assert!(BotKeys::new(&Endpoints::default()).is_ok());
    }

    #[test]
    fn adr0065_the_relay_gets_its_token_from_the_orgs_tenant_and_keeps_it() {
        let token = json!({"token_type": "Bearer", "expires_in": 3600, "ext_expires_in": 3600, "access_token": "eyJ0eXAi.first.sig"}).to_string();
        let again = json!({"token_type": "Bearer", "expires_in": 3600, "access_token": "eyJ0eXAi.second.sig"}).to_string();
        let (login, got) = fake::serve_raw(vec![ok(&token), ok(&again)]);
        let connector = BotConnector::new(
            APP,
            TENANT,
            SECRET,
            &endpoints("http://127.0.0.1:9", &login),
        )
        .unwrap();
        let t0 = Instant::now();
        assert_eq!(
            connector.token_at(t0).unwrap().expose(),
            "eyJ0eXAi.first.sig"
        );
        let asked = next(&got);
        assert!(
            asked
                .head
                .starts_with(&format!("POST /{TENANT}/oauth2/v2.0/token HTTP/1.1")),
            "{}",
            asked.head
        );
        assert!(
            asked
                .head
                .contains("content-type: application/x-www-form-urlencoded")
        );
        assert_eq!(
            asked.body,
            format!(
                "grant_type=client_credentials&client_id={APP}&client_secret=Qx8~s3cr3t.v4lue_with%2Bsymbols%26%3D&scope=https%3A%2F%2Fapi.botframework.com%2F.default"
            )
        );

        // Kept until five minutes before it expires.
        let later = t0 + Duration::from_secs(3600) - TOKEN_MARGIN - Duration::from_secs(1);
        assert_eq!(
            connector.token_at(later).unwrap().expose(),
            "eyJ0eXAi.first.sig"
        );
        assert!(quiet(&got));
        let renewed = t0 + Duration::from_secs(3600) - TOKEN_MARGIN;
        assert_eq!(
            connector.token_at(renewed).unwrap().expose(),
            "eyJ0eXAi.second.sig"
        );
        next(&got);
        // A token never shows in a debug print.
        assert!(!format!("{connector:?}").contains("eyJ0eXAi"));
        assert!(!format!("{connector:?}").contains(SECRET));
    }

    #[test]
    fn adr0065_a_refused_secret_never_appears_in_the_error() {
        let refusal = json!({
            "error": "invalid_client",
            "error_description": format!("AADSTS7000215: Invalid client secret provided: {SECRET}. Trace ID: 0e6f"),
            "error_codes": [7000215]
        })
        .to_string();
        let (login, _got) = fake::serve_raw(vec![reply(401, "", &refusal)]);
        let connector = BotConnector::new(
            APP,
            TENANT,
            SECRET,
            &endpoints("http://127.0.0.1:9", &login),
        )
        .unwrap();
        let err = connector.token().unwrap_err();
        let TeamsError::Config(text) = &err else {
            panic!("{err:?}");
        };
        assert!(text.contains("AADSTS7000215"), "{text}");
        assert!(
            !text.contains(SECRET) && !format!("{err:?}").contains(SECRET),
            "{text}"
        );

        // Bad ids never make it into a URL.
        let e = endpoints("http://127.0.0.1:9", &login);
        assert!(BotConnector::new(APP, "../evil", SECRET, &e).is_err());
        assert!(BotConnector::new("a b", TENANT, SECRET, &e).is_err());
        assert!(BotConnector::new(APP, TENANT, "", &e).is_err());
        assert!(
            BotConnector::new(
                APP,
                TENANT,
                SECRET,
                &Endpoints {
                    login: "http://login.example".into(),
                    ..Endpoints::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn adr0065_cards_go_to_the_conversation_and_are_updated_in_place() {
        let token =
            json!({"token_type": "Bearer", "expires_in": 3600, "access_token": "eyJ.tok.sig"})
                .to_string();
        let (login, logins) = fake::serve_raw(vec![ok(&token)]);
        let (service, sent) = fake::serve_raw(vec![
            reply(201, "", r#"{"id":"1761562500456"}"#),
            ok(r#"{"id":"1761562500456"}"#),
            ok(r#"{"id":"a:1newChat"}"#),
        ]);
        let connector = BotConnector::new(
            APP,
            TENANT,
            SECRET,
            &endpoints("http://127.0.0.1:9", &login),
        )
        .unwrap();
        let card = json!({"type": "message", "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": {"type": "AdaptiveCard", "version": "1.5"}}]});
        let service_url = format!("{service}/amer/");

        let id = connector.send(&service_url, CHAT, &card).unwrap();
        assert_eq!(id, "1761562500456");
        let request = next(&sent);
        assert!(
            request.head.starts_with(
                "POST /amer/v3/conversations/a%3A1qhNLqpUtmuI6U35gzjsJn7uRnCkW8NiZALHfN8AMxdbprS1uta2aT/activities HTTP/1.1"
            ),
            "{}",
            request.head
        );
        assert!(
            request
                .head
                .contains("authorization: Bearer eyJ.tok.sig\r\n")
        );
        assert!(request.head.contains("content-type: application/json"));
        assert_eq!(serde_json::from_str::<Value>(&request.body).unwrap(), card);

        let answered = json!({"type": "message", "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": {"type": "AdaptiveCard", "body": []}}]});
        connector
            .update(&service_url, CHAT, &id, &answered)
            .unwrap();
        let request = next(&sent);
        assert!(
            request.head.starts_with(
                "PUT /amer/v3/conversations/a%3A1qhNLqpUtmuI6U35gzjsJn7uRnCkW8NiZALHfN8AMxdbprS1uta2aT/activities/1761562500456 HTTP/1.1"
            ),
            "{}",
            request.head
        );
        assert_eq!(
            serde_json::from_str::<Value>(&request.body).unwrap(),
            answered
        );

        // A person the app is installed for, with no conversation kept.
        let chat = connector
            .create_conversation(&service_url, "29:1ljv6N86roXr5pjPrCJVIz6xHh5QxjI-dana")
            .unwrap();
        assert_eq!(chat, "a:1newChat");
        let request = next(&sent);
        assert!(
            request
                .head
                .starts_with("POST /amer/v3/conversations HTTP/1.1")
        );
        assert_eq!(
            serde_json::from_str::<Value>(&request.body).unwrap(),
            json!({
                "bot": {"id": format!("28:{APP}")},
                "members": [{"id": "29:1ljv6N86roXr5pjPrCJVIz6xHh5QxjI-dana"}],
                "channelData": {"tenant": {"id": TENANT}},
                "tenantId": TENANT,
                "isGroup": false
            })
        );
        // One token for all three.
        next(&logins);
        assert!(quiet(&logins));
    }

    #[test]
    fn adr0065_the_apps_token_goes_only_to_the_bot_connector() {
        let token = json!({"expires_in": 3600, "access_token": "eyJ.tok.sig"}).to_string();
        let (login, logins) = fake::serve_raw(vec![ok(&token)]);
        let connector = BotConnector::new(
            APP,
            TENANT,
            SECRET,
            &endpoints("http://127.0.0.1:9", &login),
        )
        .unwrap();
        let card = json!({"type": "message"});
        for url in [
            "https://evil.example/teams/",
            "https://smba.trafficmanager.net.evil.example/amer/",
            "https://eviltrafficmanager.net/amer/",
            "http://smba.trafficmanager.net/amer/",
            "ftp://smba.trafficmanager.net/",
        ] {
            let err = connector.send(url, CHAT, &card).unwrap_err();
            assert!(matches!(err, TeamsError::Config(_)), "{url}: {err}");
        }
        assert!(quiet(&logins), "no token was even fetched");
        assert!(
            connector
                .service_base("https://smba.trafficmanager.net/amer/")
                .is_ok()
        );
        assert!(
            connector
                .service_base("https://smba.infra.gcc.teams.microsoft.com/teams")
                .is_ok()
        );
        assert!(
            connector
                .service_base("https://SMBA.trafficmanager.net/emea/")
                .is_ok()
        );
    }

    #[test]
    fn adr0065_teams_refusals_say_whether_to_retry_or_stop() {
        let token = json!({"expires_in": 3600, "access_token": "eyJ.one.sig"}).to_string();
        let fresh = json!({"expires_in": 3600, "access_token": "eyJ.two.sig"}).to_string();
        let (login, logins) = fake::serve_raw(vec![ok(&token), ok(&fresh)]);
        let blocked = r#"{"errorCode":209,"message":"{\n  \"subCode\": \"MessageWritesBlocked\",\n  \"details\": \"Thread is blocked from message writes.\"\n}"}"#;
        let (service, sent) = fake::serve_raw(vec![
            reply(403, "", blocked),
            reply(
                429,
                "Retry-After: 7\r\n",
                r#"{"error":{"code":"Throttled","message":"Too many requests"}}"#,
            ),
            reply(
                401,
                "",
                r#"{"message":"Authorization has been denied for this request."}"#,
            ),
            reply(201, "", r#"{"id":"1"}"#),
            reply(
                404,
                "",
                r#"{"error":{"code":"ActivityNotFoundInConversation","message":"The activity was not found"}}"#,
            ),
        ]);
        let connector = BotConnector::new(
            APP,
            TENANT,
            SECRET,
            &endpoints("http://127.0.0.1:9", &login),
        )
        .unwrap();
        let card = json!({"type": "message"});

        // Blocked or removed: stop sending there.
        let err = connector.send(&service, CHAT, &card).unwrap_err();
        assert!(
            matches!(&err, TeamsError::Gone(t) if t.contains("MessageWritesBlocked")),
            "{err:?}"
        );
        // Throttled: later, when Teams says.
        let err = connector.send(&service, CHAT, &card).unwrap_err();
        assert_eq!(
            err,
            TeamsError::Unavailable {
                why: "the Bot Connector, sending to Teams, said HTTP 429: Too many requests".into(),
                retry_after: Some(Duration::from_secs(7)),
            }
        );
        // A refused token is dropped and a new one tried once.
        assert_eq!(connector.send(&service, CHAT, &card).unwrap(), "1");
        for _ in 0..3 {
            next(&sent);
        }
        let retried = next(&sent);
        assert!(
            retried
                .head
                .contains("authorization: Bearer eyJ.two.sig\r\n"),
            "{}",
            retried.head
        );
        next(&logins);
        next(&logins);
        // An update of a message that's gone.
        let err = connector.update(&service, CHAT, "1", &card).unwrap_err();
        assert!(matches!(err, TeamsError::Gone(_)), "{err:?}");
        assert!(matches!(
            connector.update(&service, CHAT, "", &card),
            Err(TeamsError::Refused(_))
        ));
    }
}
