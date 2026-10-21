//! The billing relay's two connections (ADR 0040): the relay pushing a
//! payment to an org's console, and a console asking the relay for a
//! checkout. HTTPS only, except plain HTTP to a loopback address when the
//! config allows it (development and tests), as for [`crate::ConsoleClient`].
//! Neither the relay's key nor a console's token ever appears in an error.

use glass_crypto::SecretKey;
use glass_gateway::Secret;
use glass_managed::billing::{
    CHECKOUT_PATH, CheckoutAsk, CheckoutMade, EVENTS_PATH, SIGNATURE_HEADER, sign,
};

use crate::http::{Http, Target};

/// The client for `url`: HTTPS, or plain HTTP to this machine when allowed.
fn client(url: &str, allow_insecure_http: bool, what: &str) -> Result<(Http, String), String> {
    let base = url.trim_end_matches('/').to_owned();
    let target = Target::parse(&format!("{base}/"))
        .ok_or_else(|| format!("the {what} address {url:?} is not an http(s) URL"))?;
    let http = if target.tls {
        Http::new()
    } else if allow_insecure_http && crate::console::is_loopback(&target.host) {
        Http::loopback()
    } else {
        return Err(format!(
            "the {what} address {url:?} must use HTTPS (plain HTTP only to this machine, for \
             development)"
        ));
    };
    Ok((http, base))
}

/// What the console answered a push with, when it didn't take it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The HTTP status, or `None` when the console couldn't be reached.
    pub status: Option<u16>,
    pub reason: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(f, "HTTP {status}: {}", self.reason),
            None => f.write_str(&self.reason),
        }
    }
}

/// The relay's connection to one org's console.
#[derive(Debug)]
pub struct ConsolePush {
    http: Http,
    url: String,
}

impl ConsolePush {
    pub fn new(console_url: &str, allow_insecure_http: bool) -> Result<Self, String> {
        let (http, base) = client(console_url, allow_insecure_http, "console")?;
        Ok(Self {
            http,
            url: format!("{base}{EVENTS_PATH}"),
        })
    }

    /// Pushes `event` (a `BillingEvent`'s JSON), signed now with the
    /// relay's key. `Ok` once the console answered 2xx.
    pub fn push(&self, key: &SecretKey, event: &[u8], now: i64) -> Result<(), Refused> {
        let signature = sign(key, now, event).map_err(|reason| Refused {
            status: None,
            reason,
        })?;
        let reply = self
            .http
            .post(
                &self.url,
                &[
                    ("Content-Type", "application/json"),
                    (SIGNATURE_HEADER, &signature),
                ],
                event,
            )
            .map_err(|e| Refused {
                status: None,
                reason: e.to_string(),
            })?;
        if (200..300).contains(&reply.status) {
            return Ok(());
        }
        Err(Refused {
            status: Some(reply.status),
            reason: reason(&reply.body),
        })
    }
}

/// A console's connection to the billing relay, with its own token.
#[derive(Debug)]
pub struct BillingRelay {
    http: Http,
    url: String,
    token: Secret,
}

impl BillingRelay {
    pub fn new(relay_url: &str, token: &str, allow_insecure_http: bool) -> Result<Self, String> {
        let (http, base) = client(relay_url, allow_insecure_http, "billing relay")?;
        if token.is_empty() || token.contains(['\r', '\n']) {
            return Err("the billing relay token is empty or has a line break".into());
        }
        Ok(Self {
            http,
            url: format!("{base}{CHECKOUT_PATH}"),
            token: Secret::new(token),
        })
    }

    /// A Checkout Session for this console's org, made by the relay.
    pub fn checkout(&self, ask: &CheckoutAsk) -> Result<CheckoutMade, String> {
        let body = serde_json::to_vec(ask).map_err(|e| e.to_string())?;
        let bearer = format!("Bearer {}", self.token.expose());
        let reply = self
            .http
            .post(
                &self.url,
                &[
                    ("Content-Type", "application/json"),
                    ("Authorization", &bearer),
                ],
                &body,
            )
            .map_err(|e| format!("the billing relay: {e}"))?;
        if reply.status != 200 {
            return Err(format!(
                "the billing relay: HTTP {}: {}",
                reply.status,
                reason(&reply.body)
            ));
        }
        serde_json::from_str(&reply.body)
            .map_err(|e| format!("the billing relay's answer doesn't parse: {e}"))
    }
}

/// The `error` an answer carries, or its first line, short.
fn reason(body: &str) -> String {
    let said = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.lines().next().unwrap_or_default().to_owned());
    said.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use glass_managed::billing::verify;

    use super::*;
    use crate::http::fake;

    #[test]
    fn adr0040_a_push_is_signed_by_the_relay_and_says_why_it_was_refused() {
        let key = SecretKey::generate().unwrap();
        let event = br#"{"kind":"credits_paid","org":"acme"}"#;
        let (url, got) = fake::serve(200, "{}");
        ConsolePush::new(&url, true)
            .unwrap()
            .push(&key, event, 1_792_500_000)
            .unwrap();
        let sent = got.recv().unwrap();
        assert!(
            sent.head.starts_with("POST /billing/v1/events HTTP/1.1"),
            "{}",
            sent.head
        );
        let header = sent
            .head
            .lines()
            .find_map(|l| l.strip_prefix("Glasshouse-Billing-Signature: "))
            .unwrap();
        verify(&key.public().unwrap(), header, event, 1_792_500_010).unwrap();
        assert_eq!(sent.body.as_bytes(), event);

        let (url, _got) = fake::serve(400, r#"{"error":"not signed by the relay"}"#);
        let refused = ConsolePush::new(&url, true)
            .unwrap()
            .push(&key, event, 1)
            .unwrap_err();
        assert_eq!(refused.status, Some(400));
        assert_eq!(refused.reason, "not signed by the relay");

        // Nobody listening: no status, a reason.
        let refused = ConsolePush::new("http://127.0.0.1:9", true)
            .unwrap()
            .push(&key, event, 1)
            .unwrap_err();
        assert_eq!(refused.status, None);
        assert!(!refused.reason.is_empty());
    }

    #[test]
    fn adr0040_a_console_asks_the_relay_for_a_checkout_with_its_token() {
        let (url, got) = fake::serve(
            200,
            r#"{"id":"cs_1","url":"https://checkout.stripe.com/c/pay/cs_1"}"#,
        );
        let relay = BillingRelay::new(&url, "console-token", true).unwrap();
        let made = relay
            .checkout(&CheckoutAsk {
                cents: 5000,
                success_url: "https://acme.example/#/credits/paid".into(),
                cancel_url: "https://acme.example/#/credits".into(),
            })
            .unwrap();
        assert_eq!(made.id, "cs_1");
        let sent = got.recv().unwrap();
        assert!(sent.head.starts_with("POST /v1/checkout HTTP/1.1"));
        assert!(sent.head.contains("Authorization: Bearer console-token"));
        assert!(sent.body.contains(r#""cents":5000"#));

        let (url, _got) = fake::serve(401, r#"{"error":"unknown console"}"#);
        let err = BillingRelay::new(&url, "console-token", true)
            .unwrap()
            .checkout(&CheckoutAsk {
                cents: 5000,
                success_url: String::new(),
                cancel_url: String::new(),
            })
            .unwrap_err();
        assert!(
            err.contains("401") && err.contains("unknown console"),
            "{err}"
        );
        assert!(!err.contains("console-token"));
        assert!(!format!("{relay:?}").contains("console-token"));

        // HTTPS, or plain HTTP to this machine only when allowed.
        assert!(BillingRelay::new("http://billing.example", "t", true).is_err());
        assert!(BillingRelay::new("http://127.0.0.1:7500", "t", false).is_err());
        assert!(ConsolePush::new("https://acme.console.example", false).is_ok());
    }
}
