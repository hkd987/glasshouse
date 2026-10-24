# 0039. Model credits through OpenRouter

Status: **Accepted** (2026-10-20). Built: the OpenRouter provider; the
cost and model on the record; a gateway that follows its keys; the
console's credits (one capped key per org, grants by our operators, the
Model credits page, the policy edit); computers' calls through the
console, which keeps the org's key; individuals' own keys in Settings;
payment by card through Stripe at our markup, and the providers' terms
accepted for each org; IT's choice of which agents ask models, and the IT
agent asking them (AE-4); low-balance alerts for admins and for us.
PRD MP-1, MP-4, MP-8, MP-10, MP-11, GT-3. The owner asked for a path for
orgs that use neither Bedrock nor Claude Code: admins buy model usage
from us in bulk, and we route it with OpenRouter's auto mode
(2026-10-20). The owner chose Stripe for payments and a markup over
OpenRouter's price, with requests passed through to OpenRouter, and then
that credits' requests go through our servers (2026-10-20). For credits
only, this amends ADR 0018: their prompts cross our servers.

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
   - The org's key stays on our servers too: no computer ever holds it.
3. **Computers call models on credits through the console.**
   - Policy names the provider with kind `credits`. On a managed computer
     glassd's `credits` provider sends each call to the console
     (`/device/v1/models/complete`), signed with the computer's own key
     (ADR 0030 slice P), on a connection of its own. It waits 30 s longer
    than the console's own call to OpenRouter (idle and in all), so a slow
    model times out at the console and the computer hears why, rather than
    failing over while the console still pays for the first answer.
   - The console forwards it to OpenRouter with the org's key, for a model
     the org's published policy runs on credits, at most 32,768 tokens out
     and 64 calls at once. Used-up credits are refused in words, and the
     computer fails over to the next model on its route.
   - The console keeps which computer called, the tokens and the cost,
     never the prompt or the reply (`model-usage.json`). The answer tells
     the computer what the call cost the org, at the rate it paid, for its
     audit record. Admins see spend by computer.
   - A computer sent the key by an earlier console is told to forget it:
     glassd removes it, on the record, and never keeps a key it's sent.
   - What crosses our servers was redacted on the computer already (DP-3):
     secrets always, and the personal details policy names.
4. **Policy says where it may be used, as for any provider.**
   - Turning credits on in the console is a policy edit, published like
     any rule. It adds the provider `credits`, the model `auto`
     (`openrouter/auto`), and makes it the default when the org has none.
   - The default covers public and internal data. Confidential data goes
     to credits only if IT adds them to its route (GT-3).
   - Agents still need `model.complete` in their allowed actions. IT
     chooses which agents ask AI models on the policy page, for every
     group (the same rules every model kind uses). None do until IT says
     so, except `skill-evals`, whose whole job is asking models.
5. **Admins pay by card through Stripe, at our markup.** (With more than
   one org, through the billing relay: ADR 0040.)
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
   - The owner chooses which agents use them, in Settings, under AI
     models (`settings.models_agents`, the owner's alone, never an
     agent's): each choice adds `model.complete` to that agent's rules on
     this computer. The IT agent may until the owner says otherwise (setup
     asked how agents reach models, and it sends only the request and the
     findings in words); others only on the owner's word; agents whose
     rules already ask models (`skill-evals`) show as always on.
   - Of setup's answers, only "API key" can be applied now. "Claude
     Code" and "ChatGPT" sign-ins need OAuth flows that aren't built.

## Consequences
- An org with no cloud account can turn models on in minutes. Its spend
  is capped by OpenRouter, and each call's cost and model are on the
  record.
- Model traffic on credits goes through our servers and then to further
  parties (OpenRouter, then the provider it picks). The ZDR flag is
  OpenRouter's promise for those providers; GT-3 says all of this, and
  confidential data stays off credits by default.
- Customers never hold OpenRouter access: the key is ours, used only for
  calls Glasshouse's agents make for them, which is how §5.1 describes
  incorporating the service. A lost or removed computer has no key to
  misuse; removing it from the org stops its calls at once.
- Each call takes one more hop. The console is now in the path of model
  calls on credits: it has to stay up and scale with them.

## What no code supplies
- Whether Glasshouse credits are "incorporated into our product" (§5.1)
  or "reselling API access" (§7): the owner's and counsel's call, best
  confirmed with OpenRouter in writing before the first paying org.
- Our markup, the Stripe account (its secret key and the webhook
  endpoint's secret, in the console's credits file), our refund terms
  against OpenRouter's 24 hours, and what we say about credits that
  expire after 365 days.
- Our OpenRouter balance must always cover the sum of every org's limits.
  The limits are slices of one balance. `GET /ops/credits` (our
  operators' token) says whether it does, from OpenRouter's account and
  every key's unspent limit, and logs an error when it doesn't: our
  monitoring has to poll it and page someone, which isn't set up.

## Low balance
- An org's admins are warned below a threshold they set (a tenth of what
  they bought, at least $5, until they do): on Model credits, and beside
  it in the sidebar on every page. The sidebar's figure comes from the
  console's own record of its computers' calls, so no page waits on
  OpenRouter; Model credits shows OpenRouter's own when it answers.
- Used up, the page says agents' calls on credits go to the next model
  on the route, or nowhere, until they buy more. No email or other
  notice leaves the console yet.

## Amended by ADR 0052
What its model (`specs/tla/ModelCredits.tla`) shows the design allows:
- spend can pass the key's limit by the calls already under way when it's
  reached, at most the console's 64 permits;
- a call is billed twice only when credits charged it and its answer was
  lost, so the next model on the route took it too.
