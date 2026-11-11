# 0018. The model gateway: glassd brokers every model call

Status: **Accepted** for the first slice: `model_complete` on the task
socket, routing by data label, redaction, the call record, and the scripted,
Anthropic and Bedrock providers. The local HTTP endpoints for third-party
agents (MP-7) and the secrets broker (MP-4) are **Proposed** below. PRD
DP-1, DP-2, DP-3, DP-5, MP-1, MP-2, MP-4, MP-7, MP-8, HX-7.

## Context
Agents run in jails with no network (PA-1), so none could call a model, and
the handoff card's "Data and model" line was whatever the catalog said. The
PRD wants one gateway for all model traffic that enforces labels, redaction
and the approved-model list before anything leaves (DP-1 to DP-5), with
Amazon Bedrock as the default provider and no long-lived AWS keys on a
device (MP-1, MP-2), API keys held only by a secrets broker (MP-4),
provider-neutral endpoints for third-party agents (MP-7), and failover
(MP-8).

The constraints: build time matters (docs/dev/build-performance.md); the
stack must cross-compile to `x86_64-unknown-freebsd` through
`os/freebsd/toolchain` with no cmake; licenses must pass `deny.toml`;
ADR 0007 prefers the base system; there is no secrets broker yet; and there
are no Bedrock credentials to test against.

## Decision

### Agents reach models through an action
- **`model.complete` is an action** (MCP tool `model_complete`), declared
  by `glass-gateway` and registered by `glass-first-party` like any app's,
  so `tools/list`, handoff cards, `glassctl policy check` and the policy
  lint all know it. The input is Anthropic Messages shaped, text only:
  `messages`, `max_tokens`, optional `system` and `model` (an id from
  policy's list; most agents leave it out). The result is the reply text
  and structured data: which model answered, the label, tokens and what was
  redacted.
- **It takes the action pipeline** (`runtime/src/execute.rs`): hold while
  paused, check the input, decide the tier (Never unless the agent's
  `allowed_actions` include it), **route by label**, ask if the tier says
  so, refuse if the audit log is down, send through the gateway, record.
  A new side effect, `Disclose`, says what it is: data leaves the machine
  and nothing changes, so there is no snapshot and nothing to undo, and an
  audit failure refuses it like any side effect. Its default tier is Auto:
  labels, routing and redaction govern model calls, and asking on every
  call would make model-backed agents unusable. IT can still put it at Ask
  or Never for an agent or a group.
- **The label is what the task has read.** Each completed action raises the
  task's label to the most sensitive label among the paths it read (DP-11);
  a blocked read reads nothing and doesn't count. A call carries that label,
  never less than policy's label for unlabeled content (`unlabeled`,
  default internal). Nothing in the request can lower it.
- **Refusals are tool results** the agent can read ("Not sent: no model is
  approved for confidential data …"), on the tile's activity, and audited
  as Blocked with the routing rule.
- **Third-party agents later (MP-7).** Jails have no network, so the
  OpenAI- and Anthropic-compatible endpoints will be a second per-task
  socket in the jail, `/run/glasshouse/models.sock`, speaking HTTP/1.1
  (`/v1/messages`, `/v1/chat/completions`) and feeding the same pipeline
  call. For clients that only take a TCP base URL, the jail becomes a vnet
  jail with nothing but `lo0` and a forwarder from `127.0.0.1:<port>` to
  the socket, so `ANTHROPIC_BASE_URL` and `OPENAI_BASE_URL` work unchanged;
  the key such a client sends is a dummy, since the socket's ownership is
  the authentication. Not built in this slice: changing the jail's network
  needs its own containment review and probe checks.

### HTTP and TLS: native-tls on the base system's OpenSSL
The options, with what we measured:

| Option | Against it |
|---|---|
| ureq + rustls + ring | ring compiles C and assembly in its build script (rule 5); its own bundled trust roots; no FIPS module. Alone, a clean FreeBSD `vm` build of the stack took 21 s against 14 s for the OpenSSL one, and its binary was 2.4 MB bigger. |
| ureq + native-tls | ureq compiles its native-tls connector only with its `native-tls` feature, which pulls `webpki-root-certs` (CDLA-Permissive-2.0, outside `deny.toml`). With `native-tls-no-default` the connector is silently missing and the first HTTPS call panics; we caught that from the binary, and a test now proves TLS is wired. |
| hyper | Async and more to compile, and still needs one of the TLS crates. |
| aws-lc-rs | cmake, or a large C build. |

**Chosen:** `native-tls` on the base system's OpenSSL (FreeBSD 15.1 ships
OpenSSL 3.5, `libssl.so.35`), with a small HTTP/1.1 client of our own in
`glass-gateway-providers`: `httparse` reads the reply head, the body is
framed by Content-Length, chunked encoding or the connection's end and
capped at 8 MiB, with connect and call timeouts, one request per
connection (S3's client keeps its own since the amendment of 2026-11-11,
below), no redirects, HTTPS only, and CONNECT through an `https_proxy`
set in glassd's environment by its rc.d script.

Why:
- **Base system first (ADR 0007).** One TLS stack, patched with the base
  system; the system trust store, so an org's private CAs and
  TLS-inspecting proxies work as they do for every base program; a
  FIPS-capable OpenSSL for regulated customers.
- **No C compiled.** openssl-sys's build script only preprocesses one
  header with the target's compiler to learn OpenSSL's version, then links
  the base libraries.
- **Cheapest to build**, and confined to one crate that only glassd links.

The costs:
- FreeBSD builds and checks need the sysroot (`make freebsd-sysroot`),
  including the FreeBSD clippy in `make lint`, because openssl-sys reads
  the headers. `.cargo/config.toml` points it at the clang wrapper, which
  finds headers and libraries through `--sysroot`; worktrees share the main
  checkout's sysroot. Host builds need `libssl-dev` and `pkg-config`.
- OpenSSL is C in glassd's address space, as it already is in sshd. Our HTTP
  code is small and talks only to endpoints policy names, over verified TLS.
- Measured (4-core container, clean debug build of the workspace, baseline
  and change built alternately under the same load, since another session's
  VM was running): 34.9 s → 40.2 s, +5.3 s (+15%). New units: openssl
  4.9 s, glass-gateway-providers 5.0 s, glass-gateway 2.6 s, cc 1.9 s (for
  openssl-sys's build script), each of the rest under 1 s. They are off the
  critical path (glass-runtime, then glassd) but compete for cores. A clean
  FreeBSD `vm` build of glassd took a median 61 s before and 75 s after
  under the same noisy load (40 s before on an idle machine). glassd grows
  from 6.7 to 7.7 MB (`vm` profile).

### Secrets: a root-only file until the secrets broker exists
- `model_secrets` in `glassd.toml`, by default
  `/usr/local/etc/glasshouse/model-secrets.toml`: one table per provider id
  (`api_key` for Anthropic; temporary AWS credentials for Bedrock). glassd
  reads it at start, and only if it is a regular file (not a symlink) owned
  by glassd's user with no group or other access; otherwise it's refused
  with a warning and a note in the start record, and providers that need it
  are left out, so calls fail over or are refused.
- It is never visible to agents or users: the path may not be under any
  directory mounted into a jail (config validation), and `/usr/local/etc`
  isn't. Values are `Secret`s, which have no `Display` or `Serialize` and
  debug-print as `Secret(…)`; a TOML error names a line, never its text.
  Nothing writes a credential to a log, the audit log or a tool result.
- **Bedrock (MP-1):** temporary credentials only. A key id starting `AKIA`
  (a long-lived IAM user key) is refused, a session token is required, and
  `expires` is honored. Until the secrets broker obtains and refreshes role
  or IAM Identity Center credentials itself, an admin tool writes them.

### Bedrock is implemented, untested against AWS
The Bedrock provider uses the provider-neutral Converse API with SigV4
signing (HMAC built on the workspace's `sha2`), pinned to policy's region or
a PrivateLink endpoint (MP-2). Signing is tested against AWS's documented
example and an independently computed Bedrock request; the request and
reply shapes against a local fake server. It has not run against a real
account. Next: the secrets broker fetching credentials, Bedrock Guardrails
after our checks (MP-3).

### Policy: an allowlist that only narrows
`[models]` in `policy.toml` holds the providers, the models (the allowlist,
with display names and prices), the default, routes per label (an ordered
list: the first is used, the rest take over on an outage, MP-8), the label
for unlabeled content, `max_output_tokens`, `log_prompts` and redaction.
Restricted data never routes; confidential data only where a route names a
model. Groups and agents may list `models`, and every list intersects
(MP-6). A tailoring layer can't widen it: layers hold settings and limits
that only narrow, and a setting naming a model is just a request policy
checks on every call. The handoff card's model line is the route for the
labels of the task's folders, capped by what the agent may open.

### Redaction: secrets never leave, PII comes back
Always redacted: PEM private key blocks, AWS access key ids, API keys by
prefix (`sk-` for Anthropic and OpenAI, GitHub, Slack, Google, Stripe),
JWTs, bearer tokens, and values assigned to credential-named keys. PII when
policy turns it on: emails, US SSNs, card numbers that pass Luhn, and IT's
own patterns. Placeholders read `[redacted:<kind>#<n>]`, the same value
getting the same one. Replies get PII back, only for placeholders this call
issued; secret placeholders stay. The matchers are hand-written (no
`regex`) and err toward redacting.

### The call record: sizes and hashes, not text
Each call is an `Action` audit record for `model.complete` with its tier,
rule and outcome, and `detail.model`: the label, the routing rule, model,
provider, provider model id, region, input and output tokens, estimated
cost (from policy's prices), redactions by kind, the SHA-256 and size of
the request as sent (redacted) and of the reply as received, and any models
that failed first. Refusals carry the label and the requested model.

Prompt and reply text are left out unless policy sets `log_prompts`, and
even then only redacted text is kept. The audit log lives for years, is
streamed to the org's SIEM (AU-2) and read by users and IT (AU-4), and
prompts carry the most sensitive data a task handled. Hashes let an auditor
prove what was sent when they hold the text, and match the provider's own
invocation logs; sizes, tokens and cost serve spend and anomaly review.

### Not in this slice
DP-1's pf rules (agents have no network at all, so the gateway is already
the only path; per-jail egress comes with HX-7 and MP-7), DP-4 injection
screening, DP-6 budgets, the OpenAI provider, streaming, tool use through
the gateway, and the local model (DP-20).

## Consequences
- glassd now makes outbound HTTPS connections, to endpoints policy names.
  That is new reach for the OS, not for agents: jails still have no
  network, and a model call is an audited action.
- A provider is a crate-local `ModelProvider`; the scripted one answers on
  the machine for tests, development and the e2e `models` scenario.
- Model-backed agents need no credentials or network: they call
  `model_complete` and treat refusals as information (add-agent skill).
- If the gateway outgrows glassd (ADR 0003), it moves behind the same
  action with no change for agents.

## Amended by ADR 0052
- A file's stored label stays when it's rewritten: the new file gets it
  before the rename (DP-10).
- What a task writes with `files.write` is labeled with what it had read,
  the higher of that and the file's own (DP-11), and a read's labels are
  read again after it, so a write landing meanwhile counts
  (`specs/tla/ModelGateway.tla`).

## Amended 2026-11-11 (audit 9.5): replies read as they arrive, and S3's connections kept
The client is still the one HTTP client in the workspace, and a model
call is still one request on its own connection. Two things changed in
`glass-gateway-providers/src/http.rs`:

- **A reply's body is read once, as it arrives.** `send` held the whole
  reply and decoded its chunks again from the start at every read: a
  32 MiB chunked reply took 18 s, and a built kernel patch behind a proxy
  that re-chunks, minutes. It reads through the same reader a download
  does (0.15 s for the same reply), to the same caps.
- **A client may keep its connections** (`Http::keeping_connections`),
  and S3's does. S3, DynamoDB and the console came after this decision
  and paid a TCP connection and a TLS handshake for every request; a
  computer going to sleep is hundreds of requests to one host. A kept
  connection is used again only when
  - its last reply was read to its end and nothing more came with it;
  - the server speaks HTTP/1.1 and didn't say it is closing;
  - it has sat unused under 15 s;
  - the far end hasn't closed it or said anything since (asked of the
    socket just before the request goes out).

  A request that a kept connection takes but doesn't answer is sent once
  more on a new connection only if it is a GET, HEAD, PUT or DELETE,
  which S3 may get twice (a put is the same bytes; a conditional one's
  caller is told the key is taken and looks whose it is). Any request is
  sent again if not all of it went out. A POST that went out is not: its
  caller hears it failed.

  That last rule is why DynamoDB and the console stay at a request per
  connection. Their requests are POSTs that can't be sent twice (a
  schedule's claim is a conditional write: sent twice, the second says
  another host has it, and nobody runs it), and they come one in half a
  minute, which a kept connection wouldn't outlast.
- Connections have `TCP_NODELAY`: a request is written as its head and
  then its body, and on a connection in use for a while the body waited
  in the kernel for the far end to acknowledge the head, 40 ms a small
  put.

Not verified here: the saving against S3 itself. Over TLS on this machine
a small put, HEAD or delete takes 3 ms on a new connection and 0.3 ms on a
kept one; moto, which `make aws-local-test` uses, closes every
connection, so that suite still runs a request per connection, and the
kept path is tested against the Rust stand-in (`s3::testing::StandIn`)
and a local TLS server by hand.

One thing to measure on the first real deployment: a server that sends a
reply's head and its body as two writes and waits for the first to be
acknowledged holds a GET on a kept connection for the 40 ms this end
takes to acknowledge (the Python server used by hand does; a new
connection doesn't show it). Nothing says S3 does, and every AWS SDK
keeps its connections to it, but it hasn't been seen from here. If it
did, a wake's GETs would be slower kept than not, and
`keeping_connections` on the S3 client is the one line to take out.
