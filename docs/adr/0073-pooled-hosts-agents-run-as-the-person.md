# 0073. Pooled Glasshouse OS hosts: many people to a machine, agents run as the person

Status: **Accepted, designed, not built** (2026-10-28). The owner: "there
could probably be a way since this is a full OS to share machines for
logged in users…. Since the files on a shared machine would be separated
by permissions and the S3 files for that section of the OS would be
different…. Would allow this to scale more." Asked whether agents may run
as the person instead of as root on a computer of their own: "Yes agents
can run as the person".

## Context
- **What ADR 0068 and 0072 give each person:** one computer, kept between
  tasks and slept to S3. Since ADR 0072 it can be a whole Glasshouse OS
  virtual machine, at about 3 GB of memory and 2 vCPUs per person, on
  hosts with KVM.
- **What ADR 0065 §10 and 0066 planned for AWS:** Linux agent hosts with
  KVM, running Firecracker microVMs. The agent is root inside its
  computer.
- **What a Glasshouse laptop already does** is most of what a shared
  machine needs:
  - several people's accounts;
  - agents in jails as the person, given the folders policy names;
  - undo from ZFS snapshots of the person's files;
  - glassd telling callers apart by the uid the kernel reports.

  A host where several people are signed in at once is that machine with
  more than one session, as Windows multi-session is on Azure Virtual
  Desktop.
- **Checked in the dev VM** (OpenZFS 2.4.2, 2026-10-28):
  - Each home is a ZFS dataset of its own (`zroot/home/demo`).
  - A dataset encrypted with a key of its own (AES-256-GCM) can be sent
    raw: a full stream, then the changes since. Neither stream held any
    of the file's text.
  - It arrives locked, with its key unavailable. Loaded with the key, the
    file read back whole and both snapshots were there.
  - `rctl` rules on a person (`user:demo`) are enforced:
    - with `maxproc` at 6, the next fork was refused;
    - with `vmemoryuse` at 256 MB, a larger allocation failed.
  - The kernel has NVMe built in and the ENA network driver as a module
    (2.8.3). AWS's Nitro instances need ENA 2.3.1 or later. The FreeBSD
    project publishes EC2 images for 15.
- **The stack runs in the org's own AWS account** (ADR 0065 §9), so a
  pool only ever holds one company's people.

## Decision
1. **Agents run as the person, on every kind of machine.**
   - An agent's task is a jail of the host's glassd, running as the
     person, with the folders policy gives it (`agents.folders`). That is
     the laptop's path.
   - This reverses:
     - ADR 0065 §1's "Inside, it is root and nothing asks";
     - ADR 0066's "An agent is root on its own computer";
     - ADR 0067's "root inside".
   - What follows:
     - An approval answered on the desktop is as safe as on a laptop,
       since nothing there is root.
     - The desktop's top bar shows the person's own agents, which closes
       ADR 0072's gap.
     - The ssh channel that ran agents as root goes away.
     - A tool the system lacks comes with IT's image or through an action
       at its tier, never by an agent installing it.
2. **A pooled host is a Glasshouse OS machine.**
   - On AWS it is an EC2 instance booted from our image: the OS runs
     directly on Nitro. No nested virtualization, and no Linux host on
     this path.
   - Several people are signed in to it at once, each in a session: their
     desktop, their apps and their agents.
   - Each pool is an Auto Scaling group.
3. **What's kept is the person's dataset, not a machine.**
   - **Their home** (`zroot/home/<person>`) is encrypted with a key of
     their own. That key is wrapped by the org's KMS key.
     - A host unwraps it through KMS when it loads the home, and keeps it
       in memory only.
     - Their secrets dataset (ADR 0075) shares the key.
   - **Saved to S3** under the person's own prefix, as raw ZFS streams:
     - a full stream, then the changes since;
     - a new full stream now and then, so a load replays a short chain;
     - never anything readable without the key.
   - **Snapshots go with it,** so undo survives sleep and moving hosts.
   - **Snapshots are kept by a rule.** IT sets how long (CF-61): each
     task's, then fewer as they age. The dev VM had piled up 163 before
     `vm.sh clear-snapshots`; a person's mustn't.
   - For pooled people, this replaces ADR 0065 §11's chunk store. The
     chunk store stays for the disks of dedicated machines (item 7).
4. **One session per person.**
   - **Their desktop is drawn off-screen,** and the parts that changed are
     streamed to them in the web app and the desktop app.
     - The desktop and the screenshot renderer share their drawing code
       (`glass_shell`), and the shell already tracks what changed.
   - **Their input goes into their session only,** from that stream.
     - Never through `/dev/uinput`: input devices are the whole
       machine's, so every session would see every key.
   - **ADR 0031's seat becomes the session.**
     - The settings for the whole computer (`Scope::Seat`) become each
       session's where that makes sense: idle, time zone.
     - The rest become the platform's: USB, power.
   - **An idle session** saves the home's changes, unloads it and forgets
     its key.
     - A task or a sign-in loads it again, on whichever host it's placed.
5. **Limits per person.**
   - `rctl` rules on the person cap CPU, memory and processes, and a ZFS
     quota caps their home.
   - One person's busy agents can't starve another.
6. **Before any pooled host exists:**
   - **A uid that's the same everywhere.**
     - Files in a dataset are owned by uid. ADR 0067 gives each person a
       uid per host, so a home moved to another host would belong to
       someone else, or no one.
     - Each person's uid is allocated once, in the stack's shared table
       (DynamoDB, as schedules are), and never reused.
   - **A lease:** a person's home is loaded on one host at a time.
     - It is taken by a conditional write to that table, renewed while
       held, and fenced by a number that grows each time.
     - Two hosts writing one person's home would fork it.
   - **Placement:** the front sends a person's requests to the host
     holding their lease. With none held, it picks a host in their pool
     with room, which takes the lease.
7. **Dedicated machines, IT's choice per group.**
   - Pooled is the default. A group whose labels call for a hypervisor
     between people gets a machine each. Either:
     - the same image on an EC2 instance of its own (a pool of one); or
     - a Glasshouse OS virtual machine on a KVM host for density: ADR
       0072's QEMU path, or Firecracker later.
   - The same session, home and agents-as-the-person code serve both.
8. **What "computer" means in the API.**
   - The front's routes keep their shape (ADR 0068 §5). A person's
     computer is now their session and their home.
   - In glassd, `glass-computers` gets a session backend:
     - start loads the home and starts the session;
     - sleep saves and unloads it;
     - the display and input are the session's stream;
     - tasks go to the jail sandbox as the person, not into a computer
       as root.
   - **Their desktop is theirs whenever they're signed in,** as on a
     laptop. Agents work beside them in jails, with their tiles on it.
   - **An agent that needs a screen** (computer use, NV-20) gets one of
     its own in the person's session. The person can watch it and take it
     over (AW-20), which pauses that agent only. This replaces ADR 0072's
     take-over of the whole computer.
   - **Personas' folders** (ADR 0068's `/work/<persona>`) become folders
     in the person's home. Personas share them by permission.
   - **ADR 0065's open question 3** (copies or mounts) no longer applies:
     an agent reads the person's own folders through its jail's mounts,
     as policy allows.
9. **How many people a machine holds is measured before it's claimed.**
   - A session is the shell, the apps' compositor, the apps' browser
     engine and the person's agents.
   - The browser engine, one per signed-in person, is most of its
     memory. Under emulation it took 28 s and more than one CPU to start.

## What it changes
- **ADR 0065:**
  - §1 and §10: root inside, and Linux hosts with Firecracker for every
    person. Pooled people use EC2 instances of the OS; Firecracker is a
    dedicated option.
  - §11: the chunk store, for pooled people.
  - Open question 3.
- **ADR 0066:** its agent hosts and computers.
- **ADR 0067:** uids per host, and root inside.
- **ADR 0068:** one computer per person stays, now a session and a home.
  `/work/<persona>` moves into the home.
- **ADR 0072:** it becomes the dedicated path. Take-over moves to agents'
  own screens.

## Containment, when it's built
A second person on the same kernel is a new boundary. Each of these needs
a probe check:
- one person can't see another's processes
  (`security.bsd.see_other_uids=0` and `see_other_gids=0`) or debug them
  (`security.bsd.unprivileged_proc_debug=0`);
- each session's `/tmp` and runtime directory are its own (0700);
- nobody's sockets, shared memory or message queues are reachable from
  another session;
- a home is mounted only while its lease is held, and is unreadable
  without its key;
- one session's input never reaches another.

## Slices
1. **Two sessions on one machine** (the dev VM):
   - two people signed in at once, each desktop drawn off-screen and
     streamed to the web app;
   - input per session;
   - the probe checks above.
2. **Homes:**
   - each person's home encrypted with a key of their own, wrapped by KMS
     (the local KMS stand-in);
   - saved to and loaded from S3 as raw streams;
   - snapshots kept by rule;
   - the shared uid table and the lease.
3. **Placement:** the front routes each person to the host holding their
   lease.
4. **On EC2:**
   - the image as an AMI and the pool as an Auto Scaling group in the
     stack;
   - how many sessions a machine holds, measured.
5. **Dedicated machines per group.**

## Open questions (owner)
1. Pooled by default, with dedicated machines for the groups IT chooses
   (proposed)?
2. How long snapshots are kept by default: each task's for 30 days, then
   daily ones for 90 (proposed)?
