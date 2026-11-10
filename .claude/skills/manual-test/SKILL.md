---
name: manual-test
description: Manually exercise Glasshouse (in the FreeBSD VM or on the dev host) and share screenshots of exactly what was run and what came back. Use whenever you test something by hand, demo a feature, or need to show the user a flow; the user expects screenshots in the conversation for every manual test.
---

# Manual testing with screenshots

The user wants to see what was tested and how. Every manual test produces
screenshots of real command output, sent to the user as you go.

## 1. Make sure there's something to test

- VM flows: the test environment must be up and current (`test-env` skill;
  `make freebsd vm-deploy` after code changes).
- Host-only flows: `make dev-host` in the background.

## 2. Capture each step

`tools/screenshot/termshot.py` runs commands and renders their real, colored
output as a terminal window PNG. Nothing is edited after the fact.

```sh
tools/screenshot/termshot.py --out <scratchpad>/shots/03-needs-you.png \
  --title "3 · The tile goes to Needs you (DA-12)" \
  --vm --user demo -- "glassctl tasks" "glassctl approvals"
```

- `--vm --user demo` runs in the VM as that user; `--user root` for admin
  steps (`service glassd suspend`, `glassctl verify`, reading task logs).
  Without `--vm` it runs on the host.
- Several steps in one image tell one beat of the story. Keep one idea per
  image, numbered in order, titled with what it shows and the PRD id.
- Output is forced to color (`CLICOLOR_FORCE`, exported for every command in
  a step) and `COLUMNS=110`.
- Get ids for later steps with `--json` output (e.g. `glassctl --json tasks`),
  outside the screenshot, so the shots stay readable.
- Save to the session scratchpad, not the repo.

Boot screens (the boot menu, splash and login screen) aren't terminal
output: record them from the VM's display with
`tools/screenshot/bootshots.py --out <scratchpad>/boot`, which reboots the VM
and saves each distinct screen plus a contact sheet. The VM boots UEFI like
a laptop; `VM_FIRMWARE=bios` shows the text-console path.

The desktop shell: `make shell-vm` puts it on the VM's screen with the demo
user's live tiles, then `os/freebsd/scripts/vm.sh screendump <png>` captures
what the screen shows. Drive the tiles with `glassctl` as demo, or click and
type on the screen itself (`vm.sh click <x> <y>`, `vm.sh key ctrl-spc`,
`vm.sh type "text"`), and capture again; `os/freebsd/scripts/shell-vm.sh
stop` when done. A redraw takes about 2 s in the VM, so wait for the
`drawn in` line in `/var/log/glass-shell-fb.log` before capturing. For screens without
a live glassd, `make shell-preview` renders them to PNG.

Signing in and driving the screen: the VM boots into the sign-in screen
after `make freebsd vm-deploy`. Use `os/freebsd/scripts/desk.sh`, which
waits for each step to be drawn before returning:

```sh
D=os/freebsd/scripts/desk.sh
$D ready                      # the sign-in screen (or desktop) is up
$D signin                     # password demo; prints "desktop up in ..."
$D click 24 16                # the mark: its menu opens
$D key alt-meta_l-esc         # Force Quit
$D shot <scratchpad>/03.png   # then look at it before sending
$D signout
$D log                        # the greeter's and desktop's log, no draw lines
```

The log is `/var/log/glass-greeter.log` in the VM.

## 3. Look before sending

Read each PNG. Check it shows what the title claims (right task, colors,
nothing cut off). Regenerate if not. Never send a shot that contradicts its
title.

## 4. Send them

Use `SendUserFile` with `display: "render"`, a few related shots per call,
and a caption that says in one or two sentences what each image proves.
Send as you go, not all at the end.

## Good flows to cover

- Handoff card: `glassctl plan invoice-demo "…" --read ~/Invoices --write ~/Reports`
- Run → Needs you → approve/deny: `run --yes`, `wait --until needs-you`,
  `tasks`, `approvals`, `approve <id>`
- Expanded view: `glassctl show <task>`
- Undo: `ls`, `undo --dry-run`, `undo`, `ls`
- Sleep (PM-5), as root: `service glassd suspend`, tasks, `service glassd resume`
- Containment: `run containment-probe`, then `grep -E '^(PASS|FAIL)'` in the
  task's `agent.log`, then `jls` (nothing left behind)
- A power cut with a task running (PA-2): `jls` and `ls
  /var/db/glasshouse/sandboxes`, `vm.sh power-cut`, `vm.sh wait`, the same
  again (empty), and glassd.log's "removed a sandbox left behind"
- A keyboard that comes after sign-in: `vm.sh usb-keyboard plug`, then
  `desk.sh key ctrl-spc` and `vm.sh type "..."` (QEMU sends keys to the
  keyboard plugged in last, so they arrive only if the desktop reads it);
  `vm.sh usb-keyboard unplug`; the `input:` lines in `desk.sh log`
- Signing in while glassd is down (ADR 0031): `service glassd stop`,
  `desk.sh signin`, `service glassd start`, then the greeter's "telling it
  again" and "glassd heard" lines in `/var/log/glass-greeter.log`
- Audit: `glassctl audit <task>`, `glassctl verify`
- Tailoring (TL): `glassctl tailor invoice-demo --set flag_over=1000
  --block mail.send`, then `plan`/`run` to show the layer applied, `--undo`
- Take over an ended task in the harness: `glassctl take-over <failed>`;
  Claude Code isn't in the VM, so drive the socket through `glass-mcp` with
  a scripted MCP exchange; `glassctl hand-back`
- Latency budgets: `/usr/local/libexec/glasshouse-tests/glass-e2e perf`
- IT agent (AE-4): `glassctl run it-agent "My disk is filling up" --yes`
  (with a few MB in `~/.cache`), `"restart cron"` (asks), `"the printer
  won't print"` (asks to open a ticket); `glassctl show`, `undo --change`
- Login: `cat /etc/motd`, `glassctl tasks --summary`
