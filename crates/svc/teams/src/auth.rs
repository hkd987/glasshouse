//! Checking a request from Microsoft (ADR 0065 §13): the Bot Connector
//! signs a JWT for each call it makes to the app, and only what it signed
//! for this app, from the org's tenant in Teams, gets in.
//!
//! Followed: "Authenticate requests with the Bot Connector API",
//! <https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication>,
//! "Authenticate requests from the Bot Connector service to your bot":
//! a `Bearer` token; RS256 (the OpenID metadata's only algorithm) with a
//! key from its keys document; issuer `https://api.botframework.com`;
//! audience the app's id; within its validity, give or take five minutes;
//! a `serviceurl` claim equal to the activity's `serviceUrl`; and the key
//! endorsed for the activity's channel. A refusal's words are for the
//! admin log: the caller answers 401 and nothing more.

use std::fmt;

use glass_crypto::rsa::{MIN_RSA_BITS, RsaError, verify_rs256};
use serde::Deserialize;
use serde_json::Value;

use crate::activity::{Activity, nullable};
use crate::b64;

/// Who issues the Bot Connector's tokens.
pub const ISSUER: &str = "https://api.botframework.com";
/// Teams' channel id.
pub const CHANNEL: &str = "msteams";
/// How far apart our clock and Microsoft's may be ("industry-standard").
pub const CLOCK_SKEW_SECS: i64 = 300;
/// The longest token read; Microsoft's are a few KB.
const MAX_TOKEN: usize = 16 * 1024;

/// What a request must be signed for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotAuth {
    /// The app's Microsoft App ID (its Entra application id): the token's
    /// audience.
    pub app_id: String,
    /// The org's Entra tenant: activities from any other are refused.
    pub tenant_id: String,
    /// [`ISSUER`], unless a sovereign cloud says otherwise.
    pub issuer: String,
    /// [`CHANNEL`]: only Teams.
    pub channel: String,
    /// [`CLOCK_SKEW_SECS`].
    pub skew_secs: i64,
}

impl BotAuth {
    /// Microsoft's public cloud, Teams only, for `app_id` in `tenant_id`.
    pub fn new(app_id: &str, tenant_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
            tenant_id: tenant_id.to_owned(),
            issuer: ISSUER.to_owned(),
            channel: CHANNEL.to_owned(),
            skew_secs: CLOCK_SKEW_SECS,
        }
    }
}

/// The Bot Connector's signing keys: the OpenID keys document (a JWKS),
/// with each key's `endorsements`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Jwks {
    #[serde(default, deserialize_with = "nullable")]
    pub keys: Vec<Jwk>,
}

/// One signing key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Jwk {
    #[serde(default, deserialize_with = "nullable")]
    pub kty: String,
    #[serde(default, deserialize_with = "nullable")]
    pub kid: String,
    /// The modulus and exponent, base64url.
    #[serde(default, deserialize_with = "nullable")]
    pub n: String,
    #[serde(default, deserialize_with = "nullable")]
    pub e: String,
    /// The channels it may sign for: `msteams`, …
    #[serde(default, deserialize_with = "nullable")]
    pub endorsements: Vec<String>,
}

impl Jwks {
    /// The keys document as Microsoft serves it.
    pub fn parse(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("the signing keys aren't a JWKS: {e}"))
    }

    pub fn find(&self, kid: &str) -> Option<&Jwk> {
        self.keys.iter().find(|k| !kid.is_empty() && k.kid == kid)
    }
}

impl Jwk {
    /// Whether this key signed `signed` with `signature`, RS256.
    fn verify(&self, signed: &[u8], signature: &[u8]) -> Result<(), RsaError> {
        let (Some(n), Some(e)) = (b64::decode(&self.n), b64::decode(&self.e)) else {
            return Err(RsaError::BadKey);
        };
        if self.kty != "RSA" {
            return Err(RsaError::BadKey);
        }
        verify_rs256(&n, &e, signed, signature)
    }
}

/// Why a request was refused, for the admin log. Answer 401 to all of them,
/// with no body ([`Refusal::status`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No `Authorization: Bearer` header.
    NoToken,
    /// Not a JWT this can read.
    Malformed(&'static str),
    /// Signed with another algorithm than RS256.
    Algorithm(String),
    /// Signed with a key not in the keys document: fetch it again (at most
    /// every few minutes) and check once more.
    UnknownKey(String),
    /// The key named isn't an RSA key of 2048 bits or more.
    BadKey(String),
    BadSignature,
    /// Issued by someone other than the Bot Connector.
    Issuer,
    /// For another app.
    Audience,
    Expired,
    NotYetValid,
    /// The key isn't endorsed for the activity's channel.
    NotEndorsed,
    /// The body isn't an activity.
    Activity(String),
    /// From another channel than Teams.
    Channel,
    /// From another tenant than the org's, or one that doesn't say.
    Tenant,
    /// The token's `serviceurl` isn't the activity's `serviceUrl`.
    ServiceUrl,
}

impl Refusal {
    /// The HTTP status to answer with: 401, whatever the reason, so a
    /// caller learns nothing from it.
    pub fn status(&self) -> u16 {
        401
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoToken => write!(f, "the request has no bearer token"),
            Self::Malformed(why) => write!(f, "the token isn't a JWT this reads: {why}"),
            Self::Algorithm(alg) => write!(f, "the token is signed with {alg}, not RS256"),
            Self::UnknownKey(kid) => write!(
                f,
                "the token is signed with key {kid}, which isn't among the Bot Connector's keys"
            ),
            Self::BadKey(kid) => write!(
                f,
                "the Bot Connector's key {kid} isn't an RSA key of {MIN_RSA_BITS} bits or more"
            ),
            Self::BadSignature => write!(f, "the token's signature doesn't verify"),
            Self::Issuer => write!(f, "the token wasn't issued by the Bot Connector"),
            Self::Audience => write!(f, "the token is for another app"),
            Self::Expired => write!(f, "the token has expired"),
            Self::NotYetValid => write!(f, "the token isn't valid yet"),
            Self::NotEndorsed => write!(
                f,
                "the token's key isn't endorsed for the activity's channel"
            ),
            Self::Activity(why) => write!(f, "the body isn't an activity: {why}"),
            Self::Channel => write!(f, "the activity didn't come from Teams"),
            Self::Tenant => write!(f, "the activity came from another tenant than the org's"),
            Self::ServiceUrl => write!(
                f,
                "the token's serviceurl doesn't match the activity's serviceUrl"
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// A token's parts.
struct Jwt<'a> {
    alg: String,
    kid: String,
    /// `header.payload`, what was signed.
    signed: &'a str,
    payload: Vec<u8>,
    signature: Vec<u8>,
}

impl<'a> Jwt<'a> {
    fn parse(token: &'a str) -> Result<Self, Refusal> {
        if token.len() > MAX_TOKEN {
            return Err(Refusal::Malformed("it's too long"));
        }
        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Refusal::Malformed("it isn't three parts"));
        };
        let decode = |part: &str, what| b64::decode(part).ok_or(Refusal::Malformed(what));
        let header: Value = serde_json::from_slice(&decode(header, "its header isn't base64url")?)
            .map_err(|_| Refusal::Malformed("its header isn't JSON"))?;
        if header.get("crit").is_some() {
            return Err(Refusal::Malformed("it names critical extensions"));
        }
        Ok(Self {
            alg: header["alg"].as_str().unwrap_or_default().to_owned(),
            kid: header["kid"].as_str().unwrap_or_default().to_owned(),
            signed: &token[..token.len() - signature.len() - 1],
            payload: decode(payload, "its payload isn't base64url")?,
            signature: decode(signature, "its signature isn't base64url")?,
        })
    }
}

/// The token in an `Authorization` header's value.
fn bearer(authorization: Option<&str>) -> Option<&str> {
    let value = authorization?.trim();
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty()).then_some(token)
}

/// The id of the key that signed the token in `authorization`, to find
/// the keys document that has it. Unverified.
pub fn key_id(authorization: Option<&str>) -> Option<String> {
    let token = bearer(authorization)?;
    Jwt::parse(token)
        .ok()
        .map(|jwt| jwt.kid)
        .filter(|k| !k.is_empty())
}

/// The activity in `body`, if `authorization` carries a token the Bot
/// Connector signed with one of `keys` for this app, valid at `now` (Unix
/// seconds), for this activity from the org's tenant in Teams.
pub fn verify(
    auth: &BotAuth,
    authorization: Option<&str>,
    body: &[u8],
    keys: &Jwks,
    now: i64,
) -> Result<Activity, Refusal> {
    let jwt = Jwt::parse(bearer(authorization).ok_or(Refusal::NoToken)?)?;
    if jwt.alg != "RS256" {
        return Err(Refusal::Algorithm(shown(&jwt.alg)));
    }
    let key = keys
        .find(&jwt.kid)
        .ok_or_else(|| Refusal::UnknownKey(shown(&jwt.kid)))?;
    match key.verify(jwt.signed.as_bytes(), &jwt.signature) {
        Ok(()) => {}
        Err(RsaError::BadKey) => return Err(Refusal::BadKey(shown(&jwt.kid))),
        Err(RsaError::BadSignature) => return Err(Refusal::BadSignature),
    }

    // Signed by Microsoft from here on.
    let claims: Value = serde_json::from_slice(&jwt.payload)
        .map_err(|_| Refusal::Malformed("its claims aren't JSON"))?;
    if claims["iss"].as_str() != Some(auth.issuer.as_str()) {
        return Err(Refusal::Issuer);
    }
    let for_us = match &claims["aud"] {
        Value::String(aud) => *aud == auth.app_id,
        Value::Array(auds) => auds
            .iter()
            .any(|a| a.as_str() == Some(auth.app_id.as_str())),
        _ => false,
    };
    if !for_us || auth.app_id.is_empty() {
        return Err(Refusal::Audience);
    }
    let seconds = |claim: &str| claims[claim].as_f64().map(|s| s.floor() as i64);
    let exp = seconds("exp").ok_or(Refusal::Malformed("it has no expiry"))?;
    if now > exp.saturating_add(auth.skew_secs) {
        return Err(Refusal::Expired);
    }
    if seconds("nbf").is_some_and(|nbf| now.saturating_add(auth.skew_secs) < nbf) {
        return Err(Refusal::NotYetValid);
    }

    let activity: Activity =
        serde_json::from_slice(body).map_err(|e| Refusal::Activity(e.to_string()))?;
    if activity.channel_id != auth.channel {
        return Err(Refusal::Channel);
    }
    if !key.endorsements.contains(&activity.channel_id) {
        return Err(Refusal::NotEndorsed);
    }
    let service_url = claims["serviceurl"]
        .as_str()
        .or_else(|| claims["serviceUrl"].as_str())
        .unwrap_or_default();
    if service_url.is_empty() || !service_url.eq_ignore_ascii_case(&activity.service_url) {
        return Err(Refusal::ServiceUrl);
    }
    if !activity
        .tenant()
        .is_some_and(|t| t.eq_ignore_ascii_case(&auth.tenant_id))
    {
        return Err(Refusal::Tenant);
    }
    Ok(activity)
}

/// Something from an unverified token, safe for a log line.
fn shown(text: &str) -> String {
    let safe: String = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_=+/.".contains(*c))
        .take(64)
        .collect();
    if safe.is_empty() {
        "(none)".into()
    } else {
        safe
    }
}
