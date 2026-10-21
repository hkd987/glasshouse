//! The billing relay's contract (ADR 0040): how payments reach each org's
//! admin console when one Stripe account serves every org.
//!
//! Stripe sends every event to one endpoint of ours, the relay
//! (`glass-billing`), which checks Stripe's signature and pushes what was
//! paid to the console of the org the payment names, as a
//! [`BillingEvent`] signed with the relay's own P-256 key. A console takes
//! one only with a valid signature from the relay key it was configured
//! with, no more than [`MAX_SKEW_SECS`] from its clock, for its own org;
//! the grant is once per checkout session however often it arrives.
//!
//! Consoles also ask the relay for a Checkout Session ([`CheckoutAsk`],
//! with the console's own token), so Stripe's secret key is on the relay
//! alone. The org a checkout is for comes from the token, never from the
//! request.

use glass_crypto::{PublicKey, SecretKey, sha256};
use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

/// Where the relay pushes events, on a console.
pub const EVENTS_PATH: &str = "/billing/v1/events";
/// Where a console asks the relay for a checkout.
pub const CHECKOUT_PATH: &str = "/v1/checkout";
/// The relay's signature over an event: `t=<unix seconds>,sig=<hex>`.
pub const SIGNATURE_HEADER: &str = "Glasshouse-Billing-Signature";
/// How far an event's time may be from the console's. Every delivery is
/// signed afresh, so retries hours later still pass.
pub const MAX_SKEW_SECS: i64 = 300;

/// The smallest checkout the relay makes, in cents ($10).
pub const MIN_CHECKOUT_CENTS: u64 = 1_000;
/// The largest, in cents ($25,000): a typing slip, not a purchase.
pub const MAX_CHECKOUT_CENTS: u64 = 2_500_000;

/// What the relay tells a console.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Model credits were paid for (ADR 0039).
    CreditsPaid,
}

/// One payment, as Stripe's verified event said it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingEvent {
    pub kind: EventKind,
    /// The org the checkout named.
    pub org: String,
    /// Stripe's Checkout Session: one grant per session.
    pub session: String,
    /// What was paid, in cents of `currency`.
    pub cents: u64,
    pub currency: String,
    /// When the relay took it from Stripe.
    pub at: Timestamp,
}

/// What the relay signs for an event sent at `time`.
fn signing_input(time: i64, body: &[u8]) -> Vec<u8> {
    format!(
        "glasshouse-billing-event-v1\n{time}\n{}",
        hex::encode(sha256(body))
    )
    .into_bytes()
}

/// The header value that signs `body` at `time` with the relay's key.
pub fn sign(key: &SecretKey, time: i64, body: &[u8]) -> Result<String, String> {
    let signature = key
        .sign(&signing_input(time, body))
        .map_err(|e| e.to_string())?;
    Ok(format!("t={time},sig={}", hex::encode(signature)))
}

/// Whether `header` is the relay's signature over `body`, made within
/// [`MAX_SKEW_SECS`] of `now`.
pub fn verify(key: &PublicKey, header: &str, body: &[u8], now: i64) -> Result<(), String> {
    let mut time = None;
    let mut signature = None;
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", t)) => time = t.parse::<i64>().ok(),
            Some(("sig", s)) => signature = hex::decode(s).ok(),
            _ => {}
        }
    }
    let (Some(time), Some(signature)) = (time, signature) else {
        return Err(format!("no {SIGNATURE_HEADER} to check"));
    };
    if (now - time).abs() > MAX_SKEW_SECS {
        return Err("the event's signature is too old".into());
    }
    if key.verify(&signing_input(time, body), &signature) {
        Ok(())
    } else {
        Err("the event isn't signed by the billing relay".into())
    }
}

/// `POST /v1/checkout` on the relay, with the console's token: a checkout
/// for `cents` of model credits, for the token's org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutAsk {
    pub cents: u64,
    /// Where Stripe sends the admin after paying: on the console's own
    /// address, which the relay checks.
    pub success_url: String,
    pub cancel_url: String,
}

/// The checkout Stripe made: its id and the page to send the admin to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutMade {
    pub id: String,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> Vec<u8> {
        serde_json::to_vec(&BillingEvent {
            kind: EventKind::CreditsPaid,
            org: "acme".into(),
            session: "cs_live_1".into(),
            cents: 5000,
            currency: "usd".into(),
            at: Timestamp::now(),
        })
        .unwrap()
    }

    #[test]
    fn adr0040_an_event_verifies_only_as_the_relay_signed_it() {
        let relay = SecretKey::generate().unwrap();
        let public = relay.public().unwrap();
        let body = event();
        let now = 1_792_500_000;
        let header = sign(&relay, now, &body).unwrap();
        assert!(header.starts_with(&format!("t={now},sig=")));
        verify(&public, &header, &body, now + 10).unwrap();

        // Another body, another key, an old or missing signature: refused.
        let mut tampered = body.clone();
        tampered[10] ^= 1;
        assert!(verify(&public, &header, &tampered, now).is_err());
        let other = SecretKey::generate().unwrap().public().unwrap();
        assert!(verify(&other, &header, &body, now).is_err());
        let err = verify(&public, &header, &body, now + MAX_SKEW_SECS + 1).unwrap_err();
        assert!(err.contains("too old"), "{err}");
        assert!(verify(&public, "", &body, now).is_err());
        assert!(verify(&public, "t=1,sig=zz", &body, 1).is_err());
    }

    #[test]
    fn adr0040_events_and_asks_keep_their_shape() {
        let back: BillingEvent = serde_json::from_slice(&event()).unwrap();
        assert_eq!(back.kind, EventKind::CreditsPaid);
        let text = serde_json::to_string(&back).unwrap();
        assert!(text.contains(r#""kind":"credits_paid""#), "{text}");
        assert!(
            serde_json::from_str::<CheckoutAsk>(
                r#"{"cents":5000,"success_url":"a","cancel_url":"b","org":"other"}"#
            )
            .is_err(),
            "a console can't name the org"
        );
    }
}
