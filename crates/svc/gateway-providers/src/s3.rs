//! Amazon S3 (ADR 0065 §11): where an agent's computer lives while it
//! sleeps, as chunks in the org's own bucket. Objects are put, got,
//! checked, deleted and listed by prefix, each request signed with SigV4,
//! and what's put is encrypted with the org's KMS key when it names one
//! (SSE-KMS).
//!
//! AWS's endpoint for the region is used over HTTPS, the bucket in the host
//! name. Another endpoint (an S3 interface endpoint in the VPC, or an
//! S3-compatible store) gets the bucket in the path instead. Credentials
//! come from the environment or, on an agent host, from the EC2 instance's
//! role through its metadata service (IMDSv2), fetched again before they
//! expire. A request that fails for a moment (S3's 503 SlowDown, a dropped
//! connection) is tried again a few times, after a pause.

use std::io::Read;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use glass_gateway::{ProviderError, Secret};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::http::{self, Ask, Framing, Http, Target};
use crate::sigv4::{self, uri_encode};

/// Tries of a request that fails for a moment, and the pause after the
/// first; each pause is three times the one before.
const ATTEMPTS: u32 = 4;
const FIRST_PAUSE: Duration = Duration::from_millis(100);
/// The most of a reply read that carries no object: a refusal's reason.
const REPLY_MAX: u64 = 64 * 1024;
/// The EC2 instance metadata service.
const IMDS: &str = "http://169.254.169.254";
/// Instance credentials are fetched again this long before they expire.
const REFRESH: time::Duration = time::Duration::minutes(5);

/// Which bucket, where, and what what's put is encrypted with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct S3Config {
    pub bucket: String,
    /// The bucket's region (`us-east-1`), which requests are signed for.
    pub region: String,
    /// Another endpoint than AWS's own for the region: an S3 interface
    /// endpoint in the VPC, or an S3-compatible store. Requests to it name
    /// the bucket in the path. Plain `http://` only to this machine.
    pub endpoint: Option<String>,
    /// The org's KMS key (an id, alias or ARN) to encrypt what's put with
    /// (SSE-KMS). Without one, the bucket's own default applies.
    pub kms_key_id: Option<String>,
}

/// An object a listing found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Object {
    pub key: String,
    pub size: u64,
    /// When it was last put, if S3 said.
    pub modified: Option<SystemTime>,
}

/// A client for one bucket.
#[derive(Debug)]
pub struct S3Client {
    http: Http,
    bucket: String,
    region: String,
    /// The scheme and authority requests go to.
    base: String,
    /// The Host header, as signed.
    host: String,
    /// The bucket is the path's first segment, not part of the host name.
    path_style: bool,
    kms_key_id: Option<String>,
    credentials: S3Credentials,
}

/// A request's URL and headers, signed.
struct Signed {
    url: String,
    headers: Vec<(String, String)>,
}

impl Signed {
    fn headers(&self) -> Vec<(&str, &str)> {
        self.headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }
}

/// Why a request failed: for a moment (try again) or for good.
enum Try {
    Again(String),
    Stop(String),
}

impl From<ProviderError> for Try {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::Unavailable(why) => Try::Again(why),
            ProviderError::NotConfigured(why) | ProviderError::Rejected(why) => Try::Stop(why),
        }
    }
}

impl S3Client {
    pub fn new(config: &S3Config, credentials: S3Credentials) -> Result<Self, String> {
        let bucket = config.bucket.as_str();
        if !valid_bucket(bucket) {
            return Err(format!(
                "{bucket:?} isn't an S3 bucket name: 3 to 63 lowercase letters, digits, dots \
                 and hyphens"
            ));
        }
        let region = config.region.as_str();
        if region.is_empty()
            || !region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!("{region:?} isn't an AWS region"));
        }
        let (http, base, path_style) = match config.endpoint.as_deref() {
            Some(endpoint) => {
                let base = endpoint.trim_end_matches('/');
                let target = Target::parse(&format!("{base}/"))
                    .ok_or_else(|| format!("the S3 endpoint {endpoint:?} isn't an http(s) URL"))?;
                if base
                    .split_once("://")
                    .is_some_and(|(_, rest)| rest.contains('/'))
                {
                    return Err(format!(
                        "the S3 endpoint {endpoint:?} must be a scheme and a host, without a path"
                    ));
                }
                let http = if target.tls {
                    Http::new()
                } else if crate::console::is_loopback(&target.host) {
                    Http::loopback()
                } else {
                    return Err(format!("the S3 endpoint {endpoint:?} must use HTTPS"));
                };
                (http, base.to_owned(), true)
            }
            // AWS's certificate doesn't cover a host name with a dotted
            // bucket in it.
            None if bucket.contains('.') => (
                Http::new(),
                format!("https://s3.{region}.amazonaws.com"),
                true,
            ),
            None => (
                Http::new(),
                format!("https://{bucket}.s3.{region}.amazonaws.com"),
                false,
            ),
        };
        let host = Target::parse(&format!("{base}/"))
            .map(|t| t.authority())
            .ok_or_else(|| format!("{base:?} isn't an http(s) URL"))?;
        if let Some(key) = &config.kms_key_id
            && (key.is_empty() || key.contains(['\r', '\n']))
        {
            return Err(format!("{key:?} isn't a KMS key id"));
        }
        Ok(Self {
            // A minute of silence is an outage; a large object takes as
            // long as it takes.
            http: http.for_downloads(),
            bucket: bucket.to_owned(),
            region: region.to_owned(),
            base,
            host,
            path_style,
            kms_key_id: config.kms_key_id.clone(),
            credentials,
        })
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Puts `body` at `key`, replacing whatever is there.
    pub fn put(&self, key: &str, body: &[u8]) -> Result<(), String> {
        check_key(key)?;
        let mut extra: Vec<(&str, &str)> = Vec::new();
        if let Some(kms) = &self.kms_key_id {
            extra.extend([
                ("x-amz-server-side-encryption", "aws:kms"),
                ("x-amz-server-side-encryption-aws-kms-key-id", kms.as_str()),
                // A data key per bucket, not per object: far fewer calls
                // to KMS for many small objects.
                ("x-amz-server-side-encryption-bucket-key-enabled", "true"),
            ]);
        }
        self.retrying(&format!("put {key}"), || {
            let signed = self.signed("PUT", Some(key), &[], &extra, body)?;
            let headers = signed.headers();
            let ask = Ask {
                method: "PUT",
                url: &signed.url,
                headers: &headers,
                body,
            };
            let got = self
                .http
                .download(&ask, 0, &mut std::io::sink(), REPLY_MAX)?;
            match got.status {
                200..=299 => Ok(()),
                status => Err(refused(status, &got.body)),
            }
        })
    }

    /// The object at `key`, if there is one, of at most `max` bytes.
    pub fn get(&self, key: &str, max: u64) -> Result<Option<Vec<u8>>, String> {
        check_key(key)?;
        self.retrying(&format!("get {key}"), || {
            let signed = self.signed("GET", Some(key), &[], &[], &[])?;
            let headers = signed.headers();
            let ask = Ask {
                method: "GET",
                url: &signed.url,
                headers: &headers,
                body: &[],
            };
            let mut body = Vec::new();
            let got = self.http.download(&ask, 0, &mut body, max)?;
            match got.status {
                200..=299 => Ok(Some(body)),
                404 if element(&got.body, "Code") != Some("NoSuchBucket") => Ok(None),
                status => Err(refused(status, &got.body)),
            }
        })
    }

    /// Whether `key` is there, and how long it is.
    pub fn head(&self, key: &str) -> Result<Option<u64>, String> {
        check_key(key)?;
        self.retrying(&format!("check {key}"), || {
            let signed = self.signed("HEAD", Some(key), &[], &[], &[])?;
            let headers = signed.headers();
            let mut stream = self.http.request("HEAD", &signed.url, &headers, &[])?;
            match read_head(&mut *stream)? {
                (200..=299, length) => Ok(Some(length.unwrap_or(0))),
                (404, _) => Ok(None),
                // A HEAD reply has no body to say why.
                (status, _) => Err(refused(status, "")),
            }
        })
    }

    /// Deletes `key`. Deleting what isn't there is no error.
    pub fn delete(&self, key: &str) -> Result<(), String> {
        check_key(key)?;
        self.retrying(&format!("delete {key}"), || {
            let signed = self.signed("DELETE", Some(key), &[], &[], &[])?;
            let headers = signed.headers();
            let reply = self.http.send("DELETE", &signed.url, &headers, &[])?;
            match reply.status {
                200..=299 | 404 => Ok(()),
                status => Err(refused(status, &reply.body)),
            }
        })
    }

    /// Every object whose key starts with `prefix`, in key order: a page
    /// of up to 1000 at a time (ListObjectsV2), as many pages as it takes.
    pub fn list(&self, prefix: &str) -> Result<Vec<S3Object>, String> {
        let mut objects = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self.retrying(&format!("list the keys under {prefix:?}"), || {
                let mut query = vec![("list-type", "2"), ("prefix", prefix)];
                if let Some(token) = &token {
                    query.push(("continuation-token", token));
                }
                let signed = self.signed("GET", None, &query, &[], &[])?;
                let headers = signed.headers();
                let reply = self.http.send("GET", &signed.url, &headers, &[])?;
                match reply.status {
                    200 => parse_list(&reply.body).map_err(Try::Stop),
                    status => Err(refused(status, &reply.body)),
                }
            })?;
            objects.extend(page.objects);
            match page.next {
                Some(next) if token.as_deref() == Some(next.as_str()) => {
                    return Err(format!(
                        "S3 bucket {}: listing {prefix:?} went round in a circle",
                        self.bucket
                    ));
                }
                Some(next) => token = Some(next),
                None => return Ok(objects),
            }
        }
    }

    /// Runs `call` until it succeeds, fails for good, or has been tried
    /// [`ATTEMPTS`] times.
    fn retrying<T>(
        &self,
        doing: &str,
        mut call: impl FnMut() -> Result<T, Try>,
    ) -> Result<T, String> {
        let mut pause = FIRST_PAUSE;
        let mut attempt = 1;
        loop {
            match call() {
                Ok(done) => return Ok(done),
                Err(Try::Again(_)) if attempt < ATTEMPTS => {
                    std::thread::sleep(pause);
                    pause *= 3;
                    attempt += 1;
                }
                Err(Try::Again(why) | Try::Stop(why)) => {
                    return Err(format!(
                        "S3 bucket {}: couldn't {doing}: {why}",
                        self.bucket
                    ));
                }
            }
        }
    }

    /// The URL and the signed headers for a request: `key` in the bucket,
    /// or the bucket itself, with `query` and `extra` headers.
    fn signed(
        &self,
        method: &str,
        key: Option<&str>,
        query: &[(&str, &str)],
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Signed, Try> {
        let now = OffsetDateTime::now_utc();
        let keys = self.credentials.keys(now).map_err(Try::Stop)?;
        // S3's canonical path is the path as sent: each segment encoded
        // once, never normalized.
        let mut path = String::new();
        if self.path_style {
            path.push('/');
            path.push_str(&uri_encode(&self.bucket));
        }
        if let Some(key) = key {
            path.push('/');
            path.push_str(&key.split('/').map(uri_encode).collect::<Vec<_>>().join("/"));
        }
        if path.is_empty() {
            path.push('/');
        }
        let mut pairs: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (uri_encode(k), uri_encode(v)))
            .collect();
        pairs.sort();
        let query = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let payload = hex::encode(Sha256::digest(body));
        let mut headers = extra.to_vec();
        headers.push(("x-amz-content-sha256", &payload));
        let added = sigv4::sign(
            &sigv4::Request {
                method,
                host: &self.host,
                canonical_path: &path,
                query: &query,
                headers: &headers,
                body,
            },
            &sigv4::Credentials {
                access_key_id: keys.access_key_id.expose(),
                secret_access_key: keys.secret_access_key.expose(),
                session_token: keys.session_token.as_ref().map(Secret::expose),
            },
            &self.region,
            "s3",
            now,
        );
        let mut all: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        all.extend(added.into_iter().map(|(k, v)| (k.to_owned(), v)));
        let url = if query.is_empty() {
            format!("{}{path}", self.base)
        } else {
            format!("{}{path}?{query}", self.base)
        };
        Ok(Signed { url, headers: all })
    }
}

fn valid_bucket(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name.ends_with(|c: char| c.is_ascii_alphanumeric())
}

fn check_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 1024 {
        return Err(format!("{key:?} isn't an S3 key: 1 to 1024 bytes"));
    }
    Ok(())
}

/// What S3 said when it refused, from its error document where it sent
/// one.
fn refused(status: u16, body: &str) -> Try {
    let why = match (element(body, "Code"), element(body, "Message")) {
        (Some(code), Some(message)) => {
            format!("HTTP {status}, {}: {}", unescape(code), unescape(message))
        }
        (Some(code), None) => format!("HTTP {status}, {}", unescape(code)),
        _ => format!("HTTP {status}"),
    };
    // Throttling (503 SlowDown), timeouts and S3's own errors pass.
    if matches!(status, 408 | 429 | 500 | 502 | 503 | 504) {
        Try::Again(why)
    } else {
        Try::Stop(why)
    }
}

/// Reads a reply's head and nothing more: a HEAD reply says how long the
/// object is, and has no body.
fn read_head<R: Read + ?Sized>(stream: &mut R) -> Result<(u16, Option<u64>), Try> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(head) = http::head(&buf, usize::MAX)? {
            let length = match head.framing {
                Framing::Length(n) => Some(n as u64),
                _ => None,
            };
            return Ok((head.status, length));
        }
        if buf.len() > 64 * 1024 {
            return Err(Try::Stop("the reply's head is too long".into()));
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(Try::Again(
                    "the connection closed before the reply's head was complete".into(),
                ));
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => {
                return Err(Try::Again(format!(
                    "network error reading the reply: {}",
                    e.kind()
                )));
            }
        }
    }
}

/// One page of a listing.
#[derive(Debug)]
struct Page {
    objects: Vec<S3Object>,
    /// The continuation token for the next page, if there is one.
    next: Option<String>,
}

/// A ListObjectsV2 reply (`ListBucketResult`).
fn parse_list(xml: &str) -> Result<Page, String> {
    let unreadable = |what: &str| format!("the listing's {what} can't be read");
    let mut objects = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Contents>") {
        let after = &rest[start + "<Contents>".len()..];
        let end = after
            .find("</Contents>")
            .ok_or_else(|| unreadable("objects"))?;
        let block = &after[..end];
        let key = element(block, "Key").ok_or_else(|| unreadable("keys"))?;
        let size = element(block, "Size")
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| unreadable("sizes"))?;
        let modified = element(block, "LastModified")
            .and_then(|s| OffsetDateTime::parse(s.trim(), &Rfc3339).ok())
            .map(SystemTime::from);
        objects.push(S3Object {
            key: unescape(key),
            size,
            modified,
        });
        rest = &after[end + "</Contents>".len()..];
    }
    let next = if element(xml, "IsTruncated").is_some_and(|v| v.trim() == "true") {
        let token = element(xml, "NextContinuationToken")
            .ok_or_else(|| unreadable("continuation token"))?;
        Some(unescape(token))
    } else {
        None
    };
    Ok(Page { objects, next })
}

/// The text of the first `<name>` element in `xml`.
fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(&xml[start..end])
}

/// XML's five entities and character references, as text.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| entity.strip_prefix('#').map(str::parse))
                .and_then(Result::ok)
                .and_then(char::from_u32),
        };
        match ch {
            Some(ch) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// AWS credentials for S3: given outright, from the environment, or this
/// EC2 instance's role.
#[derive(Debug)]
pub struct S3Credentials {
    source: Source,
}

#[derive(Debug)]
enum Source {
    Given(Keys),
    /// The instance's role, through IMDSv2 at `base`; the last keys
    /// fetched, until they're about to expire.
    Instance {
        http: Http,
        base: String,
        cached: Mutex<Option<Keys>>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Keys {
    pub(crate) access_key_id: Secret,
    pub(crate) secret_access_key: Secret,
    pub(crate) session_token: Option<Secret>,
    pub(crate) expires: Option<OffsetDateTime>,
}

/// The instance metadata service's credentials document.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InstanceKeys {
    code: String,
    access_key_id: String,
    secret_access_key: String,
    token: String,
    expiration: String,
}

impl S3Credentials {
    /// Keys given outright: an S3-compatible store's, or a test's. A
    /// session token goes with temporary ones.
    pub fn new(access_key_id: &str, secret_access_key: &str, session_token: Option<&str>) -> Self {
        Self {
            source: Source::Given(Keys {
                access_key_id: Secret::new(access_key_id),
                secret_access_key: Secret::new(secret_access_key),
                session_token: session_token.map(Secret::new),
                expires: None,
            }),
        }
    }

    /// `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and, with temporary
    /// keys, `AWS_SESSION_TOKEN`, from this process's environment.
    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// This EC2 instance's role, through its metadata service (IMDSv2):
    /// fetched when first needed, and again shortly before they expire.
    pub fn instance_role() -> Self {
        Self::instance_role_at(IMDS)
    }

    /// The environment's keys if it has them, else this instance's role:
    /// an agent host on EC2 needs no keys of its own.
    pub fn from_host() -> Result<Self, String> {
        Self::from_host_vars(|name| std::env::var(name).ok())
    }

    fn from_host_vars(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        if get("AWS_ACCESS_KEY_ID").is_some_and(|v| !v.is_empty()) {
            return Self::from_vars(get);
        }
        Ok(Self::instance_role_at(&metadata_base(get)?))
    }

    fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |name: &str| get(name).filter(|v| !v.is_empty());
        let id = get("AWS_ACCESS_KEY_ID").ok_or("AWS_ACCESS_KEY_ID isn't set")?;
        let secret = get("AWS_SECRET_ACCESS_KEY")
            .ok_or("AWS_ACCESS_KEY_ID is set, but AWS_SECRET_ACCESS_KEY isn't")?;
        Ok(Self::new(&id, &secret, get("AWS_SESSION_TOKEN").as_deref()))
    }

    /// The instance's role through a metadata service at `base`.
    fn instance_role_at(base: &str) -> Self {
        Self {
            source: Source::Instance {
                http: metadata_http(),
                base: base.trim_end_matches('/').to_owned(),
                cached: Mutex::new(None),
            },
        }
    }

    /// Keys good for a request at `now`.
    /// The keys in force now, as other AWS clients take them: the
    /// metering client signs with the instance role too.
    pub fn aws_credentials(&self) -> Result<crate::bedrock::AwsCredentials, String> {
        let keys = self.keys(OffsetDateTime::now_utc())?;
        Ok(crate::bedrock::AwsCredentials::from_parts(
            keys.access_key_id,
            keys.secret_access_key,
            keys.session_token,
            keys.expires,
        ))
    }

    pub(crate) fn keys(&self, now: OffsetDateTime) -> Result<Keys, String> {
        match &self.source {
            Source::Given(keys) => Ok(keys.clone()),
            Source::Instance { http, base, cached } => {
                let mut cached = cached.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(keys) = cached
                    .as_ref()
                    .filter(|k| k.expires.is_none_or(|at| at - now > REFRESH))
                {
                    return Ok(keys.clone());
                }
                let keys = from_instance(http, base)?;
                *cached = Some(keys.clone());
                Ok(keys)
            }
        }
    }
}

/// The metadata service's address: AWS's, or, by AWS's own name for
/// another (`AWS_EC2_METADATA_SERVICE_ENDPOINT`), a stand-in on this
/// machine (deploy/local), never anywhere else.
fn metadata_base(get: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let Some(endpoint) = get("AWS_EC2_METADATA_SERVICE_ENDPOINT").filter(|v| !v.is_empty()) else {
        return Ok(IMDS.to_owned());
    };
    let base = endpoint.trim_end_matches('/');
    let local = Target::parse(&format!("{base}/"))
        .is_some_and(|t| !t.tls && crate::console::is_loopback(&t.host));
    if !local {
        return Err(format!(
            "AWS_EC2_METADATA_SERVICE_ENDPOINT ({endpoint:?}) must be http:// on this \
             machine's loopback address: a stand-in for development"
        ));
    }
    Ok(base.to_owned())
}

fn metadata_http() -> Http {
    let mut http = Http::loopback();
    // On a computer that isn't an EC2 instance, nothing answers.
    http.idle = Duration::from_secs(2);
    http.total = Duration::from_secs(5);
    http
}

fn reason(e: ProviderError) -> String {
    match e {
        ProviderError::Unavailable(why)
        | ProviderError::NotConfigured(why)
        | ProviderError::Rejected(why) => why,
    }
}

/// A session token from the metadata service at `base` (IMDSv2).
fn metadata_token(http: &Http, base: &str) -> Result<String, String> {
    let mut token = Vec::new();
    let got = http
        .download(
            &Ask {
                method: "PUT",
                url: &format!("{base}/latest/api/token"),
                headers: &[("X-aws-ec2-metadata-token-ttl-seconds", "21600")],
                body: &[],
            },
            0,
            &mut token,
            4096,
        )
        .map_err(reason)?;
    if got.status != 200 {
        return Err(format!("it refused a session token (HTTP {})", got.status));
    }
    let token = String::from_utf8(token).map_err(|_| "its session token isn't text")?;
    Ok(token.trim().to_owned())
}

/// This instance's `path` under `/latest/meta-data/`, or `None` where it
/// has none (AWS answers 404): its Auto Scaling lifecycle state, say. On
/// a computer that isn't an EC2 instance, nothing answers: an error.
pub fn instance_metadata(path: &str) -> Result<Option<String>, String> {
    instance_metadata_at(&metadata_base(|name| std::env::var(name).ok())?, path)
}

fn instance_metadata_at(base: &str, path: &str) -> Result<Option<String>, String> {
    let fail = |why: String| format!("couldn't read {path} from this instance's metadata: {why}");
    let http = metadata_http();
    let token = metadata_token(&http, base).map_err(fail)?;
    let reply = http
        .send(
            "GET",
            &format!("{base}/latest/meta-data/{path}"),
            &[("X-aws-ec2-metadata-token", &token)],
            &[],
        )
        .map_err(|e| fail(reason(e)))?;
    match reply.status {
        200 => Ok(Some(reply.body.trim().to_owned())),
        404 => Ok(None),
        status => Err(fail(format!("HTTP {status}"))),
    }
}

/// The instance role's keys, from the metadata service at `base`: a
/// session token first (IMDSv2), then the role's name, then its keys.
fn from_instance(http: &Http, base: &str) -> Result<Keys, String> {
    let fail = |why: String| {
        format!("couldn't get this instance's AWS credentials from its metadata service: {why}")
    };
    let token = metadata_token(http, base).map_err(fail)?;
    let headers = [("X-aws-ec2-metadata-token", token.as_str())];
    let roles = http
        .send(
            "GET",
            &format!("{base}/latest/meta-data/iam/security-credentials/"),
            &headers,
            &[],
        )
        .map_err(|e| fail(reason(e)))?;
    match roles.status {
        200 => {}
        404 => return Err(fail("this instance has no IAM role".into())),
        status => return Err(fail(format!("listing its role, HTTP {status}"))),
    }
    let role = roles
        .body
        .lines()
        .next()
        .map(str::trim)
        .filter(|r| {
            !r.is_empty()
                && r.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+=,.@_-".contains(&b))
        })
        .ok_or_else(|| fail("it named no role".into()))?;
    let reply = http
        .send(
            "GET",
            &format!("{base}/latest/meta-data/iam/security-credentials/{role}"),
            &headers,
            &[],
        )
        .map_err(|e| fail(reason(e)))?;
    if reply.status != 200 {
        return Err(fail(format!(
            "getting the role's keys, HTTP {}",
            reply.status
        )));
    }
    let keys: InstanceKeys = serde_json::from_str(&reply.body)
        .map_err(|_| fail("its answer isn't a credentials document".into()))?;
    if keys.code != "Success" {
        return Err(fail(format!("it said {:?}", keys.code)));
    }
    let expires = OffsetDateTime::parse(&keys.expiration, &Rfc3339)
        .map_err(|_| fail("the keys' expiration isn't an RFC 3339 time".into()))?;
    Ok(Keys {
        access_key_id: Secret::new(keys.access_key_id),
        secret_access_key: Secret::new(keys.secret_access_key),
        session_token: Some(Secret::new(keys.token)),
        expires: Some(expires),
    })
}

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! A stand-in for S3 on this machine's loopback address, for tests:
    //! one bucket ([`BUCKET`]) in memory, addressed by path, and every
    //! request's SigV4 signature worked out again from [`SECRET`] and
    //! compared, as S3 does. A request signed wrong is refused (403
    //! SignatureDoesNotMatch) and remembered with why.

    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::thread;
    use std::time::{Duration, SystemTime};

    use sha2::{Digest, Sha256};
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    use super::{S3Client, S3Config, S3Credentials};
    use crate::sigv4::{hmac, uri_encode};

    pub const ACCESS_KEY_ID: &str = "AKIDSTANDIN";
    pub const SECRET: &str = "stand-in-secret-key";
    pub const REGION: &str = "us-east-1";
    pub const BUCKET: &str = "glass-computers";

    /// A request the stand-in got.
    #[derive(Debug, Clone)]
    pub struct Seen {
        pub method: String,
        /// The path and query, as sent.
        pub target: String,
        /// Names in lower case.
        pub headers: Vec<(String, String)>,
        /// Why it was refused, if it was signed wrong.
        pub refused: Option<String>,
    }

    impl Seen {
        pub fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }
    }

    #[derive(Debug, Default)]
    struct State {
        objects: BTreeMap<String, (Vec<u8>, SystemTime)>,
        seen: Vec<Seen>,
        page: usize,
        /// Statuses to answer the next requests with.
        failures: Vec<u16>,
    }

    /// A running stand-in, stopped when dropped.
    #[derive(Debug)]
    pub struct StandIn {
        addr: SocketAddr,
        state: Arc<Mutex<State>>,
        stop: Arc<AtomicBool>,
    }

    impl StandIn {
        pub fn start() -> Self {
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("a loopback port for the S3 stand-in");
            let addr = listener.local_addr().expect("the S3 stand-in's address");
            let state = Arc::new(Mutex::new(State {
                page: 1000,
                ..State::default()
            }));
            let stop = Arc::new(AtomicBool::new(false));
            let (shared, stopping) = (Arc::clone(&state), Arc::clone(&stop));
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let state = Arc::clone(&shared);
                    thread::spawn(move || serve(stream, &state));
                }
            });
            Self { addr, state, stop }
        }

        /// Its address, as an S3 endpoint.
        pub fn endpoint(&self) -> String {
            format!("http://{}", self.addr)
        }

        /// Its bucket, at its endpoint.
        pub fn config(&self) -> S3Config {
            S3Config {
                bucket: BUCKET.into(),
                region: REGION.into(),
                endpoint: Some(self.endpoint()),
                kms_key_id: None,
            }
        }

        /// Keys it takes.
        pub fn credentials() -> S3Credentials {
            S3Credentials::new(ACCESS_KEY_ID, SECRET, None)
        }

        /// A client for its bucket.
        pub fn client(&self) -> S3Client {
            S3Client::new(&self.config(), Self::credentials()).expect("a client for the stand-in")
        }

        /// The keys it holds, in order.
        pub fn keys(&self) -> Vec<String> {
            self.state().objects.keys().cloned().collect()
        }

        /// The object at `key`.
        pub fn object(&self, key: &str) -> Option<Vec<u8>> {
            self.state()
                .objects
                .get(key)
                .map(|(bytes, _)| bytes.clone())
        }

        /// Puts `bytes` at `key` behind the client's back, as anyone with
        /// access to the bucket could.
        pub fn replace(&self, key: &str, bytes: &[u8]) {
            self.state()
                .objects
                .insert(key.to_owned(), (bytes.to_vec(), SystemTime::now()));
        }

        /// The requests it got, in order.
        pub fn seen(&self) -> Vec<Seen> {
            self.state().seen.clone()
        }

        /// Lists at most `n` keys a page.
        pub fn set_page_size(&self, n: usize) {
            self.state().page = n.max(1);
        }

        /// Answers the next requests with these statuses, as S3 does when
        /// it's busy (503 SlowDown).
        pub fn fail_next(&self, statuses: &[u16]) {
            self.state().failures.extend_from_slice(statuses);
        }

        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl Drop for StandIn {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Wakes the accepting thread, which then sees it's to stop.
            let _ = TcpStream::connect(self.addr);
        }
    }

    type Request = (String, String, Vec<(String, String)>, Vec<u8>);

    fn serve(mut stream: TcpStream, state: &Mutex<State>) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let Some((method, target, headers, body)) = read_request(&mut stream) else {
            return;
        };
        let reply = answer(state, &method, &target, &headers, &body);
        let _ = stream.write_all(&reply);
    }

    fn read_request(stream: &mut TcpStream) -> Option<Request> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        let (method, target, headers, at) = loop {
            let mut parsed = [httparse::EMPTY_HEADER; 64];
            let mut request = httparse::Request::new(&mut parsed);
            if let Ok(httparse::Status::Complete(at)) = request.parse(&buf) {
                let headers: Vec<(String, String)> = request
                    .headers
                    .iter()
                    .map(|h| {
                        (
                            h.name.to_ascii_lowercase(),
                            String::from_utf8_lossy(h.value).trim().to_owned(),
                        )
                    })
                    .collect();
                break (
                    request.method?.to_owned(),
                    request.path?.to_owned(),
                    headers,
                    at,
                );
            }
            let n = stream.read(&mut chunk).ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let length: usize = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        let mut body = buf.split_off(at);
        while body.len() < length {
            let n = stream.read(&mut chunk).ok()?;
            if n == 0 {
                return None;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(length);
        Some((method, target, headers, body))
    }

    fn answer(
        state: &Mutex<State>,
        method: &str,
        target: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Vec<u8> {
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        let refused = verify(method, target, headers, body).err();
        state.seen.push(Seen {
            method: method.to_owned(),
            target: target.to_owned(),
            headers: headers.to_vec(),
            refused: refused.clone(),
        });
        if let Some(why) = refused {
            return reply(403, &error("SignatureDoesNotMatch", &why), false);
        }
        if !state.failures.is_empty() {
            let status = state.failures.remove(0);
            return reply(
                status,
                &error("SlowDown", "Please reduce your request rate."),
                false,
            );
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let path = decode(path);
        let Some(rest) = path.strip_prefix(&format!("/{BUCKET}")) else {
            return reply(
                404,
                &error("NoSuchBucket", "The specified bucket does not exist"),
                method == "HEAD",
            );
        };
        let missing = || error("NoSuchKey", "The specified key does not exist.");
        match (method, rest.strip_prefix('/')) {
            ("GET", None | Some("")) => list(&state, query),
            ("PUT", Some(key)) => {
                state
                    .objects
                    .insert(key.to_owned(), (body.to_vec(), SystemTime::now()));
                reply(200, b"", false)
            }
            ("GET", Some(key)) => match state.objects.get(key) {
                Some((bytes, _)) => reply(200, bytes, false),
                None => reply(404, &missing(), false),
            },
            ("HEAD", Some(key)) => match state.objects.get(key) {
                Some((bytes, _)) => reply(200, bytes, true),
                None => reply(404, b"", true),
            },
            ("DELETE", Some(key)) => {
                state.objects.remove(key);
                reply(204, b"", false)
            }
            _ => reply(405, &error("MethodNotAllowed", "No."), false),
        }
    }

    fn list(state: &State, query: &str) -> Vec<u8> {
        let params = params(query);
        let get = |name: &str| {
            params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        if get("list-type") != Some("2") {
            return reply(400, &error("InvalidArgument", "ListObjectsV2 only"), false);
        }
        let prefix = get("prefix").unwrap_or("");
        // Its tokens have `+`, `/` and `=` in them, as S3's do.
        let after = get("continuation-token")
            .and_then(|t| t.strip_prefix("+/")?.strip_suffix('='))
            .and_then(|t| hex::decode(t).ok())
            .map(|k| String::from_utf8_lossy(&k).into_owned())
            .unwrap_or_default();
        let matching: Vec<_> = state
            .objects
            .iter()
            .filter(|(k, _)| k.starts_with(prefix) && k.as_str() > after.as_str())
            .collect();
        let page = &matching[..matching.len().min(state.page)];
        let truncated = matching.len() > page.len();
        let mut xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult \
             xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>{BUCKET}</Name>\
             <Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys>\
             <IsTruncated>{truncated}</IsTruncated>",
            escape(prefix),
            page.len()
        );
        if let Some(token) = get("continuation-token") {
            xml.push_str(&format!(
                "<ContinuationToken>{}</ContinuationToken>",
                escape(token)
            ));
        }
        for (key, (bytes, modified)) in page {
            let modified = OffsetDateTime::from(*modified)
                .format(&Rfc3339)
                .unwrap_or_default();
            xml.push_str(&format!(
                "<Contents><Key>{}</Key><LastModified>{modified}</LastModified>\
                 <ETag>&quot;0&quot;</ETag><Size>{}</Size><StorageClass>STANDARD\
                 </StorageClass></Contents>",
                escape(key),
                bytes.len()
            ));
        }
        if let Some((last, _)) = page.last().filter(|_| truncated) {
            xml.push_str(&format!(
                "<NextContinuationToken>+/{}=</NextContinuationToken>",
                hex::encode(last)
            ));
        }
        xml.push_str("</ListBucketResult>");
        reply(200, xml.as_bytes(), false)
    }

    /// Checks a request's signature as S3 does: works out the canonical
    /// request and the signing key again, and compares.
    fn verify(
        method: &str,
        target: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<(), String> {
        let header = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        let auth = header("authorization").ok_or("no Authorization header")?;
        let fields = auth
            .strip_prefix("AWS4-HMAC-SHA256 ")
            .ok_or("the Authorization header isn't AWS4-HMAC-SHA256")?;
        let (mut credential, mut signed, mut signature) = (None, None, None);
        for field in fields.split(',') {
            let (name, value) = field
                .trim()
                .split_once('=')
                .ok_or("the Authorization header is malformed")?;
            match name {
                "Credential" => credential = Some(value),
                "SignedHeaders" => signed = Some(value),
                "Signature" => signature = Some(value),
                _ => return Err(format!("the Authorization header has {name}")),
            }
        }
        let credential = credential.ok_or("no Credential")?;
        let signed = signed.ok_or("no SignedHeaders")?;
        let signature = signature.ok_or("no Signature")?;
        let date = header("x-amz-date").ok_or("no x-amz-date header")?;
        if date.len() != 16 || date.as_bytes()[8] != b'T' || !date.ends_with('Z') {
            return Err(format!("x-amz-date {date:?} isn't YYYYMMDDTHHMMSSZ"));
        }
        let day = &date[..8];
        let scope = format!("{day}/{REGION}/s3/aws4_request");
        if credential != format!("{ACCESS_KEY_ID}/{scope}") {
            return Err(format!(
                "the credential {credential:?} isn't {ACCESS_KEY_ID}/{scope}"
            ));
        }
        let payload = header("x-amz-content-sha256").ok_or("no x-amz-content-sha256 header")?;
        if payload != hex::encode(Sha256::digest(body)) {
            return Err("x-amz-content-sha256 isn't the body's SHA-256".into());
        }
        let names: Vec<&str> = signed.split(';').collect();
        if !names.windows(2).all(|w| w[0] < w[1]) {
            return Err("SignedHeaders aren't in order".into());
        }
        for needed in ["host", "x-amz-content-sha256", "x-amz-date"] {
            if !names.contains(&needed) {
                return Err(format!("{needed} isn't signed"));
            }
        }
        for (name, _) in headers {
            if name.starts_with("x-amz-") && !names.contains(&name.as_str()) {
                return Err(format!("{name} isn't signed"));
            }
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let path = decode(path)
            .split('/')
            .map(uri_encode)
            .collect::<Vec<_>>()
            .join("/");
        let mut pairs: Vec<(String, String)> = params(query)
            .iter()
            .map(|(k, v)| (uri_encode(k), uri_encode(v)))
            .collect();
        pairs.sort();
        let query = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let mut canonical_headers = String::new();
        for name in &names {
            let value = header(name).ok_or_else(|| format!("{name} is signed but not sent"))?;
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            canonical_headers.push_str(&format!("{name}:{value}\n"));
        }
        let canonical =
            format!("{method}\n{path}\n{query}\n{canonical_headers}\n{signed}\n{payload}");
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{date}\n{scope}\n{}",
            hex::encode(Sha256::digest(canonical.as_bytes()))
        );
        let key = hmac(format!("AWS4{SECRET}").as_bytes(), day.as_bytes());
        let key = hmac(&key, REGION.as_bytes());
        let key = hmac(&key, b"s3");
        let key = hmac(&key, b"aws4_request");
        if signature != hex::encode(hmac(&key, to_sign.as_bytes())) {
            return Err("the signature doesn't match".into());
        }
        Ok(())
    }

    fn params(query: &str) -> Vec<(String, String)> {
        query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (decode(k), decode(v))
            })
            .collect()
    }

    /// Percent-decoding.
    fn decode(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            match (bytes[i], hex) {
                (b'%', Some(byte)) => {
                    out.push(byte);
                    i += 3;
                }
                (byte, _) => {
                    out.push(byte);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    fn error(code: &str, message: &str) -> Vec<u8> {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{code}</Code>\
             <Message>{}</Message></Error>",
            escape(message)
        )
        .into_bytes()
    }

    /// A reply: `body`, or for a HEAD request only its length.
    fn reply(status: u16, body: &[u8], head_only: bool) -> Vec<u8> {
        let mut head = format!("HTTP/1.1 {status} Stand-in\r\nconnection: close\r\n");
        if status != 204 {
            head.push_str(&format!("content-length: {}\r\n", body.len()));
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        if !head_only && status != 204 {
            out.extend_from_slice(body);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use time::macros::datetime;

    use super::testing::{ACCESS_KEY_ID, StandIn};
    use super::*;
    use crate::http::fake;

    #[test]
    fn adr0065_s3_puts_gets_checks_deletes_and_lists_with_signed_requests() {
        let s3 = StandIn::start();
        s3.set_page_size(2);
        let client = s3.client();
        let odd = "chunks/a key & more+ü";
        let keys = [
            "chunks/aa",
            "chunks/bb",
            odd,
            "computers/dana/versions/1.json",
            "chunks/dd",
            "chunks/ee",
        ];
        for (i, key) in keys.iter().enumerate() {
            client.put(key, &vec![i as u8; i * 100]).unwrap();
        }
        assert_eq!(client.head("chunks/bb").unwrap(), Some(100));
        assert_eq!(
            client.head("chunks/aa").unwrap(),
            Some(0),
            "an empty object"
        );
        assert_eq!(client.head("chunks/zz").unwrap(), None);
        assert_eq!(
            client.get("chunks/dd", 1 << 20).unwrap(),
            Some(vec![4; 400])
        );
        assert_eq!(client.get(odd, 1 << 20).unwrap(), Some(vec![2; 200]));
        assert_eq!(
            s3.object(odd),
            Some(vec![2; 200]),
            "stored under its own name"
        );
        assert_eq!(client.get("chunks/zz", 1 << 20).unwrap(), None);

        // Five keys at two a page: three requests, the later two going on
        // from where the one before stopped.
        let listed = client.list("chunks/").unwrap();
        let names: Vec<&str> = listed.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(
            names,
            [
                "chunks/a key & more+ü",
                "chunks/aa",
                "chunks/bb",
                "chunks/dd",
                "chunks/ee"
            ]
        );
        assert_eq!(listed[2].size, 100);
        assert!(listed.iter().all(|o| o.modified.is_some()));
        let pages: Vec<_> = s3
            .seen()
            .into_iter()
            .filter(|r| r.target.contains("list-type=2"))
            .collect();
        assert_eq!(pages.len(), 3);
        assert!(!pages[0].target.contains("continuation-token"));
        assert!(
            pages[1].target.contains("continuation-token=%2B%2F"),
            "{}",
            pages[1].target
        );

        client.delete("chunks/bb").unwrap();
        client.delete("chunks/bb").unwrap();
        assert_eq!(client.head("chunks/bb").unwrap(), None);
        assert_eq!(client.list("computers/").unwrap().len(), 1);
        assert_eq!(client.list("nothing/").unwrap(), []);
        assert_eq!(client.list("").unwrap().len(), 5);

        // Every request was signed, and the stand-in found each signature
        // right.
        let seen = s3.seen();
        assert!(seen.len() > 15);
        for request in seen {
            assert_eq!(
                request.refused, None,
                "{} {}",
                request.method, request.target
            );
            let auth = request.header("authorization").unwrap();
            assert!(
                auth.starts_with(&format!("AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/")),
                "{auth}"
            );
            assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
            assert!(request.header("x-amz-date").is_some());
            assert_eq!(request.header("x-amz-content-sha256").unwrap().len(), 64);
        }
    }

    #[test]
    fn adr0065_s3_refuses_a_request_signed_with_the_wrong_secret() {
        let s3 = StandIn::start();
        let client = S3Client::new(
            &s3.config(),
            S3Credentials::new(ACCESS_KEY_ID, "not-the-secret", None),
        )
        .unwrap();
        let err = client.put("chunks/aa", b"x").unwrap_err();
        assert!(err.contains("HTTP 403, SignatureDoesNotMatch"), "{err}");
        assert!(
            err.starts_with("S3 bucket glass-computers: couldn't put chunks/aa"),
            "{err}"
        );
        assert_eq!(s3.seen().len(), 1, "a refusal isn't tried again");
        assert!(s3.keys().is_empty());
    }

    #[test]
    fn adr0065_s3_puts_with_the_orgs_kms_key_and_a_session_token() {
        let s3 = StandIn::start();
        let config = S3Config {
            kms_key_id: Some("alias/glass-computers".into()),
            ..s3.config()
        };
        let client = S3Client::new(
            &config,
            S3Credentials::new(ACCESS_KEY_ID, testing::SECRET, Some("session-token")),
        )
        .unwrap();
        client.put("chunks/aa", b"secret stuff").unwrap();
        assert_eq!(
            client.get("chunks/aa", 100).unwrap().unwrap(),
            b"secret stuff"
        );
        let seen = s3.seen();
        assert!(seen.iter().all(|r| r.refused.is_none()), "{seen:?}");
        let put = &seen[0];
        assert_eq!(put.header("x-amz-server-side-encryption"), Some("aws:kms"));
        assert_eq!(
            put.header("x-amz-server-side-encryption-aws-kms-key-id"),
            Some("alias/glass-computers")
        );
        assert_eq!(put.header("x-amz-security-token"), Some("session-token"));
        let auth = put.header("authorization").unwrap();
        assert!(
            auth.contains(
                "SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token;\
                 x-amz-server-side-encryption;x-amz-server-side-encryption-aws-kms-key-id;\
                 x-amz-server-side-encryption-bucket-key-enabled,"
            ),
            "{auth}"
        );
        assert_eq!(
            seen[1].header("x-amz-server-side-encryption"),
            None,
            "only puts"
        );
    }

    #[test]
    fn adr0065_s3_tries_again_when_s3_is_busy() {
        let s3 = StandIn::start();
        s3.fail_next(&[503, 500]);
        let client = s3.client();
        client.put("chunks/aa", b"x").unwrap();
        assert_eq!(s3.seen().len(), 3);
        assert_eq!(s3.object("chunks/aa").unwrap(), b"x");
    }

    #[test]
    fn adr0065_s3_names_the_bucket_in_the_host_or_in_the_path() {
        let creds = || S3Credentials::new("AKID", "secret", None);
        let config = |bucket: &str, endpoint: Option<&str>| S3Config {
            bucket: bucket.into(),
            region: "eu-central-1".into(),
            endpoint: endpoint.map(str::to_owned),
            kms_key_id: None,
        };
        let aws = S3Client::new(&config("glass-computers", None), creds()).unwrap();
        assert_eq!(
            aws.base,
            "https://glass-computers.s3.eu-central-1.amazonaws.com"
        );
        assert_eq!(aws.host, "glass-computers.s3.eu-central-1.amazonaws.com");
        assert!(!aws.path_style);
        let dotted = S3Client::new(&config("glass.computers", None), creds()).unwrap();
        assert_eq!(dotted.base, "https://s3.eu-central-1.amazonaws.com");
        assert!(dotted.path_style);
        let vpce = S3Client::new(
            &config(
                "glass-computers",
                Some("https://bucket.vpce-0abc.s3.eu-central-1.vpce.amazonaws.com/"),
            ),
            creds(),
        )
        .unwrap();
        assert_eq!(
            vpce.host,
            "bucket.vpce-0abc.s3.eu-central-1.vpce.amazonaws.com"
        );
        assert!(vpce.path_style);
        assert!(
            S3Client::new(
                &config("glass-computers", Some("http://127.0.0.1:9000")),
                creds()
            )
            .is_ok()
        );

        let refused = |config: S3Config| S3Client::new(&config, creds()).unwrap_err();
        assert!(
            refused(config(
                "glass-computers",
                Some("http://minio.internal:9000")
            ))
            .contains("must use HTTPS")
        );
        assert!(
            refused(config(
                "glass-computers",
                Some("https://s3.example.com/sub")
            ))
            .contains("without a path")
        );
        assert!(refused(config("Glass_Computers", None)).contains("isn't an S3 bucket name"));
        assert!(
            refused(S3Config {
                region: String::new(),
                ..config("glass-computers", None)
            })
            .contains("isn't an AWS region")
        );
    }

    #[test]
    fn adr0065_s3_credentials_come_from_the_environment() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let now = datetime!(2026-10-27 12:00 UTC);
        let creds = S3Credentials::from_vars(vars(&[
            ("AWS_ACCESS_KEY_ID", "ASIAEXAMPLE"),
            ("AWS_SECRET_ACCESS_KEY", "wJalrXUtnFEMIEXAMPLEKEY"),
            ("AWS_SESSION_TOKEN", "token"),
        ]))
        .unwrap();
        let keys = creds.keys(now).unwrap();
        assert_eq!(keys.access_key_id.expose(), "ASIAEXAMPLE");
        assert_eq!(keys.session_token.unwrap().expose(), "token");
        assert!(!format!("{creds:?}").contains("EXAMPLEKEY"));

        let err = S3Credentials::from_vars(vars(&[("AWS_ACCESS_KEY_ID", "AKID")])).unwrap_err();
        assert!(err.contains("AWS_SECRET_ACCESS_KEY isn't"), "{err}");
        assert!(S3Credentials::from_vars(vars(&[])).is_err());
        // Without keys in the environment, an agent host uses its role.
        let host = S3Credentials::from_host_vars(vars(&[])).unwrap();
        assert!(matches!(&host.source, Source::Instance { base, .. } if base == IMDS));
        // A stand-in metadata service on this machine (deploy/local), and
        // nowhere else.
        let local = S3Credentials::from_host_vars(vars(&[(
            "AWS_EC2_METADATA_SERVICE_ENDPOINT",
            "http://127.0.0.1:7611/",
        )]))
        .unwrap();
        assert!(
            matches!(&local.source, Source::Instance { base, .. } if base == "http://127.0.0.1:7611")
        );
        for elsewhere in [
            "http://imds.example.com",
            "https://127.0.0.1:7611",
            "127.0.0.1",
        ] {
            let err = S3Credentials::from_host_vars(move |name: &str| {
                (name == "AWS_EC2_METADATA_SERVICE_ENDPOINT").then(|| elsewhere.to_owned())
            })
            .unwrap_err();
            assert!(err.contains("loopback"), "{elsewhere}: {err}");
        }
    }

    #[test]
    fn adr0065_s3_credentials_come_from_the_instance_role_over_imdsv2() {
        let first = r#"{"Code":"Success","LastUpdated":"2026-10-27T11:00:00Z","Type":"AWS-HMAC","AccessKeyId":"ASIAFIRST","SecretAccessKey":"secret-1","Token":"token-1","Expiration":"2026-10-27T18:00:00Z"}"#;
        let second = r#"{"Code":"Success","LastUpdated":"2026-10-27T17:00:00Z","Type":"AWS-HMAC","AccessKeyId":"ASIASECOND","SecretAccessKey":"secret-2","Token":"token-2","Expiration":"2026-10-28T00:00:00Z"}"#;
        let (url, got) = fake::serve_each(vec![
            (200, "imds-token"),
            (200, "glass-agent-host\n"),
            (200, first),
            (200, "imds-token-2"),
            (200, "glass-agent-host\n"),
            (200, second),
        ]);
        let creds = S3Credentials::instance_role_at(&url);
        let keys = creds.keys(datetime!(2026-10-27 12:00 UTC)).unwrap();
        assert_eq!(keys.access_key_id.expose(), "ASIAFIRST");
        assert_eq!(keys.session_token.unwrap().expose(), "token-1");

        let token = got.recv().unwrap();
        assert!(
            token.head.starts_with("PUT /latest/api/token HTTP/1.1"),
            "{}",
            token.head
        );
        assert!(
            token
                .head
                .contains("X-aws-ec2-metadata-token-ttl-seconds: 21600"),
            "{}",
            token.head
        );
        let role = got.recv().unwrap();
        assert!(
            role.head
                .starts_with("GET /latest/meta-data/iam/security-credentials/ HTTP/1.1")
        );
        assert!(role.head.contains("X-aws-ec2-metadata-token: imds-token"));
        let keys_request = got.recv().unwrap();
        assert!(keys_request.head.starts_with(
            "GET /latest/meta-data/iam/security-credentials/glass-agent-host HTTP/1.1"
        ));

        // Kept until shortly before they expire, then fetched again.
        let keys = creds.keys(datetime!(2026-10-27 17:00 UTC)).unwrap();
        assert_eq!(keys.access_key_id.expose(), "ASIAFIRST");
        assert!(got.try_recv().is_err(), "no request while they're good");
        let keys = creds.keys(datetime!(2026-10-27 17:58 UTC)).unwrap();
        assert_eq!(keys.access_key_id.expose(), "ASIASECOND");
        assert_eq!(keys.secret_access_key.expose(), "secret-2");
    }

    #[test]
    fn adr0066_an_instance_reads_its_lifecycle_state_from_its_metadata() {
        let (url, got) = fake::serve_each(vec![(200, "imds-token"), (200, "Terminated\n")]);
        let state = instance_metadata_at(&url, "autoscaling/target-lifecycle-state").unwrap();
        assert_eq!(state.as_deref(), Some("Terminated"));
        let _token = got.recv().unwrap();
        let read = got.recv().unwrap();
        assert!(
            read.head
                .starts_with("GET /latest/meta-data/autoscaling/target-lifecycle-state "),
            "{}",
            read.head
        );
        assert!(read.head.contains("X-aws-ec2-metadata-token: imds-token"));
        // Not in an Auto Scaling group: AWS has no such entry.
        let (url, _got) = fake::serve_each(vec![(200, "imds-token"), (404, "")]);
        assert_eq!(
            instance_metadata_at(&url, "autoscaling/target-lifecycle-state").unwrap(),
            None
        );
    }

    #[test]
    fn adr0065_s3_a_computer_without_a_role_says_so() {
        let (url, _got) = fake::serve_each(vec![(200, "imds-token"), (404, "")]);
        let err = S3Credentials::instance_role_at(&url)
            .keys(datetime!(2026-10-27 12:00 UTC))
            .unwrap_err();
        assert!(err.contains("this instance has no IAM role"), "{err}");
    }

    #[test]
    fn adr0065_s3_listings_and_refusals_are_read_from_s3s_xml() {
        let page = parse_list(
            "<ListBucketResult><IsTruncated>true</IsTruncated><Contents><Key>a&amp;b&#x20;&#99;</Key>\
             <LastModified>2026-10-27T15:30:12.000Z</LastModified><Size>7</Size></Contents>\
             <NextContinuationToken>1ueG/x+=</NextContinuationToken></ListBucketResult>",
        )
        .unwrap();
        assert_eq!(page.objects[0].key, "a&b c");
        assert_eq!(page.objects[0].size, 7);
        assert_eq!(
            page.objects[0].modified,
            Some(SystemTime::from(datetime!(2026-10-27 15:30:12 UTC)))
        );
        assert_eq!(page.next.as_deref(), Some("1ueG/x+="));
        assert!(
            parse_list("<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>")
                .is_err()
        );
        assert!(matches!(
            refused(503, "<Error><Code>SlowDown</Code><Message>Please reduce your request rate.</Message></Error>"),
            Try::Again(why) if why == "HTTP 503, SlowDown: Please reduce your request rate."
        ));
        assert!(matches!(refused(403, ""), Try::Stop(why) if why == "HTTP 403"));
        assert_eq!(unescape("a &unknown; & b"), "a &unknown; & b");
    }
}
