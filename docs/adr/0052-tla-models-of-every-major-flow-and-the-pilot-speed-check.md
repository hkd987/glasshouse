# 0052. TLA+ models of every major flow, and the pilot's speed check

Status: **Accepted, built** (2026-10-24). The owner: "We should do TLA+ on
all major app flows and speed check all major parts of the app looking for
improvements… fix gaps along the way. This is the final big push for
quality before our pilot. We should also look at our build as part of this
and see if we can speed it up or improve its size." Follows ADR 0051, whose
conventions it keeps (`specs/tla/README.md`).

## Context
- ADR 0051 modelled approvals, undo, studio versions, routines, managed
  sync and mail. The rest of what a pilot uses runs concurrently too:
  - chat across computers and the console;
  - shared docs and artifacts;
  - the model gateway, and the org's model credits;
  - apps shared through the store, and skills;
  - meetings;
  - take over and hand back, and a managed computer's enrollment;
  - calendar invitations, and the layers that make a setting.
- Its speed audit covered what an agent calls on a person's data. Chat
  histories, the console's pages, the gateway's per-call work and the build
  weren't measured.

## Decision
### 1. A model of each, checked on every CI run
Each model follows the code step by step, and is checked by `make tla`.
Every fix below came with a test that failed on the old code, unless it
says the model alone shows it; each fix was switched off in its model to
show it's needed.

| Model | States | TLC, one worker, four models at once |
|---|---|---|
| ChatSync | 118,903 | 8 s |
| ChatPost | 99,814 | 5 s |
| DocsCollab (three configurations) | 40,222 / 90,799 / 169,441 | 16 / 37 / 44 s |
| ArtifactsShare | 6,237 | 2 s |
| ModelGateway (three configurations) | 133,008 / 138,973 / 16,696 | 14 / 12 / 5 s |
| ModelCredits | 166,335 | 7 s |
| StoreShare | 288,504 | 36 s |
| SkillMerge | 67,866 | 16 s |
| MeetKeys | 329,554 | 25 s |
| MeetTranscript | 16,038 | 2 s |
| TakeOver | 205,574 | 8 s |
| DeviceEnrollment | 1,234 | 2 s |
| SettingsLayers | 363,862 | 12 s |
| CalendarInvite | 9,894 | 2 s |

- ADR 0051's Approvals model changed with take over (a driven task stays
  driven): 524,441 / 377,835 / 29,944 states.
- `make tla` checks all 29 configurations, four at once with a worker each
  (`TLA_JOBS`, `TLA_WORKERS`), in about two minutes on four cores. One at a
  time with two workers it took five.

### 2. What the models found, and the fixes
**Chat** (CF-91, CF-92, ADR 0030):
1. A store that couldn't be kept (a full disk) still moved the sync cursor
   on, so its messages never came again on that computer. The cursor moves
   only past what every person there kept, and glassd backs off.
2. Someone starting Chat while a sync was waiting lost messages sent
   meanwhile: the sync couldn't see the cursor's reset when it had read 0
   too. Resets are counted, and a sync that began before one leaves the
   cursor at the beginning.
3. A message sent before the person's own reply, but fetched after it, was
   never unread: the newest view marked everything seen up to the reply. It
   marks only up to what the computer has fetched.
4. When the console took a message and its answer was lost, Chat said
   "Nothing was queued; try again", and trying again posted it twice. It
   asks again with the same message, which the console takes or refuses as
   posted already; if it still can't tell, it says the message may have
   been posted. TLC caught a first fix that sealed it again after an
   old-epoch refusal: the console checks the epoch before the message's id,
   so that posted it twice.

**Where a task's data may go** (DP-2, DP-10, DP-11, MP-8, ADR 0018):
5. Rewriting a file dropped the label stored on it: a write renames a new
   file over the old one. The label goes on the new file before the rename,
   read through the open file, so a labeled file stays labeled whoever
   saves it.
6. What a task wrote carried nothing of what it had read, so a summary of
   a confidential file, saved where nothing is labeled, went where public
   data may (DP-11). `files.write` labels what it writes with the task's
   label, the higher of that and the file's, before the rename.
7. A read's label was taken at its preview. A write landing between the
   preview and the read handed the new content over under the old label.
   The labels are read again after the read. The model alone shows this
   one: the window is inside one call.
- Checked and holding: a call reaches only models approved for what its
  task had read; placeholders come back only as values their own call
  sent, and a replay's mean what they meant; failover and the desktop
  agent's ladder stay on the route; a call is billed twice only after a
  lost reply.

**Model credits** (ADR 0039, ADR 0040): a payment is granted once however
often the relay pushes it, its answer is lost or a save fails, and the
key's limit never exceeds what was bought. Two limits it states:
- spend can pass the limit by the calls already under way when it's
  reached (at most the console's 64 permits);
- a call is billed twice only when credits charged it and its answer was
  lost, so the next model took it too.

**Sharing apps through the store** (GA-8, GA-13, DV-11, ADR 0032):
8. IT's Approve named the item, not the version IT reviewed: a version
   shared while IT read the one before went to every computer unseen.
   Approve names the version now, and another is refused (409) with the
   reason.
9. A bundle whose fetch failed after its sync wasn't fetched again until
    IT's next catalog, so an approved app never reached that computer. A
    missing bundle is fetched again at a later sync, at most once a minute;
    refusals on these retries are only logged.

**Skills** (ADR 0034, SK-3):
10. Two merges on one version at once both became its next version, and a
    change made on the dropped copy then replaced the first merge's change
    everywhere. A version counts only if its proposal was made on the
    version before it (number and SHA-256) and wasn't merged already;
    merge checks the same.

**Take over and hand back** (AW-20, AW-21, AW-22, AU-10, PM-5, ADR 0015):
11. Pausing a task the person drove ended the drive without a hand back;
    a resume then ran the agent and their edits were never recorded. A
    timed-out ask or a rollback did the same. A driven task stays driven
    until its hand back, and Pause says so.
12. A second take over took a new snapshot and hid what the person had
    changed since the first. It changes nothing now.
13. Two hand backs at once: the second let the agent go on while the first
    was still working out the person's edits, so an agent's write could be
    recorded "by you", as could a step under way at take over. Take over's
    snapshot and hand back take the person's writing turn.
14. A pause landing during a hand back was lost, and a hand back landing
    before take over's snapshot left that snapshot on a running task. Only
    a hand back ends a drive.
15. Taking over a task paused for sleep: the wake ended the drive. The
    take over replaces the sleep's pause.

**A managed computer's enrollment** (ADR 0030 slice P, DV-1, AD-4):
16. A computer IT removed and then forgot heard "isn't registered" and
    registered its old key again: it was back in the org by itself. The
    console keeps forgotten keys and refuses them as removed. A computer
    set up again, with a new key, enrolls.
17. A long poll answered with what IT released while it waited, even after
    IT removed the computer meanwhile; forgotten meanwhile, the answer made
    a keyless entry on the Devices page. Long polls and reports check the
    key again before answering.
18. glassd stopping while it made the device key left the file empty:
    "damaged", never syncing again. The key is written whole before it gets
    its name, and an empty one is made again.

**Settings** (UX-1, ADR 0029, ADR 0031):
19. The desktop's settings change and the Settings app's, at once, could
    lose one or fail to save. Changes take turns in the settings handle.
20. A release that only moved someone into or out of a group told nobody,
    so USB drives, the browser's settings and the desktop's locks stayed as
    the old group had them. Members count as a change.

**Calendar, and every ask** (DA-11, NV-3, CF-21):
21. What the person approved could differ from what ran: an invitation to
    an event moved, or made a meeting, while they decided went out as it
    then was. Once they answer, the call is previewed again under the
    person's turn and refused, in words the agent can read, if it changed.
    This holds for any approved call (a delete of a folder that gained a
    file). Model calls carry what they send in their input, and are exempt.
22. Undoing the change that made an event a meeting, after its invitations
    went out, took the meeting off the organizer's event. Calendar's revert
    keeps it.

**Docs** (DC-3, DC-4, ADR 0035):
23. A computer keeps its own post at once, so another computer's earlier
    edit, still on its way, arrived behind the cursor: that computer's
    copy, pages and snapshots never had it, and when it was a change of
    roles, the demoted person's computer never heard. A copy moves only as
    far as the sync has everything (`Shared::through`); its own posts
    beyond that wait as "ahead", and an earlier message arriving late that
    involves a change of roles loads the copy again, in order.
24. A version named while an edit was on its way dropped that edit for
    every fresh reader: opening skipped every edit before the newest
    snapshot. It skips only what the snapshot covers.
25. A fresh reader could see an empty doc: opening chose the newest
    snapshot by who edits now, then refused it by who edited then. It
    chooses the newest whose sender could edit when they posted it.
26. A page lost an edit made just before its author became a viewer: each
    edit was judged by who edits now. Pages get what their computer's copy
    accepted.
27. Someone who stopped editing while their edit was on its way kept it on
    their own computer. Their copy loads again from the log.

**Artifacts** (AR-3, AR-4, ADR 0033):
28. Two updates at once on one computer took the same version number, and
    the later was never shown though its call named it. Updates take turns
    on each computer.


**Meetings** (CF-21, ADR 0030 §6, ADR 0045):
29. A computer that dropped out of a call (its window closed, asleep, off
    the network) never left it: it could open everything said after, new
    keys were still sealed to it, it kept one of the 25 seats, and a call
    everyone dropped out of kept its keys on the console. A Leave was
    usually missed too: Meet closes its connection first and tells glassd
    after the transcript's last lines, by when the others had been told no
    key was due. The console asks the relay who is still connected before
    it answers for keys, and every 30 s.
30. The first version of that fix took someone joining for someone who had
    dropped out. The relay's answer counts only for seats taken before it
    was asked (a stamp per seat).
31. A page stopped asking for a new key after six tries, even while one
    was due, so whoever left kept hearing. It asks for as long as one is.
32. A page that had just joined never asked for keys, so a key made while
    it joined could leave it hearing nobody. It asks once it's in. The
    model alone shows this one: the page's loop needs a screen.
33. An answer lost after glassd added transcript lines made Meet send them
    again: twice in the transcript, and "Hey Glass" started the agent
    twice. A batch keeps its id until glassd has it, and glassd answers one
    it has had as it did then (the last 64 per person and meeting).
34. With the organization's server out of reach, Meet took the failure for
    IT turning transcripts off: it stopped hearing and dropped the lines
    waiting. It's "not added yet" now, and Meet sends them again.

**Found while checking the fixes:**
- A FIFO put where a file was blocked the provider's open until something
  wrote to it; with a write's label read, that held the person's writing
  turn. Files are opened without waiting and refused unless regular.

### 3. The speed check
Each model's author also looked for what grows with a pilot's data on a
hot path. What was fixed:
- **The console's chat.** Each post rewrote the org's whole history, and
  each computer's sync, and each post's check for a message posted again,
  read every message's header, all under the console's one lock. At
  20,000 messages (a pilot's few weeks) a post took 80 ms and each waiting
  computer's sync 55 ms (release build), and a post wakes every waiting
  computer: with 30 of them, 1.6 s of the lock per message, every other
  request waiting.
  - Messages are a log (`chat-messages.jsonl`), a line per post; a sync
    starts past its cursor; posted ids are an index made once. Messages an
    older console kept in `chat.json` move to the log at its first start.
  - At 20,000 messages: a post 0.6 ms, a sync 0.7 ms
    (`speed_chat_on_a_pilots_history`, run by hand).
- **Docs:** every pull, push, open and read parsed the doc's whole log.
  A log only grows, so Chat's store reads only what was appended: at
  10,000 updates (a snapshot every 200), a pull with nothing new 170 ms to
  1.8 ms, an open in memory 85 ms to 5.6 ms, a read 83 ms to 10 ms, a push
  157 ms to 77 ms (`dc1_opening_a_doc_with_10000_updates`, run by hand).
  The first open on a computer still decrypts every update: about 40 ms a
  thousand, so DC-1's second passes at about 25,000.
- **Chat on a computer:** a history arriving at once (someone new, or
  added to a busy channel) was checked against every message kept, pair
  by pair, under the computer's chat lock: 11 s for 20,000 messages in a
  debug build. Headers are hashed once per sync: 0.18 s.
- **The console's catalog page** parsed IT's policy draft once per item:
  829 ms for 300 items, 24 ms now (debug build).
- **Meetings:** each meeting action, a key request or 12 s of transcript,
  opened every meeting the person was ever in to find one. It opens the
  one.
- **Skills** were read in time quadratic in their versions and
  proposals; each is looked up once.

Measured and left as they are:
- **The gateway's share of a step** is linear in the conversation it
  sends: 4.8 ms at 10,000 characters, 35 ms at 100,000 (debug build),
  60% of it redaction; under 1% of a step's model time.
- **glassd's start** in the emulated VM: 0.4 s to check its configuration,
  then 0.2–0.8 s to ready; idle, it holds 22 MB.
- **Noted for later**, small at a pilot's size: `calendar.list` parses
  every event each time; the console writes the whole fleet every 2 s
  under its lock; a computer's chat reads a person's whole store per
  action; glassd reads `installed.json` once per org app it lists.

### 4. The build
Measured on the 4-core container:

| | Before | After |
|---|---|---|
| The runtime's integration tests | 23 binaries, 1.7 GB | one (`tests/it`), 96 MB |
| Rebuilding them after a runtime change | 33 s | 17 s |
| Running them | 37 s | 24 s |
| Test binaries in `make test` | about 114 | 92 |
| Shipped binaries, release build | 65 MB | 41 MB |
| Release build of them, from clean | 402 s | 390 s |

- **One test binary for the runtime.** Each of its 23 test files was its
  own binary, linking the runtime and every app again (about 70 MB, 22 MB
  of it code): the modules of `tests/it` now, sharing `common`. Their tests
  run in one process, in parallel across what were files.
  `cargo test -p glass-runtime --test it chat::` runs one module's.
- **Release binaries without symbols, in one codegen unit.** Nothing ships
  with backtraces on, so the symbol table only made each binary a quarter
  bigger; a panic still says what and where. One codegen unit took 18% more
  off (the desktop agent, with its JavaScript engine, 30%) and built as fast
  here, the dependencies' build being most of the time. The `vm` profile
  keeps its symbols and 256 codegen units. glassd is 13.3 MB, glassctl
  9.6 MB, the desktop agent 8.7 MB; the desktop's binaries, built the same
  way, 4.3 to 7.1 MB each.
- **What's left as it is:**
  - glassctl links every app for `glassctl policy check`'s warnings (its
    own code is 0.5 MB of its 9.6);
  - Meet ships 42 MB: the speech model (27 MB, quantized) and ONNX
    Runtime's WebAssembly (14 MB); the other apps are 28 to 436 KB;
  - the crates built twice in two versions (`rand`, `hashbrown`,
    `getrandom`, `io-lifetimes`, `syn`, `toml`'s parsers) come from boa,
    yrs, str0m and cap-std, and no update resolves them yet.

## Consequences
- **Approved calls hold the person's turn** from their second look to their
  record, so a slow approved send (SMTP) delays that person's other writes
  while it runs. Reads don't take turns.
- **Files carry what they were made from:** a task's writes to the
  person's files are labeled with what it had read. Writes an agent makes
  outside actions (a harness's own shell in its jail) aren't labeled.
- **Approve names a version** in the console's API
  (`POST /api/catalog/<id>/approve` takes `version`).
- **Take over waits for a step under way** to be recorded before its
  snapshot; the hold itself is still immediate.

## Not covered
- **Chat:** removing members (not built), several computers per person
  (gap filling, CF-41), retention and holds (C3), edit and delete folding
  (argued from the pure fold, not checked).
- **Gateway:** a summary saved through other apps' writes (mail drafts,
  app stores) isn't labeled; replays across tasks; providers' own routing.
- **Store and skills:** editions, pins and rings, team owners' approval;
  a merge said to have won before a sync shows a crossing one did.
- **Seat:** sign-in and sign-out (ADR 0031 doesn't pause a person's tasks
  when they leave), glassd restarting mid-drive (tasks are in memory).
- **Calendar:** no service sync yet; double booking isn't promised;
  invitations carry no time zone.
- **Meetings:** removing someone mid-meeting (membership is fixed once
  invited); ICE timeouts; the relay refusing a join; the 2 s grace for old
  keys on frames in flight. glassd remembers transcript batches in memory,
  so one whose answer was lost as glassd restarted is added again.
- **Docs and Artifacts:** comments and resolves in Docs; two windows on one
  computer (one page in the model); the owner updating an artifact from two
  computers at once (the console would have to number versions).
- **Tailoring** has no model: a layer only ever narrows, under one lock.

## Amended 2026-11-11 (audit 9.2, 4.3): a person's chat store is parsed once

"A computer's chat reads a person's whole store per action" was noted above
for later. It was worse than one read: an action asked for the store five
to eight times (a post: seven, and the sync that follows it about seven
more), and each parsed all of a person's chat again. Docs' typing batches
go through the same store, so Docs slowed as chat history grew.

- The store as last read or saved is kept in memory with what its file
  was (length, time, which file). Asked for again while the file is as it
  was, it is the same store. Once read, a post, the sync that brings it, a
  list and a read parse nothing (`adr0052_a_persons_store_is_parsed_once_
  however_often_an_action_asks_for_it`). A file written by anything else
  is read again. At most 64 people's stores are kept.
- A change takes a copy of its own (`load_to_change`) and saves it, under
  the same lock as before; what was saved is what is kept. The store is
  still written whole at each change: an append log per conversation is
  the next step if that shows up in measurements.
- A file that isn't a store (cut short) is set aside beside itself and
  chat starts again from what the console sends; it used to read as empty
  and be written over. One that is there and can't be read at all leaves
  chat empty for now, and nothing is saved over it.

The models don't change: what a read returns is what the file holds, as
before, and a store that can't be saved over is the full disk ChatSync
already has.

The same rule ("damaged is not empty") now holds for the other apps'
files: a person's choices in their home (Routines, the apps they made,
what they installed; `ConfinedDir::read_json_or_set_aside`) and glassd's
own (Settings, Skills, Mail's sign-in and sync state, Routines' run
records; `glass-state`). A typo in a file a person may edit no longer
makes every routine vanish and the next save keep only the new one.

