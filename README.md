# Glasshouse

A FreeBSD-based enterprise desktop where people can safely hand real work to
AI agents. The OS contains, shows, logs and can undo everything an agent does.

*Glasshouse is a working name; the product name is still open.* Proprietary
and confidential; see [LICENSE](LICENSE).

## Status: milestone M0 (prototype foundations)

Working today, verified on FreeBSD 15.1 in a VM:

- **Containment** (PA-1, PA-2): every task runs one agent in its own jail with
  no network, a read-only base system and no user data; a 19-check probe
  attacks it from inside on every test run. Grants and asks die with the task.
- **Brokered actions**: agents act only through app actions (`files.*`,
  `mail.*`) over MCP; each call is tiered by IT policy (Auto, Notify, Ask,
  Never), label-checked, snapshotted and audited.
- **Approvals**: one batched ask per call, approve once or for the task,
  timeout pauses the task, only the task's owner can answer.
- **Undo**: ZFS snapshots before changes; "Undo all" and "Roll back to"
  restore files and say what can't be undone.
- **Audit**: hash-chained, tamper-evident log; rebuild any task end to end.
- **Tiles as data**: the tile, expanded view, Now bar, Activity, Changes,
  Can't touch and Timeline are in the control API, and in `glassctl`.

Not started yet: the desktop shell and GUI tiles, the model gateway and real
model-backed agents, the admin console and fleet management, screen-control
agents. See [docs/prd-traceability.md](docs/prd-traceability.md).

## Quick start

```sh
make help                    # every target
make test lint               # unit + pipeline tests, fmt, clippy, layering

# Run it on this machine (no containment; development only):
make dev-host

# Run it for real in a FreeBSD VM:
make freebsd-host-deps freebsd-fetch freebsd-sysroot   # once
make vm-up vm-wait                                      # ~100 s
make e2e-vm                                             # build, deploy, test
make vm-ssh                                             # then: su demo; glassctl ...
```

A first session in the VM, as the `demo` user:

```sh
glassctl run invoice-demo "Reconcile invoices and remind vendors" --read ~/Invoices --write ~/Reports
glassctl watch                 # live tiles
glassctl approve <id>          # the vendor reminders wait for you
glassctl show <task>           # activity, changes, can't touch, timeline
glassctl undo <task>           # put it all back (sent mail stays sent)
glassctl run containment-probe "Try to escape"
```

## Where to look

| | |
|---|---|
| [docs/architecture.md](docs/architecture.md) | How it fits together |
| [docs/repo-layout.md](docs/repo-layout.md) | Where code goes, layering rules, conventions |
| [docs/adr/](docs/adr/) | Decisions and why |
| [docs/dev/testing.md](docs/dev/testing.md) | Test levels, the VM, manual testing |
| [docs/prd-traceability.md](docs/prd-traceability.md) | PRD requirement → status → test |
| [os/freebsd/README.md](os/freebsd/README.md) | Image, VM and cross-compile tooling |
