# 0012. IT self-service runs as fixed-function system actions in glassd

Status: **Accepted** (PRD AE-4)

## Context
AE-4: an IT self-service agent diagnoses and fixes common problems (network
and VPN, certificates, printers, app installs, storage, slowdowns) from an
IT-approved list. Its system-level actions go through a dedicated broker
role, every fix is logged and undoable, and it can never change security
settings. Fixing the machine needs root; agents run unprivileged in jails
with no access to the system (PA-1).

## Decision
- **System operations are actions, run by glassd.** The `system` app
  (`glass-actions::providers::system`) declares fixed-function actions:
  `system.network.check`, `system.storage.check`,
  `system.performance.check` (read only), `system.storage.clean_cache`
  (Notify, snapshotted first), `system.service.restart` (Ask). The `helpdesk`
  app adds `helpdesk.ticket.create` (Ask, irreversible) for what can't be
  fixed. An agent picks an action and its typed input; it never names a
  path, a command or a privilege.
- **The broker role is policy.** Only agents whose `allowed_actions` include
  `system.*` see or call these actions; the default policy grants them to
  `it-agent` alone. Every call takes the same path as any action: decide,
  label checks, snapshot, audit.
- **IT's fix list is policy too.** `[fixes] services` names the rc.d services
  an agent may restart. Anything outside the list is refused in preview,
  before anyone is asked; so is a listed service that isn't set up here.
- **The machine sits behind an interface.** `SystemOps` (with `HostSystem`
  over `glass-platform::system`, which reads `ifconfig`, `route`, `df`,
  `sysctl` and `ps` per ADR 0007) lets the pipeline tests run against a fake
  machine.
- **Undo where there is something to undo.** A cleanup records its folder as
  the change's path, so per-change undo restores it from the snapshot, owned
  by the user. A restart changes nothing lasting; it's in the activity log
  and the audit record, not in Changes.
- **Never security settings.** There are no security actions in `system`,
  and `security.*` is Never in the org baseline.

## Consequences
- New fixes (certificate renewal over SCEP, printer queues, catalog app
  installs) are new actions here, each with a default tier and, if it
  changes anything, a snapshot root or a recorded way back.
- Process information is limited to the requesting user's own processes.
- The help desk outbox stands in for the ServiceNow (and similar) adapter;
  tickets carry the checks' structured results as diagnostics.
- `glass-it-agent` is scripted (keywords pick checks). The model-backed IT
  agent replaces it as a harness bundle (ADR 0010) using the same actions.
