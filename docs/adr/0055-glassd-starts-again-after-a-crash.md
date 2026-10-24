# 0055. glassd starts again after a crash

Status: **Accepted, built** (2026-10-24). The owner agreed to the first of
three next steps proposed after ADR 0054: glassd starting again on its own,
a soak test, and a run on real hardware.

## Context
- rc started glassd under `daemon(8)` without `-r`. If glassd crashed, the
  computer had no agents, no sleep for the person at the screen, and no
  managed sync, until it restarted or someone ran `service glassd start`.
- The Boot model (ADR 0053) already assumed something would start glassd
  again: its restart step was fair, "by hand, or by its supervisor". The
  code didn't do it.

## Decision
- **A supervisor restarts glassd.** rc.d/glassd runs it under `daemon -R
  <delay> -C 100`. A glassd that stops without being asked to starts again
  `glassd_restart_delay` seconds later (3 by default), at most 100 times
  per boot. At start it removes what the crashed run left (ADR 0053), and
  the sign-in screen tells it again who is at the screen.
- **rc tracks the supervisor, not glassd.** Its pid is in
  `/var/run/glassd_supervisor.pid` (`-P`). `service glassd stop` stops the
  supervisor, which stops glassd, and nothing starts it again. glassd's own
  pid stays in `/var/run/glassd.pid`.
- **A glassd that refuses to start** (no good policy, ADR 0021) is
  restarted too, every few seconds, and refuses again saying why. The cap
  keeps that from going on forever.
- **Deploying over an older glassd:** `deploy.sh` stops a glassd the old
  script started (no supervisor pid file) before restarting it, so two
  can't run. Devices take whole images and restart, so they don't need
  this.
- **The model:** `specs/tla/Boot.tla`'s restart step is now the
  supervisor's. `ADR0055_GlassdComesBackAfterACrash` checks that after a
  crash glassd runs again, or refuses saying why. With the code before
  (`Old` = `noSupervisor`) TLC shows it staying down.

## Checked
- `make e2e-vm`'s crash recovery no longer starts glassd by hand. It
  checks that glassd comes back by itself, under a new pid, removing the
  orphaned jail, and that rc sees it running.
- By hand in the VM:
  - `kill -9` brought glassd back within seconds;
  - `service glassd stop` stayed stopped;
  - `suspend` and `resume` still pause and resume agents;
  - after a power cut, glassd started at boot under its supervisor.

## Consequences
- A crash costs a few seconds without agents, and the agent that was
  running is ended, its sandbox removed (ADR 0053). Tasks are in memory, so
  they don't survive it.
- A glassd that crashes over and over stops being restarted after 100
  times. That many crashes in one boot is a bug to find in its log, not
  something to hide.
