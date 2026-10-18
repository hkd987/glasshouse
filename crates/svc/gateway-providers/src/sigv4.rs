//! AWS Signature Version 4, for Bedrock requests. HMAC-SHA256 is built on
//! the workspace's `sha2` (RFC 2104), so this adds no dependency.

use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::macros::format_description;

/// What gets signed.
pub(crate) struct Request<'a> {
    pub method: &'a str,
    /// Exactly as the Host header will carry it.
    pub host: &'a str,
    /// The canonical URI: for services other than S3, each path segment
    /// URI-encoded twice.
    pub canonical_path: &'a str,
    /// The canonical query string (empty for Bedrock).
    pub query: &'a str,
    /// Headers to sign besides host, x-amz-date and the session token.
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
}

pub(crate) struct Credentials<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub session_token: Option<&'a str>,
}

/// The headers to add to `request`: `x-amz-date`, the session token if
/// any, and `authorization`.
pub(crate) fn sign(
    request: &Request<'_>,
    credentials: &Credentials<'_>,
    region: &str,
    service: &str,
    now: OffsetDateTime,
) -> Vec<(&'static str, String)> {
    let amz_date = now
        .to_offset(time::UtcOffset::UTC)
        .format(format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .unwrap_or_default();
    let date = &amz_date[..8.min(amz_date.len())];
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    headers.push(("host".into(), request.host.to_owned()));
    headers.push(("x-amz-date".into(), amz_date.clone()));
    if let Some(token) = credentials.session_token {
        headers.push(("x-amz-security-token".into(), token.to_owned()));
    }
    headers.sort();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    let signed = signed.join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{signed}\n{}",
        request.method,
        request.canonical_path,
        request.query,
        headers
            .iter()
            .map(|(k, v)| format!("{k}:{v}\n"))
            .collect::<String>(),
        hex::encode(Sha256::digest(request.body)),
    );
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    );
    let key = signing_key(credentials.secret_access_key, date, region, service);
    let signature = hex::encode(hmac(&key, to_sign.as_bytes()));
    let mut out = vec![
        ("x-amz-date", amz_date),
        (
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
                credentials.access_key_id
            ),
        ),
    ];
    if let Some(token) = credentials.session_token {
        out.push(("x-amz-security-token", token.to_owned()));
    }
    out
}

fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> [u8; 32] {
    let k = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac(&k, region.as_bytes());
    let k = hmac(&k, service.as_bytes());
    hmac(&k, b"aws4_request")
}

/// HMAC-SHA256 (RFC 2104).
fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(data);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

/// URI-encodes one path segment the way SigV4 expects: everything but
/// unreserved characters as `%XX`.
pub(crate) fn uri_encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    // AWS's documentation example credentials, not real ones.
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex::encode(hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than a block is hashed first.
        assert_eq!(
            hex::encode(hmac(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn signs_the_aws_documentation_example() {
        // "Create a signed AWS API request", IAM ListUsers.
        assert_eq!(
            hex::encode(signing_key(SECRET, "20150830", "us-east-1", "iam")),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
        let request = Request {
            method: "GET",
            host: "iam.amazonaws.com",
            canonical_path: "/",
            query: "Action=ListUsers&Version=2010-05-08",
            headers: &[(
                "Content-Type",
                "application/x-www-form-urlencoded; charset=utf-8",
            )],
            body: b"",
        };
        let credentials = Credentials {
            access_key_id: "AKIDEXAMPLE",
            secret_access_key: SECRET,
            session_token: None,
        };
        let headers = sign(
            &request,
            &credentials,
            "us-east-1",
            "iam",
            datetime!(2015-08-30 12:36:00 UTC),
        );
        assert_eq!(headers[0], ("x-amz-date", "20150830T123600Z".to_owned()));
        assert_eq!(
            headers[1].1,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }

    #[test]
    fn signs_a_bedrock_converse_request_with_a_session_token() {
        // Expected value computed independently from the SigV4 spec.
        let body = br#"{"inferenceConfig":{"maxTokens":64},"messages":[{"content":[{"text":"Hello"}],"role":"user"}]}"#;
        let path = format!(
            "/model/{}/converse",
            uri_encode(&uri_encode("anthropic.claude-test-v1:0"))
        );
        assert_eq!(path, "/model/anthropic.claude-test-v1%253A0/converse");
        let request = Request {
            method: "POST",
            host: "bedrock-runtime.us-east-1.amazonaws.com",
            canonical_path: &path,
            query: "",
            headers: &[
                ("content-type", "application/json"),
                ("accept", "application/json"),
            ],
            body,
        };
        let credentials = Credentials {
            access_key_id: "ASIAEXAMPLE",
            secret_access_key: SECRET,
            session_token: Some("IQoJb3JpZ2luX2VjEXAMPLETOKEN"),
        };
        let headers = sign(
            &request,
            &credentials,
            "us-east-1",
            "bedrock",
            datetime!(2026-10-18 22:00:00 UTC),
        );
        let auth = &headers[1].1;
        assert!(
            auth.contains("SignedHeaders=accept;content-type;host;x-amz-date;x-amz-security-token")
        );
        assert!(
            auth.ends_with(
                "Signature=e729d51d552ffc3dfcd1e933dc56c153754433d9e91fcffeda15a6be61cae486"
            ),
            "{auth}"
        );
        assert_eq!(headers[2].0, "x-amz-security-token");
    }
}
