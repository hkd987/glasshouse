# 0039. Model credits through OpenRouter

Status: **Accepted** (2026-10-20). Built: the OpenRouter provider; the
cost and model on the record; a gateway that follows its keys; the
console's credits (one capped key per org, grants by our operators, the
Model credits page, the policy edit) and the key's delivery to computers;
individuals' own keys in Settings. Next: rotating the key; payments. PRD MP-1, MP-4, MP-8, GT-3. The owner asked for a path for
orgs that use neither Bedrock nor Claude Code: admins buy model usage
from us in bulk, and we route it with OpenRouter's auto mode
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
- OpenRouter's consumer terms forbid reselling API access (§7(4)). Its
  enterprise terms license the service for a customer's own end
  customers (§2.1). Credits expire after 365 days (§4.2) and can't be
  moved (§4.4).

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
5. **Adding credits is ours until payments exist.**
   - The console page shows the org its balance, what it has used, and
     whether credits are on.
   - Credits are added by Glasshouse, through an operator route with its
     own token, not by an org admin's session.
   - Card payments, invoices and a markup come with a payment processor,
     the owner's to choose.
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

## What no code supplies
- **An OpenRouter enterprise agreement.** The consumer terms forbid
  reselling access. No paying org can use credits until one is signed.
- A payment processor, pricing and our markup, and what we say about
  credits that expire after 365 days.
- Our OpenRouter balance must always cover the sum of every org's limits.
  The limits are slices of one balance. Low-balance alerts to us are part
  of the console work.
