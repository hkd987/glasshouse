# 0043. Anthropic's API in our own harness

Status: **Accepted, built** (2026-10-22). The owner's call after ADR
0042: "Guess we are doing anthropic api support in our harness." PRD
MP-4, MP-7, MP-8, DP-3, DP-5, AE-3, AE-4. Extends ADR 0018 (the model
gateway) and ADR 0041 (tool use, the desktop agent).

## Context
- Our agents reach models only through the gateway (ADR 0018), which
  redacts what leaves, routes by label and records each call. Anthropic
  was already a provider (MP-4) for text and tool use (ADR 0041), written
  for older models.
- Anthropic's current models (Claude Opus 5.5, the default, $4/$20 per
  million tokens in/out, cache reads $0.20; Claude Sonnet 5.5, $2/$10,
  cache reads $0.20; Claude Haiku 5.5) always think. Their documentation
  (read 2026-10-22, the `claude-api` reference):
  - **Thinking blocks carry the reasoning across tool calls** and must go
    back unchanged. Dropping them works, but "the model answers that turn
    without the reasoning those blocks carried", and the prompt cache
    restarts each time.
  - **Each block is bound to its conversation**: the `system` prompt, the
    `tools` and every earlier message must be byte-identical when it comes
    back. For accounts created on or after 2026-08-31 (every new
    customer's) a changed prefix is a 400.
  - **Text between tool calls arrives in thinking blocks**, empty by
    default (`display: "omitted"`).
  - **`max_tokens` includes thinking.** Our limits (2,048 for the desktop
    agent, 200 and 400 for the IT agent, 4,096 by policy) were set before
    thinking counted.
  - **Prompt caching** reads a prefix at 0.1× the input price (0.05× on
    Opus 5.5) and writes it at 1.25×. A breakpoint on the system prompt
    plus top-level automatic caching for the conversation tail suits tool
    loops.
  - **A refusal is HTTP 200** with `stop_reason: "refusal"` and an
    optional `stop_details.category` (`cyber`, `bio`, …), billed as usual.
- The gateway redacts every outbound call afresh (ADR 0018). Its
  placeholders are numbered by first appearance, system prompt first, so
  two calls sharing a prefix redact it identically, which the binding
  needs.
- The containment line (ADR 0018, PA-3): what an agent sends a model
  leaves only through the gateway, routed by label and redacted.

## Decision
1. **A sealed replay carries the model's own blocks to the next call.**
   When a provider needs its reply back verbatim (Anthropic does), the
   gateway returns it to the agent as `replay`: the provider's id, its
   content blocks as written (placeholders in, nothing restored) and a
   seal, an HMAC-SHA256 under a key random to the glassd process. The
   agent puts `replay` on that assistant message in its next
   `model_complete`, unchanged. The gateway sends a replay only if:
   - its seal checks: every replay in the call, or none of them goes,
     since dropping one would change what later blocks were written after
     (`replays_dropped` on the call's record says so);
   - it goes back to the provider that wrote it; another provider on the
     route gets the message's text and tool calls instead.

   A sealed replay is never redacted again: it is the provider's own
   output from this conversation, and redacting it could change it. An
   agent can't put its own words in one: the seal fails and nothing in
   the call's replays leaves. Validation: replays only on assistant
   messages, at most 64 blocks, each an object with a type, and the same
   tool calls as the message they ride on; they count toward the prompt
   size limit. A glassd restart mid-task drops its replays: the step runs
   without the earlier reasoning.
2. **Our agents send them back.** The desktop agent keeps its history
   append-only (constant instructions and tools, messages only added), so
   the binding holds across its 20 steps.
3. **Prompt caching.** The Anthropic provider sends the system prompt
   as a block marked `cache_control` (the tools come before it, so both
   are cached) and, on Anthropic's own endpoint, the top-level
   `cache_control` that caches the conversation's tail as it grows; an
   endpoint policy names instead may not take that field. Every input
   token counts in `input_tokens`; the record adds `cache_write_tokens`
   and `cache_read_tokens`. The cost counts writes at 1.25× the input
   price and reads at the model's `usd_per_mtok_cache_read` (new in
   policy), else 0.1× the input price.
4. **Refusals.** A reply with `stop_reason: "refusal"` ends the call: the
   agent gets "Claude Opus via Anthropic declined the request (cyber)",
   and the record keeps the model, tokens and cost, since it was billed.
   No failover: the route is for outages. Anthropic's own fallback
   (server side, beta) is deferred.
5. **Limits.** Policy's default `max_output_tokens` goes from 4,096 to
   16,000. The desktop agent asks for 16,000 a step, the IT agent 2,000
   to sort a request and 4,000 for a ticket's assessment. Providers wait
   up to 10 minutes for a reply, the non-streaming ceiling, since a
   reply that thinks first says nothing until it's whole.
6. **Keys and the owner's default.** The Settings app takes Anthropic API
   keys only: a Claude sign-in token (`sk-ant-oat…`) is refused and never
   kept (ADR 0042: we never take Claude.ai credentials). A computer's
   owner who connects Anthropic gets Claude Opus 5.5 by default, priced
   ($4/$20, cache reads $0.20), where it was Claude Sonnet 5.5 unpriced.
7. **A real-key check.** `tools/dev/real-model.sh --provider anthropic
   [model]` runs the IT and desktop agents on a real Anthropic key, read
   from `GLASS_ANTHROPIC_API_KEY` (never `ANTHROPIC_API_KEY`, which a
   Claude Code session in the same environment would take as its own),
   with `--host` where the VM can't reach out. With a fake key it reached
   `api.anthropic.com` and got its 401 (2026-10-22).

## Consequences
- Tool loops on Anthropic keep their reasoning and their cache, at no
  cost to containment: what goes back is what the provider wrote.
- Agents see placeholders in `replay` blocks; their `text` and
  `tool_calls` have PII back as before.
- `scripted` behaves like Anthropic: it returns a thinking block bound
  to its request and refuses a changed conversation, so the runtime
  tests and the VM suite prove the round trip through the desktop agent.
- An org whose policy sets `max_output_tokens` below 16,000 refuses the
  desktop agent's calls; the refusal says the limit.
- Not yet run on a real key: no key is in this environment.

## Not built yet
- Anthropic's server-side fallback on refusals, and fallback credit.
- `thinking.display: "updates"` (beta) for captions between tool calls.
- Streaming replies (needed beyond about 10 minutes per call).
- Skills evals cap a call at 4,096 tokens (ADR 0034), which thinking
  models may need more than.
- Forwarding an org's own Anthropic key through the console, as credits
  go through it for OpenRouter (ADR 0039).
- The same for Bedrock's and Vertex's Anthropic models.
