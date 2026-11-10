//! AWS Marketplace metering (ADR 0066): each agent host reports the agent
//! computer hours it ran, once an hour, with the Metering Service's
//! `MeterUsage` (AWS JSON 1.1, signed with SigV4).
//!
//! Glasshouse Agents is software the buyer runs in their own AWS account (an
//! AMI with a CloudFormation template), so the buyer is whoever signs:
//! `MeterUsage` must be signed with the host's EC2 instance role, and it has
//! no customer field at all. `CustomerIdentifier`, and `CustomerAWSAccountId`
//! that replaces it, belong to `BatchMeterUsage`, which a SaaS seller calls
//! from its own account on its customers' behalf. Neither applies here.
//!
//! The service keeps one record per host, dimension and hour. It rounds the
//! timestamp down to the hour and never changes a record: the same quantity
//! sent again returns the same record, and a different one is a duplicate.
//! It takes hours up to six hours old, so a host retries a failed report
//! (AWS asks for retries with backoff for up to 30 minutes) and keeps the
//! hours it couldn't send. [`MeteringError::retry`] says which to retry.

use std::fmt;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::bedrock::AwsCredentials;
use crate::http::Http;
use crate::sigv4::{self, Credentials};

/// The one usage dimension the listing meters (ADR 0066).
pub const AGENT_COMPUTER_HOURS: &str = "agent_computer_hours";

/// `X-Amz-Target` for `MeterUsage`: the service's JSON target prefix and
/// the operation.
const TARGET: &str = "AWSMPMeteringService.MeterUsage";
const CONTENT_TYPE: &str = "application/x-amz-json-1.1";
/// The name SigV4 signs Metering Service requests for.
const SIGNING_NAME: &str = "aws-marketplace";
/// The most `UsageQuantity` takes (a signed 32-bit integer).
const MAX_QUANTITY: u32 = i32::MAX.unsigned_abs();

/// One hour's usage from this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage<'a> {
    /// The listing's product code, from the stack (`MarketplaceProductCode`).
    pub product_code: &'a str,
    /// Any time in the hour being reported. It is sent as the hour's start,
    /// so a retry sends exactly the same report.
    pub hour: OffsetDateTime,
    /// Agent computer hours this host ran in that hour, whole.
    pub quantity: u32,
    /// Checks only that this host may meter (its role and the product code)
    /// and records nothing.
    pub dry_run: bool,
}

/// What a report did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Metered {
    /// Recorded, or already recorded with the same quantity.
    Recorded { record_id: String },
    /// A dry run that would have been recorded.
    WouldRecord,
}

/// Why a report wasn't recorded, in words an admin can act on. Each carries
/// the whole sentence, AWS's own error name and message included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeteringError {
    /// The account has no active subscription (`CustomerNotEntitledException`).
    /// A lapsed subscription pauses agents only (LA-3).
    NotEntitled(String),
    /// This host already reported another quantity for the hour
    /// (`DuplicateRequestException`). The first report stands.
    Duplicate(String),
    /// Too many calls; try again shortly (`ThrottlingException`).
    Throttled(String),
    /// More than six hours ago, or not yet begun (`TimestampOutOfBoundsException`).
    HourOutOfRange(String),
    /// The host may not meter: its role, or expired or wrong credentials.
    Denied(String),
    /// The product code, dimension or Region doesn't match the listing: a
    /// setup mistake no retry fixes.
    Misconfigured(String),
    /// The service or the network failed; try again.
    Unavailable(String),
    /// Never sent: the report itself is wrong.
    Invalid(String),
    /// Any other refusal.
    Refused(String),
}

impl MeteringError {
    /// Whether sending the same report again later may succeed.
    pub fn retry(&self) -> bool {
        matches!(self, Self::Throttled(_) | Self::Unavailable(_))
    }
}

impl fmt::Display for MeteringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotEntitled(m)
            | Self::Duplicate(m)
            | Self::Throttled(m)
            | Self::HourOutOfRange(m)
            | Self::Denied(m)
            | Self::Misconfigured(m)
            | Self::Unavailable(m)
            | Self::Invalid(m)
            | Self::Refused(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for MeteringError {}

/// A Metering Service client for one Region, signing as the host.
#[derive(Debug)]
pub struct MarketplaceMetering {
    http: Http,
    region: String,
    /// Scheme and host: the Region's endpoint, or a VPC endpoint's name.
    base: String,
    host: String,
    credentials: AwsCredentials,
}

impl MarketplaceMetering {
    /// A client for `region`'s endpoint (calls must stay in the host's own
    /// Region: the service refuses others), or `endpoint` (a VPC endpoint's
    /// DNS name, when private DNS doesn't answer for the regional name).
    /// `credentials` are the host's instance role's.
    /// Plain HTTP only to a stand-in on this machine's loopback address
    /// (deploy/local).
    pub fn new(region: &str, endpoint: Option<&str>, credentials: AwsCredentials) -> Self {
        let base = endpoint.map_or_else(
            || format!("https://metering.marketplace.{region}.amazonaws.com"),
            |e| e.trim_end_matches('/').to_owned(),
        );
        let local = crate::http::Target::parse(&format!("{base}/"))
            .is_some_and(|t| !t.tls && crate::console::is_loopback(&t.host));
        let http = if local { Http::loopback() } else { Http::new() };
        Self::with_http(http.quick(), region, &base, credentials)
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

    /// Reports `usage` (`MeterUsage`).
    pub fn meter(&self, usage: &Usage<'_>) -> Result<Metered, MeteringError> {
        self.send(usage, OffsetDateTime::now_utc())
    }

    fn send(&self, usage: &Usage<'_>, now: OffsetDateTime) -> Result<Metered, MeteringError> {
        let hour = check(usage, now)?;
        self.credentials.check(now).map_err(|why| {
            MeteringError::Denied(format!(
                "this host can't meter: {why}; it signs with its instance role's credentials, \
                 which must be current"
            ))
        })?;
        let body = json!({
            "ProductCode": usage.product_code,
            "Timestamp": hour.unix_timestamp(),
            "UsageDimension": AGENT_COMPUTER_HOURS,
            "UsageQuantity": usage.quantity,
            "DryRun": usage.dry_run,
        })
        .to_string();
        let headers = [("content-type", CONTENT_TYPE), ("x-amz-target", TARGET)];
        let signed = sigv4::sign(
            &sigv4::Request {
                method: "POST",
                host: &self.host,
                canonical_path: "/",
                query: "",
                headers: &headers,
                body: body.as_bytes(),
            },
            &Credentials {
                access_key_id: self.credentials.access_key_id.expose(),
                secret_access_key: self.credentials.secret_access_key.expose(),
                // Long-lived keys (a developer's) have none to send.
                session_token: Some(self.credentials.session_token.expose())
                    .filter(|t| !t.is_empty()),
            },
            &self.region,
            SIGNING_NAME,
            now,
        );
        let mut all: Vec<(&str, &str)> = headers.to_vec();
        all.extend(signed.iter().map(|(k, v)| (*k, v.as_str())));
        let reply = self
            .http
            .post(&format!("{}/", self.base), &all, body.as_bytes())
            .map_err(|e| {
                MeteringError::Unavailable(format!(
                    "AWS Marketplace metering can't be reached from this host ({e}); \
                     the report is kept and sent again"
                ))
            })?;
        let parsed: Option<Value> = serde_json::from_str(&reply.body).ok();
        if reply.status == 200 {
            return match parsed
                .as_ref()
                .and_then(|v| v.get("MeteringRecordId"))
                .and_then(Value::as_str)
            {
                Some(id) => Ok(Metered::Recorded {
                    record_id: id.to_owned(),
                }),
                None if usage.dry_run => Ok(Metered::WouldRecord),
                None => Err(MeteringError::Unavailable(
                    "AWS Marketplace answered without a metering record id; \
                     the report is sent again"
                        .into(),
                )),
            };
        }
        match refusal(reply.status, parsed.as_ref(), usage, hour) {
            Refusal::DryRunPassed if usage.dry_run => Ok(Metered::WouldRecord),
            Refusal::DryRunPassed => Err(MeteringError::Refused(
                "AWS Marketplace treated a real report as a dry run (DryRunOperation)".into(),
            )),
            Refusal::Error(e) => Err(e),
        }
    }
}

/// The hour's start, if the report may be sent.
fn check(usage: &Usage<'_>, now: OffsetDateTime) -> Result<OffsetDateTime, MeteringError> {
    let code = usage.product_code;
    if code.is_empty()
        || code.len() > 255
        || !code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-/=:_.@".contains(&b))
    {
        return Err(MeteringError::Invalid(format!(
            "{code:?} isn't an AWS Marketplace product code (letters, digits and -/=:_.@, \
             at most 255); it comes from the listing, through the stack's \
             MarketplaceProductCode"
        )));
    }
    if usage.quantity > MAX_QUANTITY {
        return Err(MeteringError::Invalid(format!(
            "{} agent computer hours is more than one report can carry ({MAX_QUANTITY})",
            usage.quantity
        )));
    }
    let at = usage.hour.unix_timestamp();
    let start = OffsetDateTime::from_unix_timestamp(at - at.rem_euclid(3600))
        .map_err(|e| MeteringError::Invalid(format!("no such hour: {e}")))?;
    if start > now {
        return Err(MeteringError::Invalid(format!(
            "the hour from {} hasn't begun yet",
            when(start)
        )));
    }
    Ok(start)
}

enum Refusal {
    /// `DryRunOperation`: the dry run would have been recorded.
    DryRunPassed,
    Error(MeteringError),
}

/// Reads an AWS JSON 1.1 error (`__type`, `message`) into a sentence.
fn refusal(status: u16, body: Option<&Value>, usage: &Usage<'_>, hour: OffsetDateTime) -> Refusal {
    let field = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| body.and_then(|v| v.get(*n)).and_then(Value::as_str))
            .map(str::to_owned)
    };
    // "com.amazonaws.marketplace#ThrottlingException", or with a
    // ":http://..." suffix, or bare.
    let code = field(&["__type", "code", "Code"])
        .map(|t| {
            let name = t.rsplit('#').next().unwrap_or(&t);
            name.split(':').next().unwrap_or(name).to_owned()
        })
        .unwrap_or_default();
    let message = field(&["message", "Message"]).unwrap_or_else(|| "no details".into());
    let said = if code.is_empty() {
        format!("HTTP {status}: {message}")
    } else {
        format!("{code}: {message}")
    };
    let hour = when(hour);
    let product = usage.product_code;
    Refusal::Error(match code.as_str() {
        "DryRunOperation" => return Refusal::DryRunPassed,
        "CustomerNotEntitledException" => MeteringError::NotEntitled(format!(
            "this AWS account has no active AWS Marketplace subscription to product {product}, \
             so agent computer hours can't be recorded ({said})"
        )),
        "DuplicateRequestException" => MeteringError::Duplicate(format!(
            "this host already reported a different number of agent computer hours for the \
             hour from {hour}; the first report stands and can't be changed ({said})"
        )),
        "ThrottlingException" | "Throttling" | "TooManyRequestsException" => {
            MeteringError::Throttled(format!(
                "AWS Marketplace is limiting how fast this account meters; the report is sent \
                 again shortly ({said})"
            ))
        }
        "TimestampOutOfBoundsException" => MeteringError::HourOutOfRange(format!(
            "AWS Marketplace takes usage up to six hours old, and the hour from {hour} is \
             outside that ({said})"
        )),
        "AccessDeniedException"
        | "UnauthorizedException"
        | "UnrecognizedClientException"
        | "InvalidClientTokenId"
        | "ExpiredTokenException"
        | "InvalidSignatureException"
        | "IncompleteSignature"
        | "MissingAuthenticationToken" => MeteringError::Denied(format!(
            "this host isn't allowed to meter: its instance role needs \
             aws-marketplace:MeterUsage, and the request must be signed with that role's \
             current credentials ({said})"
        )),
        "InvalidProductCodeException" => MeteringError::Misconfigured(format!(
            "product code {product} isn't this listing's; check the stack's \
             MarketplaceProductCode ({said})"
        )),
        "InvalidUsageDimensionException" => MeteringError::Misconfigured(format!(
            "the listing has no {AGENT_COMPUTER_HOURS} dimension ({said})"
        )),
        "InvalidEndpointRegionException" => MeteringError::Misconfigured(format!(
            "metering must go to the host's own Region's endpoint ({said})"
        )),
        "InternalServiceErrorException" | "InternalFailure" | "ServiceUnavailable" => {
            MeteringError::Unavailable(format!(
                "AWS Marketplace metering failed on its side; the report is sent again ({said})"
            ))
        }
        _ => match status {
            401 | 403 => MeteringError::Denied(format!(
                "this host isn't allowed to meter: its instance role needs \
                 aws-marketplace:MeterUsage ({said})"
            )),
            429 => MeteringError::Throttled(format!(
                "AWS Marketplace is limiting how fast this account meters; the report is sent \
                 again shortly ({said})"
            )),
            500..=599 => MeteringError::Unavailable(format!(
                "AWS Marketplace metering failed on its side; the report is sent again ({said})"
            )),
            _ if code.starts_with("Invalid") => MeteringError::Misconfigured(format!(
                "AWS Marketplace refused the report as set up ({said})"
            )),
            _ => MeteringError::Refused(format!("AWS Marketplace refused the report ({said})")),
        },
    })
}

fn when(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use glass_gateway::Secrets;
    use time::macros::datetime;

    use super::*;
    use crate::http::fake;

    const NOW: OffsetDateTime = datetime!(2026-10-27 15:07:30 UTC);

    // AWS's documentation example secret; the rest are placeholders.
    fn role() -> AwsCredentials {
        AwsCredentials::from_secrets(
            &Secrets::parse(
                "[metering]\naccess_key_id = \"ASIAEXAMPLE\"\n\
                 secret_access_key = \"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY\"\n\
                 session_token = \"token-example\"\nexpires = \"2026-10-27T18:00:00Z\"\n",
            )
            .unwrap(),
            "metering",
        )
        .unwrap()
    }

    fn client(url: &str) -> MarketplaceMetering {
        MarketplaceMetering::with_http(Http::insecure_for_tests(), "us-east-1", url, role())
    }

    fn usage(quantity: u32, dry_run: bool) -> Usage<'static> {
        Usage {
            product_code: "prod-abc123example",
            hour: datetime!(2026-10-27 14:20:05 UTC),
            quantity,
            dry_run,
        }
    }

    #[test]
    fn meters_an_hour_of_agent_computer_hours_signed_as_the_host() {
        let (url, got) = fake::serve(200, r#"{"MeteringRecordId":"rec-0001"}"#);
        let done = client(&url).send(&usage(7, false), NOW).unwrap();
        assert_eq!(
            done,
            Metered::Recorded {
                record_id: "rec-0001".into()
            }
        );

        let sent = got.recv().unwrap();
        assert!(
            sent.head.starts_with("POST / HTTP/1.1\r\n"),
            "{}",
            sent.head
        );
        let head = sent.head.to_ascii_lowercase();
        assert!(
            head.contains("x-amz-target: awsmpmeteringservice.meterusage\r\n"),
            "{head}"
        );
        assert!(head.contains("content-type: application/x-amz-json-1.1\r\n"));
        assert!(head.contains("x-amz-date: 20261027t150730z\r\n"));
        assert!(head.contains("x-amz-security-token: token-example\r\n"));
        assert!(
            head.contains(
                "authorization: aws4-hmac-sha256 \
                 credential=asiaexample/20261027/us-east-1/aws-marketplace/aws4_request, \
                 signedheaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, \
                 signature="
            ),
            "{head}"
        );
        // No customer field: the buyer is whoever signs (module docs).
        assert_eq!(sent.body, SENT_BODY);
        let body: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(
            body,
            json!({
                "ProductCode": "prod-abc123example",
                "Timestamp": datetime!(2026-10-27 14:00 UTC).unix_timestamp(),
                "UsageDimension": "agent_computer_hours",
                "UsageQuantity": 7,
                "DryRun": false,
            })
        );
    }

    #[test]
    fn plain_http_only_to_a_stand_in_on_this_machine() {
        let at = |endpoint: &str| MarketplaceMetering::new("us-east-1", Some(endpoint), role());
        assert!(!at("http://127.0.0.1:7612").http.https_only);
        assert!(at("http://metering.example.com").http.https_only);
        assert!(
            at("https://vpce-0abc.metering.marketplace.us-east-1.vpce.amazonaws.com")
                .http
                .https_only
        );
        assert!(
            MarketplaceMetering::new("us-east-1", None, role())
                .http
                .https_only
        );
    }

    /// Against the Metering Service's stand-in on this machine
    /// (deploy/local), which checks every signature: the host's keys come
    /// from the stand-in metadata service, as an agent host's come from its
    /// instance role.
    #[test]
    #[ignore = "against AWS on this machine: make aws-local-test"]
    fn adr0066_local_aws_meters_hours_signed_with_the_instance_role() {
        let var = |name: &str| {
            std::env::var(name).unwrap_or_else(|_| {
                panic!("{name} isn't set: make aws-local, then make aws-local-test")
            })
        };
        let (region, endpoint) = (var("GLASS_LOCAL_REGION"), var("GLASS_LOCAL_METERING"));
        let host = crate::s3::S3Credentials::from_host().unwrap();
        let client =
            MarketplaceMetering::new(&region, Some(&endpoint), host.aws_credentials().unwrap());
        let records = |method: &str| {
            let reply = Http::loopback()
                .send(method, &format!("{endpoint}/_fake/records"), &[], b"")
                .unwrap();
            serde_json::from_str::<Value>(&reply.body).unwrap()
        };
        records("DELETE");
        let product = var("GLASS_LOCAL_PRODUCT_CODE");
        let report = |quantity, dry_run| Usage {
            product_code: &product,
            hour: OffsetDateTime::now_utc() - time::Duration::hours(2),
            quantity,
            dry_run,
        };

        assert_eq!(
            client.meter(&report(3, true)).unwrap(),
            Metered::WouldRecord
        );
        assert_eq!(records("GET"), json!([]), "a dry run records nothing");
        let Metered::Recorded { record_id } = client.meter(&report(3, false)).unwrap() else {
            panic!("not recorded");
        };
        // Sent again, the same record; another quantity, refused.
        assert_eq!(
            client.meter(&report(3, false)).unwrap(),
            Metered::Recorded {
                record_id: record_id.clone()
            }
        );
        assert!(matches!(
            client.meter(&report(4, false)),
            Err(MeteringError::Duplicate(_))
        ));
        let kept = records("GET");
        assert_eq!(kept[0]["record_id"], json!(record_id));
        assert_eq!(kept[0]["quantity"], json!(3));
        assert_eq!(kept[0]["dimension"], json!(AGENT_COMPUTER_HOURS));

        // Keys that aren't the role's current ones are refused.
        let keys = host.keys(OffsetDateTime::now_utc()).unwrap();
        let forged = AwsCredentials::from_parts(
            keys.access_key_id,
            glass_gateway::Secret::new("not-the-roles-secret"),
            keys.session_token,
            None,
        );
        let refused = MarketplaceMetering::new(&region, Some(&endpoint), forged)
            .meter(&report(3, false))
            .unwrap_err();
        assert!(matches!(refused, MeteringError::Denied(_)), "{refused:?}");
    }

    /// The body `meters_an_hour_of_agent_computer_hours_signed_as_the_host`
    /// sends, byte for byte.
    const SENT_BODY: &str = r#"{"DryRun":false,"ProductCode":"prod-abc123example","Timestamp":1793109600,"UsageDimension":"agent_computer_hours","UsageQuantity":7}"#;

    #[test]
    fn the_signature_matches_an_independent_signer() {
        // botocore 1.43's SigV4Auth(credentials, "aws-marketplace",
        // "us-east-1") over the same request, at NOW, signs the same headers
        // and gives this signature.
        let headers = [("content-type", CONTENT_TYPE), ("x-amz-target", TARGET)];
        let signed = sigv4::sign(
            &sigv4::Request {
                method: "POST",
                host: "metering.marketplace.us-east-1.amazonaws.com",
                canonical_path: "/",
                query: "",
                headers: &headers,
                body: SENT_BODY.as_bytes(),
            },
            &Credentials {
                access_key_id: "ASIAEXAMPLE",
                secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                session_token: Some("token-example"),
            },
            "us-east-1",
            SIGNING_NAME,
            NOW,
        );
        assert_eq!(
            signed[1].1,
            "AWS4-HMAC-SHA256 Credential=ASIAEXAMPLE/20261027/us-east-1/aws-marketplace/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, \
             Signature=dc6670b9b3ca7f11d80d2f94c91db5ff0997769dd937b20098b713436a90647e"
        );
    }

    #[test]
    fn a_dry_run_checks_permission_and_records_nothing() {
        let (url, got) = fake::serve(
            412,
            r#"{"__type":"DryRunOperation","message":"Request would have succeeded, but DryRun flag is set."}"#,
        );
        assert_eq!(
            client(&url).send(&usage(0, true), NOW).unwrap(),
            Metered::WouldRecord
        );
        let body: Value = serde_json::from_str(&got.recv().unwrap().body).unwrap();
        assert_eq!(body["DryRun"], json!(true));
    }

    #[test]
    fn refusals_read_as_sentences_and_say_whether_to_retry() {
        type Kind = fn(&MeteringError) -> bool;
        let cases: [(u16, &'static str, Kind, &str, bool); 9] = [
            (
                400,
                r#"{"__type":"com.amazonaws.marketplace.metering#CustomerNotEntitledException","message":"no subscription"}"#,
                |e| matches!(e, MeteringError::NotEntitled(_)),
                "no active AWS Marketplace subscription to product prod-abc123example",
                false,
            ),
            (
                400,
                r#"{"__type":"DuplicateRequestException","message":"different quantity"}"#,
                |e| matches!(e, MeteringError::Duplicate(_)),
                "already reported a different number of agent computer hours for the hour from 2026-10-27T14:00:00Z",
                false,
            ),
            (
                // The Metering Service throttles with a 400, not a 429.
                400,
                r#"{"__type":"ThrottlingException:http://internal.amazon.com/coral/","message":"Rate exceeded"}"#,
                |e| matches!(e, MeteringError::Throttled(_)),
                "limiting how fast this account meters",
                true,
            ),
            (
                400,
                r#"{"__type":"TimestampOutOfBoundsException","message":"out of range"}"#,
                |e| matches!(e, MeteringError::HourOutOfRange(_)),
                "takes usage up to six hours old",
                false,
            ),
            (
                400,
                r#"{"__type":"AccessDeniedException","Message":"not authorized to perform: aws-marketplace:MeterUsage"}"#,
                |e| matches!(e, MeteringError::Denied(_)),
                "needs aws-marketplace:MeterUsage",
                false,
            ),
            (
                403,
                r#"{"message":"The security token included in the request is invalid."}"#,
                |e| matches!(e, MeteringError::Denied(_)),
                "HTTP 403: The security token included in the request is invalid.",
                false,
            ),
            (
                400,
                r#"{"__type":"InvalidProductCodeException","message":"bad code"}"#,
                |e| matches!(e, MeteringError::Misconfigured(_)),
                "check the stack's MarketplaceProductCode",
                false,
            ),
            (
                400,
                r#"{"__type":"InvalidEndpointRegionException","message":"wrong Region"}"#,
                |e| matches!(e, MeteringError::Misconfigured(_)),
                "the host's own Region's endpoint",
                false,
            ),
            (
                500,
                r#"{"__type":"InternalServiceErrorException","message":"oops"}"#,
                |e| matches!(e, MeteringError::Unavailable(_)),
                "failed on its side",
                true,
            ),
        ];
        for (status, reply, kind, says, retry) in cases {
            let (url, _got) = fake::serve(status, reply);
            let err = client(&url).send(&usage(3, false), NOW).unwrap_err();
            assert!(kind(&err), "{status} {reply}: {err:?}");
            let text = err.to_string();
            assert!(text.contains(says), "{text}");
            assert_eq!(err.retry(), retry, "{text}");
            assert!(!text.contains("EXAMPLEKEY") && !text.contains("token-example"));
        }
    }

    /// An address nothing listens on: a port this test held and let go.
    fn nobody() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    }

    #[test]
    fn an_unreachable_service_is_retried() {
        let err = client(&nobody()).send(&usage(1, false), NOW).unwrap_err();
        assert!(matches!(err, MeteringError::Unavailable(_)), "{err:?}");
        assert!(err.retry());
    }

    #[test]
    fn a_wrong_report_never_leaves_the_host() {
        // Any request would fail as unreachable; these fail before one.
        let metering = client(&nobody());
        let bad_code = Usage {
            product_code: "prod abc",
            ..usage(1, false)
        };
        let empty_code = Usage {
            product_code: "",
            ..usage(1, false)
        };
        let future = Usage {
            hour: datetime!(2026-10-27 16:00 UTC),
            ..usage(1, false)
        };
        for wrong in [bad_code, empty_code, future, usage(u32::MAX, false)] {
            let err = metering.send(&wrong, NOW).unwrap_err();
            assert!(matches!(err, MeteringError::Invalid(_)), "{err:?}");
            assert!(!err.retry());
        }
        // The current hour, not yet over, may be reported.
        let current = Usage {
            hour: datetime!(2026-10-27 15:00 UTC),
            ..usage(1, false)
        };
        assert!(matches!(
            metering.send(&current, NOW),
            Err(MeteringError::Unavailable(_))
        ));

        let expired = metering
            .send(&usage(1, false), datetime!(2026-10-27 18:00 UTC))
            .unwrap_err();
        assert!(matches!(expired, MeteringError::Denied(_)), "{expired:?}");
        assert!(
            expired
                .to_string()
                .contains("expired at 2026-10-27T18:00:00Z")
        );
    }

    #[test]
    fn calls_stay_in_the_hosts_region() {
        let metering = MarketplaceMetering::new("eu-west-1", None, role());
        assert_eq!(
            metering.base,
            "https://metering.marketplace.eu-west-1.amazonaws.com"
        );
        assert_eq!(
            metering.host,
            "metering.marketplace.eu-west-1.amazonaws.com"
        );
        let private = MarketplaceMetering::new(
            "eu-west-1",
            Some("https://vpce-0abc.metering-marketplace.eu-west-1.vpce.amazonaws.com/"),
            role(),
        );
        assert_eq!(
            private.host,
            "vpce-0abc.metering-marketplace.eu-west-1.vpce.amazonaws.com"
        );
        assert!(!format!("{metering:?}").contains("EXAMPLEKEY"));
    }
}
