//! The relay end to end, in process: Stripe's signed events in, signed
//! pushes out to stand-in consoles over HTTP (ADR 0040).

#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glass_crypto::PublicKey;
use glass_gateway_providers::stripe::signature_header;
use glass_http::{Request, Response};
use glass_managed::billing::{BillingEvent, EVENTS_PATH, SIGNATURE_HEADER, verify};
use glass_types::Timestamp;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::config::{Config, DEV_CONSOLE_TOKEN, DEV_OPERATOR_TOKEN, DEV_WEBHOOK_SECRET};
use crate::outbox::{Outbox, Timing};
use crate::relay::{DevCheckouts, Relay};

/// A console that checks the relay's signature and answers with the
/// statuses it's given, then 200.
async fn console(
    relay_key: PublicKey,
    statuses: Vec<u16>,
) -> (String, mpsc::UnboundedReceiver<BillingEvent>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let statuses = Arc::new(Mutex::new(VecDeque::from(statuses)));
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (tx, statuses, key) = (tx.clone(), Arc::clone(&statuses), relay_key.clone());
            tokio::spawn(async move {
                let Ok(request) = glass_http::read_request(&mut stream).await else {
                    return;
                };
                let signature = request
                    .header(&SIGNATURE_HEADER.to_ascii_lowercase())
                    .unwrap_or_default()
                    .to_owned();
                let now = Timestamp::now().unix_seconds();
                let status = statuses.lock().unwrap().pop_front().unwrap_or(200);
                let response = if request.path != EVENTS_PATH {
                    Response::text(404, "no")
                } else if let Err(e) = verify(&key, &signature, &request.body, now) {
                    Response::json(400, &json!({ "error": e }))
                } else if status != 200 {
                    Response::json(status, &json!({ "error": "busy" }))
                } else {
                    tx.send(serde_json::from_slice(&request.body).unwrap())
                        .unwrap();
                    Response::json(200, &json!({}))
                };
                let _ = glass_http::write_response(&mut stream, response, false).await;
            });
        }
    });
    (url, rx)
}

/// A relay on `dir`, delivering in the background until the handle is
/// aborted (a process that stopped).
fn relay(
    dir: &std::path::Path,
    consoles: &[(&str, &str)],
) -> (Arc<Relay>, tokio::task::JoinHandle<()>) {
    let specs: Vec<String> = consoles.iter().map(|(o, u)| format!("{o}={u}")).collect();
    let config = Config::development(&specs).unwrap();
    let key = glass_managed::device::kept_secret(&dir.join("relay.key"), "the relay key").unwrap();
    let timing = Timing {
        first: Duration::from_secs(1),
        longest: Duration::from_secs(2),
        give_up: Duration::from_secs(3600),
    };
    let outbox = Outbox::open(dir, timing).unwrap();
    let relay = Relay::new(config, key, outbox, Arc::new(DevCheckouts::default()));
    let worker = tokio::spawn(Arc::clone(&relay).deliver());
    (relay, worker)
}

fn request(method: &str, path: &str, headers: &[(&str, String)], body: Vec<u8>) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: String::new(),
        headers: headers
            .iter()
            .map(|(n, v)| ((*n).to_ascii_lowercase(), v.clone()))
            .collect(),
        body,
    }
}

/// Stripe's event for a paid checkout, signed for the development endpoint.
fn paid(session: &str, org: &str, cents: u64, secret: &str) -> Request {
    let body = json!({
        "id": format!("evt_{session}"),
        "type": "checkout.session.completed",
        "data": { "object": {
            "id": session, "mode": "payment", "payment_status": "paid",
            "amount_total": cents, "currency": "usd",
            "metadata": { "org": org, "kind": "model_credits" }
        }}
    })
    .to_string()
    .into_bytes();
    let at = Timestamp::now().unix_seconds();
    request(
        "POST",
        "/stripe/webhook",
        &[("Stripe-Signature", signature_header(secret, at, &body))],
        body,
    )
}

fn json_of(response: &Response) -> Value {
    serde_json::from_slice(&response.body).unwrap()
}

async fn next(rx: &mut mpsc::UnboundedReceiver<BillingEvent>, secs: u64) -> Option<BillingEvent> {
    tokio::time::timeout(Duration::from_secs(secs), rx.recv())
        .await
        .ok()
        .flatten()
}

async fn ops(relay: &Relay) -> Value {
    let auth = format!("Bearer {DEV_OPERATOR_TOKEN}");
    let r = relay
        .handle(&request(
            "GET",
            "/ops/deliveries",
            &[("Authorization", auth)],
            Vec::new(),
        ))
        .await;
    assert_eq!(r.status, 200);
    json_of(&r)
}

#[tokio::test]
async fn adr0040_a_paid_checkout_reaches_its_orgs_console_once_signed_by_the_relay() {
    let dir = tempfile::tempdir().unwrap();
    let key = glass_managed::device::kept_secret(&dir.path().join("relay.key"), "k").unwrap();
    let public = key.public().unwrap();
    let (acme_url, mut acme) = console(public.clone(), vec![]).await;
    let (globex_url, mut globex) = console(public, vec![]).await;
    let (relay, _) = relay(dir.path(), &[("acme", &acme_url), ("globex", &globex_url)]);

    let r = relay
        .handle(&paid("cs_1", "acme", 5000, DEV_WEBHOOK_SECRET))
        .await;
    assert_eq!(r.status, 200);
    let event = next(&mut acme, 5).await.expect("acme's console got it");
    assert_eq!(
        (event.org.as_str(), event.session.as_str(), event.cents),
        ("acme", "cs_1", 5000)
    );
    assert_eq!(event.currency, "usd");

    // Stripe sends events again: the console gets it once. The other org's
    // console never sees it.
    let r = relay
        .handle(&paid("cs_1", "acme", 5000, DEV_WEBHOOK_SECRET))
        .await;
    assert_eq!(r.status, 200);
    assert!(next(&mut acme, 2).await.is_none());
    assert!(globex.try_recv().is_err());
    let seen = ops(&relay).await;
    assert_eq!(seen["delivered"][0]["session"], "cs_1");
    assert_eq!(seen["pending"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn adr0040_only_stripe_signed_events_count_and_unknown_orgs_are_kept_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let (relay, _) = relay(dir.path(), &[("acme", "http://127.0.0.1:9")]);

    let forged = relay
        .handle(&paid("cs_2", "acme", 5000, "whsec_not_ours"))
        .await;
    assert_eq!(forged.status, 400);
    assert!(ops(&relay).await["pending"].as_array().unwrap().is_empty());

    // A payment naming no console of ours: kept for our operators, and
    // Stripe hears 200, so it doesn't send it again for days.
    let r = relay
        .handle(&paid("cs_3", "nobody", 5000, DEV_WEBHOOK_SECRET))
        .await;
    assert_eq!(r.status, 200);
    let seen = ops(&relay).await;
    assert_eq!(seen["unroutable"][0]["session"], "cs_3");
    assert_eq!(seen["unroutable"][0]["org"], "nobody");

    // Not a payment: answered, nothing queued.
    let body = br#"{"type":"customer.created","data":{"object":{"id":"cus_1"}}}"#.to_vec();
    let at = Timestamp::now().unix_seconds();
    let other = request(
        "POST",
        "/stripe/webhook",
        &[(
            "Stripe-Signature",
            signature_header(DEV_WEBHOOK_SECRET, at, &body),
        )],
        body,
    );
    assert_eq!(relay.handle(&other).await.status, 200);

    // Deliveries are our operators' to read, nobody else's.
    let r = relay
        .handle(&request("GET", "/ops/deliveries", &[], Vec::new()))
        .await;
    assert_eq!(r.status, 401);
    let console_token = format!("Bearer {DEV_CONSOLE_TOKEN}");
    let r = relay
        .handle(&request(
            "GET",
            "/ops/deliveries",
            &[("Authorization", console_token)],
            Vec::new(),
        ))
        .await;
    assert_eq!(r.status, 401);
}

#[tokio::test]
async fn adr0040_a_console_that_is_down_gets_the_payment_once_it_is_back() {
    let dir = tempfile::tempdir().unwrap();
    let key = glass_managed::device::kept_secret(&dir.path().join("relay.key"), "k").unwrap();
    let (url, mut acme) = console(key.public().unwrap(), vec![503, 503]).await;
    let (relay, _) = relay(dir.path(), &[("acme", &url)]);

    assert_eq!(
        relay
            .handle(&paid("cs_4", "acme", 2500, DEV_WEBHOOK_SECRET))
            .await
            .status,
        200
    );
    // Waiting: our operators see why.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let seen = ops(&relay).await;
    assert_eq!(seen["pending"][0]["session"], "cs_4");
    assert!(
        seen["pending"][0]["last_error"]
            .as_str()
            .unwrap()
            .contains("503"),
        "{seen}"
    );
    // Tried again after 1 s, then 2 s: the third try is taken.
    let event = next(&mut acme, 10)
        .await
        .expect("delivered once the console answered");
    assert_eq!(event.session, "cs_4");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(ops(&relay).await["delivered"][0]["attempts"], 3);
}

#[tokio::test]
async fn adr0040_payments_queued_while_a_console_was_unreachable_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        // Nothing listens at the console's address yet.
        let (relay, worker) = relay(dir.path(), &[("acme", "http://127.0.0.1:9")]);
        assert_eq!(
            relay
                .handle(&paid("cs_5", "acme", 9900, DEV_WEBHOOK_SECRET))
                .await
                .status,
            200
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(ops(&relay).await["pending"][0]["session"], "cs_5");
        // The process stops.
        worker.abort();
    }
    let key = glass_managed::device::kept_secret(&dir.path().join("relay.key"), "k").unwrap();
    let (url, mut acme) = console(key.public().unwrap(), vec![]).await;
    let (_relay, _worker) = relay(dir.path(), &[("acme", &url)]);
    // The queue kept it, and it goes where the config says acme's console
    // is now.
    let event = next(&mut acme, 10)
        .await
        .expect("delivered after the restart");
    assert_eq!(event.session, "cs_5");
}

#[tokio::test]
async fn adr0040_a_console_gets_checkouts_for_its_own_org_back_to_its_own_address() {
    let dir = tempfile::tempdir().unwrap();
    let (relay, _) = relay(dir.path(), &[("acme", "http://127.0.0.1:7400")]);
    let ask = |cents: u64, back: &str| {
        json!({ "cents": cents, "success_url": back, "cancel_url": "http://127.0.0.1:7400/#/credits" })
            .to_string()
            .into_bytes()
    };
    let token = format!("Bearer {DEV_CONSOLE_TOKEN}");

    let made = relay
        .handle(&request(
            "POST",
            "/v1/checkout",
            &[("Authorization", token.clone())],
            ask(5000, "http://127.0.0.1:7400/#/credits/paid"),
        ))
        .await;
    assert_eq!(made.status, 200, "{}", String::from_utf8_lossy(&made.body));
    let made = json_of(&made);
    assert!(made["id"].as_str().unwrap().starts_with("cs_dev_relay_"));
    assert_eq!(made["url"], "http://127.0.0.1:7400/#/credits/paid");

    for (headers, body, status) in [
        (vec![], ask(5000, "http://127.0.0.1:7400/"), 401),
        (
            vec![("Authorization", "Bearer guess".to_owned())],
            ask(5000, "http://127.0.0.1:7400/"),
            401,
        ),
        (
            vec![("Authorization", token.clone())],
            ask(5000, "https://evil.example/"),
            400,
        ),
        (
            vec![("Authorization", token.clone())],
            ask(5000, "http://127.0.0.1:74000/"),
            400,
        ),
        (
            vec![("Authorization", token.clone())],
            ask(500, "http://127.0.0.1:7400/"),
            400,
        ),
        (
            vec![("Authorization", token.clone())],
            br#"{"cents":5000,"success_url":"x","cancel_url":"y","org":"globex"}"#.to_vec(),
            400,
        ),
    ] {
        let r = relay
            .handle(&request("POST", "/v1/checkout", &headers, body))
            .await;
        assert_eq!(r.status, status, "{}", String::from_utf8_lossy(&r.body));
    }
}

// A delivery whose outcome can't be written down stays due, so the next
// round came at once: on a full disk the relay pushed without a pause.
#[test]
fn a_round_that_couldnt_be_recorded_isnt_tried_again_at_once() {
    use crate::relay::wait_secs;
    // Nothing waiting: a minute.
    assert_eq!(wait_secs(None, 1_000, true), 60);
    // The next is due in 20 s, or was due already.
    assert_eq!(wait_secs(Some(1_020), 1_000, true), 20);
    assert_eq!(wait_secs(Some(990), 1_000, true), 0);
    assert_eq!(wait_secs(Some(5_000), 1_000, true), 60);
    // Due already because its outcome wasn't recorded: not at once.
    assert_eq!(wait_secs(Some(990), 1_000, false), 30);
    assert_eq!(wait_secs(Some(1_045), 1_000, false), 45);
}
