# Architecture

This is how the prototype is built today. The PRD's "Containment and platform
architecture" section is the target; [prd-traceability.md](prd-traceability.md)
tracks the gap.

## The one idea

An agent is a first-class principal: its own identity (agent *for* user, on
one task), its own sandbox, its own grants, its own audit trail, its own
undo. Safety comes from the OS, not from the model behaving well.

## Processes

```text
 glassctl / desktop shell ──control socket (JSON-RPC, authorized by uid)──┐
                                                                           │
                                                   ┌───────────────────────▼──────┐
                                                   │ glassd (root)                 │  HTTPS (base OpenSSL)
                                                   │  runtime: tasks, tiles, undo  │──────────────────────►
                                                   │  broker: decide + approvals   │  model providers policy
                                                   │  actions: files, mail, ...    │  names: Amazon Bedrock,
                                                   │  model gateway: route, redact │  Anthropic API
                                                   │  audit log writer             │
                                                   └──┬──────────────┬─────────────┘
                         task socket (MCP), one per task │              │ zfs(8), jail(8)
 ┌───────────────────────────────────────────────────────▼──┐           │
 │ jail glass-<id>  (no network, read-only base, own /work)  │◄──────────┘
 │   agent process, as the user's uid                         │
 └────────────────────────────────────────────────────────────┘
```

- **glassd** is the only privileged process. It hosts the runtime, broker,
  action providers and the model gateway as libraries (ADR 0003) and serves
  two kinds of socket. It is also the only process that reaches a model:
  agents have no network, and ask through the `model_complete` action
  (ADR 0018).
- **The control socket** (`/var/run/glasshouse/control.sock`, mode 0666)
  carries the `glass-api` methods. Every call is authorized by the caller's
  kernel-reported uid: users see and act on their own tasks; root sees all.
  Answering an approval is always the task owner's call.
- **The app socket** (`/var/run/glasshouse/apps.sock`, mode 0666) is how
  apps reach the system (UI-5, ADR 0024). It carries only `app.open`,
  `app.call`, `app.status` and `app.close`, authorized by uid like the
  control socket. An open app is a task whose principal is the app ("by
  mail for demo"): its calls run the same pipeline as an agent's, limited
  to what its manifest (read by glassd from the installed copy) declares.
  `glass-app-bridge`, a user-level process, serves the apps' pages and
  relays their calls; in this slice only over loopback HTTP for
  development.
- **A task socket** is created per task *inside* that task's jail, owned by
  the task's user, mode 0600. It speaks MCP (ADR 0004). It is the agent's only
  way to affect anything outside the jail. An *attended* agent (the user's
  own Claude Code) has no jail: its socket is in
  `/var/run/glasshouse/attended/`, the user's alone, and the user connects
  it with `glass-mcp` (ADR 0015).

## A task's life

1. **Plan** (`task.plan`): resolve the agent from the catalog, compute the
   effective policy for (user, agent), apply the user's restrictions (stricter
   only, DA-10), validate the folders, and return the handoff card. No side
   effects.
2. **Start** (`task.start`): create the task record and its tile, then the
   supervisor (`runtime/src/supervise.rs`) prepares app state, snapshots
   writable folders, creates the jail, binds the task socket in it and starts
   the agent with `jexec -u <user>`.
3. **Act**: every MCP tool call runs the pipeline in `runtime/src/execute.rs`:

   ```text
   hold while paused → preview → decide (tier, then labels) → ask if needed
     → snapshot if reversible → refuse side effects if audit is down
     → invoke → audit + feed + changes
   ```

   Providers compute a side-effect-free *preview* first (what will this read
   and write?), so the broker can decide, check labels, snapshot and log
   before anything happens. A model call takes the same path, with one more
   step after the tier: routing by the labels the task has read (below).
4. **End**: on exit, stop or failure the supervisor always tears down: closes
   the socket, withdraws open asks and task grants, destroys the jail (PA-2).
   The record, snapshots and audit trail stay for review and undo.

## Containment (PA-1)

Each task gets a jail from a generated `jail.conf` + `fstab`
(`platform/src/sandbox/jail.rs`), created and removed with `jail -f`:

| Surface | Control |
|---|---|
| Files | Root is an empty directory. Base system (`/bin /lib /libexec /usr/*`) and agent programs are nullfs read-only. Private `/work` and `/tmp`. No user data mounted (ADR 0005). |
| Network | `ip4 = disable`, `ip6 = disable`. Models are reached through glassd's gateway (`model_complete`), never from the jail. Per-jail egress (pf per vnet jail) comes with harnesses in jails (HX-7) and the local model endpoints (MP-7). |
| Processes | Jailed processes see and signal only their own jail. |
| Privilege | `securelevel = 3`, no mounts, no raw sockets, no child jails, devfs ruleset 4. |
| Identity | Runs as the user's uid, so files it causes to be written are the user's; the jail confines what that uid can reach. |
| Resources | The person comes first: the agent starts at `nice` 10 (`nice -n 10 jexec …`), and the jail's `exec.prestart` adds rctl(8) limits on CPU (half the machine), memory (a quarter of it, committed and resident), disk throughput and processes, which its `exec.poststop` removes with the jail (PA-2). `[sandbox.limits]` in `glassd.toml`; needs `kern.racct.enable=1` at boot. The rctl rules, `nice` and the sysctls it reads live in `glass-platform` (PL-1); on Linux the same limits are cgroup v2's `cpu.max`, `memory.max`, `io.max` and `pids.max`. |

`glass-probe` attacks all of this from inside and must come back 20/20; the
e2e suite runs it on every change to the VM.

The broker side matters as much: glassd runs as root and acts for agents, so
all file actions go through `ConfinedDir` (cap-std), which refuses `..`,
absolute paths and symlinks that leave the grant.

## Decisions, approvals, labels

- Policy at start comes from local disk only, checked once by glassd as it
  starts: `policy.toml`, or the built-in default without one. If the file
  doesn't validate, glassd enforces the last policy that did
  (`<state_dir>/policy/last-good.toml`) and says why; with no good copy it
  refuses to start. Managed settings will be fetched in the background
  after the desktop is up and applied whole, never on the boot path
  (ADR 0021).
- Tiers come from policy (`glass-policy`): org baseline, then group and agent
  rules (stricter wins between them), then task restrictions (stricter only).
  Actions outside an agent's allow-list are Never. Each decision records the
  rule that made it.
- Labels come from folder rules in policy and `glasshouse.label` extended
  attributes on the file and its folders; the most sensitive wins. Above the
  agent's limit, even Auto reads are blocked. Restricted is never opened.
- Asks live in the approval desk (`glass-broker`): one approval per call
  (batches carry a review list), answerable once or for the rest of the task,
  timing out into a paused task, withdrawn when the task ends.

## Model gateway (ADR 0018)

Every model call goes through glassd, so policy, redaction, attribution and
logging happen in one place (DP-1).

```text
agent ── model_complete ──► pipeline: tier → route by label → ask? → send → record
                                              │                      │
                         policy [models] ◄────┘    glass-gateway: redact, try the
                         (allowlist, routes,       route in order, restore PII,
                          groups/agents narrow)    describe the call ──► providers
```

- **The label is what the task has read.** Each completed action raises the
  task's label to the most sensitive label of the paths it read (DP-11). A
  call carries it, floored at policy's label for unlabeled content.
- **Policy routes it** (`glass_policy::ModelRules`): `[models]` lists
  providers and models (the allowlist, DP-2), the default, and per-label
  routes in order, the rest taking over on an outage (MP-8). Group and
  agent `models` lists intersect; restricted data never routes, and
  confidential data only where a route says. No route is a tool result the
  agent can read, audited as Blocked. The handoff card's model line is the
  route for the labels of the task's folders.
- **The gateway sends it** (`glass-gateway`): secrets and configured PII
  become placeholders before anything leaves (DP-3), and PII comes back in
  the reply. Providers implement `ModelProvider`: scripted (on the machine,
  for tests and development), Anthropic Messages (MP-4) and Bedrock
  Converse with SigV4 and temporary credentials only (MP-1, MP-2), in
  `glass-gateway-providers`, the one crate with HTTP and TLS (native-tls on
  the base system's OpenSSL).
- **The record** is the action's audit entry with `detail.model`: model,
  provider, tokens, cost, label, redactions, and the sizes and SHA-256 of
  what crossed, but no prompt text unless policy sets `log_prompts` (DP-5).
- **Credentials** come from a root-only file, `model-secrets.toml`, the
  stand-in for the secrets broker (MP-4), refused unless it is private to
  glassd's user and outside everything mounted into a jail.

## Agent text on screen

Agents write much of what people read: captions, the files and recipients
in feed lines, changes and asks, the line they log before failing. That
text is shown, never obeyed: a control character or direction override in
it could clear a terminal, hide a line or forge an approval (PA-3). It is
display-sanitized at the API boundary and again in clients, with one
function, `glass_api::display_line`: line breaks and tabs become a space,
other controls and invisible characters become U+FFFD, so tampering shows.
glassd applies it where views are built (`Task::view` and `Task::detail`,
`TaskState::note`, the approval desk's `open`, the undo report), through each
view type's `DisplaySafe` impl, which names every field so a new one doesn't
compile until someone decides. `glassctl` passes everything it prints
through it again (`--json` escapes instead, losslessly), and the shell's
tiles too. The audit log keeps what the agent sent; `glassctl audit` shows
it sanitized.

## Undo

File-level restore from snapshots (ADR 0006), of only the files the task
changed (ADR 0017). The runtime snapshots each writable root when the task
starts and before every Notify-tier change, and snapshots app data (such as
mail drafts) before its first change. Every change records its files
(`ChangeEntry.paths`) and which snapshots existed when it ran, and the
runtime notes how the task left each file. "Undo all", "Roll back to" a
marker and the Changes view's per-row Undo restore the files of the
changes they cover from the latest snapshot before them, and leave alone,
and report, any file someone changed since. Edits the user makes while
driving become changes of the task at hand back, undoable like the agent's.
Irreversible changes are listed, never silently ignored, and every rollback
is audited.

An ended task can be retried (`task.retry`): the same request and scope as
a new task, checked against today's policy. The first run's changes stay
undoable on the first task.

## Sleep (PM-5)

When the machine is about to sleep, FreeBSD's `/etc/rc.suspend` runs every
rc.d script with the `suspend` keyword and waits for it before telling the
kernel to continue. `rc.d/glassd` has that keyword: `glassd suspend` calls
`system.suspend`, which holds every running agent at its next safe point
(tile: "Paused for sleep"), waits up to 2 s for actions already under way,
and returns. `rc.resume` runs `glassd resume` on wake, and those agents carry
on; anything the user had paused stays paused. Approval asks stay open across
sleep.

## Audit

`glass-audit` writes one JSON record per line, each carrying the SHA-256 of
its body, each body carrying the previous record's hash. `glassctl verify`
proves nothing was edited, dropped or reordered. If an append fails, the
pipeline refuses further side effects (fail closed).

## Constraints from the PRD that shape new code

- **US-only, x86-only for V1.** One target: `x86_64-unknown-freebsd`.
- **Signed software only (EP-2) and a read-only signed base (EP-1).** Agent
  programs already come only from read-only directories; new components
  must not assume they can write to the base system at runtime.
- **Agents are directory identities (PA-7) with declared privileges (DV-3).**
  See ADR 0008 for how that layers onto today's policy.
- **Telemetry in OCSF (EP-7, AU-2).** Audit events will need an OCSF
  mapping; keep event fields typed and stable.
- **A lapsed license pauses agents only (LA-3).** Licensing gates the
  runtime, never the desktop, data or security updates.
- **Users never see FreeBSD (BR-1).** User-facing text names no base-system
  component; the image is branded by the overlay and checked in the e2e
  suite (ADR 0011). Notices and the SBOM are generated from the build.
- **The shell is a control-API client (UI-1 … UI-6, ADR 0009).** Rust on
  Smithay and iced; apps are TypeScript web apps whose manifest actions
  join the action registry. Keep everything the shell shows in `glass-api`.
- **A general OS, not one workflow ("Everyday work").** Mail, calendar,
  files, documents through adapters, forms, research, IT self-service and
  multi-system workflows all go through the same action registry and
  pipeline. New capability is a new action provider with a default tier,
  never a special path.
- **Privileged fixes are fixed-function actions (AE-4, ADR 0012).** The
  IT agent's `system.*` actions run in glassd; policy allows them to that
  agent only, and IT's `[fixes]` list bounds what they may change.
- **Agents are harness bundles; hard limits stay in the OS (AE-1,
  ADR 0010).** Nothing an agent ships or an improvement loop edits can
  change tiers, Never rules, labels or scope, so keep every limit enforced
  in `glassd`, never delegated to the agent.
- **A calm, Mac-like desktop (DK-1, UX).** Menu bar, dock, window
  controls on the left, tiles at the top right, Ctrl Space command bar. All
  styling comes from one theme (UX-10), and anything a person waits on has
  a latency budget checked on every build (UX-22).
- **Users shape their tools; the OS keeps it safe (TL, GA, ADR 0014).**
  Tailoring is data the OS checks (settings for declared points, limits
  that only narrow), never code an agent ships. Apps built on request are
  ordinary apps under the same sandbox, tiers and audit; there is no
  separate path for generated code.
- **Agents may one day run elsewhere (FD-1 to FD-4, ADR 0020).** The task
  socket is the only agent interface and no decision depends on where a
  call comes from; task state, change records included, stays serializable
  data; identity is never a host name, a per-machine uid or a path.
- **Build on pkgbase, stay portable above it (BS-1 to BS-6, PL-1, PL-2,
  ADR 0022).** An image is a list of packages from one repository; our
  code and changes ship as packages too; devices take whole images, never
  `pkg` runs. FreeBSD facilities are reached only through `glass-platform`,
  and everything above it builds and tests on Linux.
- **Our cloud services run on AWS (GT-21)** and hold only licensing, policy
  and telemetry metadata; customer data stays on devices and in the
  customer's own Bedrock account (GT-3).

## Code map

| Crate | Layer | What it owns |
|---|---|---|
| `glass-types` | lib | Ids, tiers, labels, action names, principals |
| `glass-policy` | lib | Policy schema, evaluation, default policy, model routing |
| `glass-audit` | lib | Hash-chained log, verification |
| `glass-rpc` | lib | JSON-RPC over Unix sockets, MCP types |
| `glass-api` | lib | Control API contract (shell, CLI), display text |
| `glass-platform` | lib | Jails, ZFS, confined file access, users, xattrs, system facts, the console framebuffer and evdev input |
| `glass-agent-sdk` | lib | Convenience MCP client for Rust agents |
| `glass-actions` | lib | The action contract: providers, previews, task context, registry |
| `glass-app-files`, `-mail`, `-calendar`, `-system`, `-helpdesk` | apps | Each app's actions (ADR 0013) |
| `glass-first-party` | svc | The apps the OS ships, as one registry |
| `glass-broker` | svc | Decisions and the approval desk |
| `glass-gateway` | svc | The model gateway: the `model.complete` action, redaction, routing over providers with failover, the call record, the secrets stand-in (ADR 0018) |
| `glass-gateway-providers` | svc | Providers that leave the machine: Anthropic Messages and Amazon Bedrock over HTTPS; the only crate with an HTTP and TLS stack |
| `glass-tailoring` | svc | Personal layers on agents: customization points, versions, re-checks (ADR 0014) |
| `glass-runtime` | svc | Tasks, pipeline, supervision, undo, both APIs |
| `glassd`, `glassctl`, `glass-mcp`, `glass-app-bridge`, `glass-demo-agent`, `glass-it-agent`, `glass-probe` | bin | Executables |
| `glass-e2e` | tests | End-to-end scenarios |
| `@glasshouse/sdk`, Mail, Calendar | apps/ (TypeScript) | The first-party web apps and the SDK they share: bridge client, manifests, attribution, offline reads (ADR 0024, `apps/README.md`) |
| `glass-shell`, `glass-shell-preview`, `glass-shell-fb` | shell/ (own workspace) | The desktop shell's screens (iced, software rendering), their headless renderer, and the desktop on the console framebuffer with live tiles; a control-API client (ADR 0009, 0016) |
