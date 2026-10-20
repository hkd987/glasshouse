# 0039. Model credits through OpenRouter

Status: **Accepted** (2026-10-20). Built: the OpenRouter provider; the
cost and model on the record; a gateway that follows its keys; the
console's credits (one capped key per org, grants by our operators, the
Model credits page, the policy edit) and the key's delivery to computers;
individuals' own keys in Settings; payment by card through Stripe at our
markup, and the providers' terms accepted for each org. Next: whether
credits' requests go through our servers (open, below); rotating the key.
PRD MP-1, MP-4, MP-8, MP-10, MP-11, GT-3. The owner asked for a path for
orgs that use neither Bedrock nor Claude Code: admins buy model usage
from us in bulk, and we route it with OpenRouter's auto mode
(2026-10-20). The owner chose Stripe for payments and a markup over
OpenRouter's price, with requests passed through to OpenRouter
(2026-10-20).

## Context
- Agents reach models only through the model gateway (ADR 0018). Policy
  names the providers and models, and routes each data label to models in
  order. Keys live in a root-only secrets file, outside every jail.
- Today an org needs its own Bedrock account (MP-1) or an Anthropic key
  (MP-4). Many small orgs have neither. An individual has neither either,
  though first-run setup asks them how agents should reach models.
- OpenRouter sells one balance that reaches many models. `openrouter/auto`
  picks a model per request. Each reply says which model answered
  (`model`) and what the call cost (`usage.cost`). A request can demand
  providers that keep no data and don't train on it
  (`provider: {zdr: true, data_collection: "deny"}`). A management key
  creates API keys with a credit `limit` that OpenRouter enforces, and
  reports each key's `usage` and `limit_remaining`. Used-up credits answer
  HTTP 402.
- OpenRouter's terms (openrouter.ai/terms, read 2026-10-20):
  - forbid using it "for purposes of reselling API access to Models"
    (§7, item 4) and to "sell or otherwise transfer the access granted"
    (§7, item 14);
  - provide for giving "your customers (to the extent you incorporate
    the Service into your own products and services) access to Models"
    (§5.1), with those customers bound by the model providers' terms and
    us answerable for them (§5.1, §5.2, §5.5);
  - let credits expire 365 days after purchase (§4.2); they aren't
    refundable after 24 hours (§4.1) nor transferable (§4.4).
  Which of those Glasshouse credits are is for the owner and counsel; the
  design below decides what they look like.

## Decision
1. **OpenRouter is a provider kind (`openrouter`).**
   - Every request carries `zdr: true` and `data_collection: "deny"`, so
     a call no such provider can serve fails rather than going elsewhere.
   - Used-up credits fail over to the next model on the route, with words
     that say so.
   - The audit record carries what the call cost and which model
     answered.
2. **One key per org, capped at what the org bought.**
   - The console holds our management key, on our servers. Neither org
     admins nor computers ever see it.
   - When an org first gets credits, the console creates the org's key
     with `limit` = the credits it bought. More credits raise the limit.
     OpenRouter refuses calls past it, so no computer can overspend
     whatever it does.
   - Per-computer keys were rejected: a limit caps one key. An org's pool
     would have to be split and rebalanced, or overspent N times.
   - A proxy through the console was rejected (ADR 0018). Model traffic
     leaves from the computer, and our servers never hold prompts.
3. **Computers get the org's key over their signed sync (ADR 0030 slice
   P).**
   - The key arrives as its own field of the sync reply, only to a
     computer whose request its own registered key signed. It is never in
     a signed release, which is kept and logged.
   - glassd keeps it root only beside its state, outside every jail. It
     rebuilds the gateway when the key arrives, changes or goes.
4. **Policy says where it may be used, as for any provider.**
   - Turning credits on in the console is a policy edit, published like
     any rule. It adds the provider `credits`, the model `auto`
     (`openrouter/auto`), and makes it the default when the org has none.
   - The default covers public and internal data. Confidential data goes
     to credits only if IT adds them to its route (GT-3).
   - Agents still need `model.complete` in their allowed actions.
5. **Admins pay by card through Stripe, at our markup.**
   - The console makes a Stripe Checkout Session and sends the admin to
     Stripe's page; card details never reach us. Each session names the
     org in its metadata.
   - Stripe's webhook (`/stripe/webhook`) says it was paid. The console
     takes it only with a valid `Stripe-Signature` for the endpoint's
     secret, at most 5 minutes old, for its own org, and grants what the
     event says was paid, once per session however often Stripe sends it.
     Coming back from Stripe's page grants nothing.
   - Our `markup` (in the console's credits file, required: 0.25 is 25 %)
     is taken at the grant: $100 paid raises the key's OpenRouter limit by
     $80. The org sees its own dollars throughout: bought, spent and left
     are OpenRouter's numbers at the rate it paid. The markup is never
     shown.
   - Our operators can still add credits (an invoice, a gift) with their
     own token, at the same markup. Admins can't.
   - Before an org buys, or agents use credits, an admin accepts
     OpenRouter's terms and the model providers' for the org (§5.1); the
     console records who and when.
6. **Individuals use the same provider with their own key.**
   - On a computer no organization manages, its owner (ADR 0038) connects
     an OpenRouter or Anthropic key in Settings. glassd keeps it root
     only, and adds the provider and a default model to the computer's
     own policy.
   - Of setup's answers, only "API key" can be applied now. "Claude
     Code" and "ChatGPT" sign-ins need OAuth flows that aren't built.

## Consequences
- An org with no cloud account can turn models on in minutes. Its spend
  is capped by OpenRouter, and each call's cost and model are on the
  record.
- Model traffic goes to a further party (OpenRouter, then the provider it
  picks). The ZDR flag is OpenRouter's promise for those providers; the
  PRD's GT-3 wording has to say so, and confidential data stays off it by
  default.
- A computer that leaves the org keeps a key until it's rotated. Rotating
  (create, push, disable the old one) is the next step, and so is
  rotating when IT removes a computer.
- **Open: should credits' requests go through our servers?** Today the
  org's OpenRouter key is on each of its computers, readable by root, so
  an org's IT could use it outside Glasshouse: that is close to the
  "transfer the access granted" §7 forbids. Passing requests through our
  servers keeps the key with us (the computer signs each request with its
  own key, ADR 0030 slice P), makes rotation moot and meters each call
  exactly, but prompts (redacted on the computer) would cross our
  servers, which GT-3 says they don't. The owner decides; until then
  credits stay as built.

## What no code supplies
- Whether Glasshouse credits are "incorporated into our product" (§5.1)
  or "reselling API access" (§7): the owner's and counsel's call, best
  confirmed with OpenRouter in writing before the first paying org.
- Our markup, the Stripe account (its secret key and the webhook
  endpoint's secret, in the console's credits file), our refund terms
  against OpenRouter's 24 hours, and what we say about credits that
  expire after 365 days.
- Our OpenRouter balance must always cover the sum of every org's limits.
  The limits are slices of one balance. Low-balance alerts to us are part
  of the console work.
