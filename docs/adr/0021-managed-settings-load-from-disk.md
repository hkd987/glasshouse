# 0021. Managed settings: boot from what's on disk, fetch in the background

Status: **Accepted** for how glassd starts (local disk only, one process,
the last good policy as a fallback) and for the store's atomic apply and
rollback. The background fetch from a management service, live apply to a
running glassd and staged rollout are **Proposed**. PRD AD-1 … AD-5,
AD-15, AD-3, EP-12, UX-22.

## Context
Policy comes from a local `policy.toml` (or the built-in default) and the
machine's config from `glassd.toml`. There is no management service yet,
but there will be: IT sets agent policy from our console, Intune and Entra
(AD-15), per group (AD-1), versioned and rolled out in stages (AD-3), with
signed update metadata (EP-12). People expect the machine to boot fast
whatever IT does, and offline: a laptop opened on a plane, a hotel network
behind a captive portal, a management service having a bad day.

What boot did before this decision: `rc.d/glassd` ran `glassd --check`
first, a whole second glassd that read and validated the config and
policy, then started the real one, which did it all again; a policy file
that didn't validate stopped glassd, and with it every agent, until
someone fixed the file by hand. Nothing waited on the network for policy,
but the image's network setup did (a synchronous DHCP lease before the rest
of startup).

## Decision
- **Boot reads local disk only.** glassd starts from the policy file in
  `glassd.toml` (`policy`), or the built-in default when there is none.
  Nothing on the way to the login screen waits on the network or a server:
  not policy, not DHCP or the default route it brings (`ifconfig_DEFAULT`
  is asynchronous, `defaultroute_delay=0`), not devices
  (devd handles the boot-time ones in the background, `devd_flags=-n`).
- **One process checks once.** glassd validates its config and policy as it
  starts (`glass_policy::store`), so boot pays for one glassd, not two.
  `rc.d/glassd` passes `--console-errors`: if glassd refuses to start, the
  reason goes to the system console as well as its log, naming Glasshouse
  only (BR-1). Started by hand (`service glassd start`), the rc script
  still runs `glassd --check` first, so a refusal shows on the terminal that
  asked.
- **The last good policy is the fallback.** Once glassd is up, the policy
  in force is recorded as `<state_dir>/policy/last-good.toml` (off the start
  path; written to a temporary file, synced, renamed), and the one it
  replaces as `previous.toml`. If the policy file doesn't validate at the
  next start, glassd enforces the last good policy instead of refusing to
  run, and says so in its log, on the console and in the audit log
  (`PolicyLoaded` with `source: last-good`, the rejected file and why).
  Only when there is no good copy either does it refuse (fail closed).
  `glassctl status` names the source ("last good policy (... was
  rejected)").
- **New settings are validated first and swapped in whole.**
  `PolicyStore::apply` checks the text, then replaces the file with one
  rename, keeping what it replaced for `PolicyStore::rollback`. A crash
  leaves the old file or the new one, never a mix.
- **Proposed: the background fetch.** After the desktop is up, glassd (or
  an unprivileged helper it supervises) polls the management service when
  the machine is online, with backoff, never at boot. A fetched policy is
  checked against its signature (keys from the image, EP-11, EP-12),
  validated against this machine's actions (`Policy::lint` over the
  registry), and applied with `PolicyStore::apply` to a managed policy file
  under `<state_dir>/policy/` (enrolled machines point `policy` there; the
  image's own `/usr/local/etc` will be read-only, EP-1). The running glassd
  then swaps policy and registry in one pointer store: tasks started after
  use the new policy; running tasks keep the effective policy they started
  with, the scope the person agreed to on the handoff card (risk signals
  that must narrow running tasks, PA-8, are their own mechanism). The
  validation and file writes run off glassd's async workers and outside the
  locks `task.list` and `task.get` need, and tile updates are coalesced
  (50 ms per task), so applying settings never stalls the shell.
- **Proposed: rollout and rollback.** AD-3's stages are the service's job
  (which machines get a version when); the machine reports the version in
  force (a content hash in the `PolicyLoaded` record). Rolling back is the
  service sending the previous version, or `PolicyStore::rollback` when the
  service can't be reached.
- **Proposed: the rest of the machine's settings** (the catalog, AD-5, and
  `[sandbox.limits]`) move into the same store with the same rules. Until
  then a `glassd.toml` that doesn't validate stops glassd (fail closed),
  with the reason on the console.

## Consequences
- Managed settings add the cost of reading one small file to boot, and a
  management outage never delays it. An offline machine keeps enforcing the
  last policy it applied; whether that should ever expire (and pause agents,
  as a lapsed license does, LA-3) is open.
- A bad push or a bad hand edit can't stop agents everywhere at once: the
  machine keeps the last good policy and says why. The flip side is that a
  policy change that fails validation silently doesn't take effect, so the
  service must read back what each machine reports as in force.
- `last-good.toml` is root's, in glassd's 0700 state directory, and is only
  ever a copy of a policy that validated and was in force; a damaged copy
  is not trusted (it must validate again before use).
- Boot timings, before and after, are in docs/dev/performance.md ("Boot"),
  measured with `tools/boot/timeline.py`.
