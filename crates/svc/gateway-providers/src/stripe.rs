//! Stripe (ADR 0039): how orgs pay for model credits. The console makes a
//! Checkout Session on Stripe's own page and is told it was paid by a
//! webhook, whose signature it checks with the endpoint's secret. What
//! was paid comes from Stripe's event, never from what the console asked.
//! Card details never reach Glasshouse.

use glass_gateway::Secret;
use serde::Deserialize;

use crate::http::Http;
use crate::sigv4::{hmac, uri_encode};

/// Stripe's API, unless the config names another (tests).
pub const DEFAULT_ENDPOINT: &str = "https://api.stripe.com";
/// How old a webhook's signature may be, as Stripe's own libraries allow.
pub const SIGNATURE_TOLERANCE_SECS: i64 = 300;

/// The console's side of Stripe, with our secret key.
#[derive(Debug)]
pub struct Stripe {
    http: Http,
    endpoint: String,
    key: Secret,
}

/// A checkout Stripe made: its id and the page to send the person to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub id: String,
    pub url: String,
}

/// What to sell.
#[derive(Debug, Clone, Copy)]
pub struct CheckoutRequest<'a> {
    /// The org paying: kept in the session's metadata, so each org's
    /// console takes only its own payments.
    pub org: &'a str,
    pub cents: u64,
    pub success_url: &'a str,
    pub cancel_url: &'a str,
}

#[derive(Deserialize)]
struct Made {
    id: String,
    url: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: String,
}

impl Stripe {
    pub fn new(endpoint: Option<&str>, secret_key: &str) -> Self {
        Self::with_http(Http::new(), endpoint, Secret::new(secret_key))
    }

    fn with_http(http: Http, endpoint: Option<&str>, key: Secret) -> Self {
        Self {
            http,
            endpoint: endpoint
                .unwrap_or(DEFAULT_ENDPOINT)
                .trim_end_matches('/')
                .to_owned(),
            key,
        }
    }

    /// A Checkout Session for `cents` of model credits, paid on Stripe's
    /// page.
    pub fn checkout(&self, request: CheckoutRequest<'_>) -> Result<Checkout, String> {
        let cents = request.cents.to_string();
        let body = form(&[
            ("mode", "payment"),
            ("line_items[0][quantity]", "1"),
            ("line_items[0][price_data][currency]", "usd"),
            ("line_items[0][price_data][unit_amount]", &cents),
            (
                "line_items[0][price_data][product_data][name]",
                "Glasshouse model credits",
            ),
            ("success_url", request.success_url),
            ("cancel_url", request.cancel_url),
            ("client_reference_id", request.org),
            ("metadata[org]", request.org),
            ("metadata[kind]", "model_credits"),
        ]);
        let bearer = format!("Bearer {}", self.key.expose());
        let reply = self
            .http
            .post(
                &format!("{}/v1/checkout/sessions", self.endpoint),
                &[
                    ("authorization", bearer.as_str()),
                    ("content-type", "application/x-www-form-urlencoded"),
                ],
                body.as_bytes(),
            )
            .map_err(|e| format!("Stripe {e}"))?;
        if reply.status != 200 {
            let message = serde_json::from_str::<ErrorBody>(&reply.body)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| "no details".into());
            return Err(format!("Stripe said HTTP {}: {message}", reply.status));
        }
        let made: Made =
            serde_json::from_str(&reply.body).map_err(|e| format!("Stripe's reply: {e}"))?;
        match made.url {
            Some(url) if url.starts_with("https://") => Ok(Checkout { id: made.id, url }),
            _ => Err("Stripe made the checkout without a page to pay on".into()),
        }
    }
}

/// `pairs` as `application/x-www-form-urlencoded`.
fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", uri_encode(k), uri_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Whether `body` is what Stripe signed in `header` (`Stripe-Signature:
/// t=…,v1=…`) with the endpoint's `secret`, at most
/// [`SIGNATURE_TOLERANCE_SECS`] from `now` (Unix seconds).
pub fn verify_webhook(secret: &str, header: &str, body: &[u8], now: i64) -> Result<(), String> {
    let mut at = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", t)) => at = t.parse::<i64>().ok(),
            Some(("v1", sig)) => signatures.push(sig),
            _ => {}
        }
    }
    let at = at.ok_or("the signature has no time")?;
    if (now - at).abs() > SIGNATURE_TOLERANCE_SECS {
        return Err("the signature is too old or from the future".into());
    }
    let mut signed = format!("{at}.").into_bytes();
    signed.extend_from_slice(body);
    let expected = hex::encode(hmac(secret.as_bytes(), &signed));
    let matches = |given: &str| {
        given.len() == expected.len()
            && given
                .bytes()
                .zip(expected.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    };
    if signatures.into_iter().any(matches) {
        Ok(())
    } else {
        Err("the signature doesn't match".into())
    }
}

/// The `Stripe-Signature` header Stripe sends with `body` at `at`, signed
/// with `secret`: for tests and development tools that stand in for it.
pub fn signature_header(secret: &str, at: i64, body: &[u8]) -> String {
    let mut signed = format!("{at}.").into_bytes();
    signed.extend_from_slice(body);
    format!(
        "t={at},v1={}",
        hex::encode(hmac(secret.as_bytes(), &signed))
    )
}

/// A checkout that was paid, from Stripe's event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paid {
    /// The Checkout Session's id: one grant per session, however often
    /// Stripe sends the event.
    pub session: String,
    /// What was paid, in cents, as Stripe says.
    pub cents: u64,
    /// The org it names in its metadata.
    pub org: Option<String>,
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    data: EventData,
}

#[derive(Deserialize)]
struct EventData {
    object: Session,
}

#[derive(Deserialize)]
struct Session {
    id: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    payment_status: Option<String>,
    #[serde(default)]
    amount_total: Option<u64>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    metadata: std::collections::BTreeMap<String, String>,
}

/// The paid checkout a verified event says, if it says one: a completed
/// session paid at once, or a delayed payment that succeeded. Others
/// (unpaid sessions, other events) are `None`.
pub fn paid_checkout(body: &[u8]) -> Result<Option<Paid>, String> {
    let event: Event = serde_json::from_slice(body).map_err(|e| format!("not an event: {e}"))?;
    let session = event.data.object;
    let paid = match event.kind.as_str() {
        "checkout.session.completed" => session.payment_status.as_deref() == Some("paid"),
        "checkout.session.async_payment_succeeded" => true,
        _ => false,
    };
    if !paid
        || session.mode.as_deref() != Some("payment")
        || session.metadata.get("kind").map(String::as_str) != Some("model_credits")
    {
        return Ok(None);
    }
    if session.currency.as_deref() != Some("usd") {
        return Err("a checkout not in US dollars".into());
    }
    let cents = session
        .amount_total
        .filter(|c| *c > 0)
        .ok_or("a paid checkout with no amount")?;
    Ok(Some(Paid {
        session: session.id,
        cents,
        org: session.metadata.get("org").cloned(),
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::http::fake;

    #[test]
    fn adr0039_a_checkout_sells_credits_on_stripes_page_for_the_org() {
        let (url, got) = fake::serve(
            200,
            r#"{"id":"cs_test_1","object":"checkout.session","url":"https://checkout.stripe.com/c/pay/cs_test_1"}"#,
        );
        let stripe = Stripe::with_http(
            Http::insecure_for_tests(),
            Some(&url),
            Secret::new("sk_test_x"),
        );
        let made = stripe
            .checkout(CheckoutRequest {
                org: "acme",
                cents: 10_000,
                success_url: "https://console.example/#/credits/paid",
                cancel_url: "https://console.example/#/credits/cancelled",
            })
            .unwrap();
        assert_eq!(made.id, "cs_test_1");
        assert!(made.url.starts_with("https://checkout.stripe.com/"));
        let sent = got.recv().unwrap();
        assert!(
            sent.head.starts_with("POST /v1/checkout/sessions HTTP/1.1"),
            "{}",
            sent.head
        );
        assert!(
            sent.head
                .to_ascii_lowercase()
                .contains("authorization: bearer sk_test_x")
        );
        for part in [
            "mode=payment",
            "line_items%5B0%5D%5Bprice_data%5D%5Bunit_amount%5D=10000",
            "line_items%5B0%5D%5Bprice_data%5D%5Bcurrency%5D=usd",
            "metadata%5Borg%5D=acme",
            "metadata%5Bkind%5D=model_credits",
            "success_url=https%3A%2F%2Fconsole.example%2F%23%2Fcredits%2Fpaid",
        ] {
            assert!(sent.body.contains(part), "{part} in {}", sent.body);
        }

        let (url, _got) = fake::serve(
            400,
            r#"{"error":{"type":"invalid_request_error","message":"Invalid API Key provided"}}"#,
        );
        let stripe = Stripe::with_http(Http::insecure_for_tests(), Some(&url), Secret::new("k"));
        let err = stripe
            .checkout(CheckoutRequest {
                org: "acme",
                cents: 100,
                success_url: "https://x/",
                cancel_url: "https://x/",
            })
            .unwrap_err();
        assert!(err.contains("Invalid API Key"), "{err}");
    }

    const EVENT: &str = r#"{"id":"evt_1","type":"checkout.session.completed","data":{"object":{"id":"cs_test_1","object":"checkout.session","mode":"payment","payment_status":"paid","amount_total":10000,"currency":"usd","client_reference_id":"acme","metadata":{"org":"acme","kind":"model_credits"}}}}"#;

    #[test]
    fn adr0039_only_a_fresh_signature_with_our_secret_lets_a_webhook_in() {
        let secret = "whsec_test_secret";
        let now = 1_800_000_000;
        let body = EVENT.as_bytes();
        let header = signature_header(secret, now, body);
        verify_webhook(secret, &header, body, now + 10).unwrap();
        // Several signatures, as when Stripe rolls the secret: one is enough.
        verify_webhook(secret, &format!("v1=00ff,{header},v0=abc"), body, now).unwrap();

        let tampered = EVENT.replace("10000", "1000000");
        for (secret_used, header, body, at) in [
            ("whsec_other", header.clone(), body, now),
            (secret, header.clone(), tampered.as_bytes(), now),
            (secret, header.clone(), body, now + 301),
            (secret, "v1=deadbeef".to_owned(), body, now),
            (secret, String::new(), body, now),
        ] {
            assert!(
                verify_webhook(secret_used, &header, body, at).is_err(),
                "{header}"
            );
        }
    }

    #[test]
    fn adr0039_a_paid_event_names_the_session_the_amount_and_the_org() {
        assert_eq!(
            paid_checkout(EVENT.as_bytes()).unwrap(),
            Some(Paid {
                session: "cs_test_1".into(),
                cents: 10_000,
                org: Some("acme".into()),
            })
        );
        // Not yet paid (a bank debit), then paid later.
        let unpaid = EVENT.replace("\"paid\"", "\"unpaid\"");
        assert_eq!(paid_checkout(unpaid.as_bytes()).unwrap(), None);
        let later = unpaid.replace(
            "checkout.session.completed",
            "checkout.session.async_payment_succeeded",
        );
        assert!(paid_checkout(later.as_bytes()).unwrap().is_some());
        // Someone else's checkout, another event, or another currency.
        let other = EVENT.replace("model_credits", "t-shirts");
        assert_eq!(paid_checkout(other.as_bytes()).unwrap(), None);
        let expired = EVENT.replace("checkout.session.completed", "checkout.session.expired");
        assert_eq!(paid_checkout(expired.as_bytes()).unwrap(), None);
        assert!(paid_checkout(EVENT.replace("\"usd\"", "\"eur\"").as_bytes()).is_err());
        assert!(paid_checkout(b"not json").is_err());
    }
}
