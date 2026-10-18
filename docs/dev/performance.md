# Keeping the OS fast for the people using it

Build speed is in [build-performance.md](build-performance.md). This page is
about the other kind: how long a person waits on Glasshouse.

## The budgets

`glass-e2e perf` (part of `make e2e-vm`) measures each of these on a live
glassd in the dev VM and fails when a 95th percentile goes over its budget:

| What the person waits for | Budget (p95) | Source | VM today (p50 / p95) |
|---|---|---|---|
| Handoff card (`task.plan`) | 200 ms | ceiling | 3 / 4 ms |
| Tiles (`task.list`) | 100 ms | ceiling | 3 / 4 ms |
| Expanded view (`task.get`) | 200 ms | AW-10 | 4 / 4 ms |
| Take over | 1 s | AW-20 | 203 / 245 ms |
| An action that needs no snapshot (reads, lookups) | 100 ms | ceiling | 3 / 12 ms |
| An action that snapshots first (writes) | 750 ms | ceiling | 243 / 391 ms |
| Start to the agent's first step | 4 s | ceiling | 1.7 / 2.6 s |
| Undo preview (`task.undo` dry run) | 2 s | ceiling | 0.8–0.9 / 1.0–1.3 s |

The VM emulates its CPU (no KVM in the cloud containers), so it's several
times slower than a laptop, and starting a process alone costs ~70 ms in it.
Read the table this way:

- **PRD targets** (AW-10, AW-20) must hold even in the VM, which leaves room
  to spare on real hardware.
- **Ceilings** are about 1.5× what the VM measures today (the start of a
  range when it varies run to run). They're there to catch a change that
  makes something slower. When you make something
  faster, lower its ceiling in `tests/e2e/src/perf.rs` and update this table.

Action times come from the audit trail: every action record carries
`detail.elapsed_ms`, the OS's time from the agent's call to the outcome
(`crates/svc/runtime/src/execute.rs`). For an Ask it includes the person's
decision, so the perf scenario leaves Asks out.

## Where the time goes

- **Snapshots.** Every write takes a ZFS snapshot first (AU-20), and so do
  task start and take over. A snapshot is one `zfs snapshot` call: the
  snapshotter remembers which dataset holds each granted folder (checked
  against the folder's device), so it doesn't run `zfs list` per folder
  each time. That cut take over from 355 to 203 ms and writes from 367 to
  243 ms in the VM.
- **`zfs diff`.** Undo and hand back ask ZFS what changed since a snapshot.
  It walks every change in the whole dataset (the user's home) since then,
  not only the task's folders, so it costs most of a second in the VM and
  grows as the home churns and the task ages. It's the whole undo preview
  today.
- **Sandbox start.** Creating the jail, mounting the agent's directories and
  the first snapshot make up most of "start to first step".
- **Audit.** Records are written with `fsync` (`Durability::Sync`), a few
  milliseconds each in the VM.

## Rules for new code

1. **Anything a person waits on gets a budget** in `glass-e2e perf` before
   it ships, with the PRD id when the PRD names a number (AW-10, AW-20, PM-1,
   UI-7).
2. **No process spawns on hot paths** when a cached answer is safe. Starting
   `zfs`, `jexec` or `service` is cheap on a laptop and slow in the VM, which
   makes the VM a good place to notice them.
3. **Keep blocking work off the async runtime**: file system, ZFS and
   process calls go through `spawn_blocking`, as `execute.rs` does, so one
   slow call doesn't stall every other task's tile.
4. **Read paths stay in memory.** `task.list` and `task.get` answer from the
   runtime's state without touching disk; keep it that way.
5. **Measure before optimizing**: run `glass-e2e perf` in the VM
   (`su demo -c '/usr/local/libexec/glasshouse-tests/glass-e2e perf'`) before
   and after, and say both numbers in the commit message.

## Known gaps

- **Snapshot retention (AU-20, 30 days).** Snapshots are never destroyed
  yet. They cost nothing until data diverges, but they will hold disk space
  on a busy machine.
- **Undo driven by the broker's change records.** Agents change files only
  through actions (ADR 0005), so the runtime already knows every path a
  task changed. Restoring just those paths would make undo cost what the
  task changed rather than what the home changed, and would stop undo from
  rolling back the user's own edits in the same folder (ADR 0006's known
  limit). It needs take-over edits recorded as changes too.
- **GUI budgets** (cold launch under 1 s, UI-7; resume under 2 s, PM-1)
  wait for the GUI and real hardware.
