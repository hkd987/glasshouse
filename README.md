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
  Can't touch and Timeline are in the control API, and in `glassctl`,
  mapped field by field to the mockups in `docs/design/`.
- **Sleep** (PM-5): agents pause before the machine sleeps and carry on
  after it wakes, through FreeBSD's own `rc.suspend`/`rc.resume`.
- **IT self-service** (AE-4, first slice): an IT agent checks the network,
  storage and performance, cleans up caches (undoable), restarts services
  from IT's approved list after asking, and opens a help desk ticket with
  diagnostics when it can't fix something.
- **Branding** (BR-1, BR-2): the image says Glasshouse from the boot menu to
  the login screen, and ships the open-source notices it owes.

Not started yet: the desktop shell and GUI tiles, the model gateway and real
model-backed agents, the admin console and fleet management, screen-control
agents. See [docs/prd-traceability.md](docs/prd-traceability.md).

## Quick start

```sh
make help                    # every target
make test lint               # unit + pipeline tests, fmt, clippy, layering

# A full test environment, from a fresh checkout (Linux x86_64):
make test-env                # tools, FreeBSD 15.1 image, VM, build, deploy
make e2e-vm                  # the end-to-end suite in the VM (~2 min)
make vm-ssh                  # then: su demo; glassctl agents

# Or run it on this machine with no containment (development only):
make dev-host
```

Setting up, the daily loop and troubleshooting:
[docs/dev/test-environment.md](docs/dev/test-environment.md).

A first session in the VM, as the `demo` user:

```sh
glassctl run invoice-demo "Reconcile invoices and remind vendors" --read ~/Invoices --write ~/Reports
glassctl watch                 # live tiles
glassctl approve <id>          # the vendor reminders wait for you
glassctl show <task>           # activity, changes, can't touch, timeline
glassctl undo <task>           # put it all back (sent mail stays sent)
glassctl run it-agent "My disk is filling up" --yes
glassctl run containment-probe "Try to escape"
```

## Where to look

| | |
|---|---|
| [docs/architecture.md](docs/architecture.md) | How it fits together |
| [docs/repo-layout.md](docs/repo-layout.md) | Where code goes, layering rules, conventions |
| [docs/adr/](docs/adr/) | Decisions and why |
| [docs/dev/test-environment.md](docs/dev/test-environment.md) | Set up a test environment |
| [docs/dev/testing.md](docs/dev/testing.md) | Test levels, the VM, manual testing |
| [docs/design/](docs/design/README.md) | The mockups and the API behind each element |
| [CLAUDE.md](CLAUDE.md) and [.claude/skills/](.claude/skills/) | How Claude works on this repo |
| [docs/prd-traceability.md](docs/prd-traceability.md) | PRD requirement → status → test |
| [os/freebsd/README.md](os/freebsd/README.md) | Image, VM and cross-compile tooling |
