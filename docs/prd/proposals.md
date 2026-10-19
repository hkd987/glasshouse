# Proposed PRD changes

Requests from the product owner that the PRD doesn't cover yet. Each is
being built as described; the PRD doc itself is the owner's to change.

## Terminal and Activity Monitor (requested 2026-10-19, now UI-8 and UI-9 in the PRD)
- **Terminal.** A first-party terminal: the person's shell in a window,
  themed with the rest of the system (UX-10 already assumes one). Native
  (UI-6, ADR 0025), since it needs a pseudo-terminal. Agents can't drive it
  (no actions declared); developers keep the Linux workspace (DW-*) for
  unsigned tools.
- **Activity Monitor.** What's using the machine: CPU, memory and power per
  app and per agent, with Force Quit, and "Ask the agent" about a slowdown
  (AE-4, UX-30). Extends PM-7's per-app power view. Native (UI-6, ADR 0025),
  since it reads the machine's processes.

## Publishing user-built apps beyond the org (decided 2026-10-19, now DV-13)
Enterprise apps can never go public. Only single-user apps built on a
device no organization manages can go to the public store.

## GNU readline in the image (found 2026-10-19, open)
The desktop needs libxkbcommon (keymaps) and pixman (drawing) from FreeBSD's
packages. libxkbcommon's package depends on libxml2 for its command-line
tools, and FreeBSD's libxml2 package depends on GNU readline, which is
GPLv3. Nothing of ours uses readline; the library itself needs only libc.
For now images take just the files the desktop needs out of those packages
(`os/freebsd/images/desktop.third-party`), all MIT. The decision: build our
own trimmed packages in our repository (BS-2, ADR 0022 item 3), or ship
readline with a source offer and accept GPLv3's installation-information
terms, which matter once boot is locked down (EP-2, EP-11).
