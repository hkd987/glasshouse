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
   - **A private network:** a VPC whose agent subnets have no route to the
     internet. VPC endpoints reach Bedrock, S3 and the rest; anything else
     goes through the policy proxy, if policy allows it at all.
   - **The admin console** (`glass-console`), behind their load balancer,
     signed into with their SSO (IAM Identity Center or Entra): AD-6's
     production console.
   - **Agent hosts:** glassd on our signed image, the broker and the rules
     at the edge for the agents they run, writing the audit record (and
     streaming it to the org's S3 or SIEM, AU-2).
   - **Agent computers: one EC2 instance each,** from our signed image,
     in a private subnet, with no AWS permissions of its own and a
     security group that lets it reach only its host's broker. Nitro is
     the hypervisor: no nested virtualization, which AWS supports for KVM
     and Hyper-V, not bhyve. Undo of a task is an EBS snapshot of its
     disk. Its screen reaches its host only, and through it the person
     (§4).
   - **Models through their Bedrock** (MP-1, MP-2): already the plan.
   - **Updates** from our update server, signed, mirrored by their console
     (ADR 0061); new computer images the same way.

   So the `VmSandbox` has two backends behind one contract: bhyve on a
   laptop, EC2 in an org's AWS. People reach hosted agents from a
   Glasshouse laptop's Agents view, and from any browser through the
   console: the hosted product doesn't need the Glasshouse laptop, which
   is the richer way in.

## What it needs
- **Hardware virtualization** (Intel VT-x, AMD-V). The cloud sessions
  this is built in have none, and the emulated dev VM can't run bhyve
  inside it. Each slice that boots a guest is tested on a laptop or a
  host with nested virtualization.
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
3. **The Agents view and click in:** live screens in tiles, watch, take
   over, hand back; the console's view and IT's switch per agent.
4. **In the org's AWS (§9):** the stack; agent hosts on EC2; computers as
   EC2 instances, the edge as the VPC and security groups; undo by EBS
   snapshots; the console's browser view to click in; licensing and
   metering (LA, AWS Marketplace). Built and tested in a sandbox AWS
   account of our own, which also gives slice 1 its hardware: metal
   instances run bhyve natively.

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
  It adds a CloudFormation stack and its tests, the AMIs, and EC2's API
  to the platform layer.
- Grok Bot runs on xAI's computers; I found no sign of it deploying
  into a customer's cloud. Running in theirs, with their models, their
  keys and their audit, is the enterprise difference.
