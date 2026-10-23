// Routines' logic (ADR 0047): schedules as the picker shows them and back,
// times as people say them, how a run went, names and folders.

import assert from "node:assert/strict";
import { test } from "node:test";

import { type Routine, type Run, choiceOf, choiceProblem, folder, nameFor, outcome, past, scheduleOf, upcoming, whenLine } from "../src/routines.ts";

test("adr0047 schedules read into the picker and back into the words routines take", () => {
  for (const words of ["daily 09:00", "weekdays 09:00", "weekends 10:00", "fri 17:00", "hourly"]) {
    assert.equal(scheduleOf(choiceOf(words)), words);
  }
  assert.deepEqual(choiceOf(null), { kind: "off" });
  assert.equal(scheduleOf({ kind: "off" }), null);
  // One it can't show is kept as it is.
  assert.deepEqual(choiceOf("mon,thu 08:30"), { kind: "other", text: "mon,thu 08:30" });
  assert.equal(scheduleOf(choiceOf("mon,thu 08:30")), "mon,thu 08:30");
  assert.equal(choiceProblem({ kind: "daily", at: "25:00" }), "Pick a time of day.");
  assert.equal(choiceProblem({ kind: "weekly", day: "fri", at: "17:00" }), null);
});

test("adr0047 times read as people say them", () => {
  const now = new Date(2026, 9, 23, 10, 0); // Friday 23 October, 10:00
  const at = (d: number, h: number, m = 0) => new Date(2026, 9, d, h, m).toISOString();
  assert.equal(upcoming(at(23, 17), now), "today at 17:00");
  assert.equal(upcoming(at(24, 9), now), "tomorrow at 09:00");
  assert.equal(upcoming(at(27, 9), now), "Tuesday at 09:00");
  assert.equal(upcoming(at(30, 17), now), "Oct 30 at 17:00");
  assert.equal(past(new Date(2026, 9, 23, 9, 59, 30).toISOString(), now), "just now");
  assert.equal(past(at(23, 9, 15), now), "45 minutes ago");
  assert.equal(past(at(23, 7), now), "today at 07:00");
  assert.equal(past(at(22, 17), now), "yesterday at 17:00");
  assert.equal(past(at(19, 17), now), "Monday at 17:00");
});

const routine = (extra: Partial<Routine> = {}): Routine => ({
  routine: "archive-newsletters",
  name: "Archive newsletters",
  version: 1,
  request: "Move the newsletters from Vendor News in my Inbox to Archive.",
  schedule: "fri 17:00",
  runs: "every Friday at 17:00",
  calls: ["mail_list", "mail_move"],
  read: [],
  write: [],
  paused: false,
  next_run_at: new Date(2026, 9, 23, 17, 0).toISOString(),
  last_run: null,
  note: "",
  saved_at: new Date(2026, 9, 16, 17, 0).toISOString(),
  ...extra,
});

test("adr0047 a routine's line says when it runs next, or that it's paused", () => {
  const now = new Date(2026, 9, 23, 10, 0);
  assert.equal(whenLine(routine(), now), "Runs every Friday at 17:00 · next today at 17:00");
  assert.equal(whenLine(routine({ paused: true, next_run_at: null }), now), "Paused · it runs only when you start it");
  assert.equal(whenLine(routine({ schedule: null, runs: "when started", next_run_at: null }), now), "Runs when you start it");
});

test("adr0047 a run says whether it needed a model", () => {
  const run = (extra: Partial<Run>): Run => ({
    task: "t",
    version: 1,
    scheduled: true,
    started_at: "",
    ended_at: "",
    outcome: "done",
    model: false,
    said: "",
    changes: 2,
    ...extra,
  });
  assert.deepEqual(outcome(run({})), { text: "Ran with no model", tone: "ok" });
  assert.deepEqual(outcome(run({ model: true })), { text: "The agent stepped in", tone: "stepped-in" });
  assert.deepEqual(outcome(run({ outcome: "failed" })), { text: "Didn't finish", tone: "bad" });
});

test("adr0047 a repeat's name starts from its words, and folders from home", () => {
  assert.equal(nameFor("move the newsletters from Vendor News in my Inbox to Archive."), "Move the newsletters from Vendor News");
  assert.equal(nameFor("Archive them."), "Archive them");
  assert.equal(folder("/home/demo/Mail", "demo"), "~/Mail");
  assert.equal(folder("/usr/home/demo", "demo"), "~");
  assert.equal(folder("/srv/shared", "demo"), "/srv/shared");
});
