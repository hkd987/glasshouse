# 0065. Agent computers

Status: **Proposed** (2026-10-27): the direction is the owner's, the open
questions below are theirs to settle. The owner: "thinking this is the way
forward with sandboxed VMs for agents moving forward to give them this
level of OS", and "its own service ... you can handle your agents and then
you can click into the VM ... this is the big difference from the other
agent frameworks." Then: "This should be like a enterprise grokbot so it
deploys on their own AWS" (2026-10-27): the hosted form is decided, §9.

## Context
- Today an agent runs in a jail (PA-1). Each thing it does on the person's
  behalf is an action, at a tier policy sets: Auto, Notify, Ask, Never.
  IT can already set a group's tiers to Auto, class by class or all of
  them (`"*" = "auto"`). A few actions stay Never whatever IT sets: the
  person's passwords, pairing, OS updates, routines (`PERSONS_OWN`).
- That makes the person's computer safe, and keeps agents small. An agent
  can't install a tool, run code it wrote, keep a browser open or use a
  desktop app, because there's nowhere that is its own.
- Raising every tier to Auto on the person's computer isn't the answer:
  sends, deletions and posts would go out with nobody asked, and undo
  can't recall what left the machine.
- The PRD already plans the place: bhyve VMs as the "high-risk sandbox"
  for untrusted and agent-written code, and agents with a screen of
  their own (NV-20), which a person can take over (AW-20) and see as a
  live thumbnail (AW-2). It also plans agents that move between laptop
  and cloud under the same policy (FD-1, FD-2). The developer workspace
  (DW-1) is a VM too. None of it is built.
- Others, as of October 2026:
  - **Grok Bot** (xAI, beta since August): named bots, each working on a
    cloud computer, one coordinating the rest; admins turn it on for
    enterprises. Reports say an account's bots share one machine, one set
    of files and one set of logins.
  - **Windows' agent workspace:** agents in a separate Windows session,
    each with its own account, given the folders it's granted, logged. A
    policy layer for it was in early preview at Build 2026.
  - **Sandbox platforms** (Daytona, Docker Sandboxes, Modal and others):
    microVMs for agents, built for developers. No person's data, and no
    rules at the edge beyond the network.

  Giving an agent a computer isn't new. What none of them has is what
  Glasshouse already is: the person's data reached only through a broker
  with tiers, labels deciding where data may go, an audit record that
  can't be edited, and undo.

## Decision (proposed)
1. **An agent computer per agent.** An agent IT allows one gets a bhyve VM
   of its own, kept between tasks, like a teammate with a desk: its
   tools, its browser and its scratch files stay. Inside, it is root and
   nothing asks. Nothing in it is the person's, so there is nothing to ask
   about. Two agents never share a computer, their files or their logins.
2. **The rules sit at its edge,** the same rules as today:
   - **Files:** the files a task is given go in as copies. What the agent
     changes comes back as a set of changes, at `files.write`'s tier,
     snapshotted and undoable as now.
   - **Mail, calendar, chat, the org's apps:** calls through the task
     socket, as from a jail, at their tiers. The person's passwords and
     tokens never enter the computer: the broker holds them.
   - **The network:** none by default. Model calls go through the gateway,
     where labels decide the model. Other traffic goes through a host
     proxy that allows what policy names, and is on the record.
   - **The record:** every call across the edge is audited as now. What
     happens inside is the agent's own business, but the computer's
     screen can be recorded (CF-63's protections apply).
3. **Undo.** Each task starts from a snapshot of the computer's disk (a
   ZFS volume). Undo of the task rolls the computer back with the
   person's files. "Start this computer over" goes back to the image.
4. **Click in.** Each agent's tile shows its computer's screen, live
   (AW-2). Clicking it opens the screen to watch; Take over pauses the
   agent and hands the person the keyboard and mouse, Hand back returns
   them (AW-20; ADR 0015 hands an ended task to the person's harness the
   same way). The same view is in the admin
   console, for IT, under its roles (ADR 0028).
5. **The Agents view.** One place to handle them: each agent, its
   computer's state (off, idle, working, waiting on you), what it costs in
   memory and model credits, its tasks, its screen. Start, stop, start
   over, and remove. IT sees and controls the same for the fleet.
6. **Local or hosted.** The same computer runs on the laptop (private,
   offline, free) or on an agent host: the org's server, or a Glasshouse
   service. Calls cross the edge to the same brokers either way (FD-1). A
   hosted computer keeps working while the laptop sleeps, and is moved
   with its snapshot (FD-2). The hosted service is the product the owner
   describes: agents that keep working, each with a computer you can
   click into.
7. **Jails stay.** A jail starts in milliseconds and costs nothing; a VM
   takes seconds and 0.5 to 1 GB. Quick tasks run in jails; an agent that
   needs a computer has one. IT chooses per agent (`runs_in =
   "computer"` in the catalog and policy), and caps how many run at once
   on a laptop.
8. **Behind the platform layer.** A `VmSandbox` next to `JailSandbox`
   (the `Sandbox` trait was made for a second backend). The task socket
   reaches the guest through a virtio-console port, not the network. The
   screen comes out through bhyve's framebuffer (VNC), to glassd only.
9. **Hosted means in the org's own AWS account** (the owner, 2026-10-27):
   an enterprise Grok Bot that runs where the org's data already is. We
   ship it; the org deploys it; nothing runs on our servers and no data
   leaves their account. One stack (CloudFormation, later on AWS
   Marketplace) puts in their account:
   - **A private network:** a VPC whose agent hosts have no route to the
     internet. VPC endpoints reach Bedrock, S3 and KMS; anything else goes
     through the policy proxy, if policy allows it at all.
   - **The admin console** (`glass-console`), behind their load balancer,
     signed into with their SSO (IAM Identity Center or Entra): AD-6's
     production console.
   - **Models through their Bedrock** (MP-1, MP-2): already the plan.
   - **Updates** from our update server, signed, mirrored by their console
     (ADR 0061), host and computer images the same way.
   - **Agent hosts and agent computers,** §10 and §11.
10. **Agent computers on AWS are Firecracker microVMs** (the owner,
    2026-10-27), many to an agent host, not an EC2 instance each:
    - **Why:** a microVM starts in well under a second and costs a few MB
      beside its guest's memory, so a host carries dozens; and a running
      computer can be snapshotted whole and put away (§11).
    - **Agent hosts** are EC2 instances with KVM: the 8th-generation Intel
      types with nested virtualization (C8i, M8i, R8i, from February 2026,
      KVM supported as the hypervisor inside, Firecracker shown running on
      an `m8i.large`), or metal ones for density. They run Linux: KVM is
      Linux's. The host service (the broker, the rules at the edge, the
      audit record) is glassd's runtime on Linux, a second target for the
      host role only (PL-1); the desktop stays as it is.
    - **Each computer** runs under Firecracker's jailer: our signed guest
      kernel, a read-only base disk (the signed image, cached on each
      host) and a data disk of its own. **No network device at all:** the
      task socket, the screen and the policy proxy are vsock ports to its
      host.
    - **The screen:** Firecracker has no display device, so the guest draws
      on a virtual display of its own (a headless compositor) and streams
      it over vsock to the host, which relays it to the person (§4): from
      the console in a browser, or a Glasshouse laptop.
    - **Files** go in as copies (Firecracker has no shared folders, and
      copies were the choice anyway), changes come back over vsock.
    - Cloud Hypervisor, also KVM, is the fallback if a computer needs a
      device Firecracker lacks (shared folders, a GPU).
11. **No hot files for a computer that isn't working** (the owner,
    2026-10-27): S3 is where a computer lives; a host's disk is a cache.
    - **The data disk in S3** as content-addressed chunks (a few MB each,
      compressed, named by their SHA-256, encrypted with the org's KMS
      key). A version of the disk is a list of chunks; saving one uploads
      only the chunks S3 doesn't have yet, so versions and agents share
      what's the same.
    - **Spinning down:** idle past the time IT sets (15 minutes, say), the
      computer is paused and snapshotted whole (memory and device state:
      Firecracker's full snapshot), memory compressed with its empty pages
      left out, the disk's new chunks uploaded, and every local file
      removed. An idle agent costs S3, nothing else.
    - **Spinning up,** for a task or a person clicking in: the snapshot and
      the chunks it needs fetched, the computer resumed where it was, its
      programs still open. A snapshot the host's Firecracker can't restore
      (snapshots are bound to the version that made them; an update moved
      it) means a cold start from the disk: open programs are lost, files
      never are.
    - **Undo** of a task restores the disk version it started from; "start
      this computer over" is an empty data disk. Both are lists of chunks,
      so they cost nothing to keep.
    - **Retention** is IT's (CF-61), by S3 lifecycle; removing an agent
      removes its chunks.
    - **Moving** between the laptop and AWS (FD-2) goes by the data disk,
      a cold start on the other side: a memory snapshot doesn't cross
      hypervisors.
    - Incremental memory snapshots (Firecracker's diff snapshots) would
      make spinning down cheaper; they're a developer preview, so full
      snapshots first.

12. **How people use it: one service, four ways in** (the owner,
    2026-10-27: "a really clean way for non technical and technical
    employees"). Every way in is the person, signed in with the org's SSO,
    under the same policy, tiers, approvals and audit record. An approval
    answered in one is answered everywhere. IT sees each and can turn any
    off.
    - **The web app,** for everyone, at an address in the org's domain
      (`agents.<org>`), served by their console. Home is their agents,
      like a team chat: each with its name, what it's doing in a line,
      Working, Needs you or Asleep, and its screen, live. Opening one
      shows a conversation: ask in plain words; its answers, the files it
      made and its questions come back as cards, Approve and Deny on
      them. Beside it, its computer: watch, Take over, Hand back. A new
      agent comes from the ones IT approved, never a blank form. Nothing
      technical on the way: no terminal, no settings IT didn't leave to
      the person.
    - **The desktop app,** the same web app in a small native window, for
      Mac and Windows: a menu-bar icon that says who needs you,
      notifications, a file dropped on an agent goes to it, the sign-in
      kept in the system's keychain. One interface to build; the app
      adds only what a browser can't. Tauri (Rust, permissively licensed).
      On a Glasshouse laptop the same agents are in the desktop itself:
      tiles, the command bar, take over.
    - **The CLI,** `glass`, for technical people and scripts, grown from
      `glassctl`: `glass login` (SSO, a code in the browser), `glass
      agents`, `glass ask <agent> "..."` (the answer streamed), `glass
      watch`, `glass approve`, `glass cp` to put files in and get them
      out, `glass shell <agent>` (a terminal in its computer, on the
      record), `glass open <agent>` (its screen in the browser), and
      `--json` on all of them. Tokens per person, short-lived.
    - **For tools:** the same API (HTTP and a stream of events), an MCP
      server so a person's Claude Code, an IDE or a CI job hands work to
      an agent and gets the result (as `glass-mcp` does for a local task
      today), and webhooks out.
    - **Microsoft Teams,** §13, with the web app first. Slack after.

13. **Agents ask in Teams** (the owner, 2026-10-27: "we should support
    your agent reaching out and asking permission in teams"; CF-90). The
    org's Teams gets a Glasshouse Agents app, installed for everyone by
    its Teams admin.
    - **An agent reaches out:** when it needs the person, the app messages
      them in Teams, one to one, as a card: which agent, what it wants to
      do, the detail, and Approve, Don't and Open. Answered, the card
      changes for everyone who can see it to say who answered, when, and
      what happened (Adaptive Cards' universal actions). Answered
      elsewhere first, the card says so and its buttons go.
    - **Only the person answers.** Teams says who clicked (their Entra
      object id). The service matches it to the person the approval is
      for, through the org's directory (AD-1). Anyone else clicking a
      forwarded card is refused, unless policy names them a delegate:
      CF-90's "delegate an approval to a manager". Each card answers one
      approval once, and an old one has expired.
    - **Labels decide what goes into Teams** (ADR 0018): a card carries the
      request's words only when its data may go there. Otherwise it says
      "Reconciler needs you" and Open, and the detail stays in the web
      app.
    - **Asking an agent** in a chat with the app: "@Reconciler which
      invoices don't match?". The answer comes back there, with the same
      cards. A colleague not on Glasshouse can be asked to review or
      approve too (CF-90).
    - **On the record:** each answer is audited as from any way in, "in
      Teams" beside it.
    - **In the org's AWS:** Teams reaches the app through Microsoft's Bot
      Framework, which calls an HTTPS address. That address is the stack's
      only door from the internet: a small relay on its own, in a public
      subnet, that accepts only requests Microsoft signed for the org's
      app, and passes on only messages and answers. The agent hosts stay
      private. The app's registration is in the org's Entra tenant, the
      relay's secret in their Secrets Manager.
    - **Not in Teams:** taking over a computer, or watching it. Open goes to
      the web app for those.

## What it needs
- **Hardware virtualization** (Intel VT-x, AMD-V). The cloud sessions
  this is built in have none, and the emulated dev VM can't run bhyve
  inside it. Each slice that boots a guest is tested on a laptop, or on
  AWS: Firecracker on a nested-virtualization instance, bhyve on a metal
  one.
- **A guest image:** Linux, since agents' tools expect it (apt, Docker,
  Chrome), built and signed by us like the OS (ADR 0061, ADR 0064).
- **Memory:** about 0.5 to 1 GB per running computer; on a 16 GB laptop,
  two or three at once.

## Slices
1. **Spike, on hardware.** glassd boots a guest through `VmSandbox`, the
   task socket over virtio-console, the screen over VNC. Measured: start
   time, memory, an agent's first step. A probe check that the guest
   sees no host file, device or network it wasn't given (PA-3: an escape
   is a P0).
2. **The edge:** files in as copies and changes out, at their tiers; the
   egress proxy; per-task snapshots and undo; audit. Mostly testable here
   against a stand-in for the VM.
3. **The ways in (§12, §13):** the web app (agents, conversations,
   approvals, live screens, click in, take over, hand back), Teams (the
   app, approval cards, asking in a chat, the relay), then the desktop
   app around the web app, the `glass` CLI and the API with its MCP
   server; the console's view and IT's switch per agent.
4. **In the org's AWS (§9 to §11):** the stack; Linux agent hosts with
   Firecracker; the chunk store in S3, spinning down and up, undo by disk
   versions; the console's browser view to click in; licensing and
   metering (LA, AWS Marketplace). Built and tested in a sandbox AWS
   account of our own, which also gives slice 1 its hardware.

## Open questions (owner)
1. One computer per agent (kept, like a desk: proposed), or a fresh one
   per task (cleaner, slower)?
2. IT allows it per agent (proposed), or per group of people?
3. Files in as copies (proposed: safer), or read-only mounts (faster, but
   the agent sees later changes)?
4. ~~Hosted where?~~ Decided: the org's own AWS (§9). Still open: Azure
   and Google Cloud later, and whether we ever run a hosted service of our
   own for orgs without a cloud.
5. May an agent computer hold logins of its own (an agent's own email or
   accounts, PA-7), or only what the broker lends it per call?

## Consequences
- Agents get a whole OS to work in without the person's computer giving
  anything up: the promise stays "contained, visible, attributable and
  undoable", with a bigger container.
- A second architecture of containment to keep sound: bhyve, the guest
  image, the console port, the proxy. Each is a probe check.
- The laptop list (ADR 0057) needs VT-x on, which the pilot models have.
- AW-2, NV-20 and AW-20 come with slice 3; FD-1 and FD-2 with slice 4.
- Deploying into the org's AWS turns several pilot blockers into one
  stack: the production console (AD-6), Bedrock (MP-1, MP-2) and SSO.
  It adds a CloudFormation stack and its tests, the host and guest
  images, and Linux with KVM and Firecracker to the platform layer, for
  the agent host only.
- An idle agent costs its S3 storage; a host's cost is spread over the
  computers running on it, and hosts scale with the work.
- Grok Bot runs on xAI's computers; I found no sign of it deploying
  into a customer's cloud. Running in theirs, with their models, their
  keys and their audit, is the enterprise difference.
