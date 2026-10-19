# 0031. The seat, and the settings a computer applies itself

Status: **Accepted** (2026-10-19). Building: the seat, blank screen and
sleep. PRD UX-1, AD-2, PM-5; the owner asked that IT control every device
setting per role (2026-10-19) and chose four groups to add: updates and
sleep, network and VPN, sharing and devices, notifications and region.
Extends ADR 0029.

## Context
- ADR 0029 put settings in policy, per group, and so far every one is
  applied by drawing (the clock, the lock screen) or by a file glassd owns
  the format of (the browser's policy). The new groups change the
  computer itself: sleep it, set its time zone, keep USB storage off.
- glassd runs as root (`rc.d/glassd`, no `-u`); nothing checks it.
- The desktop runs as the person. It can't sleep the computer or turn a
  panel off. The sign-in screen (`glass-greeter`) runs as root, starts
  the desktop with `su -l`, and never tells glassd anything.
- So glassd doesn't know who is at the screen or when they last touched
  it. Four things wait on that one fact:
  - sleep after idle (here);
  - per-role settings for the whole computer (ADR 0029, decision 4);
  - chat's presence for lowered tiers (ADR 0030, C2);
  - remote help's consent answers (ADR 0028, "Before slice (b)").
- The kernel reports a Unix socket peer's pid (`LOCAL_PEERCRED`'s
  `cr_pid`, FreeBSD 13 and later); `PeerCred.pid` carries it and nothing
  reads it yet.
- What the computer can't do yet, per the survey of 2026-10-19:
  - no updater: updates are to be whole signed images in boot
    environments (ADR 0022, item 5; EP-1 not started);
  - no Wi-Fi or VPN configuration; OpenVPN isn't in the image;
  - no Bluetooth (excluded from the image) and no print service;
  - no screen recording, no notification API beyond agents' banners;
  - one language (the desktop's strings are English).

## Decision
1. **A setting enters the table only with the code that applies it and a
   test that the computer does.** ADR 0029 says the table names "every
   setting a computer has"; a knob wired to nothing would tell IT
   something false. So, of the four groups:

   | Setting | Scope | Now |
   |---|---|---|
   | Blank the screen after idle | person | built here |
   | Sleep after idle | person | built here |
   | Time zone | computer | next: glassd sets it, the desktop's clock follows without signing out |
   | USB storage allowed or off | computer | next, with a devfs rule and a probe check |
   | Do Not Disturb for agents' banners | person | next |
   | Wi-Fi networks, VPN | computer | after decision 4 below |
   | Update window and restart deadline | computer | with the updater (EP-1) |
   | Bluetooth, printers | computer | when the image has them |
   | Screen recording, app notifications, language | person | when the computer has them |

2. **The seat: who is at the screen, told by the sign-in screen.**
   - When someone signs in, the sign-in screen calls `seat.begin`
     with their login and the pid of the `su` it started. Only root may.
   - When their desktop ends, it calls `seat.end`.
   - The desktop reports that the person is using it with
     `seat.active`, at most every 15 seconds while there is input.
   - glassd takes `seat.active` only from the desktop itself:
     - a connection from the seat's uid;
     - whose pid's parent is that `su`.
     `su -l -c` runs one child, which execs the desktop; the person's
     other programs are the desktop's children, not `su`'s, so none of
     them can keep the computer awake or put it to sleep.
   - glassd checks the parent once per connection, with the base
     system's `ps`, and remembers the answer.
   - The same registration is what ADR 0028 step 1 and ADR 0030's
     presence need. They get it from here; neither builds its own.
3. **glassd applies what changes the computer, through base commands.**
   - glassd already restarts services this way (`glass_platform::system`,
     `cmd::run`, ADR 0007, ADR 0012).
   - Sleep:
     - pause agents first (PM-5's `Runtime::suspend`);
     - then `acpiconf -s 3`;
     - then wake them.
     - Idle restarts from the wake, so the computer doesn't sleep again
       at once.
   - Blank stays in the desktop: it stops drawing and shows black, and
     the first key or move only wakes it. It is named "blank," not "off":
     turning a panel off (DPMS) waits for the display driver.
   - Writing base-system config (Wi-Fi, time zone) is new for glassd. Each
     slice that does it gets a containment review and, where it adds a
     surface, a probe check.
4. **No secrets in policy, Wi-Fi's included.** Policy goes to every
   computer in the ring, the console's database, previews and diffs; the
   project already keeps credentials out of it (mail, model keys).
   - Wi-Fi starts with the network's name and security type: the person
     types the password once, or signs in with their own account
     (WPA2-Enterprise).
   - A shared password sealed to each computer's key comes with device
     keys (ADR 0030, P).
   - The VPN is WireGuard: it is in the base system (`if_wg`) and
     BSD-licensed. Its keys are made per computer, which is how
     WireGuard works anyway: the computer makes its pair and reports the
     public half. OpenVPN would be a new package in the image.

## Consequences
- The dev host has no sign-in screen, so it has no seat. Sleep never
  happens there, and runtime tests register a seat themselves.
- `make e2e-vm` doesn't suspend the VM: S3 under QEMU without KVM may not
  resume. Runtime tests prove the decision: idle past the setting means
  agents paused, the sleep command run once, then woken. A unit test
  covers the command. Real hardware stays to do in traceability, as
  PM-5's row says today.
- Per-role settings for the whole computer (the browser's) can now be
  the seat's person's. ADR 0029 decision 4 changes when that lands.
- Not this work's, noted so it isn't lost: glassd doesn't check at start
  that it runs as root.
