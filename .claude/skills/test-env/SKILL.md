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
  It goes into the boot environment the VM runs, as does everything under
  `/usr/local/etc` (glassd.toml included): after a kernel patch or OS
  update test left the VM on another one (`bectl list`), get back on
  `default` before deploying or editing config.
- Run something as root: `make vm-run CMD="..."` or
  `os/freebsd/scripts/vm.sh run '...'`.
- As the demo user: `os/freebsd/scripts/vm.sh run "su demo -c 'glassctl tasks'"`
  (plain `su`, not `su -l`, keeps output clean; HOME is still set).
- A power cut (nothing shut down or written first), for crash recovery:
  `os/freebsd/scripts/vm.sh power-cut`, then `vm.sh wait`. Not `vm.sh
  reset`, which deletes the VM's disk.
- Full suite: `make e2e-vm` (~2 min). Run it after any change to the runtime,
  broker, actions, sandbox, undo or audit.
- Logs: `/var/log/glassd.log`; per task `/var/db/glasshouse/tasks/<id>/agent.log`.
- A managed VM (what IT sets in the console reaches it): `make console-dev &`,
  then `os/freebsd/scripts/enroll-dev.sh`; `--undo` before `make e2e-vm`.
- Agents on a real model: `tools/dev/real-model.sh [openrouter-model]` runs
  the IT and desktop agents in the VM's jails through OpenRouter, with the
  key from `OPENROUTER_API_KEY` (set in the environment's settings, never in
  chat), and puts the VM back after. `--provider anthropic [model]` uses
  Anthropic directly (default `claude-opus-5-5`) with the key from
  `GLASS_ANTHROPIC_API_KEY`; never `ANTHROPIC_API_KEY`, which a Claude Code
  session in the same environment would take as its own. In a cloud session the VM's TLS to
  openrouter.ai fails (the session proxy re-signs HTTPS and the VM doesn't
  trust it; never change that): use `--host`, which runs them on the dev
  host (nothing contained) where this machine's trust applies.
- Agents in the VM's own jails on Anthropic, from a cloud session:
  `tools/dev/model-relay.py 8787 &` on the host (the key from
  `GLASS_ANTHROPIC_API_KEY`, put in there, never in the VM), forwarded with
  `ssh -R 8787:127.0.0.1:8787`; the VM's glassd.toml `model_relay = true`
  and policy's anthropic provider `endpoint = "http://127.0.0.1:8787"`, its
  secrets file a placeholder key. Put glassd.toml and policy back after.
- The agents on a real model on the VM's own desktop (screenshots of an
  agent using the OS): `tools/dev/real-desktop.sh up [model]` (default
  `claude-haiku-5-5`, about a tenth of a cent a task) runs a dev-host glassd
  on Anthropic with a seeded workday (Inbox, this week's calendar, notes,
  invoices) and puts the VM's desktop on it; then `vm.sh key ctrl-spc`,
  `vm.sh type "..."`, `vm.sh key ret`, `vm.sh screendump`, and click to
  approve or roll back. Its Mail is at http://mail.localhost:7300.
  `real-desktop.sh down` deletes the key file and stops it all.
- Hacking on our apps and sharing them back to the org's store, on a real
  model (TL-5, GA-8): `tools/dev/real-remix.sh up` starts the admin console
  (http://127.0.0.1:7410) and an enrolled dev-host glassd on Claude Haiku
  5.5, which steps up to Sonnet. Its desktop agent may use `apps.*`. It
  also starts the Store (http://store.localhost:7411). Then:
  - `real-remix.sh run "Make my own version of Mail where …, and share it
    with everyone"` approves what the agent asks and prints each model
    call's cost;
  - `real-remix.sh review <catalog-id>` is IT's side in a browser: review,
    approve, the Store installs it, it opens, with screenshots;
  - `real-remix.sh down` stops it and deletes the key file.
  It uses the console suite's ports, so stop it before `make console-test`.
- No credits, or a failure to make happen on purpose:
  `tools/dev/fake-model.sh [--it] [--ladder] <scenario> [request]` runs the
  agent on the dev host against `tools/dev/fake-anthropic.py`. That is a
  local stand-in for the Messages API that checks requests as the API does,
  over the real provider, gateway and TLS. Scenarios include `overload2`,
  `ratelimit2`, `refusal2`, `maxtokens`, `credit` and `billing`.
  `build/dev-fake-model/requests.jsonl` keeps every request and reply.

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
`perf` over budget on "action with snapshot first" or "take over": the VM
has piled up test snapshots; `os/freebsd/scripts/vm.sh clear-snapshots`.
