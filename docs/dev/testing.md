# Testing

Three levels, each proving something the one below can't.

| Level | Where | Runs on | Proves |
|---|---|---|---|
| Unit | `#[cfg(test)]` in each crate | any host, `make test` | Policy math, hash chains, framing, path confinement, label rules |
| Pipeline integration | `crates/svc/runtime/tests/pipeline.rs` | any host, `make test` | The whole action pipeline (tiers, asks, labels, snapshots, undo, pause, take over, isolation) on development backends, with the test playing the agent over the real MCP socket |
| End to end | `tests/e2e` + `tests/e2e/run-vm.sh` | the FreeBSD VM, `make e2e-vm` | Real jails, real ZFS, real users: containment (PA-1), teardown (PA-2), xattr labels, cross-user isolation, crash recovery |

## The FreeBSD VM

See [os/freebsd/README.md](../../os/freebsd/README.md). First time:

```sh
make freebsd-host-deps freebsd-fetch freebsd-sysroot
make vm-up vm-wait          # ~100 s under software emulation
make e2e-vm                 # cross-compile, deploy, run the suite (~1 min)
```

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
glassctl run containment-probe "Try to escape"   # must end Done, 19/19
```

As root: `glassctl verify`, `jls`, `zfs list -t snapshot`, `tail /var/log/glassd.log`.

On a Linux or macOS laptop, `make dev-host` runs glassd with the insecure host
sandbox for fast iteration on everything but containment.

## Rules

- A change to containment, the broker or the audit log needs an e2e run.
- The probe must stay honest: it ends Failed on the host backend (11/19) and
  Done in a jail (19/19). If you add a sandbox surface, add a probe check.
- No test may depend on wall-clock luck: poll with a deadline, never sleep
  and hope.
