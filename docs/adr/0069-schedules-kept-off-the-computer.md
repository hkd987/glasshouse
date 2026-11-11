# 0069. Schedules are kept off the computer, and any host runs them

Status: **Accepted, built** (2026-10-28). The owner: "The cron jobs should
live in a db that then kicks off the job on the users machine. Since if the
machine is wound down it won't run."

## Context
- A person's computer sleeps when unused (ADR 0065 §11, ADR 0066): its
  disk goes to S3 and nothing of it runs. Cron inside it would run only
  while it happened to be awake.
- Hosts come and go. The Auto Scaling group replaces them, and a host
  being replaced puts its computers to sleep for another to wake.
  Anything kept only on one host goes with it.
- People already ask their agents for work from the web app, `glass` and
  Teams. What they couldn't do was say "every weekday at nine".

## Decision
1. **A schedule is directions to one of the person's agents, on a
   schedule:** a persona (ADR 0068), what to ask it, when, and a time
   zone. "When" is written as routines write it: `weekdays 09:00`,
   `daily 18:30`, `mon,thu 08:30`, `hourly`. The zone is an IANA name
   (`Europe/London`); `glass` sends the computer's own.
2. **Schedules are kept off the computer**, where every host reads them:
   - on an agent host, a DynamoDB table the stack makes. It's keyed by
     the person's key in the store and the schedule's id, encrypted under
     the stack's key, recoverable to any second of the last 35 days, and
     kept when the stack is deleted;
   - on a laptop or a development host, a file in glassd's state.
3. **Every host looks for what's due every half minute,** through a
   sparse index (`due`) that only schedules that are on are in. The first
   host to claim a slot runs it:
   - **The claim** is a write of the whole schedule that holds only if
     its `version` is still the one the host read (`PutItem` with
     `version = :seen`). A second host's claim fails. So does a claim
     read through an index that lags a write, or one racing the person
     turning the schedule off.
   - **Running it:** the claiming host starts a task as the person, on
     their computer. If the computer is asleep, the task wakes it from the
     store. A host that never saw the person makes them from the
     schedule's sign-in subject, name and email, which is all it keeps of
     them, and records that it acted for them because of their schedule.
   - **How it went** is written back when the task ends: done, failed or
     stopped.
4. **Nothing is replayed:**
   - A slot late by more than an hour (no host was up) is recorded as
     missed, and the next slot is counted from now.
   - A slot that comes while the last run is still going lets that run
     carry on and passes. On the host running it, the task's tile says
     whether it's going. Seen from another host, it's taken as going for
     six hours from its claim.
5. **Limits:**
   - 20 schedules on per person.
   - Directions of 2,000 characters at most.
   - Ten runs in a row that fail or can't start turn the schedule off,
     saying why. Turning it on again forgets them.
6. **Only the person's own:** each change checks the caller's key in the
   store against the schedule's. A schedule names only a persona on the
   caller's own computer, checked when it's made and again at each run. A
   persona removed since makes a refused run.
7. **The table is trusted as the hosts' own state is.** A host acts for
   the person a schedule names, so whoever can write the table can start
   tasks as anyone in it. The same goes for the bucket's computer records
   (ADR 0066). Only the hosts' role may write it: IAM, the VPC endpoint's
   policy, and the account's own administrators. What it holds of a
   person (their sign-in subject, name and email, and their directions)
   is encrypted under the stack's key.
8. **On the record:** making, turning off and on, and removing a schedule;
   each run started, each slot let pass, each refusal; and the session a
   host opened for the person (`EventKind::Schedule`, `Session`).
9. **A host being replaced claims nothing,** and refuses a task it
   claimed just before it was told. That slot is recorded refused; the
   next one runs on another host.

## Why not routines
Routines (DA-4, ADR 0044, ADR 0047) are a desktop feature:
- a saved task with a script that does it with no model;
- versions the person approves;
- run in the computer's local time, by the glassd on that laptop;
- kept in root's files and the person's home there.

Schedules are agent-host directions:
- a prompt to a persona;
- on a computer that is usually asleep;
- kept centrally;
- run by whichever host is up.

Their "when" is written the same way, and their runs are tasks like any
other, under the same tiers, audit and undo.

## Proof
- **`ScheduleClaims.tla`**, two configurations, checked by `make tla`:
  two hosts look through an index a write behind, claim, start, end,
  crash and are replaced while the person turns the schedule off and on.
  - Invariants: a slot starts once, never while off, never on a host
    being replaced, never beside a run still going.
  - Its Old switches fail as they should.
  - TLC found a real bug in the code as first written: a slot two hours
    late, marked missed over a run still going, let the next slot start a
    second run beside it. The fix checks for a run going before
    lateness. `adr0069_a_late_slot_never_starts_a_second_run_beside_one_going`
    reproduces it.
- **Runtime tests (`tests/it/computers.rs`, `adr0069_*`):**
  - a schedule wakes its sleeping computer, runs once and records the
    run;
  - a slot late by more than an hour is let pass;
  - a host being replaced claims nothing;
  - refusals: agents that aren't the caller's, unreadable schedules and
    zones, a zone that's a path, no directions or too many, the 21st;
  - another person sees and changes none of them;
  - ten failures turn a schedule off, and turning it on forgets them;
  - zones follow daylight saving (New York, the day before clocks go
    forward).
- **Against DynamoDB in moto** (`make aws-local-test`):
  - `adr0069_local_aws_one_claim_holds`: an id is taken once, the second
    claim fails, and a schedule turned off leaves the index;
  - `tests/local-aws/schedules.sh`, 31 checks on two hosts sharing the
    table. Dana's first schedule runs once with both hosts looking. Host
    A is told it's being replaced and sleeps her computer to S3. Her next
    schedule, in London's time, runs on host B, which never had her
    computer; B wakes it and her file from host A is there. A schedule
    she turned off doesn't run. Sam sees none of hers and can't change or
    direct them (404). Bad schedules, zones, directions and fields are
    400; a 21st is 409.

## Open
- **More than one host needs the lease** (ADR 0066, open). Nothing yet
  says which host has a computer awake. With two hosts, one may claim a
  run while the person's computer is awake on the other: its task finds
  no computer on that host, and the run is refused. A host that found the
  computer asleep in the store could wake a second copy. The stack's
  default is one host (`MinHosts` = `MaxHosts` = 1), where none of this
  arises. The lease comes before a stack runs more than one, and
  `ScheduleClaims` then gains it as a second actor.
- **A run longer than six hours,** seen from another host, is taken as
  over: the next slot may start beside it. With one host, the task's tile
  decides and there's no limit.
- **Minute slots only.** No "every 15 minutes" yet. The web app has no
  schedules page yet; the API, `glass` and the record do.

## Amended 2026-11-11 (audit 4.11): a schedule a host can't read is left out
The table is shared by hosts that are a version apart while an update goes
round. A host failed its whole answer on one item it couldn't read: one
schedule written by the newer version (who it runs as, of a kind the older
doesn't know) made "what is due" fail on every older host every half
minute, so they ran nothing for anyone, and that person could neither list
nor add schedules there. An item damaged by hand did the same for good.

- A host leaves such an item out of both answers (a person's schedules,
  what is due) and logs it once, by owner, id and version, never by what
  it says. A host that can read it runs it.
- Left out, it is untouched: a write is one item's own and only over a
  version the host read, and removing by id still works.
- A person's count toward the twenty leaves it out too, on that host.
- The file store (a laptop, a development host) is one host's own file:
  unreadable, it is an error as before, never read as less than it holds
  and written over.

- A host keeps what it doesn't know in a schedule it does read, and
  writes it back with the rest (`StoredSchedule::other`). A claim writes
  the whole schedule, and an older host wrote it back without what the
  newer one had added. This is for the schedule's own fields: one added
  inside who it runs as, or inside its last run, an older host still
  drops when it writes those.

Still open from the same finding: nothing says which version of the
format an item is.

`tests/local-aws/schedules.sh` runs its two hosts with such a schedule in
the table from start to end.
