# 0072. A person's computer can run Glasshouse OS, its desktop taken over

Status: **Accepted, slice 1 built** (2026-10-28). The owner: "When I click
in from the agent desktop app to the agent OS to take over, I don't want to
just click into the CLI I want to take over the full glasshouse OS with the
visuals".

## Context
- Each person has one computer that their agents share (ADR 0065, 0066,
  0068). It is a Linux microVM on AWS (Firecracker), or Linux namespaces
  on a developer's machine.
- Its screen is a terminal. Taking it over gives the person its keyboard
  as typed lines, run as root there, and its agents wait meanwhile.
- The owner wants the person to land on the whole Glasshouse OS, with its
  desktop, apps and windows, and drive it with the mouse and keyboard.

## Decision
1. **The computer can be a Glasshouse OS install** (`hypervisor = "qemu"`
   in glassd's `[sandbox.computers]`):
   - a virtual machine booted from the OS image;
   - its own copy-on-write disk (a qcow2 overlay on the image), kept
     between tasks as any computer is.
2. **The agents stay in the host's glassd.**
   - Their model, policy, approvals, undo and audit are the host's, as
     for any computer.
   - Their commands run in the computer over its exec channel (ssh on
     loopback, a port forwarded to it) in `/work/<task>`, as root there.
   - The task's socket is forwarded in to
     `/run/glasshouse/tasks/<task>.sock`, and removed as the task ends.
3. **The display is the virtual machine's own** (a VGA screen, 1280×800).
   - glassd reads it with QMP's `screendump`, as a PNG.
   - The front passes it on as base64 with a version (the first 8 bytes of
     its SHA-256). A request `since` that version gets no picture.
   - The web app, and so the desktop app (ADR 0071), shows it on the
     computer's page: Desktop and Terminal tabs. It looks every 350 ms, or
     every 3 s in a hidden tab.
4. **Input goes through QMP's `input-send-event`**, to a USB tablet and
   keyboard.
   - The pointer is absolute, mapped from where the person points on the
     picture. Buttons and the wheel go in as well.
   - Keys go by their place (the browser's `code`, mapped to QEMU's key
     names), so the computer's own layout applies. A key its keyboard
     hasn't, such as a media key or F13 and up, is left out, not the
     rest with it.
   - Each button change is a report of its own, with where the pointer
     is, as `vm.sh click` sends them.
   - 1 to 64 events go in a call, in order.
5. **Who sees and drives it.**
   - Only its person sees its display. To anyone else it isn't there
     (404), as for everything of a person's.
   - Its pointer and keys are theirs only while they have taken it over.
     Watching, the picture is all there is.
   - While they have it, asks to their agents are refused, as for typed
     lines (ADR 0065).
   - Taking over and handing back are on the record, around everything
     the person does on the desktop. The pictures and each key or click
     are not.
     - This differs from typed lines (ADR 0065). Each typed line runs as
       root, so it is recorded with its text and refused if the record
       can't take it.
     - On the desktop, the person works as their own account, as on a
       laptop.
6. **Opening the display turns the computer on**, as opening a laptop
   does. Until it's up, the page says "Your computer is starting".
7. **The person's desktop is signed in as the sign-in screen does it.**
   - The screen and input devices are handed to the account, with a fresh
     runtime directory for the apps' windows, and `su -l` for the session.
   - The account is `desktop_user`: `demo` on the development image.
8. **No network out.** It uses QEMU's restricted user network
   (`restrict=on`): nothing leaves but the exec channel's forward, on the
   host's loopback.

## Cost
- A full OS per person: 3 GB of memory and 2 vCPUs as configured here (2
  to 4 GB is workable). A namespaces computer takes tens of MB.
- Its disk grows with use, on top of the image.
- It needs hosts with KVM. Without it, as in this environment, QEMU
  emulates the processor, so each step takes seconds and the desktop's
  browser engine starts in about 30 s.

## On AWS (designed, not built)
- **QEMU with KVM on instances that allow it** (metal, or nested
  virtualization). The display path is the same as here. This is the
  recommended path.
- **Firecracker** has no display device. The guest would need a streamer
  of its own (the framebuffer read in the guest and sent over vsock), and
  an input device fed the same way. That's more to build and to keep
  contained.

## Containment
- The agents are root on the same machine the person drives. That is the
  person's computer (ADR 0068), and nothing else is reachable from it.
- Input is refused unless its person has the keyboard. Asks are refused
  while they do.
- The display is its person's alone. glass-agents finds the computer by
  the signed-in person, as for its other routes.
- The exec channel takes a fixed line:
  - `env -i` with only the task's variables, quoted;
  - only `/work/<one name>` as its folder;
  - only the image's agents (`agents_dir`) as its program.
- No network out, as above.

## Tested
- **Unit tests:**
  - `glass-computers`: only its person sees the display and drives it, and
    only once taken over; a terminal computer has none.
  - QEMU's: the task's line, the input's reports (press and release
    apart, the pointer kept on the screen, wheel notches), keys and keys
    left out.
  - The web app's: where a point lands, and the buttons.
- **`tests/local-aws/negative.sh`:** a terminal computer has no desktop
  to show or drive (409); another person can neither see nor drive one
  (404); input of the wrong shape (400).
- **By hand, with screenshots.** The desktop app under Xvfb, the local AWS
  stack, an agent host on Claude Haiku, and Dana's computer booted from
  the dev VM's image under emulation:
  - watching the desktop;
  - take over;
  - the dock's launcher opened by a click;
  - Terminal opened;
  - a command typed into it;
  - hand back;
  - an ask whose file the agent wrote on the computer, then shown with
    `cat` in that same Terminal.

## Found while building it
- The desktop started without a session's runtime directory could draw
  but not open apps' windows. It now signs in as the sign-in screen does.
- sshd leaves a forwarded socket's file behind. The task's line removes
  it as the agent ends, so a task still leaves no socket (AgentComputers'
  invariant).
- Under emulation, the desktop's browser engine takes about 30 s to start
  after the first sign-in. A click on a web app before then does nothing.
- The development image has no Files app, though the dock shows one.

## What it doesn't do yet
- **The desktop's agents.** Its top bar shows the computer's own glassd,
  so it says "No agents running" while the person's agents work.
  - The plan: a read-only view of the person's agents from the host,
    forwarded in, for the desktop's tiles.
  - Answering approvals or undoing from inside stays refused there, since
    agents are root inside.
- **Who is at the screen.** The computer's glassd isn't told (`seat.begin`,
  ADR 0031), as the sign-in screen would tell it. What follows the person
  (USB, the browser's profile) doesn't yet.
- **The exec channel and the image are development ones:**
  - ssh with the dev VM's key;
  - the dev VM's disk as the image, so the dev VM must be off while
    computers use it.

  Production needs a key made per computer (or a vsock agent), and a
  released, read-only image.
- **Sleep to the store** for these computers isn't tried. `suspend`
  powers the computer down.
- **The picture is polled PNGs.** That is enough for one person watching.
  A stream (VNC's changed regions, or video) comes before many people
  use it.
- **Not yet:** sound, the clipboard, dropping files, a screen sized to the
  window, and a live thumbnail on the agent's card.
- **What the person does on the desktop** isn't on the host's record,
  beyond taking over and handing back.
  - That holds only while the desktop's account can't become root. The
    development image's `demo` can't: it isn't in wheel, and there's no
    sudo or doas.
  - Production keeps it so, or carries the computer's own audit log to
    the host's.
- **Not in CI**, and no containment probe run inside one yet: both need
  the image and KVM.
