//! The relay's HTTP side and its delivery worker (ADR 0040).
//!
//! - `POST /stripe/webhook`: Stripe's events. Checked against the
//!   endpoint's secret; a paid checkout for model credits is queued on disk
//!   for the console of the org it names, then answered 200. Only a queue
//!   that can't be written answers otherwise, so Stripe retries exactly
//!   when we lost nothing yet.
//! - `POST /v1/checkout`: a console asking for a Checkout Session, with its
//!   token. The org is the token's; Stripe sends the admin back only to
//!   that console's own address.
//! - `GET /ops/deliveries`, `POST /ops/deliveries/retry`: our operators,
//!   with their token: what's queued, dead or unroutable, and sending a
//!   dead one again.
//!
//! The worker pushes each queued payment to its console, signed afresh
//! with the relay's key every attempt, and waits longer after each failure
//! until the console takes it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use glass_crypto::SecretKey;
use glass_gateway_providers::billing::ConsolePush;
use glass_gateway_providers::stripe::{self, Checkout, CheckoutRequest};
use glass_http::{ReadError, Request, Response};
use glass_managed::billing::{
    BillingEvent, CHECKOUT_PATH, CheckoutAsk, CheckoutMade, EventKind, MAX_CHECKOUT_CENTS,
    MIN_CHECKOUT_CENTS,
};
use glass_types::Timestamp;
use serde::Deserialize;
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

use crate::config::Config;
use crate::outbox::{Delivery, Outbox, Unroutable};

/// How the relay makes Checkout Sessions.
pub trait Checkouts: Send + Sync + std::fmt::Debug {
    fn checkout(&self, request: CheckoutRequest<'_>) -> Result<Checkout, String>;
}

impl Checkouts for stripe::Stripe {
    fn checkout(&self, request: CheckoutRequest<'_>) -> Result<Checkout, String> {
        stripe::Stripe::checkout(self, request)
    }
}

/// Development: a checkout that sends the admin straight back, as if paid.
/// The payment itself still arrives only as a signed webhook.
#[derive(Debug, Default)]
pub struct DevCheckouts {
    made: std::sync::atomic::AtomicU64,
}

impl Checkouts for DevCheckouts {
    fn checkout(&self, request: CheckoutRequest<'_>) -> Result<Checkout, String> {
        let n = self.made.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        Ok(Checkout {
            id: format!("cs_dev_relay_{n}"),
            url: request.success_url.to_owned(),
        })
    }
}

pub struct Relay {
    config: Config,
    key: Arc<SecretKey>,
    outbox: Mutex<Outbox>,
    checkouts: Arc<dyn Checkouts>,
    pushes: Mutex<HashMap<String, Arc<ConsolePush>>>,
    wake: Notify,
}

fn now() -> i64 {
    Timestamp::now().unix_seconds()
}

fn ok(value: serde_json::Value) -> Response {
    Response::json(200, &value)
}

fn error(status: u16, message: &str) -> Response {
    Response::json(status, &json!({ "error": message }))
}

fn bearer(request: &Request) -> &str {
    request
        .header("authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or_default()
}

/// Whether `url` is on the console at `base`: Stripe sends admins back to
/// their own console only.
fn on_console(url: &str, base: &str) -> bool {
    url.strip_prefix(base.trim_end_matches('/'))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '#', '?']))
}

impl Relay {
    pub fn new(
        config: Config,
        key: SecretKey,
        outbox: Outbox,
        checkouts: Arc<dyn Checkouts>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            key: Arc::new(key),
            outbox: Mutex::new(outbox),
            checkouts,
            pushes: Mutex::new(HashMap::new()),
            wake: Notify::new(),
        })
    }

    fn outbox(&self) -> std::sync::MutexGuard<'_, Outbox> {
        self.outbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Serves until the listener fails, delivering in the background.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> anyhow::Result<()> {
        tokio::spawn(Arc::clone(&self).deliver());
        loop {
            let (stream, _) = listener.accept().await?;
            let relay = Arc::clone(&self);
            tokio::spawn(async move { relay.connection(stream).await });
        }
    }

    async fn connection(&self, mut stream: TcpStream) {
        let response = match glass_http::read_request_up_to(&mut stream, glass_http::MAX_BODY).await
        {
            Ok(request) => self.handle(&request).await,
            Err(ReadError::Gone) => return,
            Err(ReadError::Bad(status, why)) => Response::text(status, why),
        };
        let _ = glass_http::write_response(&mut stream, response, false).await;
    }

    pub async fn handle(&self, request: &Request) -> Response {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/stripe/webhook") => self.webhook(request),
            ("POST", CHECKOUT_PATH) => self.checkout(request).await,
            ("GET", "/ops/deliveries") => self.deliveries(request),
            ("POST", "/ops/deliveries/retry") => self.retry(request),
            ("GET", "/healthz") => Response::text(200, "ok"),
            _ => error(404, "no such page"),
        }
    }

    /// Stripe's event: a paid checkout is queued for its org's console.
    fn webhook(&self, request: &Request) -> Response {
        let signature = request.header("stripe-signature").unwrap_or_default();
        if let Err(e) =
            stripe::verify_webhook(&self.config.webhook_secret, signature, &request.body, now())
        {
            tracing::warn!("a webhook refused: {e}");
            return error(400, &e);
        }
        let paid = match stripe::paid_checkout(&request.body) {
            Ok(Some(paid)) => paid,
            Ok(None) => return ok(json!({})),
            Err(e) => {
                tracing::warn!("a signed webhook that isn't a payment we read: {e}");
                return error(400, &e);
            }
        };
        let console = paid.org.as_deref().and_then(|org| self.config.console(org));
        let Some(console) = console else {
            let kept = self.outbox().unroutable_payment(Unroutable {
                session: paid.session.clone(),
                org: paid.org.clone(),
                cents: paid.cents,
                at: Timestamp::now(),
            });
            return match kept {
                Ok(_) => {
                    tracing::warn!(
                        session = %paid.session,
                        org = paid.org.as_deref().unwrap_or("none"),
                        "a payment for no console of ours: kept as unroutable"
                    );
                    ok(json!({}))
                }
                Err(e) => {
                    tracing::error!("keeping an unroutable payment: {e:#}");
                    error(500, "not kept; send it again")
                }
            };
        };
        let event = BillingEvent {
            kind: EventKind::CreditsPaid,
            org: console.org.clone(),
            session: paid.session.clone(),
            cents: paid.cents,
            currency: "usd".into(),
            at: Timestamp::now(),
        };
        let queued = self.outbox().queue(event, &console.url, now());
        match queued {
            Ok(true) => {
                tracing::info!(
                    org = %console.org,
                    session = %paid.session,
                    cents = paid.cents,
                    "payment queued for its console"
                );
                self.wake.notify_one();
                ok(json!({}))
            }
            Ok(false) => {
                tracing::info!(session = %paid.session, "a payment Stripe sent again: taken already");
                ok(json!({}))
            }
            Err(e) => {
                tracing::error!(session = %paid.session, "queueing a payment: {e:#}");
                error(500, "not queued; send it again")
            }
        }
    }

    /// A console's checkout, for its own org.
    async fn checkout(&self, request: &Request) -> Response {
        let Some(console) = self.config.console_with_token(bearer(request)) else {
            return error(401, "That isn't a console's token.");
        };
        let ask: CheckoutAsk = match serde_json::from_slice(&request.body) {
            Ok(a) => a,
            Err(e) => return error(400, &format!("not a checkout request: {e}")),
        };
        if !(MIN_CHECKOUT_CENTS..=MAX_CHECKOUT_CENTS).contains(&ask.cents) {
            return error(
                400,
                &format!(
                    "A checkout is between ${} and ${}.",
                    MIN_CHECKOUT_CENTS / 100,
                    MAX_CHECKOUT_CENTS / 100
                ),
            );
        }
        if !on_console(&ask.success_url, &console.url) || !on_console(&ask.cancel_url, &console.url)
        {
            return error(400, "Stripe sends admins back to their own console only.");
        }
        let checkouts = Arc::clone(&self.checkouts);
        let org = console.org.clone();
        let made = tokio::task::spawn_blocking(move || {
            checkouts.checkout(CheckoutRequest {
                org: &org,
                cents: ask.cents,
                success_url: &ask.success_url,
                cancel_url: &ask.cancel_url,
            })
        })
        .await;
        match made {
            Ok(Ok(checkout)) => {
                tracing::info!(org = %console.org, session = %checkout.id, cents = ask.cents, "checkout made");
                ok(serde_json::to_value(CheckoutMade {
                    id: checkout.id,
                    url: checkout.url,
                })
                .unwrap_or_default())
            }
            Ok(Err(e)) => {
                tracing::warn!(org = %console.org, "a checkout Stripe didn't make: {e}");
                error(502, &e)
            }
            Err(e) => error(500, &e.to_string()),
        }
    }

    fn operator(&self, request: &Request) -> bool {
        self.config.is_operator(bearer(request))
    }

    /// What's queued, dead, unroutable and lately delivered.
    fn deliveries(&self, request: &Request) -> Response {
        if !self.operator(request) {
            return error(401, "Only Glasshouse's operators read deliveries.");
        }
        let outbox = self.outbox();
        let shown = |d: &Delivery| {
            json!({
                "org": d.event.org,
                "session": d.event.session,
                "cents": d.event.cents,
                "paid_at": d.event.at,
                "attempts": d.attempts,
                "next_at": d.next_at,
                "last_error": d.last_error,
                "delivered_at": d.delivered_at,
            })
        };
        let listed = (|| -> anyhow::Result<serde_json::Value> {
            Ok(json!({
                "pending": outbox.list("outbox")?.iter().map(shown).collect::<Vec<_>>(),
                "dead": outbox.list("dead")?.iter().map(shown).collect::<Vec<_>>(),
                "unroutable": outbox.unroutable()?,
                "delivered": outbox.delivered_log(20)?.iter().map(shown).collect::<Vec<_>>(),
            }))
        })();
        match listed {
            Ok(v) => ok(v),
            Err(e) => error(500, &format!("{e:#}")),
        }
    }

    /// Sends a dead delivery again.
    fn retry(&self, request: &Request) -> Response {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Retry {
            session: String,
        }
        if !self.operator(request) {
            return error(401, "Only Glasshouse's operators retry deliveries.");
        }
        let Ok(retry) = serde_json::from_slice::<Retry>(&request.body) else {
            return error(400, "name the session to send again");
        };
        let mut outbox = self.outbox();
        let org = outbox
            .list("dead")
            .ok()
            .and_then(|dead| dead.into_iter().find(|d| d.event.session == retry.session))
            .map(|d| d.event.org);
        let Some(console) = org.and_then(|o| self.config.console(&o)) else {
            return error(
                404,
                "No dead delivery for that session, or its console is gone.",
            );
        };
        match outbox.revive(&retry.session, &console.url, now()) {
            Ok(true) => {
                drop(outbox);
                self.wake.notify_one();
                ok(json!({}))
            }
            Ok(false) => error(404, "No dead delivery for that session."),
            Err(e) => error(500, &format!("{e:#}")),
        }
    }

    /// Pushes what's due, then sleeps until the next is due or one is
    /// queued.
    pub(crate) async fn deliver(self: Arc<Self>) {
        loop {
            let due = self.outbox().due(now()).unwrap_or_else(|e| {
                tracing::error!("reading the outbox: {e:#}");
                Vec::new()
            });
            let mut pushing = tokio::task::JoinSet::new();
            for delivery in due {
                pushing.spawn(Arc::clone(&self).deliver_one(delivery));
            }
            while pushing.join_next().await.is_some() {}
            let next = self.outbox().next_due().ok().flatten();
            let wait = next.map_or(60, |at| (at - now()).clamp(0, 60));
            tokio::select! {
                () = self.wake.notified() => {}
                () = tokio::time::sleep(Duration::from_secs(wait.unsigned_abs())) => {}
            }
        }
    }

    fn push_to(&self, url: &str) -> Result<Arc<ConsolePush>, String> {
        let mut pushes = self.pushes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(push) = pushes.get(url) {
            return Ok(Arc::clone(push));
        }
        let push = Arc::new(ConsolePush::new(url, self.config.allow_insecure_http)?);
        pushes.insert(url.to_owned(), Arc::clone(&push));
        Ok(push)
    }

    async fn deliver_one(self: Arc<Self>, delivery: Delivery) {
        let body = match serde_json::to_vec(&delivery.event) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("encoding a delivery: {e}");
                return;
            }
        };
        let key = Arc::clone(&self.key);
        // Where its console is now, if the config moved it since.
        let url = self
            .config
            .console(&delivery.event.org)
            .map_or_else(|| delivery.console.clone(), |c| c.url.clone());
        let pushed = match self.push_to(&url) {
            Ok(push) => tokio::task::spawn_blocking(move || {
                push.push(&key, &body, now()).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string())),
            Err(e) => Err(e),
        };
        let (org, session, attempt) = (
            delivery.event.org.clone(),
            delivery.event.session.clone(),
            delivery.attempts + 1,
        );
        match pushed {
            Ok(()) => match self.outbox().delivered(delivery) {
                Ok(()) => tracing::info!(%org, %session, attempt, "delivered to its console"),
                Err(e) => tracing::error!(%org, %session, "delivered, but not recorded: {e:#}"),
            },
            Err(why) => match self.outbox().failed(delivery, &why, now()) {
                Ok(Some(next)) => tracing::warn!(
                    %org, %session, attempt,
                    "its console didn't take it ({why}); trying again in {}s",
                    next - now()
                ),
                Ok(None) => tracing::error!(
                    %org, %session, attempt,
                    "its console didn't take it ({why}); given up, see /ops/deliveries"
                ),
                Err(e) => tracing::error!(%org, %session, "recording a failed delivery: {e:#}"),
            },
        }
    }
}
