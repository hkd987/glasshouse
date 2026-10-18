---
name: test-env
description: Spin up, use, reset or troubleshoot the Glasshouse test environment (the FreeBSD 15.1 dev VM under QEMU, plus the host dev loop). Use when a task needs to run Glasshouse for real, deploy to the VM, run the end-to-end suite, or when the VM, deploy or cross-compile misbehaves.
---

# Glasshouse test environment

Full guide for people: `docs/dev/test-environment.md`. This is the short
version for doing it.

## Is it up?

```sh
make vm-status            # "running: pid …, ssh … up" or not running
```

## Bring it up

- Nothing set up yet (fresh container): `make test-env` (tools, FreeBSD
  images, sysroot, VM boot, build, deploy). ~5 minutes without KVM, most of
  it downloading and booting. Run it in the background and keep working.
- Set up but the VM is down: `make vm-up vm-wait vm-deploy`.
- `vm-wait` blocks until SSH answers; first boot without KVM takes ~100 s.

## Use it

- Deploy after a code change: `make freebsd vm-deploy` (~12 s; `vm` profile).
- Run something as root: `make vm-run CMD="..."` or
  `os/freebsd/scripts/vm.sh run '...'`.
- As the demo user: `os/freebsd/scripts/vm.sh run "su demo -c 'glassctl tasks'"`
  (plain `su`, not `su -l`, keeps output clean; HOME is still set).
- Full suite: `make e2e-vm` (~2 min). Run it after any change to the runtime,
  broker, actions, sandbox, undo or audit.
- Logs: `/var/log/glassd.log`; per task `/var/db/glasshouse/tasks/<id>/agent.log`.

## Rules

- Starting a daemon over `vm.sh run` hangs unless its output is redirected:
  `service glassd restart >/dev/null 2>&1`.
- Never kill the QEMU process to "restart" the VM; use `make vm-down`.
- Keep AVX off in the VM under TCG (the default); QEMU 8.2 panics the
  FreeBSD kernel's ZFS SIMD code with it on.
- The VM is disposable: `make vm-reset vm-up vm-wait vm-deploy` gives a
  clean first boot.

## When it breaks

See the troubleshooting table in `docs/dev/test-environment.md`. Most common:
no sysroot (`make freebsd-sysroot`), glassd refusing a bad config
(`make vm-run CMD="glassd --check"`), or a deploy that hung on a daemon.
