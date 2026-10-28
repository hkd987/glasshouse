//! A stand-in for the Bot Connector's signing key, for tests here and in
//! other crates (`features = ["testing"]`): a fresh RSA key, its keys
//! document, and tokens signed with it.

use glass_crypto::rsa::testing::RsaTestKey;
use serde_json::{Value, json};

use crate::b64;

/// A signing key with an id.
pub struct TestKey {
    pub kid: String,
    key: RsaTestKey,
}

impl TestKey {
    /// A new 2048-bit key named `kid`.
    pub fn generate(kid: &str) -> Self {
        Self {
            kid: kid.to_owned(),
            key: RsaTestKey::generate(2048),
        }
    }

    /// This key's entry in a keys document, endorsed for `endorsements`.
    pub fn jwk(&self, endorsements: &[&str]) -> Value {
        json!({
            "kty": "RSA",
            "use": "sig",
            "kid": self.kid,
            "n": b64::encode(&self.key.modulus()),
            "e": b64::encode(&self.key.exponent()),
            "endorsements": endorsements,
        })
    }

    /// A keys document holding this key alone, endorsed for Teams.
    pub fn jwks(&self) -> String {
        json!({"keys": [self.jwk(&["msteams"])]}).to_string()
    }

    /// A token with `claims`, signed RS256 with this key.
    pub fn sign(&self, claims: &Value) -> String {
        let header = json!({"alg": "RS256", "typ": "JWT", "kid": self.kid, "x5t": self.kid});
        self.sign_with_header(&header, claims)
    }

    /// A token with `header` and `claims`, signed RS256 with this key
    /// whatever the header says.
    pub fn sign_with_header(&self, header: &Value, claims: &Value) -> String {
        let signed = format!(
            "{}.{}",
            b64::encode(header.to_string().as_bytes()),
            b64::encode(claims.to_string().as_bytes())
        );
        let signature = self.key.sign_rs256(signed.as_bytes());
        format!("{signed}.{}", b64::encode(&signature))
    }
}

/// The claims the Bot Connector signs for a call to `app_id` about the
/// conversation at `service_url`, valid from `now` for an hour.
pub fn claims(app_id: &str, service_url: &str, now: i64) -> Value {
    json!({
        "serviceurl": service_url,
        "nbf": now - 5,
        "exp": now + 3600,
        "iss": crate::auth::ISSUER,
        "aud": app_id,
    })
}
