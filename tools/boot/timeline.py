#!/usr/bin/env python3
"""timeline: power-cycle the dev VM and print where its boot time goes,
phase by phase, from power on to the login prompt and glassd ready.

    tools/boot/timeline.py [--runs 3] [--trace] [--top 15] [--json out.json]

Each run shuts the VM down cleanly, starts QEMU and timestamps every line
the VM prints on its serial console, so the firmware, boot loader, boot menu,
kernel loading and the rest show up in the host's clock. After the login
prompt it reads, over SSH, when glassd said it was ready (its log, mapped
onto the host's clock through the guest's own).

  --trace   also turn on boottrace(4) for the runs: the kernel's milestones
            (init starting, the startup scripts finishing) and each rc
            script's time. Tracing runs every rc script through
            boottrace(8), an extra program start per script, so traced
            runs are slower; compare scripts with each other, and compare
            totals only with other traced runs.

Honours VM_DIR, VM_SSH_PORT and the other VM knobs, like vm.sh. The VM must
be running (vm.sh up && vm.sh wait). Without KVM the VM's CPU is emulated
and many times slower than hardware: compare runs with each other, not with
a laptop. See docs/dev/performance.md ("Boot").
"""

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
VM = REPO / "os/freebsd/scripts/vm.sh"
VM_DIR = Path(os.environ.get("VM_DIR", REPO / "build/vm"))
CONSOLE = VM_DIR / "console.log"
LOADER_CONF = "/boot/loader.conf"
TRACE_TUNABLE = "kern.boottrace.enabled=1"
ANSI = re.compile(r"\x1b\[[0-9;?=]*[A-Za-z]|\x1b[c78]")

# What the serial console shows, in order. Each marks the start of the next phase.
SERIAL_MARKS = {
    "loader": re.compile(r"Consoles: "),           # the boot loader starts
    "menu": re.compile(r"Autoboot in "),           # the menu is drawn and counting down
    "kernel_load": re.compile(r"Loading kernel"),  # the menu is done
    "kernel": re.compile(r"Start @ "),             # the loader jumps to the kernel
    "login": re.compile(r"login: *$", re.M),       # getty on the serial console
}
# Ours, always listed in a traced run.
OURS = re.compile(r"/usr/local/etc/rc\.d/(glass\w*)")
TRACE_LINE = re.compile(
    r"^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\S+)\s+(.*?)\s+(\d+)\s+([\d.]+)\s+(\d+)\s+(\d+)\s*$")


def vm(*args: str, check: bool = True) -> str:
    result = subprocess.run([str(VM), *args], capture_output=True, text=True)
    if check and result.returncode != 0:
        sys.exit(f"timeline: vm.sh {args[0]} failed: {result.stderr.strip()}")
    return result.stdout


def set_tracing(on: bool) -> None:
    if on:
        vm("run", f"grep -qx '{TRACE_TUNABLE}' {LOADER_CONF} || echo '{TRACE_TUNABLE}' >> {LOADER_CONF}")
    else:
        vm("run", f"sed -i '' -e '/^{TRACE_TUNABLE}$/d' {LOADER_CONF}")


def power_cycle(timeout: float) -> tuple[float, dict[str, float]]:
    """Shuts the VM down and starts it. Returns the host's wall-clock time at
    power on and when each serial mark first appeared, up to the login
    prompt. Marks are looked for as bytes arrive, not per line: the boot
    menu draws with cursor moves and no line breaks."""
    vm("down")
    t0 = time.time()
    vm("up")
    marks: dict[str, float] = {}
    pos, window = 0, b""
    while time.time() - t0 < timeout:
        data = CONSOLE.read_bytes() if CONSOLE.exists() else b""
        if len(data) > pos:
            now = time.time()
            window = (window + data[pos:])[-8192:]
            pos = len(data)
            text = ANSI.sub("", window.decode("utf-8", "replace"))
            for name, pattern in SERIAL_MARKS.items():
                if name not in marks and pattern.search(text):
                    marks[name] = now
            if "login" in marks:
                return t0, marks
        time.sleep(0.02)
    sys.exit(f"timeline: no login prompt within {timeout:.0f}s; see {CONSOLE}")


def wait_ssh(timeout: float = 300) -> None:
    vm("ssh-close", check=False)
    deadline = time.time() + timeout
    while subprocess.run([str(VM), "run", "true"], capture_output=True).returncode != 0:
        if time.time() > deadline:
            sys.exit("timeline: SSH didn't come up after the login prompt")
        time.sleep(1)


def guest_offset() -> float:
    """Guest wall clock minus host wall clock, to about a tenth of a second:
    waits in the guest for its clock's second to turn over."""
    best = None
    for _ in range(3):
        before = time.time()
        out = vm("run", 's=$(date +%s); while [ "$(date +%s)" = "$s" ]; do :; done; date +%s')
        after = time.time()
        guess = int(out.split()[-1]) - after
        if best is None or after - before < best[1]:
            best = (guess, after - before)
    return best[0]


def parse_iso(stamp: str) -> float:
    return datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()


def guest_facts(trace: bool) -> dict:
    out = vm("run", "grep 'glassd ready' /var/log/glassd.log | tail -1"
             + ("; echo @@; sysctl -n kern.boottrace.log" if trace else ""))
    sections = out.split("@@\n")
    m = re.search(r"(\d{4}-\d\d-\d\dT[\d:.]+Z)", ANSI.sub("", sections[0]))
    facts = {"glassd_ready": parse_iso(m.group(1)) if m else None}
    if trace and len(sections) > 1:
        facts["trace"] = parse_trace(sections[1])
    return facts


def parse_trace(text: str) -> dict:
    """boottrace(4)'s log: kernel milestones and each rc script's start and
    done, in milliseconds since the CPU started."""
    kernel, opened, scripts = {}, {}, []
    for line in text.splitlines():
        m = TRACE_LINE.match(line)
        if not m:
            continue
        msecs, process, event, pid = int(m.group(2)), m.group(4), m.group(5), m.group(6)
        if process == "boottrace":
            # Long paths cut the "start"/"done" suffix off: pair by pid.
            name = re.sub(r" (start|done)$", "", event)
            if pid in opened:
                start_name, start = opened.pop(pid)
                scripts.append((start_name, start, msecs - start))
            else:
                opened[pid] = (name, msecs)
        elif process in ("kernel", "init"):
            kernel.setdefault(event.rstrip("."), msecs)
    return {"kernel": kernel, "scripts": scripts}


def one_run(n: int, trace: bool, timeout: float) -> dict:
    print(f"run {n}: power cycling ...", file=sys.stderr, flush=True)
    t0, marks = power_cycle(timeout)
    wait_ssh()
    offset = guest_offset()
    facts = guest_facts(trace)
    rel = {k: (marks[k] - t0 if k in marks else None) for k in SERIAL_MARKS}
    run = {"serial": rel}
    if facts["glassd_ready"] and facts["glassd_ready"] - offset > t0:
        run["glassd_ready"] = facts["glassd_ready"] - offset - t0
    trace_facts = facts.get("trace")
    if trace_facts and rel["kernel"] is not None:
        kernel = trace_facts["kernel"]
        # boottrace counts from the CPU starting; line its first kernel event
        # up with the loader's jump to the kernel on the serial console.
        base = min(kernel.values()) if kernel else 0
        at = lambda ms: rel["kernel"] + (ms - base) / 1000  # noqa: E731
        run["kernel_events"] = {k: at(v) for k, v in kernel.items()}
        run["scripts"] = [(name, at(start), took / 1000)
                          for name, start, took in trace_facts["scripts"]]
    print(f"run {n}: login prompt {rel['login']:.1f}s after power on", file=sys.stderr, flush=True)
    return run


def phases(run: dict) -> list[tuple[str, float | None]]:
    s, k = run["serial"], run.get("kernel_events", {})
    span = lambda a, b: (b - a) if a is not None and b is not None else None  # noqa: E731
    menu_end = s["kernel_load"]
    rows = [
        ("firmware: power on to the boot loader", s["loader"]),
        ("boot loader: disks, config, menu drawn", span(s["loader"], s["menu"] or menu_end)),
        ("boot menu wait (autoboot_delay)", span(s["menu"], menu_end)),
        ("loading the kernel and modules", span(menu_end, s["kernel"])),
    ]
    init = k.get("init(8) starting")
    rc_done = k.get("/etc/rc finished")
    if init is not None and rc_done is not None:
        rows += [
            ("kernel: devices, root file system", span(s["kernel"], init)),
            ("startup scripts (rc)", span(init, rc_done)),
            ("rc done to login prompt", span(rc_done, s["login"])),
        ]
    else:
        rows.append(("kernel and startup scripts", span(s["kernel"], s["login"])))
    rows.append(("total: power on to login prompt", s["login"]))
    if "glassd_ready" in run:
        rows.append(("glassd ready (after power on)", run["glassd_ready"]))
    return rows


def fmt(values: list[float]) -> str:
    if not values:
        return "      -"
    med = statistics.median(values)
    rng = f"  ({min(values):.1f}-{max(values):.1f})" if len(values) > 1 else ""
    return f"{med:6.1f}s{rng}"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--trace", action="store_true", help="per-script times through boottrace(4)")
    ap.add_argument("--top", type=int, default=15, help="slowest rc scripts to list (with --trace)")
    ap.add_argument("--timeout", type=float, default=900, help="seconds to wait for the login prompt")
    ap.add_argument("--json", type=Path, help="also write every run's numbers here")
    args = ap.parse_args()

    if subprocess.run([str(VM), "status"], capture_output=True).returncode != 0:
        sys.exit("timeline: the VM isn't running (vm.sh up && vm.sh wait)")
    set_tracing(args.trace)
    try:
        runs = [one_run(n + 1, args.trace, args.timeout) for n in range(args.runs)]
    finally:
        if args.trace:
            set_tracing(False)

    cpus = os.environ.get("VM_CPUS", "default")
    accel = "KVM" if os.access("/dev/kvm", os.W_OK) else "emulated (TCG)"
    print(f"\nBoot timeline: cold boot, VM_CPUS={cpus}, {accel}, {len(runs)} run(s)"
          f"{', traced' if args.trace else ''}; median (min-max)\n")
    table = [phases(r) for r in runs]
    for i, (name, _) in enumerate(table[0]):
        values = [t[i][1] for t in table if i < len(t) and t[i][1] is not None]
        print(f"  {name:<42} {fmt(values)}")

    if args.trace:
        took: dict[str, list[float]] = {}
        for r in runs:
            for name, _, secs in r.get("scripts", []):
                took.setdefault(name, []).append(secs)
        ranked = sorted(took.items(), key=lambda kv: -statistics.median(kv[1]))
        print(f"\n  slowest rc scripts (traced, each one program start slower than untraced):")
        for name, secs in ranked[: args.top]:
            print(f"    {name:<44} {fmt(secs)}")
        ours = [(n, s) for n, s in ranked if OURS.search(n)]
        if ours:
            print("\n  ours:")
            for name, secs in ours:
                print(f"    {name:<44} {fmt(secs)}")
        print(f"\n  {len(took)} rc scripts ran; median {statistics.median(statistics.median(v) for v in took.values()):.2f}s each")

    if args.json:
        args.json.write_text(json.dumps(runs, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
