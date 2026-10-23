# Testing

Three levels, each proving something the one below can't.

| Level | Where | Runs on | Proves |
|---|---|---|---|
| Unit | `#[cfg(test)]` in each crate | any host, `make test` | Policy math, hash chains, framing, path confinement, label rules |
| Pipeline integration | `crates/svc/runtime/tests/` (`pipeline.rs`, `models.rs`, …) | any host, `make test` | The whole action pipeline (tiers, asks, labels, snapshots, undo, pause, sleep, take over, isolation, model calls through the gateway with a scripted provider) on development backends, with the test playing the agent over the real MCP socket |
| End to end | `tests/e2e` + `tests/e2e/run-vm.sh` | the FreeBSD VM, `make e2e-vm` | Real jails, real ZFS, real users: containment (PA-1), teardown (PA-2), xattr labels, cross-user isolation, sleep through the rc.d hooks (PM-5), crash recovery |

Mail against a real server (ADR 0037): the tests that need IMAP and SMTP
(`glass-gateway-providers`' `mail::tests`, `crates/svc/runtime/tests/mail.rs`)
each start their own `mox localserve` on a loopback address of their own.
mox (0.0.17, MIT) is a build-time test tool, never in the image:
`tools/dev/mox.sh build` builds it from Go's module proxy into the main
checkout's `build/tools/mox-0.0.17/` (`MOX` names another binary), and the
tests skip, saying so, without it.

## The FreeBSD VM

Set it up with `make test-env`; see [test-environment.md](test-environment.md).
Then `make e2e-vm` cross-compiles, deploys and runs the suite: about a
minute in a 4-CPU VM on an idle host (it was 2.5 min). What keeps it short:

- **One SSH connection, reused** (`ControlMaster` in `scripts/lib.sh`): each
  `vm.sh run` costs ~0.1 s instead of ~0.9 s against the emulated VM.
  `vm.sh ssh-close` drops it; `up`, `down`, `wait` and reboots do it for you.
- **Scenarios side by side.** `glass-e2e all` runs the ones that share
  nothing in parallel (six at a time, under the per-user task limit), then
  `invoices`, `tailor` and `perf` alone: they send mail others count,
  change the user's layer, or measure latency. A new scenario goes in
  `PARALLEL` in `tests/e2e/src/main.rs` only if it sends no mail, changes
  no shared settings and keeps one task live at a time.
- **4 virtual CPUs** by default (`VM_CPUS`, capped at the host's cores):
  multi-threaded TCG gives each its own host thread.
- Every step and scenario prints how long it took; look there first when the
  suite slows down.

`models` (the model gateway, ADR 0018) runs in a step of its own: it needs
a model in policy, so `run-vm.sh` installs the test policy
`glass-e2e models-policy` prints (a scripted model; nothing leaves the VM),
restarts glassd, runs it, and puts the old policy back.

Run one scenario while working on it: `make freebsd vm-deploy`, then
`tests/e2e/run-vm.sh` is the whole suite, or in the VM
`su demo -c '/usr/local/libexec/glasshouse-tests/glass-e2e <name>'`.

The VM has a `demo` user (uid 1001) whose home is its own ZFS dataset, seeded
with sample invoices by `post-install.sh`.

## Manual testing

In the VM (`make vm-ssh`), as the demo user:

```sh
su demo
glassctl run invoice-demo "Reconcile invoices and remind vendors" --read ~/Invoices --write ~/Reports
glassctl watch              # live tiles; Ctrl-C to leave
glassctl approvals
glassctl approve <id>       # or: deny, --for-task
glassctl show <task>        # Now, Activity, Changes, Can't touch, Timeline
glassctl undo <task> --dry-run
glassctl run containment-probe "Try to escape"   # must end Done, 29/29
```

As root: `glassctl verify`, `jls`, `zfs list -t snapshot`, `tail /var/log/glassd.log`.

On a Linux or macOS laptop, `make dev-host` runs glassd with the insecure host
sandbox for fast iteration on everything but containment.

Share what you tested: `tools/screenshot/termshot.py` saves the real output
of a sequence of commands (host or VM, as any user) as a PNG. The
`manual-test` skill describes the flow.

## Rules

- A change to containment, the broker or the audit log needs an e2e run.
- The probe must stay honest: it ends Failed on the host backend (13/22) and
  Done in a jail (29/29). If you add a sandbox surface, add a probe check.
  The two socket checks ("reach the control socket", "reach the app
  socket") look where an image keeps glassd's sockets
  (`/var/run/glasshouse`), so on the host backend, whose sockets are in
  `build/dev/run`, they pass without proving anything; they count only in
  a jail on an image. Making them real everywhere needs glassd to tell
  the probe where its sockets are (an agent environment variable). "Reach
  the app bridge" connects to the person's bridge port (ADR 0027); on the
  host backend it passes unless a bridge is listening there.
- No test may depend on wall-clock luck: poll with a deadline, never sleep
  and hope.
