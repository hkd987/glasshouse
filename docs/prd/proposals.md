# Proposed PRD changes

Requests from the product owner that the PRD doesn't cover yet. Each is
being built as described; the PRD doc itself is the owner's to change.

## Terminal and Activity Monitor (requested 2026-10-19)
- **Terminal.** A first-party terminal: the person's shell in a window,
  themed with the rest of the system (UX-10 already assumes one). Native
  (UI-6, ADR 0025), since it needs a pseudo-terminal. Agents can't drive it
  (no actions declared); developers keep the Linux workspace (DW-*) for
  unsigned tools.
- **Activity Monitor.** What's using the machine: CPU, memory and power per
  app and per agent, with Force Quit, and "Ask the agent" about a slowdown
  (AE-4, UX-30). Extends PM-7's per-app power view. Native (UI-6, ADR 0025),
  since it reads the machine's processes.

## Publishing user-built apps beyond the org (asked 2026-10-19, open)
The PRD keeps org-built apps private (DV-10) and lists on the public
exchange only through the developer program (DV-1, DV-20). The owner asked
about users' apps going to an enterprise store and then a global one. Built
as the PRD says for now (personal → team → org catalog, GA-8); a reviewed,
opt-in "publish to the exchange" step is the open question.

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
