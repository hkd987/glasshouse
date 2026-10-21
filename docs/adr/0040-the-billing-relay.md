# 0040. The billing relay: one Stripe endpoint, payments pushed to each org's console

Status: **Accepted** (2026-10-21). Built: `glass-billing` (Stripe's
webhooks, the queue, pushes, checkouts for consoles, the operators'
view); consoles taking payments from it; the console e2e running the
whole chain. PRD MP-10. The owner asked for a way to get Stripe's
webhooks and push them out into client admin panels (2026-10-21).
Amends ADR 0039 §5, where each console took Stripe's webhooks itself.

## Context
- ADR 0039 has each org's console make Stripe Checkout Sessions with our
  Stripe secret key and take Stripe's webhook at its own
  `/stripe/webhook`, keeping only payments whose metadata names its org.
- Every console is on our servers, one per org (ADR 0023), all on one
  Stripe account of ours. Stripe allows an account 16 webhook endpoints,
  and sends every event to every endpoint: past 16 orgs that breaks, and
  before it every console sees every org's payments and holds our Stripe
  secret key.
- Stripe retries a webhook it didn't get a 2xx for, for up to three days,
  and may send one more than once. A console can be down, deploying or
  moved when one arrives.

## Decision
1. **One endpoint for every org: the billing relay.** `glass-billing`
   (`crates/bin/glass-billing`), on our servers, never in an image, is
   the only webhook endpoint on our Stripe account. It holds Stripe's
   secret key and the endpoint's secret; consoles hold neither.
2. **It checks, then queues, then answers.** A webhook counts only with a
   valid `Stripe-Signature` for the endpoint's secret, at most 5 minutes
   old. A paid checkout for model credits (`metadata.kind`) is written to
   disk for the console of the org its metadata names, one file per
   checkout session, before the relay answers 200. Only a queue it can't
   write answers otherwise, so Stripe retries exactly when nothing was
   kept. A session is queued once however often Stripe sends it. A
   payment naming no console of ours is kept as unroutable and answered
   200, so Stripe doesn't retry it for days; our operators see it.
3. **It pushes to the org's console, signed with its own key.** The
   relay's P-256 key (kept like a device key, owner only;
   `glass-billing public-key` prints the public half) signs each
   `BillingEvent` (org, session, cents, currency, when) over
   `glasshouse-billing-event-v1`, the time and the body's SHA-256
   (`glass_managed::billing`). The console's `POST /billing/v1/events`
   takes one only with the relay's signature, made within 5 minutes, for
   its own org, and grants it once per session, as ADR 0039's webhook did.
   Another org's event is refused, so the relay's misroute shows.
4. **Until the console takes it.** The relay pushes at once, then after
   5 s, and three times longer each time up to an hour, signing every try
   afresh, for three days; then the delivery is dead. It always pushes to
   where the config says the org's console is now. `GET /ops/deliveries`
   shows pending (with the last error), dead, unroutable and lately
   delivered; `POST /ops/deliveries/retry` sends a dead one again. Both
   need our operators' token. A restart loses nothing.
5. **Consoles ask it for checkouts.** `POST /v1/checkout` with the
   console's own token (the relay keeps its SHA-256). The org is the
   token's, never the request's; Stripe sends the admin back only to that
   console's own address; $10 to $25,000.
6. **A console takes payments from one source.** With `billing_url`,
   `billing_token` and `billing_public_key` it uses the relay, and its
   `/stripe/webhook` is closed. A console alone on a Stripe account may
   still take Stripe's webhooks itself (`stripe_secret_key`,
   `stripe_webhook_secret`); never both.

## Consequences
- Any number of orgs on one Stripe account and one endpoint. Stripe's
  secret key is on one service; a console that leaks can't charge cards
  or forge payments for other orgs.
- Payments survive a console's outage or redeploy, and our operators can
  see and resend any that didn't land.
- The relay is one more service to run and watch. While it's down,
  Stripe holds and retries the webhooks (three days), and admins can't
  start a checkout.
- OpenRouter's management key is still on each console (ADR 0039): the
  next thing to centralize the same way.

## Not built
- TLS and the address Stripe calls are our web servers', in front, as for
  the console.
- Refunds and disputes (`charge.refunded`, `charge.dispute.created`) are
  answered 200 and not acted on; what they should do to credits is the
  owner's call with ADR 0039's refund terms.
