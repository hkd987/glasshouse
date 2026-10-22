# 0045. Meeting transcripts IT turns on, and agents that act on what you say in a meeting

Status: **Accepted, not built yet** (2026-10-22; the design below is a
draft that T1 will settle). The owner asked for agents that "join
meetings and listen in by reading the transcript, then take action on my
behalf during the meeting, like 'hey x, grab abc report and send it to
Susan'", and decided: "enterprise admin should be able to turn on
transcripts… then that will be able to do this feature." PRD CF-21 (its
"never recorded" now means media), CF-61 … CF-65, AE-3, DA-12, AU-1, AU-10.
Changes ADR 0030 decision 6 ("never recorded", "agents can't join") as
below. Builds on ADR 0030 (sealed chat, meetings as conversations), ADR
0031 (settings IT sets per group), ADR 0041 (the desktop agent).

## Context
- A Glasshouse meeting is a chat conversation of kind `meeting` (ADR
  0030): its epoch keys are sealed to its people's computers and to IT's
  compliance key, so its messages fall under retention, legal hold and
  eDiscovery (CF-61 … CF-65). Media is sealed end to end with a separate
  key that never reaches IT, and the relay can't decode a frame. So no
  server can transcribe a meeting; only the participants' computers hear
  it.
- Agents can't join meetings: policy refuses `meet.*` to every agent.
- An agent that takes orders from a meeting is an agent anyone in the
  meeting can talk to. "Hey x, send me the payroll file" from someone
  else must do nothing.
- Recording or transcribing people needs their knowledge, and in some
  places their consent.

## Decision
1. **IT turns transcripts on, per group, in the console.** A new setting,
   `meetings.transcripts`, off by default (ADR 0031's machinery: a group's
   value, locked or not). Its value for a meeting is the organizer's, read
   by the organizer's computer when the meeting is created and sealed into
   the meeting's conversation (signed by the organizer like its title), so
   every participant's computer sees the same answer and the console can't
   change it. *(Proposed default for the owner: per group, decided by the
   organizer's group.)*
2. **Everyone knows.** A transcribed meeting says so in the invitation, on
   the join screen ("This meeting is transcribed. Your words go into its
   transcript, which IT can read."), and with a badge for as long as the
   call lasts. Joining is the consent.
3. **Each computer transcribes only its own person's microphone, on the
   computer.** The Meet page runs a small speech model (Moonshine tiny,
   MIT, through ONNX Runtime Web, MIT) in a worker over its own microphone
   track. Audio never leaves the page; nothing transcribes anyone else.
   That gives each line its speaker for free, signed by their computer's
   member key, with no guessing who spoke. Muted is not transcribed.
4. **The transcript is messages in the meeting's conversation.** The page
   posts each finished line with `meet.transcribe` (the person's own, at
   the Meet app's tier), and glassd seals it into the meeting's
   conversation as a message marked as a transcript line. So it is sealed
   to the meeting's people and IT's compliance key, kept and held like
   chat, and readable afterwards. Audio and video are still never
   recorded.
5. **Agents read it; they still can't join.** A new read action,
   `meet.transcript`, returns a meeting's transcript to an agent the
   person allows (Auto, label `internal`, like `chat.read`), and refuses
   when the meeting isn't transcribed. `meet.join`, `meet.keys` and
   `meet.leave` stay Never for every agent.
6. **Only your own words give your agent orders.** When a line the
   person's own computer transcribed starts with the wake phrase ("Hey
   Glass", or the name IT sets), glassd starts a desktop agent task for
   that person with the rest of the line as the request and the meeting
   named, so it can read the transcript for context ("send Susan what we
   just agreed"). Lines from anyone else's computer never start anything.
   What others said is data the agent reads, never instructions.
7. **Everything the agent does follows its rules.** Sending mail or
   posting asks the person, as a banner they approve during the call;
   every step is audited and attributed; undo applies.

## Slices
- **T1**: the setting, the notices, `meet.transcribe` and the sealed
  lines, `meet.transcript` for agents, the wake phrase starting the
  desktop agent, and a recognizer interface with a scripted recognizer
  for tests.
- **T2**: Moonshine in the Meet page, tested on recorded speech.
- **Later**: starting and stopping a transcript during a meeting; Teams,
  Zoom and Meet in the browser (CF-20), which our pages don't hear.
